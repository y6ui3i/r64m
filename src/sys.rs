//! The system calls of a program (ADR 0017).
//!
//! Memory is r64m's own (ADR 0008): `brk`, `mmap`, `munmap` and `mprotect` are
//! answered from the guest's reservation and written into its list of regions. So are
//! the few calls that are about the CPU (`arch_prctl`) or the process as r64m keeps it.
//! On a Linux host everything else that is known is handed to the host's kernel: the
//! guest's descriptors are r64m's, a pointer is `base + g`, and an error comes back as
//! the kernel gave it (the numbers are the same on x86-64 and arm64). On any other
//! host r64m keeps what a program without a kernel needs: `write` to the standard
//! output and error, and the exits.
//!
//! A call that is not known is answered ENOSYS and named once on the standard error.

use crate::cpu::{Cpu, RAX, RDI, RDX, RSI};
use crate::elf::INTERP_AT;
#[cfg(target_os = "linux")]
use crate::mem::Access;
use crate::mem::{MapError, Mem, PAGE};
use std::collections::BTreeSet;
#[cfg(not(target_os = "linux"))]
use std::io::Write;

#[cfg(not(target_os = "linux"))]
const EBADF: u64 = 9;
const ENOMEM: u64 = 12;
const EFAULT: u64 = 14;
const EINVAL: u64 = 22;
const ENOTTY: u64 = 25;
const ENOSYS: u64 = 38;

const R10: usize = 10;
const R8: usize = 8;
const R9: usize = 9;

/// Where what the program writes goes.
#[derive(PartialEq, Eq)]
pub enum Output {
    /// To r64m's own standard output and error.
    Host,
    /// Nowhere, as under the oracle that takes the traces: a trace has the output to
    /// itself.
    Nowhere,
}

/// What r64m keeps of the process for its system calls.
pub struct Sys {
    pub out: Output,
    /// Where the program's own memory after its segments ends (`brk`), and where it
    /// began.
    brk: u64,
    brk_start: u64,
    /// Below here the next mapping without an address of its own is put: they go
    /// downwards from under the interpreter, as Linux's do.
    mmap_top: u64,
    /// Where the kernel is to write a zero when the thread ends (`set_tid_address`).
    /// Kept; nothing reads it until there are threads.
    pub clear_tid: u64,
    /// What the program asked to be done at each signal (`rt_sigaction`): the
    /// handler, the flags, the restorer and the mask, as it gave them. Kept and given
    /// back; no handler is called until signals are delivered (ADR 0017, "Not yet").
    actions: [[u64; 4]; 65],
    /// The signals the program has blocked, as it sees them. The host's own mask is
    /// this without the two a fault in generated code arrives by.
    blocked: u64,
    /// The calls that were named as not known.
    named: BTreeSet<u64>,
    /// The guest's pages whose mapping or protection a call has just changed: blocks
    /// made from them are to be forgotten before the program goes on (ADR 0016).
    pub changed: Option<(u64, u64)>,
}

impl Sys {
    /// For a program whose segments end at `brk`.
    pub fn new(out: Output, brk: u64) -> Sys {
        // A gap under the interpreter, as much as a stack: nothing of the two meets.
        let mmap_top = INTERP_AT - (256 << 20);
        Sys { out, brk, brk_start: brk, mmap_top, clear_tid: 0, actions: [[0; 4]; 65], blocked: 0, named: BTreeSet::new(), changed: None }
    }

    fn change(&mut self, start: u64, end: u64) {
        self.changed = Some(match self.changed {
            Some((a, b)) => (a.min(start), b.max(end)),
            None => (start, end),
        });
    }
}

/// What the kernel's own error for a refused mapping would be.
fn map_errno(e: MapError) -> u64 {
    match e {
        MapError::Range => EINVAL,
        _ => ENOMEM,
    }
}

/// Answers the call the registers describe. `Some` is the program's exit code.
pub fn call(c: &mut Cpu, m: &mut Mem, s: &mut Sys) -> Option<i32> {
    let (a, b, d) = (c.reg[RDI], c.reg[RSI], c.reg[RDX]);
    let (e, f, g) = (c.reg[R10], c.reg[R8], c.reg[R9]);
    let number = c.reg[RAX];
    let result: Result<u64, u64> = match number {
        1 => write(m, s, a, b, d),
        9 => mmap(m, s, a, b, d, e, f, g),
        10 => mprotect(m, s, a, b, d),
        11 => munmap(m, s, a, b),
        12 => Ok(brk(m, s, a)),
        13 => sigaction(m, s, a, b, d, e),
        14 => sigprocmask(m, s, a, b, d, e),
        16 => ioctl(m, a, b, d),
        20 => writev(m, s, a, b, d),
        60 | 231 => return Some((a & 0xff) as i32),
        158 => arch_prctl(c, m, a, b),
        218 => {
            s.clear_tid = a;
            Ok(unsafe { libc::getpid() } as u64)
        }
        #[cfg(target_os = "linux")]
        _ if let Some(result) = handed_on(number, a, b, d) => result,
        _ => {
            if s.named.insert(number) {
                eprintln!("r64m: system call {number} is not known: answered ENOSYS");
            }
            Err(ENOSYS)
        }
    };
    c.reg[RAX] = match result {
        Ok(v) => v,
        Err(errno) => errno.wrapping_neg(),
    };
    None
}

// ---- memory -----------------------------------------------------------------------------------

/// Sets the end of the program's own memory, if it can be set there, and says where
/// it is now.
fn brk(m: &mut Mem, s: &mut Sys, to: u64) -> u64 {
    if to < s.brk_start {
        return s.brk;
    }
    let (old, new) = (s.brk.next_multiple_of(PAGE), to.next_multiple_of(PAGE));
    let done = if new > old {
        m.map(old, new - old, crate::mem::R | crate::mem::W)
    } else if new < old {
        s.change(new, old);
        m.unmap(new, old - new)
    } else {
        Ok(())
    };
    if done.is_ok() {
        s.brk = to;
    }
    s.brk
}

const MAP_SHARED: u64 = 1;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_FIXED_NOREPLACE: u64 = 0x10_0000;

/// The guest's protection bits are r64m's own: read 1, write 2, run 4.
fn prot_of(prot: u64) -> Result<u8, u64> {
    if prot & !7 != 0 { Err(EINVAL) } else { Ok(prot as u8) }
}

#[allow(clippy::too_many_arguments)]
fn mmap(m: &mut Mem, s: &mut Sys, addr: u64, len: u64, prot: u64, flags: u64, fd: u64, offset: u64) -> Result<u64, u64> {
    let prot = prot_of(prot)?;
    if len == 0 || len > m.size() || offset % PAGE != 0 {
        return Err(EINVAL);
    }
    let size = len.next_multiple_of(PAGE);
    let file = flags & MAP_ANONYMOUS == 0;
    if file && flags & MAP_SHARED != 0 && prot & crate::mem::W != 0 {
        // What is written would have to reach the file: not yet.
        return Err(ENOSYS);
    }
    let at = if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0 {
        if addr % PAGE != 0 {
            return Err(EINVAL);
        }
        addr
    } else {
        s.mmap_top.checked_sub(size).ok_or(ENOMEM)?
    };
    if file {
        // The file's bytes are read into memory of the guest's own: the same to a
        // program that maps privately, which is what a loader and a compiler do.
        m.map(at, size, crate::mem::R | crate::mem::W).map_err(map_errno)?;
        s.change(at, at + size);
        read_file_into(m, fd, at, len, offset)?;
        m.protect(at, size, prot).map_err(map_errno)?;
    } else {
        m.map(at, size, prot).map_err(map_errno)?;
        s.change(at, at + size);
    }
    if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) == 0 {
        s.mmap_top = at;
    }
    Ok(at)
}

#[cfg(target_os = "linux")]
fn read_file_into(m: &mut Mem, fd: u64, at: u64, len: u64, offset: u64) -> Result<(), u64> {
    let mut done = 0;
    while done < len {
        let n = unsafe {
            libc::pread(fd as i32, m.base().add((at + done) as usize) as *mut libc::c_void, (len - done) as usize, (offset + done) as i64)
        };
        match n {
            // The file ends before the mapping does: the rest reads as zero.
            0 => break,
            n if n < 0 => return Err(errno()),
            n => done += n as u64,
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn read_file_into(_m: &mut Mem, _fd: u64, _at: u64, _len: u64, _offset: u64) -> Result<(), u64> {
    Err(ENOSYS)
}

fn munmap(m: &mut Mem, s: &mut Sys, addr: u64, len: u64) -> Result<u64, u64> {
    m.unmap(addr, len).map_err(map_errno)?;
    s.change(addr, addr + len.next_multiple_of(PAGE));
    Ok(0)
}

fn mprotect(m: &mut Mem, s: &mut Sys, addr: u64, len: u64, prot: u64) -> Result<u64, u64> {
    let prot = prot_of(prot)?;
    if len == 0 {
        return Ok(0);
    }
    m.protect(addr, len, prot).map_err(map_errno)?;
    s.change(addr, addr + len.next_multiple_of(PAGE));
    Ok(0)
}

// ---- the CPU ------------------------------------------------------------------------------------

fn arch_prctl(c: &mut Cpu, m: &mut Mem, what: u64, value: u64) -> Result<u64, u64> {
    match what {
        0x1001 => c.gs_base = value,
        0x1002 => c.fs_base = value,
        0x1003 => m.write_u64(value, c.fs_base).map_err(|_| EFAULT)?,
        0x1004 => m.write_u64(value, c.gs_base).map_err(|_| EFAULT)?,
        _ => return Err(EINVAL),
    }
    Ok(0)
}

// ---- handed to the host's kernel --------------------------------------------------------------

/// The host's own error of the call just made.
#[cfg(target_os = "linux")]
fn errno() -> u64 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(5) as u64
}

/// The host's address of `[at, at + len)` of the guest's memory, if the guest may do
/// `access` to all of it.
#[cfg(target_os = "linux")]
fn host(m: &Mem, at: u64, len: u64, access: Access) -> Result<*mut libc::c_void, u64> {
    m.check(at, len, access).map_err(|_| EFAULT)?;
    Ok(unsafe { m.base().add(at as usize) } as *mut libc::c_void)
}

#[cfg(target_os = "linux")]
fn write(m: &Mem, s: &Sys, fd: u64, at: u64, len: u64) -> Result<u64, u64> {
    // No more at once than Linux writes at once.
    let len = len.min(0x7fff_f000);
    let from = host(m, at, len, Access::Read)?;
    if s.out == Output::Nowhere && (fd == 1 || fd == 2) {
        return Ok(len);
    }
    match unsafe { libc::write(fd as i32, from, len as usize) } {
        n if n < 0 => Err(errno()),
        n => Ok(n as u64),
    }
}

#[cfg(not(target_os = "linux"))]
fn write(m: &Mem, s: &Sys, fd: u64, at: u64, len: u64) -> Result<u64, u64> {
    if fd != 1 && fd != 2 {
        return Err(EBADF);
    }
    // No more at once than Linux writes at once.
    let len = len.min(0x7fff_f000);
    let mut bytes = vec![0u8; len as usize];
    m.read_bytes(at, &mut bytes).map_err(|_| EFAULT)?;
    if s.out == Output::Host {
        let done = if fd == 1 {
            std::io::stdout().write_all(&bytes)
        } else {
            std::io::stderr().write_all(&bytes)
        };
        done.map_err(|e| e.raw_os_error().unwrap_or(5) as u64)?;
    }
    Ok(len)
}

/// `writev`: each piece in turn, as one `write` each; what was written before a piece
/// that could not be is the answer, as the kernel's is.
fn writev(m: &Mem, s: &Sys, fd: u64, vector: u64, count: u64) -> Result<u64, u64> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let mut total = 0;
    for i in 0..count {
        let at = m.read_u64(vector + 16 * i).map_err(|_| EFAULT)?;
        let len = m.read_u64(vector + 16 * i + 8).map_err(|_| EFAULT)?;
        if len == 0 {
            continue;
        }
        match write(m, s, fd, at, len) {
            Ok(n) => {
                total += n;
                if n < len {
                    break;
                }
            }
            Err(e) if total == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(total)
}

/// Of `ioctl` only what asks a terminal for its size: a C library asks it of the
/// standard output to choose how to buffer.
#[cfg(target_os = "linux")]
fn ioctl(m: &Mem, fd: u64, request: u64, arg: u64) -> Result<u64, u64> {
    const TIOCGWINSZ: u64 = 0x5413;
    if request != TIOCGWINSZ {
        return Err(ENOTTY);
    }
    let to = host(m, arg, 8, Access::Write)?;
    match unsafe { libc::ioctl(fd as i32, TIOCGWINSZ as _, to) } {
        n if n < 0 => Err(errno()),
        n => Ok(n as u64),
    }
}

#[cfg(not(target_os = "linux"))]
fn ioctl(_m: &Mem, _fd: u64, _request: u64, _arg: u64) -> Result<u64, u64> {
    Err(ENOTTY)
}

// ---- signals, as far as they go without being delivered ---------------------------------------

/// `rt_sigaction`: what was asked for is kept and what was asked before is given
/// back. The structure is the kernel's for x86-64: handler, flags, restorer, mask.
fn sigaction(m: &mut Mem, s: &mut Sys, signal: u64, new: u64, old: u64, size: u64) -> Result<u64, u64> {
    // Not for no signal, nor for the two that cannot be caught.
    if size != 8 || signal == 0 || signal > 64 || (new != 0 && (signal == 9 || signal == 19)) {
        return Err(EINVAL);
    }
    let was = s.actions[signal as usize];
    if new != 0 {
        let mut asked = [0; 4];
        for (i, word) in asked.iter_mut().enumerate() {
            *word = m.read_u64(new + 8 * i as u64).map_err(|_| EFAULT)?;
        }
        s.actions[signal as usize] = asked;
    }
    if old != 0 {
        for (i, word) in was.iter().enumerate() {
            m.write_u64(old + 8 * i as u64, *word).map_err(|_| EFAULT)?;
        }
    }
    Ok(0)
}

/// `rt_sigprocmask`: the program's own set of blocked signals, which is also the
/// host's but for the two signals r64m needs for itself.
fn sigprocmask(m: &mut Mem, s: &mut Sys, how: u64, new: u64, old: u64, size: u64) -> Result<u64, u64> {
    if size != 8 {
        return Err(EINVAL);
    }
    let was = s.blocked;
    if new != 0 {
        let set = m.read_u64(new).map_err(|_| EFAULT)?;
        s.blocked = match how {
            0 => was | set,
            1 => was & !set,
            2 => set,
            _ => return Err(EINVAL),
        };
        // SIGKILL and SIGSTOP are never blocked.
        s.blocked &= !(1 << 8 | 1 << 18);
        block_on_host(s.blocked);
    }
    if old != 0 {
        m.write_u64(old, was).map_err(|_| EFAULT)?;
    }
    Ok(0)
}

/// Blocks on the host what the guest has blocked, without SIGBUS (7) and SIGSEGV
/// (11): a fault in generated code must arrive (ADR 0014, point 7). A signal's bit is
/// its number less one, on both.
#[cfg(target_os = "linux")]
fn block_on_host(blocked: u64) {
    let host = blocked & !(1 << 6 | 1 << 10);
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for signal in 1..=64 {
            if host >> (signal - 1) & 1 != 0 {
                libc::sigaddset(&mut set, signal);
            }
        }
        libc::pthread_sigmask(libc::SIG_SETMASK, &set, std::ptr::null_mut());
    }
}

#[cfg(not(target_os = "linux"))]
fn block_on_host(_blocked: u64) {}

// ---- handed on as they are -------------------------------------------------------------------

/// The calls whose arguments are numbers that mean the same to both kernels: who the
/// process is, and `fcntl` for a descriptor's own flags.
#[cfg(target_os = "linux")]
fn handed_on(number: u64, a: u64, b: u64, d: u64) -> Option<Result<u64, u64>> {
    let done = |n: i64| if n < 0 { Err(errno()) } else { Ok(n as u64) };
    Some(unsafe {
        match number {
            39 => done(libc::getpid() as i64),
            102 => done(libc::getuid() as i64),
            104 => done(libc::getgid() as i64),
            105 => done(libc::setuid(a as u32) as i64),
            106 => done(libc::setgid(a as u32) as i64),
            107 => done(libc::geteuid() as i64),
            108 => done(libc::getegid() as i64),
            110 => done(libc::getppid() as i64),
            186 => done(libc::gettid() as i64),
            72 => return Some(fcntl(a, b, d)),
            _ => return None,
        }
    })
}

/// `fcntl` for copying a descriptor and for its flags. The flags of an open file
/// (`F_GETFL`, `F_SETFL`) are numbered differently on the two kernels in four places.
#[cfg(target_os = "linux")]
fn fcntl(fd: u64, cmd: u64, arg: u64) -> Result<u64, u64> {
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    let arg = match cmd {
        // F_DUPFD, F_GETFD, F_SETFD, F_DUPFD_CLOEXEC.
        0 | 1 | 2 | 1030 | F_GETFL => arg,
        F_SETFL => open_flags(arg, true),
        _ => return Err(EINVAL),
    };
    match unsafe { libc::fcntl(fd as i32, cmd as i32, arg) } {
        n if n < 0 => Err(errno()),
        n if cmd == F_GETFL => Ok(open_flags(n as u64, false)),
        n => Ok(n as u64),
    }
}

/// The flags of `open` from x86-64's numbers to arm64's (`to_host`) or back. Four
/// differ: O_DIRECT, O_LARGEFILE, O_DIRECTORY and O_NOFOLLOW are 4000h, 8000h, 10000h
/// and 20000h on x86-64, and 10000h, 20000h, 4000h and 8000h on arm64.
#[cfg(any(target_os = "linux", test))]
fn open_flags(flags: u64, to_host: bool) -> u64 {
    const PAIRS: [(u64, u64); 4] = [(0x4000, 0x1_0000), (0x8000, 0x2_0000), (0x1_0000, 0x4000), (0x2_0000, 0x8000)];
    let mut out = flags & !0x3_c000;
    for (guest, host) in PAIRS {
        let (from, to) = if to_host { (guest, host) } else { (host, guest) };
        if flags & from != 0 {
            out |= to;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{R, W};

    fn guest() -> (Cpu, Mem, Sys) {
        let mut m = Mem::reserve().unwrap();
        m.map(0x10_0000, 4096, R | W).unwrap();
        (Cpu::new(0, 0), m, Sys::new(Output::Nowhere, 0x20_0000))
    }

    /// One call with up to six arguments; what it left in RAX.
    fn ask(c: &mut Cpu, m: &mut Mem, s: &mut Sys, number: u64, args: &[u64]) -> u64 {
        c.reg[RAX] = number;
        for (reg, value) in [RDI, RSI, RDX, R10, R8, R9].into_iter().zip(args) {
            c.reg[reg] = *value;
        }
        assert_eq!(call(c, m, s), None);
        c.reg[RAX]
    }

    #[test]
    fn brk_grows_and_shrinks_the_programs_own_memory() {
        let (mut c, mut m, mut s) = guest();
        assert_eq!(ask(&mut c, &mut m, &mut s, 12, &[0]), 0x20_0000);
        assert_eq!(ask(&mut c, &mut m, &mut s, 12, &[0x20_1234]), 0x20_1234);
        m.write_u64(0x20_1ff8, 7).unwrap();
        assert!(m.write_u64(0x20_2000, 7).is_err());
        // Under where it began: refused, and where it is comes back.
        assert_eq!(ask(&mut c, &mut m, &mut s, 12, &[0x1f_0000]), 0x20_1234);
        // Shrunk to its beginning: the page is gone, and said to have changed.
        s.changed = None;
        assert_eq!(ask(&mut c, &mut m, &mut s, 12, &[0x20_0000]), 0x20_0000);
        assert!(m.read_u64(0x20_1ff8).is_err());
        assert_eq!(s.changed, Some((0x20_0000, 0x20_2000)));
    }

    #[test]
    fn mmap_gives_memory_downwards_and_where_asked() {
        let (mut c, mut m, mut s) = guest();
        // Private and anonymous, to be read and written, twice: one under the other.
        let first = ask(&mut c, &mut m, &mut s, 9, &[0, 5000, 3, 0x22, u64::MAX, 0]);
        let second = ask(&mut c, &mut m, &mut s, 9, &[0, 4096, 3, 0x22, u64::MAX, 0]);
        assert_eq!((first % 4096, first - second), (0, 4096));
        assert!(first < INTERP_AT);
        m.write_u64(first + 8184, 1).unwrap();
        assert_eq!(m.read_u64(second), Ok(0));
        // At an address of the program's choosing, over what is there.
        m.write_u64(0x10_0000, 9).unwrap();
        assert_eq!(ask(&mut c, &mut m, &mut s, 9, &[0x10_0000, 4096, 1, 0x32, u64::MAX, 0]), 0x10_0000);
        assert_eq!(m.read_u64(0x10_0000), Ok(0));
        assert!(m.write_u64(0x10_0000, 1).is_err());
        // mprotect lets it be written; munmap takes it away. Each says what changed.
        s.changed = None;
        assert_eq!(ask(&mut c, &mut m, &mut s, 10, &[0x10_0000, 4096, 3]), 0);
        assert_eq!(s.changed.take(), Some((0x10_0000, 0x10_1000)));
        m.write_u64(0x10_0000, 1).unwrap();
        assert_eq!(ask(&mut c, &mut m, &mut s, 11, &[0x10_0000, 4096]), 0);
        assert!(m.read_u64(0x10_0000).is_err());
        // Refused: no length, an address off a page, a protection that is not one,
        // and protecting what is not mapped.
        assert_eq!(ask(&mut c, &mut m, &mut s, 9, &[0, 0, 3, 0x22, u64::MAX, 0]), EINVAL.wrapping_neg());
        assert_eq!(ask(&mut c, &mut m, &mut s, 9, &[0x10_0800, 4096, 3, 0x32, u64::MAX, 0]), EINVAL.wrapping_neg());
        assert_eq!(ask(&mut c, &mut m, &mut s, 9, &[0, 4096, 8, 0x22, u64::MAX, 0]), EINVAL.wrapping_neg());
        assert_eq!(ask(&mut c, &mut m, &mut s, 10, &[0x10_0000, 4096, 3]), ENOMEM.wrapping_neg());
    }

    #[test]
    fn fs_and_gs_are_set_and_read_by_arch_prctl() {
        let (mut c, mut m, mut s) = guest();
        assert_eq!(ask(&mut c, &mut m, &mut s, 158, &[0x1002, 0x1234_5000]), 0);
        assert_eq!(ask(&mut c, &mut m, &mut s, 158, &[0x1001, 0x77]), 0);
        assert_eq!((c.fs_base, c.gs_base), (0x1234_5000, 0x77));
        assert_eq!(ask(&mut c, &mut m, &mut s, 158, &[0x1003, 0x10_0000]), 0);
        assert_eq!(m.read_u64(0x10_0000), Ok(0x1234_5000));
        assert_eq!(ask(&mut c, &mut m, &mut s, 158, &[0x1003, 0x30_0000]), EFAULT.wrapping_neg());
        assert_eq!(ask(&mut c, &mut m, &mut s, 158, &[0x3001, 0]), EINVAL.wrapping_neg());
    }

    #[test]
    fn what_a_signal_is_to_do_is_kept_and_given_back() {
        let (mut c, mut m, mut s) = guest();
        for (i, word) in [0x40_1000u64, 0x0400_0004, 0x40_2000, 0xff].into_iter().enumerate() {
            m.write_u64(0x10_0000 + 8 * i as u64, word).unwrap();
        }
        // Signal 2: set, with nothing asked back; then asked back while set again.
        assert_eq!(ask(&mut c, &mut m, &mut s, 13, &[2, 0x10_0000, 0, 8]), 0);
        m.write_u64(0x10_0000, 1).unwrap();
        assert_eq!(ask(&mut c, &mut m, &mut s, 13, &[2, 0x10_0000, 0x10_0100, 8]), 0);
        assert_eq!((m.read_u64(0x10_0100), m.read_u64(0x10_0118)), (Ok(0x40_1000), Ok(0xff)));
        // SIGKILL cannot be given a handler, though what it does can be read.
        assert_eq!(ask(&mut c, &mut m, &mut s, 13, &[9, 0x10_0000, 0, 8]), EINVAL.wrapping_neg());
        assert_eq!(ask(&mut c, &mut m, &mut s, 13, &[9, 0, 0x10_0100, 8]), 0);
        assert_eq!(ask(&mut c, &mut m, &mut s, 13, &[2, 0, 0, 16]), EINVAL.wrapping_neg());
    }

    #[test]
    fn the_blocked_signals_are_the_programs_own_set() {
        let (mut c, mut m, mut s) = guest();
        // Block 2, 11 and 9; then unblock 2; the old set comes back each time.
        m.write_u64(0x10_0000, 1 << 1 | 1 << 10 | 1 << 8).unwrap();
        assert_eq!(ask(&mut c, &mut m, &mut s, 14, &[0, 0x10_0000, 0x10_0008, 8]), 0);
        assert_eq!(m.read_u64(0x10_0008), Ok(0));
        m.write_u64(0x10_0000, 1 << 1).unwrap();
        assert_eq!(ask(&mut c, &mut m, &mut s, 14, &[1, 0x10_0000, 0x10_0008, 8]), 0);
        // SIGKILL was never in it; SIGSEGV is, to the program.
        assert_eq!(m.read_u64(0x10_0008), Ok(1 << 1 | 1 << 10));
        assert_eq!(s.blocked, 1 << 10);
        assert_eq!(ask(&mut c, &mut m, &mut s, 14, &[5, 0x10_0000, 0, 8]), EINVAL.wrapping_neg());
        // Put back, so that this test's thread is as it was.
        m.write_u64(0x10_0000, 0).unwrap();
        assert_eq!(ask(&mut c, &mut m, &mut s, 14, &[2, 0x10_0000, 0, 8]), 0);
    }

    #[test]
    fn the_flags_of_open_are_renumbered_both_ways() {
        // O_RDWR, O_CREAT and O_CLOEXEC are the same; the four that differ change
        // places and come back.
        assert_eq!(open_flags(0x8_0042, true), 0x8_0042);
        for (guest, host) in [(0x4000u64, 0x1_0000u64), (0x8000, 0x2_0000), (0x1_0000, 0x4000), (0x2_0000, 0x8000)] {
            assert_eq!(open_flags(guest | 0x241, true), host | 0x241);
            assert_eq!(open_flags(host | 0x241, false), guest | 0x241);
        }
        assert_eq!(open_flags(open_flags(0x3_c000, true), false), 0x3_c000);
    }

    #[test]
    fn a_call_that_is_not_known_is_refused_and_writev_writes_each_piece() {
        let (mut c, mut m, mut s) = guest();
        assert_eq!(ask(&mut c, &mut m, &mut s, 9999, &[]), ENOSYS.wrapping_neg());
        // Two pieces of 3 and 4 bytes and an empty one, to the standard output, which
        // goes nowhere here: seven written.
        for (i, word) in [0x10_0800u64, 3, 0x10_0900, 0, 0x10_0a00, 4].into_iter().enumerate() {
            m.write_u64(0x10_0000 + 8 * i as u64, word).unwrap();
        }
        assert_eq!(ask(&mut c, &mut m, &mut s, 20, &[1, 0x10_0000, 3]), 7);
        // A piece that is not the program's to read.
        m.write_u64(0x10_0000, 0x30_0000).unwrap();
        assert_eq!(ask(&mut c, &mut m, &mut s, 20, &[1, 0x10_0000, 3]), EFAULT.wrapping_neg());
        // The exit's code is its low byte.
        c.reg[RAX] = 231;
        c.reg[RDI] = 0x1_02;
        assert_eq!(call(&mut c, &mut m, &mut s), Some(2));
    }
}
