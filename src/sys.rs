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

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub mod signal;

use crate::cpu::{Cpu, RAX, RDI, RDX, RSI};
use crate::elf::INTERP_AT;
#[cfg(target_os = "linux")]
use crate::mem::Access;
use crate::mem::{MapError, Mem, PAGE};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
#[cfg(not(target_os = "linux"))]
use std::io::Write;

#[cfg(not(target_os = "linux"))]
const EBADF: u64 = 9;
const ENOMEM: u64 = 12;
#[cfg(target_os = "linux")]
const EINTR: u64 = 4;
const EFAULT: u64 = 14;
const EINVAL: u64 = 22;
#[cfg(target_os = "linux")]
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

/// What r64m keeps of the process for its system calls, the same for every thread
/// of it: held by one thread at a time, and never across a call that waits.
pub struct Proc {
    /// Where the program's own memory after its segments ends (`brk`), and where it
    /// began.
    brk: u64,
    brk_start: u64,
    /// Below here the next mapping without an address of its own is put: they go
    /// downwards from under the interpreter, as Linux's do.
    mmap_top: u64,
    /// What the program asked to be done at each signal (`rt_sigaction`): the
    /// handler, the flags, the restorer and the mask, as it gave them. Kept and given
    /// back; no handler is called until signals are delivered (ADR 0019, point 2).
    actions: [[u64; 4]; 65],
    /// The calls that were named as not known.
    named: BTreeSet<u64>,
    /// The System V segments attached (`shmat`): where, and how long each is.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    segments: std::collections::BTreeMap<u64, u64>,
}

/// How a thread or the process ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// `exit`: this thread, with this code (the process's, if it is the last).
    Thread(i32),
    /// `exit_group`: every thread.
    Group(i32),
}

/// What r64m keeps for one thread's system calls, and its hold on the process's.
pub struct Sys {
    pub out: Output,
    proc: Arc<Mutex<Proc>>,
    /// Where a zero is to be written, and whoever waits there woken, when the thread
    /// ends (`set_tid_address`, `clone`).
    pub clear_tid: u64,
    /// The signals the thread has blocked, as it sees them. The host's own mask is
    /// this without the two a fault in generated code arrives by.
    blocked: u64,
    /// The stack the thread gave for handlers that ask for another (`sigaltstack`):
    /// where it begins and how large it is; no size if there is none.
    alt_stack: (u64, u64),
    /// The set of blocked signals the call just answered waited with, if it had one
    /// of its own for its duration: what arrived is then delivered by it.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    waited_with: Option<u64>,
    /// For `execve`, which is r64m started again: where r64m itself is, and the
    /// options it was given. And the program's own path, which is what a program
    /// that asks who it is must be told.
    pub exe: String,
    pub options: Vec<String>,
    pub program: String,
    /// What a new thread of the guest's is to run: r64m's own loop, given by `main`,
    /// with the thread's registers, its handle on the memory and its `Sys`.
    pub thread: Option<fn(Cpu, Mem, Sys)>,
    /// The guest's pages whose mapping or protection a call has just changed: blocks
    /// made from them are to be forgotten before the program goes on (ADR 0016).
    pub changed: Option<(u64, u64)>,
}

impl Sys {
    /// For a program whose segments end at `brk`.
    pub fn new(out: Output, brk: u64) -> Sys {
        // A gap under the interpreter, as much as a stack: nothing of the two meets.
        let mmap_top = INTERP_AT - (256 << 20);
        let proc = Proc { brk, brk_start: brk, mmap_top, actions: [[0; 4]; 65], named: BTreeSet::new(), segments: Default::default() };
        #[allow(unused_mut)]
        let mut s = Sys {
            out,
            proc: Arc::new(Mutex::new(proc)),
            clear_tid: 0,
            blocked: 0,
            alt_stack: (0, 0),
            waited_with: None,
            exe: String::new(),
            options: Vec::new(),
            program: String::new(),
            thread: None,
            changed: None,
        };
        #[cfg(target_os = "linux")]
        linux::inherited(&mut s);
        s
    }

    /// The same process's, for a new thread: what is the thread's own begins as its
    /// maker's is.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn for_thread(&self) -> Sys {
        Sys {
            out: if self.out == Output::Host { Output::Host } else { Output::Nowhere },
            proc: self.proc.clone(),
            clear_tid: 0,
            blocked: self.blocked,
            alt_stack: (0, 0),
            waited_with: None,
            exe: self.exe.clone(),
            options: self.options.clone(),
            program: self.program.clone(),
            thread: self.thread,
            changed: None,
        }
    }

    /// The process's part, held until what comes back is dropped.
    fn proc(&self) -> std::sync::MutexGuard<'_, Proc> {
        self.proc.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn change(&mut self, start: u64, end: u64) {
        self.changed = Some(match self.changed {
            Some((a, b)) => (a.min(start), b.max(end)),
            None => (start, end),
        });
    }
}

/// How many threads the guest has started.
static THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Has the guest ever started a thread?
pub fn threads_started() -> bool {
    THREADS.load(std::sync::atomic::Ordering::Relaxed) != 0
}

/// The calling thread of the guest's ends (`exit`): the word it asked to have
/// cleared is cleared and whoever waits on it woken, as the kernel does for a thread
/// of its own, and then the host's thread ends. If it is the last, the process ends
/// with `code`.
pub fn end_thread(m: &mut Mem, s: &Sys, code: i32) -> ! {
    #[cfg(target_os = "linux")]
    linux::end_thread(m, s, code);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (m, s);
        std::process::exit(code)
    }
}

/// This thread's signals are noted in `c` from now (see `signal::attend`).
pub fn attend(c: &mut Cpu) {
    #[cfg(target_os = "linux")]
    signal::attend(c);
    #[cfg(not(target_os = "linux"))]
    let _ = c;
}

/// Enters the guest's handlers for the signals that wait. A frame that cannot be
/// written ends the process as a fault on its stack does.
pub fn deliver(c: &mut Cpu, m: &mut Mem, s: &mut Sys) {
    #[cfg(target_os = "linux")]
    if !signal::deliver(c, m, s) {
        signal::die_of(libc::SIGSEGV);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (m, s);
        c.kick = 0;
    }
}

/// A fault of the guest's own: true if its handler for it was entered.
pub fn fault(c: &mut Cpu, m: &mut Mem, s: &mut Sys, stop: &crate::cpu::Stop) -> bool {
    #[cfg(target_os = "linux")]
    return signal::fault(c, m, s, stop) == Some(true);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (c, m, s, stop);
        false
    }
}

/// The process ends for a fault of the guest's that nothing handles: as the signal
/// ends one where there are signals, and with r64m's own code where there are not.
/// What r64m itself lacks (an instruction not written) is r64m's code everywhere.
pub fn die_of(stop: &crate::cpu::Stop) -> ! {
    #[cfg(target_os = "linux")]
    if !matches!(stop, crate::cpu::Stop::NotWritten(_)) {
        signal::die_of(signal::signal_of(stop));
    }
    let _ = stop;
    std::process::exit(2)
}

/// What the kernel's own error for a refused mapping would be.
fn map_errno(e: MapError) -> u64 {
    match e {
        MapError::Range => EINVAL,
        _ => ENOMEM,
    }
}

/// Whether each system call is said on standard error (`--calls`): a tool for finding
/// what a program that stops or waits was doing.
pub static SAY_CALLS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn thread_number() -> i64 {
    #[cfg(target_os = "linux")]
    return unsafe { libc::syscall(libc::SYS_gettid) } as i64;
    #[cfg(not(target_os = "linux"))]
    0
}

/// Answers the call the registers describe. `Some` is the end of the thread or of
/// the process.
pub fn call(c: &mut Cpu, m: &mut Mem, s: &mut Sys) -> Option<Exit> {
    // A signal that arrived since the last look is delivered before the call is
    // made, not after it: the call may wait, and the signal was to cut that short.
    // The guest comes back to its SYSCALL instruction when the handler is done.
    // (Between this look and the kernel taking the call a signal can still slip in
    // and wait out the call: a few instructions wide, and not closed yet.)
    if c.kick != 0 {
        c.rip = c.rip.wrapping_sub(2);
        return None;
    }
    let (a, b, d) = (c.reg[RDI], c.reg[RSI], c.reg[RDX]);
    let (e, f, g) = (c.reg[R10], c.reg[R8], c.reg[R9]);
    let number = c.reg[RAX];
    // `--calls`: each call as it is made and, on a line of its own, what it answered
    // (a call that waits is seen waiting).
    let said = SAY_CALLS.load(std::sync::atomic::Ordering::Relaxed);
    if said {
        eprintln!("r64m: call {} {number}({a:#x}, {b:#x}, {d:#x}, {e:#x}, {f:#x}, {g:#x}) at {:#x}", thread_number(), c.rip);
    }
    let result: Result<u64, u64> = match number {
        1 => write(m, s, a, b, d),
        9 => mmap(m, s, a, b, d, e, f, g),
        10 => mprotect(m, s, a, b, d),
        11 => munmap(m, s, a, b),
        12 => Ok(brk(m, s, a)),
        13 => sigaction(m, s, a, b, d, e),
        14 => {
            let done = sigprocmask(m, s, a, b, d, e);
            #[cfg(target_os = "linux")]
            signal::look_again(c, s);
            done
        }
        // rt_sigreturn: the registers are the frame's, the A register among them.
        #[cfg(target_os = "linux")]
        15 => match signal::sigreturn(c, m, s) {
            Ok(()) => return None,
            Err(e) => Err(e),
        },
        131 => sigaltstack(m, s, a, b),
        20 => writev(m, s, a, b, d),
        25 => mremap(m, s, a, b, d, e),
        #[cfg(target_os = "linux")]
        30 => shmat(m, s, a, b, d),
        #[cfg(target_os = "linux")]
        67 => shmdt(m, s, a),
        60 => return Some(Exit::Thread((a & 0xff) as i32)),
        231 => return Some(Exit::Group((a & 0xff) as i32)),
        158 => arch_prctl(c, m, a, b),
        218 => {
            s.clear_tid = a;
            Ok(unsafe { libc::getpid() } as u64)
        }
        _ => match handed_on(c, m, s, number, [a, b, d, e, f, g]) {
            Some(result) => result,
            None => {
                if s.proc().named.insert(number) {
                    eprintln!("r64m: system call {number} is not known: answered ENOSYS");
                }
                Err(ENOSYS)
            }
        },
    };
    // A call that a signal cut short is started again after the signal's handler,
    // if the handler asked for that and the call is one that can be: the guest comes
    // back to the SYSCALL instruction with the call's number in the A register.
    #[cfg(target_os = "linux")]
    if result == Err(EINTR) && !matches!(number, 7 | 23 | 34 | 35 | 128 | 130 | 230 | 232 | 270 | 271 | 281) && signal::restarts(s) {
        c.rip = c.rip.wrapping_sub(2);
        return None;
    }
    c.reg[RAX] = match result {
        Ok(v) => v,
        Err(errno) => errno.wrapping_neg(),
    };
    if said {
        eprintln!("r64m: call {} {number} = {:#x}", thread_number(), c.reg[RAX] as i64);
    }
    #[cfg(target_os = "linux")]
    if let Some(during) = s.waited_with.take() {
        if !signal::deliver_after_wait(c, m, s, during) {
            signal::die_of(libc::SIGSEGV);
        }
    }
    None
}

// ---- memory -----------------------------------------------------------------------------------

/// Sets the end of the program's own memory, if it can be set there, and says where
/// it is now.
fn brk(m: &mut Mem, s: &mut Sys, to: u64) -> u64 {
    let proc = s.proc.clone();
    let mut p = proc.lock().unwrap_or_else(|e| e.into_inner());
    if to < p.brk_start {
        return p.brk;
    }
    let (old, new) = (p.brk.next_multiple_of(PAGE), to.next_multiple_of(PAGE));
    let done = if new > old {
        m.map(old, new - old, crate::mem::R | crate::mem::W)
    } else if new < old {
        s.change(new, old);
        m.unmap(new, old - new)
    } else {
        Ok(())
    };
    if done.is_ok() {
        p.brk = to;
    }
    p.brk
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
    // Shared: the host's own shared mapping at the place, where that can be (Linux:
    // R048). Elsewhere a shared mapping is the private copy it was before, but
    // for one of a file that is to be written, which a copy cannot be.
    let shared = flags & MAP_SHARED != 0 && cfg!(target_os = "linux");
    if file && flags & MAP_SHARED != 0 && prot & crate::mem::W != 0 && !shared {
        return Err(ENOSYS);
    }
    let at = if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0 {
        if addr % PAGE != 0 {
            return Err(EINVAL);
        }
        addr
    } else {
        // Taken at once, so that no other thread is given the same.
        let mut p = s.proc();
        let at = p.mmap_top.checked_sub(size).ok_or(ENOMEM)?;
        p.mmap_top = at;
        at
    };
    if shared {
        m.map_shared(at, size, prot, file.then_some((fd as i32, offset))).map_err(map_errno)?;
        s.change(at, at + size);
    } else if file {
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

/// `mremap`: a mapping made smaller where it is, or larger at a new place (if it may
/// move) with what it held.
fn mremap(m: &mut Mem, s: &mut Sys, old: u64, old_len: u64, new_len: u64, flags: u64) -> Result<u64, u64> {
    const MAYMOVE: u64 = 1;
    if old % PAGE != 0 || new_len == 0 || flags & !MAYMOVE != 0 {
        return Err(EINVAL);
    }
    // Of an address that is not mapped the answer is EFAULT whatever else is asked:
    // musl finds the end of the first thread's stack by asking to grow one page
    // after another, downwards, until that is the answer (R047: Node went on asking
    // for ever).
    let Some(prot) = m.regions().iter().find(|r| r.start <= old && old < r.end).map(|r| r.prot & 7) else {
        return Err(EFAULT);
    };
    let (was, now) = (old_len.next_multiple_of(PAGE), new_len.next_multiple_of(PAGE));
    if now <= was {
        if now < was {
            m.unmap(old + now, was - now).map_err(map_errno)?;
            s.change(old + now, old + was);
        }
        return Ok(old);
    }
    if flags & MAYMOVE == 0 {
        return Err(ENOMEM);
    }
    let to = {
        let mut p = s.proc();
        let to = p.mmap_top.checked_sub(now).ok_or(ENOMEM)?;
        p.mmap_top = to;
        to
    };
    m.map(to, now, crate::mem::R | crate::mem::W).map_err(map_errno)?;
    let mut held = vec![0u8; was as usize];
    m.read_bytes(old, &mut held).map_err(|_| EFAULT)?;
    m.write_bytes(to, &held).map_err(|_| EFAULT)?;
    m.protect(to, now, prot).map_err(map_errno)?;
    m.unmap(old, was).map_err(map_errno)?;
    s.change(old, old + was);
    s.change(to, to + now);
    Ok(to)
}

/// `shmat`: a System V segment attached at a place in the guest's memory (R048). The
/// host's kernel puts it there (in place of what the reservation had: SHM_REMAP), so
/// it is the same memory in every process that attaches it, and in a child.
#[cfg(target_os = "linux")]
fn shmat(m: &mut Mem, s: &mut Sys, id: u64, addr: u64, flags: u64) -> Result<u64, u64> {
    const READ_ONLY: u64 = 0x1000;
    // How long it is: the sixth word of what IPC_STAT says of it.
    let mut about = [0u64; 16];
    if unsafe { libc::shmctl(id as i32, libc::IPC_STAT, about.as_mut_ptr() as *mut libc::shmid_ds) } != 0 {
        return Err(errno());
    }
    let size = about[6].next_multiple_of(PAGE);
    if size == 0 || addr % PAGE != 0 {
        return Err(EINVAL);
    }
    let at = if addr != 0 {
        addr
    } else {
        let mut p = s.proc();
        let at = p.mmap_top.checked_sub(size).ok_or(ENOMEM)?;
        p.mmap_top = at;
        at
    };
    let prot = if flags & READ_ONLY != 0 { crate::mem::R } else { crate::mem::R | crate::mem::W };
    m.adopt(at, size, prot, |host| {
        let got = unsafe { libc::shmat(id as i32, host as *const libc::c_void, (flags & READ_ONLY) as i32 | libc::SHM_REMAP) };
        if got as isize == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    })
    .map_err(map_errno)?;
    s.proc().segments.insert(at, size);
    s.change(at, at + size);
    Ok(at)
}

/// `shmdt`: the place is unmapped, which is how the kernel knows it detached.
#[cfg(target_os = "linux")]
fn shmdt(m: &mut Mem, s: &mut Sys, addr: u64) -> Result<u64, u64> {
    let Some(size) = s.proc().segments.remove(&addr) else {
        return Err(EINVAL);
    };
    m.unmap(addr, size).map_err(map_errno)?;
    s.change(addr, addr + size);
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

#[cfg(target_os = "linux")]
use linux::errno;

/// The calls handed to the host's kernel, where the host is Linux.
#[cfg(target_os = "linux")]
fn handed_on(c: &mut Cpu, m: &mut Mem, s: &mut Sys, number: u64, a: [u64; 6]) -> Option<Result<u64, u64>> {
    linux::call(c, m, s, number, a)
}

#[cfg(not(target_os = "linux"))]
fn handed_on(_c: &mut Cpu, _m: &mut Mem, _s: &mut Sys, _number: u64, _a: [u64; 6]) -> Option<Result<u64, u64>> {
    None
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


// ---- signals, as far as they go without being delivered ---------------------------------------

/// `rt_sigaction`: what was asked for is kept and what was asked before is given
/// back. The structure is the kernel's for x86-64: handler, flags, restorer, mask.
fn sigaction(m: &mut Mem, s: &mut Sys, signal: u64, new: u64, old: u64, size: u64) -> Result<u64, u64> {
    // Not for no signal, nor for the two that cannot be caught.
    if size != 8 || signal == 0 || signal > 64 || (new != 0 && (signal == 9 || signal == 19)) {
        return Err(EINVAL);
    }
    let was = s.proc().actions[signal as usize];
    if new != 0 {
        let mut asked = [0; 4];
        for (i, word) in asked.iter_mut().enumerate() {
            *word = m.read_u64(new + 8 * i as u64).map_err(|_| EFAULT)?;
        }
        s.proc().actions[signal as usize] = asked;
        #[cfg(target_os = "linux")]
        signal::disposition(signal, asked[0]);
    }
    if old != 0 {
        for (i, word) in was.iter().enumerate() {
            m.write_u64(old + 8 * i as u64, *word).map_err(|_| EFAULT)?;
        }
    }
    Ok(0)
}

/// `sigaltstack`: the stack for handlers that ask for another, kept and given back.
/// The structure: where it begins, its state (2: there is none), its size.
fn sigaltstack(m: &mut Mem, s: &mut Sys, new: u64, old: u64) -> Result<u64, u64> {
    let (was_sp, was_size) = s.alt_stack;
    if new != 0 {
        let sp = m.read_u64(new).map_err(|_| EFAULT)?;
        let state = m.read_u32(new + 8).map_err(|_| EFAULT)?;
        let size = m.read_u64(new + 16).map_err(|_| EFAULT)?;
        s.alt_stack = if state & 2 != 0 { (0, 0) } else { (sp, size) };
    }
    if old != 0 {
        m.write_u64(old, was_sp).map_err(|_| EFAULT)?;
        m.write_u32(old + 8, if was_size == 0 { 2 } else { 0 }).map_err(|_| EFAULT)?;
        m.write_u64(old + 16, was_size).map_err(|_| EFAULT)?;
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
    fn mremap_shrinks_in_place_and_grows_by_moving() {
        let (mut c, mut m, mut s) = guest();
        let at = ask(&mut c, &mut m, &mut s, 9, &[0, 3 * 4096, 3, 0x22, u64::MAX, 0]);
        m.write_u64(at, 0x1111).unwrap();
        m.write_u64(at + 4096, 0x2222).unwrap();
        // Smaller: where it is, the rest gone.
        assert_eq!(ask(&mut c, &mut m, &mut s, 25, &[at, 3 * 4096, 2 * 4096, 0]), at);
        assert!(m.read_u64(at + 2 * 4096).is_err());
        // Larger, not allowed to move: refused. Allowed: somewhere else, with what it
        // held, and the old place gone.
        assert_eq!(ask(&mut c, &mut m, &mut s, 25, &[at, 2 * 4096, 5 * 4096, 0]), ENOMEM.wrapping_neg());
        // Of what is not mapped: EFAULT, whether or not it may move.
        assert_eq!(ask(&mut c, &mut m, &mut s, 25, &[at + 2 * 4096, 4096, 2 * 4096, 0]), EFAULT.wrapping_neg());
        assert_eq!(ask(&mut c, &mut m, &mut s, 25, &[at + 2 * 4096, 4096, 2 * 4096, 1]), EFAULT.wrapping_neg());
        let to = ask(&mut c, &mut m, &mut s, 25, &[at, 2 * 4096, 5 * 4096, 1]);
        assert!(to != at && to % 4096 == 0);
        assert_eq!((m.read_u64(to), m.read_u64(to + 4096), m.read_u64(to + 4 * 4096)), (Ok(0x1111), Ok(0x2222), Ok(0)));
        m.write_u64(to + 4 * 4096, 1).unwrap();
        assert!(m.read_u64(at).is_err());
        assert_eq!(ask(&mut c, &mut m, &mut s, 25, &[at + 1, 4096, 8192, 1]), EINVAL.wrapping_neg());
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
        assert_eq!(call(&mut c, &mut m, &mut s), Some(Exit::Group(2)));
        // And `exit` is the thread's.
        c.reg[RAX] = 60;
        c.reg[RDI] = 7;
        assert_eq!(call(&mut c, &mut m, &mut s), Some(Exit::Thread(7)));
    }
}
