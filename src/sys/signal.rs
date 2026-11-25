//! Signals delivered to the guest's handlers (ADR 0019, point 2). For a Linux host.
//!
//! The kernel does what is a kernel's: it chooses the thread, it holds a signal back
//! while the thread blocks it, it cuts a waiting call short. r64m gives the host's
//! signal a handler of its own that only notes it for the thread it arrived on and
//! sets a word that every block looks at. Between blocks and between the
//! interpreter's instructions the guest's handler is then entered as Linux enters one
//! on x86-64: a frame on the guest's stack with what the signal was and every
//! register, floating point's too, and the guest's restorer as the address to return
//! to. `rt_sigreturn` puts the registers back from the frame.

use super::{block_on_host, Sys, EFAULT};
use crate::cpu::{self, Cpu, Stop, RAX, RDI, RDX, RSI, RSP};
use crate::mem::{Access, Mem};
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicU64, Ordering};

const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
/// The flags a handler may change by what it leaves in its frame: the six of
/// arithmetic and the direction.
const FLAGS_OF_THE_PROGRAM: u64 = 0xcd5;

/// What has arrived for one thread and waits to be delivered: a bit for each signal,
/// what the kernel said of each, and where the thread's "look" word is.
struct Waiting {
    pending: AtomicU64,
    info: UnsafeCell<[[u8; 128]; 65]>,
    kick: Cell<*mut u32>,
}

thread_local! {
    static WAITING: Waiting = const {
        Waiting { pending: AtomicU64::new(0), info: UnsafeCell::new([[0; 128]; 65]), kick: Cell::new(std::ptr::null_mut()) }
    };
}

/// The host's handler: the signal is noted for this thread, and its blocks are told
/// to leave. Nothing else is done here.
extern "C" fn arrived(signal: libc::c_int, info: *mut libc::siginfo_t, _context: *mut libc::c_void) {
    WAITING.with(|w| {
        if (1..=64).contains(&signal) {
            if !info.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(info as *const u8, (*w.info.get())[signal as usize].as_mut_ptr(), 128) };
            }
            w.pending.fetch_or(1 << (signal - 1), Ordering::AcqRel);
        }
        let kick = w.kick.get();
        if !kick.is_null() {
            unsafe { kick.write_volatile(1) };
        }
    });
}

/// From now this thread's signals set `c.kick`: the `Cpu` must stay where it is for
/// as long as the thread runs the guest.
pub fn attend(c: &mut Cpu) {
    WAITING.with(|w| {
        w.kick.set(&mut c.kick);
        if w.pending.load(Ordering::Acquire) != 0 {
            c.kick = 1;
        }
    });
}

/// The guest gave `signal` a handler (or took it away): the host's signal gets
/// r64m's, or is ignored or left to the kernel as the guest asked. Not the two a
/// fault in generated code arrives by: those are r64m's, and the guest's handlers for
/// them are entered for the guest's own faults (`fault`).
pub fn disposition(signal: u64, handler: u64) {
    if signal == libc::SIGSEGV as u64 || signal == libc::SIGBUS as u64 {
        return;
    }
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        match handler {
            0 => act.sa_sigaction = libc::SIG_DFL,
            1 => act.sa_sigaction = libc::SIG_IGN,
            _ => {
                act.sa_sigaction = arrived as *const () as usize;
                // No restart by the kernel: a call cut short comes back, the handler
                // is entered, and r64m starts the call again if the guest asked for
                // that (`restarts`). While r64m's handler runs nothing else arrives.
                act.sa_flags = libc::SA_SIGINFO;
                libc::sigfillset(&mut act.sa_mask);
            }
        }
        libc::sigaction(signal as i32, &act, std::ptr::null_mut());
    }
}

/// Is a signal waiting whose handler asked for calls it cuts short to be started
/// again?
pub fn restarts(s: &Sys) -> bool {
    let pending = WAITING.with(|w| w.pending.load(Ordering::Acquire));
    let actions = s.proc().actions;
    (1..=64).any(|signal| pending >> (signal - 1) & 1 != 0 && actions[signal][0] > 1 && actions[signal][1] & SA_RESTART != 0)
}

/// The signals noted for this thread and not yet delivered.
pub fn noted() -> u64 {
    WAITING.with(|w| w.pending.load(Ordering::Acquire))
}

/// Sets the look word again if something waits that is no longer blocked: after the
/// set of blocked signals has changed.
pub fn look_again(c: &mut Cpu, s: &Sys) {
    if WAITING.with(|w| w.pending.load(Ordering::Acquire)) & !s.blocked != 0 {
        unsafe { (&mut c.kick as *mut u32).write_volatile(1) };
    }
}

/// Enters the guest's handlers for what waits. To be called between instructions,
/// with the flags worked out. False if a frame could not be written: the guest's
/// stack is not there, which ends it.
pub fn deliver(c: &mut Cpu, m: &mut Mem, s: &mut Sys) -> bool {
    deliver_with(c, m, s, None)
}

/// The same, just after a call that waited with another set of blocked signals for
/// its duration (`rt_sigsuspend`, `ppoll`, `pselect6`, `epoll_pwait`): what arrived
/// meanwhile is delivered by that set, and the first handler's frame holds the set
/// the thread had before, which `rt_sigreturn` puts back: as the kernel does it. A
/// program that keeps a signal blocked but for such a call is counting on this.
pub fn deliver_after_wait(c: &mut Cpu, m: &mut Mem, s: &mut Sys, during: u64) -> bool {
    deliver_with(c, m, s, Some(during & !(1 << 8 | 1 << 18)))
}

fn deliver_with(c: &mut Cpu, m: &mut Mem, s: &mut Sys, during: Option<u64>) -> bool {
    unsafe { (&mut c.kick as *mut u32).write_volatile(0) };
    let waiting = WAITING.with(|w| w.pending.swap(0, Ordering::AcqRel));
    // The set to put back after the first handler; then each saves what it finds.
    let mut before = during.map(|set| std::mem::replace(&mut s.blocked, set));
    for signal in 1..=64usize {
        if waiting >> (signal - 1) & 1 == 0 {
            continue;
        }
        // Blocked (by the thread, or by a handler just entered for another): it waits.
        if s.blocked >> (signal - 1) & 1 != 0 {
            WAITING.with(|w| w.pending.fetch_or(1 << (signal - 1), Ordering::AcqRel));
            continue;
        }
        let action = s.proc().actions[signal];
        if action[0] <= 1 {
            continue;
        }
        let info = WAITING.with(|w| unsafe { (*w.info.get())[signal] });
        let saved = before.take().unwrap_or(s.blocked);
        if !enter(c, m, s, signal as u64, &info, action, [0; 3], saved) {
            return false;
        }
    }
    // No handler was entered: the thread's own set again.
    if let Some(old) = before {
        s.blocked = old;
    }
    look_again(c, s);
    true
}

/// The frame, and the handler's first instruction. `trap` is what a fault adds to
/// the saved registers: its kind, its error code and the address.
#[allow(clippy::too_many_arguments)]
fn enter(c: &mut Cpu, m: &mut Mem, s: &mut Sys, signal: u64, info: &[u8; 128], action: [u64; 4], trap: [u64; 3], saved_blocked: u64) -> bool {
    let [handler, flags, restorer, mask] = action;
    let (alt_sp, alt_size) = s.alt_stack;
    let on_alt = alt_size != 0 && c.reg[RSP].wrapping_sub(alt_sp) < alt_size;
    let mut sp = if flags & SA_ONSTACK != 0 && alt_size != 0 && !on_alt {
        alt_sp + alt_size
    } else {
        // Past what the function interrupted may keep under its stack pointer.
        c.reg[RSP].wrapping_sub(128)
    };
    // Floating point's state, 512 bytes on a line of 64.
    sp = sp.wrapping_sub(512) & !63;
    let fp = sp;
    let mut image = [0u8; 512];
    image[..416].copy_from_slice(&cpu::fx_image(c));
    // The return address, the context (304 bytes), what the signal was (128).
    sp = (sp.wrapping_sub(8 + 304 + 128) & !15).wrapping_sub(8);
    let (context, about) = (sp + 8, sp + 8 + 304);
    let mut frame = [0u64; 39];
    frame[0] = restorer;
    // The context: flags, link, the alternate stack (where, its state, its size).
    frame[3] = alt_sp;
    frame[4] = if alt_size == 0 { 2 } else { on_alt as u64 };
    frame[5] = alt_size;
    // The registers, in the kernel's order.
    let r = &c.reg;
    let saved = [r[8], r[9], r[10], r[11], r[12], r[13], r[14], r[15], r[7], r[6], r[5], r[3], r[2], r[0], r[1], r[4], c.rip, c.rflags];
    frame[6..24].copy_from_slice(&saved);
    // The segments (CS for 64-bit code, SS), the fault's error code and kind, the
    // set of blocked signals as it was (twice: the old place and the new), the
    // fault's address, and where floating point's state is.
    frame[24] = 0x33 | 0x2b << 48;
    frame[25] = trap[1];
    frame[26] = trap[0];
    frame[27] = saved_blocked;
    frame[28] = trap[2];
    frame[29] = fp;
    frame[38] = saved_blocked;
    let mut bytes = [0u8; 39 * 8];
    for (i, word) in frame.iter().enumerate() {
        bytes[8 * i..8 * i + 8].copy_from_slice(&word.to_le_bytes());
    }
    if m.write_bytes(fp, &image).is_err() || m.write_bytes(sp, &bytes).is_err() || m.write_bytes(about, info).is_err() {
        return false;
    }
    c.reg[RDI] = signal;
    c.reg[RSI] = about;
    c.reg[RDX] = context;
    c.reg[RAX] = 0;
    c.reg[RSP] = sp;
    c.rip = handler;
    c.rflags &= !(cpu::DF | 0x100);
    // While it runs: what it asked to have blocked, and itself unless it said not.
    s.blocked |= mask | if flags & SA_NODEFER == 0 { 1 << (signal - 1) } else { 0 };
    s.blocked &= !(1 << 8 | 1 << 18);
    block_on_host(s.blocked);
    if flags & SA_RESETHAND != 0 {
        s.proc().actions[signal as usize][0] = 0;
        disposition(signal, 0);
    }
    true
}

/// `rt_sigreturn`: the registers, the flags, floating point's state and the set of
/// blocked signals as the frame at the stack pointer has them (a handler may have
/// changed them there).
pub fn sigreturn(c: &mut Cpu, m: &mut Mem, s: &mut Sys) -> Result<(), u64> {
    let context = c.reg[RSP];
    let word = |i: u64| m.read_u64(context + 8 * i).map_err(|_| EFAULT);
    let mut saved = [0u64; 18];
    for (i, w) in saved.iter_mut().enumerate() {
        *w = word(5 + i as u64)?;
    }
    let (fp, blocked) = (word(28)?, word(37)?);
    let mut image = [0u8; 416];
    if fp != 0 {
        m.read_bytes(fp, &mut image).map_err(|_| EFAULT)?;
        cpu::fx_take(c, &image);
    }
    let order = [8, 9, 10, 11, 12, 13, 14, 15, 7, 6, 5, 3, 2, 0, 1, 4];
    for (i, reg) in order.into_iter().enumerate() {
        c.reg[reg] = saved[i];
    }
    c.rip = saved[16];
    c.rflags = c.rflags & !FLAGS_OF_THE_PROGRAM | saved[17] & FLAGS_OF_THE_PROGRAM;
    s.blocked = blocked & !(1 << 8 | 1 << 18);
    block_on_host(s.blocked);
    look_again(c, s);
    Ok(())
}

/// A fault of the guest's own: the signal x86-64 Linux makes of it, for the guest's
/// handler if it has one that may be entered; else the process ends as that signal
/// ends one. `None` for what is not the guest's fault but r64m's lack.
pub fn fault(c: &mut Cpu, m: &mut Mem, s: &mut Sys, stop: &Stop) -> Option<bool> {
    // (the signal, its code, the address, the kind of trap, the trap's error code)
    let (signal, code, addr, trap, error) = match stop {
        Stop::Fault(f) => {
            // Not mapped at all, or mapped and not allowed; a write, and from a program.
            let mapped = m.check(f.addr, 1, Access::Read).is_ok();
            (libc::SIGSEGV as u64, if mapped { 2 } else { 1 }, f.addr, 14, 4 | (f.access == Access::Write) as u64 * 2 | mapped as u64)
        }
        Stop::Divide => (libc::SIGFPE as u64, 1, c.rip, 0, 0),
        Stop::Undefined => (libc::SIGILL as u64, 2, c.rip, 6, 0),
        Stop::Protection => (libc::SIGSEGV as u64, 0x80, 0, 13, 0),
        Stop::NotWritten(_) | Stop::Again => return None,
    };
    let action = s.proc().actions[signal as usize];
    if action[0] > 1 && s.blocked >> (signal - 1) & 1 == 0 {
        let mut info = [0u8; 128];
        info[0..4].copy_from_slice(&(signal as u32).to_le_bytes());
        info[8..12].copy_from_slice(&(code as u32).to_le_bytes());
        info[16..24].copy_from_slice(&addr.to_le_bytes());
        let cr2 = if trap == 14 { addr } else { 0 };
        let blocked = s.blocked;
        if enter(c, m, s, signal, &info, action, [trap, error, cr2], blocked) {
            return Some(true);
        }
    }
    Some(false)
}

/// The process ends as `signal` ends one that does nothing about it.
pub fn die_of(signal: i32) -> ! {
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signal);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(signal);
    }
    std::process::exit(128 + signal)
}

/// The signal a stop is, for `die_of`.
pub fn signal_of(stop: &Stop) -> i32 {
    match stop {
        Stop::Divide => libc::SIGFPE,
        Stop::Undefined => libc::SIGILL,
        _ => libc::SIGSEGV,
    }
}
