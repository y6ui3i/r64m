//! Where r64m's own time goes (`--profile`): a timer interrupts the process a
//! thousand times for each second of CPU time it uses, and the place it was at is
//! noted. At the end the places are printed with how often each was met, and how many
//! were in generated code.
//!
//! A tool for the one thing (ADR 0018): it shows time spent that is not spent
//! behaving as x86-64. The places are addresses in r64m's own code; `main`'s address
//! is printed beside them so that they can be matched to the names in the file r64m
//! was built as. While it is on, a system call of the guest's that waits may be cut
//! short by the timer, as by any signal.

use std::sync::atomic::{AtomicUsize, Ordering};

/// The places noted, in the order they were met; no more than this many (a minute
/// and more of CPU time).
const MOST: usize = 1 << 17;
static PLACES: [AtomicUsize; MOST] = [const { AtomicUsize::new(0) }; MOST];
static COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(target_os = "macos")]
unsafe fn pc_of(context: *mut libc::c_void) -> usize {
    unsafe { (*(*(context as *mut libc::ucontext_t)).uc_mcontext).__ss.__pc as usize }
}

#[cfg(not(target_os = "macos"))]
unsafe fn pc_of(context: *mut libc::c_void) -> usize {
    unsafe { (*(context as *mut libc::ucontext_t)).uc_mcontext.pc as usize }
}

extern "C" fn tick(_signal: libc::c_int, _info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    if n < MOST {
        PLACES[n].store(unsafe { pc_of(context) }, Ordering::Relaxed);
    }
}

/// Starts the timer.
pub fn start() -> std::io::Result<()> {
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = tick as *const () as usize;
        act.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART;
        libc::sigemptyset(&mut act.sa_mask);
        if libc::sigaction(libc::SIGPROF, &act, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let every = libc::timeval { tv_sec: 0, tv_usec: 1000 };
        let timer = libc::itimerval { it_interval: every, it_value: every };
        if libc::setitimer(libc::ITIMER_PROF, &timer, std::ptr::null_mut()) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The places noted, as they were met (for a second reading of them, after `report`).
pub fn places() -> Vec<usize> {
    let n = COUNT.load(Ordering::Relaxed).min(MOST);
    PLACES.iter().take(n).map(|p| p.load(Ordering::Relaxed)).collect()
}

/// Stops the timer and prints what was noted: `generated` says of a place whether it
/// is in generated code, `anchor` is the address of a function whose name is known,
/// and `guest` says of a place in generated code which block it is in (by the guest's
/// address of its first instruction), which of the guest's instructions, and how many
/// host instructions from the block's start (R045). What comes back is the blocks by
/// how often they were met, the most first.
pub fn report(generated: impl Fn(usize) -> bool, anchor: usize, guest: impl Fn(usize) -> Option<(u64, u64, u32)>) -> Vec<(u64, usize)> {
    unsafe {
        let off: libc::itimerval = std::mem::zeroed();
        libc::setitimer(libc::ITIMER_PROF, &off, std::ptr::null_mut());
    }
    let n = COUNT.load(Ordering::Relaxed).min(MOST);
    let mut by_place = std::collections::BTreeMap::new();
    let mut by_guest = std::collections::BTreeMap::new();
    let mut by_block = std::collections::BTreeMap::new();
    let mut in_generated = 0;
    for place in PLACES.iter().take(n).map(|p| p.load(Ordering::Relaxed)) {
        if generated(place) {
            in_generated += 1;
            if let Some((block, at, word)) = guest(place) {
                *by_guest.entry((block, word, at)).or_insert(0usize) += 1;
                *by_block.entry(block).or_insert(0usize) += 1;
            }
        } else {
            *by_place.entry(place).or_insert(0usize) += 1;
        }
    }
    eprintln!("r64m: profile: {n} samples, {in_generated} in generated code; main is at {anchor:#x}");
    for (place, count) in by_place {
        eprintln!("r64m: profile: {count} {place:#x}");
    }
    for ((block, word, at), count) in by_guest {
        eprintln!("r64m: profile: guest {count} {block:#x} {word} {at:#x}");
    }
    let mut blocks: Vec<(u64, usize)> = by_block.into_iter().collect();
    blocks.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    blocks
}
