//! Loading a program (ADR 0005): the segments of a static x86-64 ELF put into the
//! guest's memory, each with the protection its header asks, and the first stack made
//! as Linux makes it.
//!
//! An executable linked to a fixed address (`ET_EXEC`) is put there. One that is
//! not (`ET_DYN`) is put at `PIE_AT`; and if the program names an interpreter (the
//! dynamic linker), that file is put at `INTERP_AT` and is where the program begins
//! to run: it finds the program by the auxiliary vector, as it does under Linux
//! (ADR 0017, point 3).

use crate::mem::{Fault, MapError, Mem, PAGE, R, W, X};

/// How much stack a program gets. Linux's usual limit; mapped at once, not grown.
pub const STACK_SIZE: u64 = 8 << 20;

const PT_LOAD: u32 = 1;
const PT_INTERP: u32 = 3;
const ET_DYN: u16 = 3;
const AT_BASE: u64 = 7;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_SECURE: u64 = 23;

/// Where a program that is not linked to a fixed address is put, and where its
/// interpreter is: Linux's places for them with randomizing off, brought down to
/// fit the smallest reservation (2^40 bytes, ADR 0013).
pub const PIE_AT: u64 = 0x55_5555_4000;
pub const INTERP_AT: u64 = 0x7f_f7fc_0000;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;

// The entries of the auxiliary vector that are given (Linux's numbers).
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_ENTRY: u64 = 9;
const AT_RANDOM: u64 = 25;

#[derive(Debug)]
pub enum ElfError {
    /// What is wrong with the file, in words for the user.
    Bad(&'static str),
    /// The memory would not take a segment or the stack.
    Map(MapError),
}

impl From<MapError> for ElfError {
    fn from(e: MapError) -> ElfError {
        ElfError::Map(e)
    }
}

impl std::fmt::Display for ElfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ElfError::Bad(what) => write!(f, "{what}"),
            ElfError::Map(e) => write!(f, "the guest's memory would not take it: {e:?}"),
        }
    }
}

/// What the loader knows of a program once it is in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// Where it begins to run.
    pub entry: u64,
    /// Where its program headers are in memory (0 if no segment holds them), the size
    /// of one and how many: the program's C library reads them.
    pub phdr: u64,
    pub phent: u64,
    pub phnum: u64,
    /// The page after its highest segment: where `brk` begins.
    pub brk: u64,
    /// Where its interpreter was put, if it has one and that was loaded (`AT_BASE`);
    /// else 0.
    pub interp: u64,
}

fn bad<T>(what: &'static str) -> Result<T, ElfError> {
    Err(ElfError::Bad(what))
}

/// A little-endian number of `N` bytes at `at`; the callers have checked the length.
fn le<const N: usize>(img: &[u8], at: usize) -> u64 {
    let mut v = [0u8; 8];
    v[..N].copy_from_slice(&img[at..at + N]);
    u64::from_le_bytes(v)
}

/// A fault while r64m itself writes what it has just mapped to be written is a mistake
/// in the loader, not the file's.
fn own(r: Result<(), Fault>) {
    r.expect("the loader writes only what it mapped")
}

/// The file a program asks to be started by (`PT_INTERP`), if it names one.
pub fn interpreter(img: &[u8]) -> Option<Vec<u8>> {
    if img.len() < 64 || img[..4] != *b"\x7fELF" {
        return None;
    }
    let (phoff, phent, phnum) = (le::<8>(img, 32), le::<2>(img, 54), le::<2>(img, 56));
    for i in 0..phnum {
        let ph = phoff.checked_add(i * phent)? as usize;
        if img.len() < ph + 56 || le::<4>(img, ph) != PT_INTERP as u64 {
            continue;
        }
        let (offset, size) = (le::<8>(img, ph + 8) as usize, le::<8>(img, ph + 32) as usize);
        let name = img.get(offset..offset.checked_add(size)?)?;
        // Without the zero that ends it.
        return Some(name.split(|&b| b == 0).next().unwrap_or(name).to_vec());
    }
    None
}

/// Puts the program's segments into `mem`: where it is linked to be, or at `PIE_AT`
/// if it can be put anywhere.
pub fn load(mem: &mut Mem, img: &[u8]) -> Result<Loaded, ElfError> {
    load_at(mem, img, PIE_AT, None)
}

/// The same, of a file that is open: its pages are mapped where they can be, not
/// copied (R053).
pub fn load_file(mem: &mut Mem, file: &File) -> Result<Loaded, ElfError> {
    load_at(mem, file, PIE_AT, file.descriptor())
}

/// A file as r64m reads it to load it: the host's own mapping of it where there is
/// one (the kernel's pages, nothing read that is not looked at, given back when
/// this is dropped), or its bytes read whole. Node's is 130 MB, of which loading
/// looks at the headers.
pub struct File {
    file: std::fs::File,
    held: Held,
}

enum Held {
    Mapped(*const u8, usize),
    Read(Vec<u8>),
}

impl File {
    pub fn open(path: &str) -> std::io::Result<File> {
        let file = std::fs::File::open(path)?;
        let len = file.metadata()?.len() as usize;
        #[cfg(unix)]
        if len != 0 {
            use std::os::fd::AsRawFd;
            let p = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
            if p != libc::MAP_FAILED {
                return Ok(File { file, held: Held::Mapped(p as *const u8, len) });
            }
        }
        let bytes = std::fs::read(path)?;
        Ok(File { file, held: Held::Read(bytes) })
    }

    /// The host's descriptor, for mapping the file's pages into the guest.
    fn descriptor(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            Some(self.file.as_raw_fd())
        }
        #[cfg(not(unix))]
        None
    }
}

impl std::ops::Deref for File {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.held {
            Held::Mapped(p, len) => unsafe { std::slice::from_raw_parts(*p, *len) },
            Held::Read(bytes) => bytes,
        }
    }
}

impl Drop for File {
    fn drop(&mut self) {
        if let Held::Mapped(p, len) = self.held {
            unsafe { libc::munmap(p as *mut libc::c_void, len) };
        }
    }
}

/// Puts a program's interpreter into `mem` at `INTERP_AT`, and says in `program`
/// where: the program then begins at the interpreter's entry, which is given back.
pub fn load_interpreter(mem: &mut Mem, img: &[u8], program: &mut Loaded) -> Result<u64, ElfError> {
    load_interpreter_from(mem, img, None, program)
}

/// The same, of a file that is open (see `load_file`).
pub fn load_interpreter_file(mem: &mut Mem, file: &File, program: &mut Loaded) -> Result<u64, ElfError> {
    load_interpreter_from(mem, file, file.descriptor(), program)
}

fn load_interpreter_from(mem: &mut Mem, img: &[u8], file: Option<i32>, program: &mut Loaded) -> Result<u64, ElfError> {
    if img.len() >= 18 && le::<2>(img, 16) != ET_DYN as u64 {
        return bad("the interpreter is linked to a fixed address");
    }
    let interp = load_at(mem, img, INTERP_AT, file)?;
    program.interp = INTERP_AT;
    Ok(interp.entry)
}

/// `file`: the descriptor `img` is the bytes of, if its pages may be mapped.
fn load_at(mem: &mut Mem, img: &[u8], anywhere: u64, file: Option<i32>) -> Result<Loaded, ElfError> {
    if img.len() < 64 || img[..4] != *b"\x7fELF" {
        return bad("not an ELF file");
    }
    if img[4] != 2 || img[5] != 1 || le::<2>(img, 18) != EM_X86_64 as u64 {
        return bad("not an x86-64 ELF");
    }
    // What is added to every address the file names.
    let bias = match le::<2>(img, 16) as u16 {
        ET_EXEC => 0,
        ET_DYN => anywhere,
        _ => return bad("not an executable (a file to be linked, or a core)"),
    };
    let entry = le::<8>(img, 24).wrapping_add(bias);
    let phoff = le::<8>(img, 32);
    let phent = le::<2>(img, 54);
    let phnum = le::<2>(img, 56);
    if phent < 56 {
        return bad("the program headers are too small");
    }
    match phnum.checked_mul(phent).and_then(|n| n.checked_add(phoff)) {
        Some(end) if end <= img.len() as u64 => {}
        _ => return bad("a program header is past the end of the file"),
    }

    let mut loaded = Loaded { entry, phdr: 0, phent, phnum, brk: 0, interp: 0 };
    // The ends of what is mapped so far, to refuse two segments on one page.
    let mut taken: Vec<(u64, u64)> = Vec::new();

    for i in 0..phnum {
        let ph = (phoff + i * phent) as usize;
        if le::<4>(img, ph) != PT_LOAD as u64 {
            continue;
        }
        let flags = le::<4>(img, ph + 4);
        let offset = le::<8>(img, ph + 8);
        let Some(vaddr) = le::<8>(img, ph + 16).checked_add(bias) else {
            return bad("a segment does not fit in the guest's memory");
        };
        let filesz = le::<8>(img, ph + 32);
        let memsz = le::<8>(img, ph + 40);
        if memsz == 0 {
            continue;
        }
        if filesz > memsz {
            return bad("a segment is larger in the file than in memory");
        }
        match offset.checked_add(filesz) {
            Some(end) if end <= img.len() as u64 => {}
            _ => return bad("a segment is past the end of the file"),
        }
        let start = vaddr & !(PAGE - 1);
        let end = match vaddr.checked_add(memsz).and_then(|e| e.checked_next_multiple_of(PAGE)) {
            Some(end) if end <= mem.size() => end,
            _ => return bad("a segment does not fit in the guest's memory"),
        };
        if taken.iter().any(|&(s, e)| start < e && s < end) {
            return bad("two segments share a page");
        }
        taken.push((start, end));

        // The pages the file fills are the file's own, mapped privately, where that
        // can be (R053: the host's page the guest's, the segment placed in memory
        // as it lies in the file): what follows the file's bytes in their last
        // page is cleared, as the kernel clears it, and the pages after are the
        // guest's own. Otherwise all of it is the guest's own, and the file's
        // bytes are written while it can be written. Either way it is then given
        // the protection asked for, and what the file does not fill is zero.
        let filled = (vaddr + filesz).next_multiple_of(PAGE);
        let mapped = match file {
            Some(fd) if filesz != 0 && vaddr % PAGE == offset % PAGE => match mem.map_file(start, filled - start, R | W, fd, offset & !(PAGE - 1)) {
                Ok(()) => true,
                Err(crate::mem::MapError::Range) => false,
                Err(e) => return Err(e.into()),
            },
            _ => false,
        };
        if mapped {
            if memsz > filesz {
                let rest = (filled - (vaddr + filesz)) as usize;
                own(mem.write_bytes(vaddr + filesz, &vec![0; rest]));
            }
            if filled < end {
                mem.map(filled, end - filled, R | W)?;
            }
        } else {
            mem.map(start, end - start, R | W)?;
            own(mem.write_bytes(vaddr, &img[offset as usize..(offset + filesz) as usize]));
        }
        let mut prot = 0;
        if flags & 4 != 0 {
            prot |= R;
        }
        if flags & 2 != 0 {
            prot |= W;
        }
        if flags & 1 != 0 {
            prot |= X;
        }
        mem.protect(start, end - start, prot)?;

        if offset <= phoff && phoff < offset + filesz {
            loaded.phdr = vaddr + (phoff - offset);
        }
        loaded.brk = loaded.brk.max(end);
    }
    if taken.is_empty() {
        return bad("nothing in the file is to be loaded");
    }
    Ok(loaded)
}

/// Maps the stack at the top of the guest's memory and puts on it what Linux puts
/// there for a new program: the count of arguments, pointers to them, to the
/// environment and the auxiliary vector, and below all that the stack pointer, which is
/// given back. It is a multiple of 16, as the ABI has it at a program's first
/// instruction.
pub fn first_stack(
    mem: &mut Mem,
    loaded: &Loaded,
    args: &[&[u8]],
    env: &[&[u8]],
) -> Result<u64, ElfError> {
    // One page under the top is left unmapped, as Linux leaves the last page.
    let top = mem.size() - PAGE;
    let bottom = top - STACK_SIZE;
    mem.map(bottom, STACK_SIZE, R | W)?;

    // The strings and the 16 bytes AT_RANDOM points at go highest.
    let mut at = top;
    let mut put = |mem: &mut Mem, bytes: &[u8], nul: bool| -> Result<u64, ElfError> {
        let len = bytes.len() as u64 + nul as u64;
        if len > at - bottom - PAGE {
            return bad("the arguments and the environment do not fit on the stack");
        }
        at -= len;
        // The terminating zero is there already: the stack is new.
        own(mem.write_bytes(at, bytes));
        Ok(at)
    };
    // Not random yet: a program's run is to be the same every time while r64m is held
    // to traces. To be the host's random bytes when programs that need them are run.
    let random = put(mem, &[0x5a; 16], false)?;
    let mut arg_at = Vec::with_capacity(args.len());
    for a in args {
        arg_at.push(put(mem, a, true)?);
    }
    let mut env_at = Vec::with_capacity(env.len());
    for e in env {
        env_at.push(put(mem, e, true)?);
    }

    let aux = [
        (AT_PHDR, loaded.phdr),
        (AT_PHENT, loaded.phent),
        (AT_PHNUM, loaded.phnum),
        (AT_PAGESZ, PAGE),
        (AT_ENTRY, loaded.entry),
        (AT_RANDOM, random),
    ];
    let mut aux = aux.to_vec();
    // What a dynamic linker asks besides: where it is itself, who the process is, and
    // that it need not distrust its environment. (A static program's stack is left
    // as it was: the traces of the real CPU have the stack pointer in every line.)
    if loaded.interp != 0 {
        let (uid, euid, gid, egid) = unsafe { (libc::getuid(), libc::geteuid(), libc::getgid(), libc::getegid()) };
        aux.extend([
            (AT_BASE, loaded.interp),
            (AT_UID, uid as u64),
            (AT_EUID, euid as u64),
            (AT_GID, gid as u64),
            (AT_EGID, egid as u64),
            (AT_SECURE, 0),
        ]);
    }
    aux.push((AT_NULL, 0));
    let mut words = Vec::with_capacity(3 + args.len() + env.len() + 2 * aux.len());
    words.push(args.len() as u64);
    words.extend(&arg_at);
    words.push(0);
    words.extend(&env_at);
    words.push(0);
    for (key, value) in aux {
        words.push(key);
        words.push(value);
    }

    let size = 8 * words.len() as u64;
    if size > at - bottom - PAGE {
        return bad("the arguments and the environment do not fit on the stack");
    }
    let rsp = (at - size) & !15;
    for (i, w) in words.iter().enumerate() {
        own(mem.write_u64(rsp + 8 * i as u64, *w));
    }
    Ok(rsp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{Access, Region};

    /// One segment of a file to be made: flags, address, bytes, size in memory.
    struct Seg(u32, u64, &'static [u8], u64);

    /// A static executable with these segments. The first is laid from offset 0 of the
    /// file, so that it holds the headers, as a linker lays it; the bytes given for it
    /// follow the headers.
    fn image(entry: u64, segs: &[Seg]) -> Vec<u8> {
        let headers = 64 + 56 * segs.len();
        let mut img = Vec::new();
        img.extend(b"\x7fELF\x02\x01\x01\x00");
        img.extend([0; 8]);
        img.extend(2u16.to_le_bytes());
        img.extend(62u16.to_le_bytes());
        img.extend(1u32.to_le_bytes());
        img.extend(entry.to_le_bytes());
        img.extend(64u64.to_le_bytes());
        img.extend(0u64.to_le_bytes());
        img.extend(0u32.to_le_bytes());
        for v in [64u16, 56, segs.len() as u16, 0, 0, 0] {
            img.extend(v.to_le_bytes());
        }
        let mut offset = 0u64;
        for (i, s) in segs.iter().enumerate() {
            let filesz = s.2.len() as u64 + if i == 0 { headers as u64 } else { 0 };
            let memsz = s.3.max(filesz);
            if i > 0 {
                // Each later segment's bytes begin on a page of the file.
                offset = offset.next_multiple_of(PAGE);
            }
            img.extend(1u32.to_le_bytes());
            img.extend(s.0.to_le_bytes());
            for v in [offset, s.1, s.1, filesz, memsz, PAGE] {
                img.extend(v.to_le_bytes());
            }
            offset += filesz;
        }
        for (i, s) in segs.iter().enumerate() {
            if i > 0 {
                img.resize(img.len().next_multiple_of(PAGE as usize), 0);
            }
            img.extend(s.2);
        }
        img
    }

    fn mem() -> Mem {
        Mem::reserve().unwrap()
    }

    /// What a linker makes: headers and read-only data, code, data with room after it.
    fn program() -> Vec<u8> {
        image(
            0x40_1004,
            &[
                Seg(4, 0x40_0000, b"read only", 0),
                Seg(5, 0x40_1000, b"\x90\x90\x90\x90\x0f\x05", 0),
                Seg(6, 0x40_3ff8, b"datadata", 0x2000),
            ],
        )
    }

    #[test]
    fn segments_go_where_the_headers_say_with_their_protection() {
        let mut m = mem();
        let img = program();
        let l = load(&mut m, &img).unwrap();
        assert_eq!(
            l,
            Loaded { entry: 0x40_1004, phdr: 0x40_0040, phent: 56, phnum: 3, brk: 0x40_6000, interp: 0 }
        );
        assert_eq!(
            m.regions(),
            &[
                Region { start: 0x40_0000, end: 0x40_1000, prot: R },
                Region { start: 0x40_1000, end: 0x40_2000, prot: R | X },
                Region { start: 0x40_3000, end: 0x40_6000, prot: R | W },
            ]
        );
        // The headers are in memory where AT_PHDR will say, and the data after them.
        assert_eq!(m.read::<4>(0x40_0000), Ok(*b"\x7fELF"));
        assert_eq!(m.read_u32(l.phdr), Ok(1));
        assert_eq!(m.read::<9>(0x40_0000 + 64 + 3 * 56), Ok(*b"read only"));
        // Code can be fetched and not written; data written and not fetched.
        let mut code = [0u8; 2];
        m.fetch(l.entry, &mut code).unwrap();
        assert_eq!(code, [0x0f, 0x05]);
        assert_eq!(m.write_u8(l.entry, 0), Err(Fault { addr: l.entry, access: Access::Write }));
        assert_eq!(m.fetch(0x40_3ff8, &mut code), Err(Fault { addr: 0x40_3ff8, access: Access::Fetch }));
        // A segment that begins off a page: its bytes at its address, zero before them
        // and zero after them to the end of what it has in memory, and to the page's end.
        assert_eq!(m.read_u64(0x40_3ff0), Ok(0));
        assert_eq!(m.read::<8>(0x40_3ff8), Ok(*b"datadata"));
        assert_eq!(m.read_u64(0x40_4000), Ok(0));
        assert_eq!(m.read_u64(0x40_5ff8), Ok(0));
        m.write_u64(0x40_5ff8, 1).unwrap();
    }

    #[test]
    fn a_program_that_can_be_put_anywhere_is_put_at_its_place_and_its_interpreter_at_another() {
        // The same file said to be position-independent: every address it names is
        // as far from `PIE_AT` as it was from nothing.
        let mut img = program();
        img[16] = 3;
        let mut m = mem();
        let mut l = load(&mut m, &img).unwrap();
        let fixed = load(&mut mem(), &program()).unwrap();
        assert_eq!((l.entry, l.phdr, l.brk), (fixed.entry + PIE_AT, fixed.phdr + PIE_AT, fixed.brk + PIE_AT));
        assert_eq!(m.read::<8>(0x40_3ff8 + PIE_AT), Ok(*b"datadata"));
        assert_eq!((l.interp, interpreter(&img)), (0, None));
        // As an interpreter it goes to `INTERP_AT`, beside the program, and the
        // program is told where; one linked to a fixed address is not an interpreter.
        let entry = load_interpreter(&mut m, &img, &mut l).unwrap();
        assert_eq!((entry, l.interp), (fixed.entry + INTERP_AT, INTERP_AT));
        assert_eq!(m.read::<8>(0x40_3ff8 + INTERP_AT), Ok(*b"datadata"));
        assert!(matches!(load_interpreter(&mut mem(), &program(), &mut l), Err(ElfError::Bad(_))));
        // The first stack then says where the interpreter is (AT_BASE, 7), which a
        // static program's does not.
        let has_base = |m: &mut Mem, l: &Loaded| {
            let rsp = first_stack(m, l, &[], &[]).unwrap();
            // Past the count, the end of the arguments and the end of the environment.
            (0..20).map(|i| m.read_u64(rsp + 24 + 16 * i).unwrap()).take_while(|&k| k != 0).any(|k| k == 7)
        };
        assert!(has_base(&mut m, &l));
        assert!(!has_base(&mut mem(), &fixed));
    }

    #[test]
    fn the_interpreter_a_program_names_is_read_from_its_header() {
        // One header of kind 3 whose bytes in the file are a name and its zero.
        let mut img = program();
        let name = b"/lib/ld-musl-x86_64.so.1\0";
        let at = img.len() as u64;
        img.extend(name);
        let ph = 64 + 56 * 2;
        img[ph..ph + 4].copy_from_slice(&3u32.to_le_bytes());
        img[ph + 8..ph + 16].copy_from_slice(&at.to_le_bytes());
        img[ph + 32..ph + 40].copy_from_slice(&(name.len() as u64).to_le_bytes());
        assert_eq!(interpreter(&img).as_deref(), Some(&name[..name.len() - 1]));
        assert_eq!(interpreter(b"not a program"), None);
    }

    #[test]
    fn what_is_not_a_static_x86_64_executable_is_refused_in_words() {
        let refused = |img: &[u8]| match load(&mut mem(), img) {
            Err(ElfError::Bad(what)) => what,
            other => panic!("not refused: {other:?}"),
        };
        let good = program();
        assert_eq!(refused(b"#!/bin/sh\n"), "not an ELF file");
        assert_eq!(refused(&good[..63]), "not an ELF file");
        let mut img = good.clone();
        img[18] = 183; // arm64
        assert_eq!(refused(&img), "not an x86-64 ELF");
        let mut img = good.clone();
        img[4] = 1; // 32-bit
        assert_eq!(refused(&img), "not an x86-64 ELF");
        let mut img = good.clone();
        img[16] = 1; // a file to be linked
        assert!(refused(&img).starts_with("not an executable"));
        let mut img = good.clone();
        img[56] = 200; // more headers than the file holds
        assert_eq!(refused(&img), "a program header is past the end of the file");
        let mut img = good.clone();
        img.truncate(0x2004); // the last segment's bytes cut short
        assert_eq!(refused(&img), "a segment is past the end of the file");
        // The second header's size in the file made larger than its size in memory.
        let mut img = good.clone();
        img[64 + 56 + 32] = 0xff;
        img[64 + 56 + 33] = 0xff;
        assert_eq!(refused(&img), "a segment is larger in the file than in memory");
        // Its address moved to the top of all addresses.
        let mut img = good.clone();
        img[64 + 56 + 16..64 + 56 + 24].copy_from_slice(&(u64::MAX - 0xfff).to_le_bytes());
        assert_eq!(refused(&img), "a segment does not fit in the guest's memory");
        // Two on one page.
        let img = image(0x40_0000, &[Seg(5, 0x40_0000, b"", 0), Seg(6, 0x40_0800, b"x", 0)]);
        assert_eq!(refused(&img), "two segments share a page");
        // Nothing to load.
        let mut img = good.clone();
        for i in 0..3 {
            img[64 + 56 * i] = 4; // a note
        }
        assert_eq!(refused(&img), "nothing in the file is to be loaded");
    }

    /// The zero-ended string at `addr`.
    fn string(m: &Mem, addr: u64) -> Vec<u8> {
        let mut s = Vec::new();
        loop {
            match m.read_u8(addr + s.len() as u64).unwrap() {
                0 => return s,
                b => s.push(b),
            }
        }
    }

    #[test]
    fn the_first_stack_is_linuxs() {
        let mut m = mem();
        let l = load(&mut m, &program()).unwrap();
        let rsp = first_stack(&mut m, &l, &[b"prog", b"-v", b""], &[b"HOME=/", b"A=b"]).unwrap();
        assert_eq!(rsp % 16, 0);
        let top = m.size() - PAGE;
        assert!(rsp < top && rsp > top - PAGE);
        // The stack is 8 MB that can be read and written and not run, under one page
        // that is not there.
        assert_eq!(
            m.regions().last(),
            Some(&Region { start: top - STACK_SIZE, end: top, prot: R | W })
        );
        // The count, the arguments, a zero, the environment, a zero.
        let word = |i: u64| m.read_u64(rsp + 8 * i).unwrap();
        assert_eq!(word(0), 3);
        assert_eq!(string(&m, word(1)), b"prog");
        assert_eq!(string(&m, word(2)), b"-v");
        assert_eq!(string(&m, word(3)), b"");
        assert_eq!(word(4), 0);
        assert_eq!(string(&m, word(5)), b"HOME=/");
        assert_eq!(string(&m, word(6)), b"A=b");
        assert_eq!(word(7), 0);
        // Then the auxiliary vector, ended by a zero pair.
        let mut aux = Vec::new();
        let mut i = 8;
        loop {
            let (key, value) = (word(i), word(i + 1));
            i += 2;
            if key == 0 {
                assert_eq!(value, 0);
                break;
            }
            aux.push((key, value));
        }
        let random = aux[5].1;
        assert_eq!(
            aux,
            [(3, l.phdr), (4, 56), (5, 3), (6, 4096), (9, l.entry), (25, random)]
        );
        assert_eq!(m.read::<16>(random), Ok([0x5a; 16]));
        // Everything pointed at lies above the words and inside the stack, and no two
        // strings lie on each other.
        let words_end = rsp + 8 * i;
        let mut pointed: Vec<(u64, u64)> = [1, 2, 3, 5, 6]
            .iter()
            .map(|&i| (word(i), word(i) + string(&m, word(i)).len() as u64 + 1))
            .collect();
        pointed.push((random, random + 16));
        pointed.sort();
        assert!(pointed[0].0 >= words_end);
        assert!(pointed.last().unwrap().1 <= top);
        assert!(pointed.windows(2).all(|w| w[0].1 <= w[1].0));
    }

    #[test]
    fn a_program_with_no_arguments_has_a_stack_all_the_same() {
        let mut m = mem();
        let l = load(&mut m, &program()).unwrap();
        let rsp = first_stack(&mut m, &l, &[], &[]).unwrap();
        assert_eq!(rsp % 16, 0);
        assert_eq!(m.read_u64(rsp), Ok(0));
        assert_eq!(m.read_u64(rsp + 8), Ok(0));
        assert_eq!(m.read_u64(rsp + 16), Ok(0));
        assert_eq!(m.read_u64(rsp + 24), Ok(3));
    }

    #[test]
    fn arguments_larger_than_the_stack_are_refused() {
        let mut m = mem();
        let l = load(&mut m, &program()).unwrap();
        let huge: &'static [u8] = vec![b'x'; STACK_SIZE as usize].leak();
        assert!(matches!(
            first_stack(&mut m, &l, &[huge], &[]),
            Err(ElfError::Bad("the arguments and the environment do not fit on the stack"))
        ));
    }
}
