//! Translations kept from one process to the next (ADR 0021, R084).
//!
//! With `R64M_CACHE=dir` (handed on to children as r64m's option `--cache`), a
//! process reads one file at its start: the one for its program file and r64m's own
//! build. It holds blocks as the translator gave them (`Built`), each with the
//! guest's bytes it was made from. A kept block is used only if those bytes are the
//! guest's now, byte for byte. The blocks a process made that the file did not have
//! are added as it ends or execs: a new file is written and renamed over the old.
//!
//! A file: `MAGIC`, how many blocks; the index, a (key, offset) for each block in
//! the order of the keys; then the entries, in any order (R087: the old ones as
//! they lay, the new ones after them).

use super::translate::Built;
use crate::mem::Mem;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

const MAGIC: &[u8; 8] = b"r64mtc03";

/// The cache of this process: where its file is, and what the file held when the
/// process began (mapped, never written; a rename by another leaves it as it was).
struct Kept {
    file: PathBuf,
    /// The file's bytes, or none if there was no file yet.
    bytes: &'static [u8],
    /// How many blocks the file holds; its index follows the header.
    n: usize,
}

static KEPT: OnceLock<Kept> = OnceLock::new();
/// The blocks made in this process that the file did not have, written out as they
/// will be in the file, one after another into one buffer (R087).
pub(crate) struct New {
    data: Vec<u8>,
    /// Each block's key and where its entry begins in `data`, in the order made.
    index: Vec<(u64, usize)>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl New {
    /// A child of a fork forgets its parent's new blocks: the parent writes them.
    pub(crate) fn forget(&mut self) {
        self.data = Vec::new();
        self.index = Vec::new();
    }
}

static NEW: Mutex<New> = Mutex::new(New { data: Vec::new(), index: Vec::new() });

/// The new blocks, held over a fork, that no other thread holds them as the child
/// is copied (R064's way to a child that waits for ever; R087).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn held() -> Option<MutexGuard<'static, New>> {
    NEW.lock().ok()
}

/// The key of a block: its address, and how it was made (ADR 0021, point 3).
pub fn key(rip: u64, ordered: bool) -> u64 {
    let counted = crate::cpu::COUNT.load(std::sync::atomic::Ordering::Relaxed);
    rip << 2 | (ordered as u64) << 1 | counted as u64
}

/// Whether a cache is in use in this process.
pub fn on() -> bool {
    KEPT.get().is_some()
}

fn fnv(h: &mut u64, bytes: &[u8]) {
    for &b in bytes {
        *h ^= b as u64;
        *h = h.wrapping_mul(0x100_0000_01b3);
    }
}

fn identity(path: &Path) -> Option<[u64; 4]> {
    use std::os::unix::fs::MetadataExt;
    let s = std::fs::metadata(path).ok()?;
    Some([s.dev(), s.ino(), s.size(), (s.mtime() as u64).wrapping_mul(1_000_000_000).wrapping_add(s.mtime_nsec() as u64)])
}

/// Opens the cache in `dir` for `program`. Nothing if the directory is not the
/// user's own or may be written by others (ADR 0021, point 5): r64m says so once.
pub fn open(dir: &Path, program: &Path) {
    use std::os::unix::fs::MetadataExt;
    let Ok(d) = std::fs::metadata(dir) else {
        eprintln!("r64m: the cache {}: not to be had; not used", dir.display());
        return;
    };
    if !d.is_dir() || d.uid() != unsafe { libc::geteuid() } || d.mode() & 0o022 != 0 {
        eprintln!("r64m: the cache {}: not a directory of this user's that only they may write; not used", dir.display());
        return;
    }
    let (Some(prog), Some(me)) = (identity(program), identity(Path::new("/proc/self/exe")).or_else(|| std::env::current_exe().ok().and_then(|p| identity(&p)))) else {
        return;
    };
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in prog.iter().chain(&me) {
        fnv(&mut h, &v.to_le_bytes());
    }
    fnv(&mut h, env!("CARGO_PKG_VERSION").as_bytes());
    let file = dir.join(format!("{h:016x}"));
    let bytes = map(&file).filter(|b| b.len() >= 16 && &b[..8] == MAGIC);
    let n = bytes.map_or(0, |b| u64::from_le_bytes(b[8..16].try_into().unwrap()) as usize);
    let bytes = match bytes {
        Some(b) if b.len() >= 16 + 16 * n => b,
        _ => &[][..],
    };
    let _ = KEPT.set(Kept { file, bytes, n: if bytes.is_empty() { 0 } else { n } });
}

/// The file's bytes, mapped for reading for the life of the process.
fn map(file: &Path) -> Option<&'static [u8]> {
    use std::os::unix::io::AsRawFd;
    let f = std::fs::File::open(file).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
    (p != libc::MAP_FAILED).then(|| unsafe { std::slice::from_raw_parts(p as *const u8, len) })
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The block kept for `key`, if there is one and the guest's bytes are what it was
/// made from.
pub fn find(key: u64, m: &Mem) -> Option<Built> {
    let k = KEPT.get()?;
    find_in(k.bytes, k.n, key, m)
}

/// The same, in a file's bytes `b` that hold `n` blocks.
pub(crate) fn find_in(b: &[u8], n: usize, key: u64, m: &Mem) -> Option<Built> {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let at = 16 + 16 * mid;
        let here = u64_at(b, at)?;
        if here == key {
            let off = u64_at(b, at + 8)? as usize;
            return read(b.get(off..)?, m);
        }
        if here < key {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

/// The pieces of the guest's code a block was made from: its own instructions and
/// each one read past them (`Built::read`), in order, those that touch made one.
fn pieces(rip: u64, b: &Built) -> Vec<(u64, u64)> {
    let mut all: Vec<(u64, u64)> = std::iter::once((rip, b.end)).chain(b.read.iter().copied()).collect();
    all.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(all.len());
    for (from, to) in all {
        match out.last_mut() {
            Some(last) if from <= last.1 => last.1 = last.1.max(to),
            _ => out.push((from, to)),
        }
    }
    out
}

/// An entry: end, seen (two words, the first above the second if none), then body
/// and the counts of pieces, words, starts, pads and lookups; each piece (where, how
/// long, its bytes); the words, starts, pads and lookups.
pub(crate) fn read(e: &[u8], m: &Mem) -> Option<Built> {
    let (end, seen_lo, seen_hi) = (u64_at(e, 0)?, u64_at(e, 8)?, u64_at(e, 16)?);
    let (body, nr, nw, ns, np, nl) = (u32_at(e, 24)?, u32_at(e, 28)? as usize, u32_at(e, 32)? as usize, u32_at(e, 36)? as usize, u32_at(e, 40)? as usize, u32_at(e, 44)? as usize);
    let mut at = 48;
    let mut now = Vec::new();
    for _ in 0..nr {
        let (from, len) = (u64_at(e, at)?, u32_at(e, at + 8)? as usize);
        at += 12;
        let kept = e.get(at..at + len)?;
        now.resize(len, 0);
        m.read_bytes(from, &mut now).ok()?;
        if now != kept {
            return None;
        }
        at += len;
    }
    let mut words = Vec::with_capacity(nw);
    for i in 0..nw {
        words.push(u32_at(e, at + 4 * i)?);
    }
    at += 4 * nw;
    let mut starts = Vec::with_capacity(ns);
    for i in 0..ns {
        starts.push((u32_at(e, at + 12 * i)?, u64_at(e, at + 12 * i + 4)?));
    }
    at += 12 * ns;
    let mut pads = Vec::with_capacity(np);
    for i in 0..np {
        pads.push((u32_at(e, at + 12 * i)?, u64_at(e, at + 12 * i + 4)?));
    }
    at += 12 * np;
    let mut lookups = Vec::with_capacity(nl);
    for i in 0..nl {
        lookups.push(u32_at(e, at + 4 * i)?);
    }
    let seen = (seen_lo <= seen_hi).then_some((seen_lo, seen_hi));
    Some(Built { words, starts, end, seen, read: Vec::new(), pads, lookups, counted: None, body })
}

/// Keeps a block just made at `rip`, to be written out as the process ends.
/// Nothing if no cache is in use or its bytes cannot be read.
pub fn keep(key: u64, rip: u64, b: &Built, m: &Mem) {
    if !on() || b.counted.is_some() {
        return;
    }
    if let Ok(mut new) = NEW.lock() {
        let at = new.data.len();
        if put(&mut new.data, rip, b, m) {
            new.index.push((key, at));
        }
    }
}

/// A block's entry, as `read` takes it. None if its bytes cannot be read.
#[cfg(test)]
pub(crate) fn entry(rip: u64, b: &Built, m: &Mem) -> Option<Vec<u8>> {
    let mut e = Vec::new();
    put(&mut e, rip, b, m).then_some(e)
}

/// A block's entry written at the end of `e`; false, and `e` as it was, if its
/// bytes cannot be read.
fn put(e: &mut Vec<u8>, rip: u64, b: &Built, m: &Mem) -> bool {
    let begun = e.len();
    let pieces = pieces(rip, b);
    let (lo, hi) = b.seen.unwrap_or((1, 0));
    for v in [b.end, lo, hi] {
        e.extend_from_slice(&v.to_le_bytes());
    }
    for v in [b.body, pieces.len() as u32, b.words.len() as u32, b.starts.len() as u32, b.pads.len() as u32, b.lookups.len() as u32] {
        e.extend_from_slice(&v.to_le_bytes());
    }
    for (from, to) in pieces {
        let len = (to - from) as usize;
        e.extend_from_slice(&from.to_le_bytes());
        e.extend_from_slice(&(len as u32).to_le_bytes());
        let at = e.len();
        e.resize(at + len, 0);
        if m.read_bytes(from, &mut e[at..]).is_err() {
            e.truncate(begun);
            return false;
        }
    }
    for w in &b.words {
        e.extend_from_slice(&w.to_le_bytes());
    }
    for (w, a) in b.starts.iter().chain(&b.pads) {
        e.extend_from_slice(&w.to_le_bytes());
        e.extend_from_slice(&a.to_le_bytes());
    }
    for l in &b.lookups {
        e.extend_from_slice(&l.to_le_bytes());
    }
    true
}

/// Writes out what this process added (as it ends or execs): the file's blocks and
/// the new ones, in a new file renamed over the old. A block another process wrote
/// meanwhile may be lost (the last rename wins): it is made again, never wrong. Not
/// written again for a few blocks more: unless the new are a sixty-fourth of what the
/// file holds, they are made again next time instead (R087).
pub fn save() {
    let Some(k) = KEPT.get() else { return };
    let new = match NEW.lock() {
        Ok(mut new) => std::mem::replace(&mut *new, New { data: Vec::new(), index: Vec::new() }),
        Err(_) => return,
    };
    if new.index.is_empty() || new.index.len() * 64 < k.n {
        return;
    }
    let Some((head, parts)) = layout(k.bytes, k.n, &new) else { return };
    let tmp = k.file.with_extension(format!("{}", std::process::id()));
    let written = std::fs::File::create(&tmp).and_then(|f| {
        use std::io::Write;
        let mut f = std::io::BufWriter::with_capacity(1 << 20, f);
        f.write_all(&head)?;
        for part in parts {
            f.write_all(part)?;
        }
        f.flush()
    });
    if written.is_err() || std::fs::rename(&tmp, &k.file).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// How long the entry at the start of `e` is.
fn entry_len(e: &[u8]) -> Option<usize> {
    let (nr, nw, ns, np, nl) = (u32_at(e, 28)? as usize, u32_at(e, 32)? as usize, u32_at(e, 36)? as usize, u32_at(e, 40)? as usize, u32_at(e, 44)? as usize);
    let mut at = 48;
    for _ in 0..nr {
        at += 12 + u32_at(e, at + 8)? as usize;
    }
    let len = at + 4 * nw + 12 * (ns + np) + 4 * nl;
    (len <= e.len()).then_some(len)
}

/// A file of the blocks of the file `b` (holding `n`) and the new ones: its header
/// and index, and the pieces of data to follow it. For a key in both, the new
/// entry: it was made because the file's did not fit the guest's bytes (R087).
/// Mostly the file's data as it lies then the new data, nothing copied; where an
/// entry of the file's is dropped, each entry kept is written on its own, so that
/// what nothing points to is not kept. None if `b` is not whole.
fn layout<'a>(b: &'a [u8], n: usize, new: &'a New) -> Option<(Vec<u8>, Vec<&'a [u8]>)> {
    let start = 16 + 16 * n;
    let old = if n == 0 { &[][..] } else { b.get(start..)? };
    // (key, which: 0 new, 1 the file's, where its entry is in its own data)
    let mut all: Vec<(u64, u8, usize)> = Vec::with_capacity(n + new.index.len());
    for i in 0..n {
        let at = 16 + 16 * i;
        let off = (u64_at(b, at + 8)? as usize).checked_sub(start)?;
        all.push((u64_at(b, at)?, 1, off));
    }
    all.extend(new.index.iter().map(|&(key, at)| (key, 0, at)));
    // Stable: of a key twice, the new entry, and of two new ones the first made.
    all.sort_by_key(|&(key, which, _)| (key, which));
    let had = all.len();
    all.dedup_by_key(|&mut (key, _, _)| key);
    let whole = all.len() == had;
    let data = (16 + 16 * all.len()) as u64;
    let mut head = Vec::with_capacity(16 + 16 * all.len());
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&(all.len() as u64).to_le_bytes());
    let mut parts = Vec::new();
    let mut off = data;
    for &(key, which, at) in &all {
        let from = if which == 1 { old } else { &new.data[..] };
        let here = if whole {
            data + if which == 1 { 0 } else { old.len() as u64 } + at as u64
        } else {
            let e = from.get(at..)?;
            let len = entry_len(e)?;
            parts.push(&e[..len]);
            off += len as u64;
            off - len as u64
        };
        head.extend_from_slice(&key.to_le_bytes());
        head.extend_from_slice(&here.to_le_bytes());
    }
    if whole {
        parts = vec![old, &new.data[..]];
    }
    Some((head, parts))
}

#[cfg(test)]
pub(crate) fn merged(b: &[u8], n: usize, new: &[(u64, Vec<u8>)]) -> Option<Vec<u8>> {
    let mut each = New { data: Vec::new(), index: Vec::new() };
    for (key, e) in new {
        each.index.push((*key, each.data.len()));
        each.data.extend_from_slice(e);
    }
    let (head, parts) = layout(b, n, &each)?;
    Some([&[&head[..]][..], &parts].concat().concat())
}
