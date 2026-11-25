//! Signals as a program meets them, for r64m to be held to the real machine by
//! (`tests/guest.rs`): each case prints one line, and the lines are the same wherever
//! x86-64 Linux is done rightly.

use std::arch::asm;
use std::hint::black_box;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering::SeqCst};

static TICKS: AtomicI64 = AtomicI64::new(0);
static GOT: AtomicI64 = AtomicI64::new(0);
static WHERE: AtomicI64 = AtomicI64::new(0);
static SKIP: AtomicI64 = AtomicI64::new(0);
static ORDER: [AtomicI64; 4] = [const { AtomicI64::new(0) }; 4];
static N: AtomicUsize = AtomicUsize::new(0);
static mut OTHER: [u8; 65536] = [0; 65536];

type Handler = extern "C" fn(i32, *mut libc::siginfo_t, *mut libc::c_void);

extern "C" fn tick(_: i32, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // Floating point of its own, to be put right again when it returns.
    black_box(black_box(3.25f64).sqrt() * black_box(-7.5f64));
    TICKS.fetch_add(1, SeqCst);
}

extern "C" fn note(signal: i32, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    GOT.store(signal as i64, SeqCst);
}

/// Steps over the instruction that faulted (its length is in `SKIP`) and changes a
/// register in the frame.
extern "C" fn over(signal: i32, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let context = unsafe { &mut *(context as *mut libc::ucontext_t) };
    GOT.store(signal as i64, SeqCst);
    WHERE.store(unsafe { (*info).si_addr() } as i64, SeqCst);
    context.uc_mcontext.gregs[libc::REG_RIP as usize] += SKIP.load(SeqCst);
    context.uc_mcontext.gregs[libc::REG_R12 as usize] = 0x1234;
}

extern "C" fn on_other(_: i32, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    let here = 0u8;
    let at = &raw const here as usize;
    let other = &raw const OTHER as usize;
    GOT.store((at >= other && at < other + 65536) as i64, SeqCst);
}

fn mark(what: i64) {
    ORDER[N.fetch_add(1, SeqCst).min(3)].store(what, SeqCst);
}

extern "C" fn inner(_: i32, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    mark(2);
}

extern "C" fn outer(_: i32, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    mark(1);
    unsafe { libc::raise(libc::SIGUSR2) };
    mark(3);
}

/// Raises itself once from inside itself, which it may only if it said so.
extern "C" fn again(signal: i32, _: *mut libc::siginfo_t, _: *mut libc::c_void) {
    let depth = TICKS.fetch_add(1, SeqCst);
    mark(10 + depth);
    if depth == 0 {
        unsafe { libc::raise(signal) };
        mark(20);
    }
}

fn set(signal: i32, handler: Handler, flags: i32) {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as usize;
        action.sa_flags = flags | libc::SA_SIGINFO;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(signal, &action, std::ptr::null_mut());
    }
}

fn timer(first_us: i64, every_us: i64) {
    let t = libc::itimerval {
        it_interval: libc::timeval { tv_sec: 0, tv_usec: every_us },
        it_value: libc::timeval { tv_sec: 0, tv_usec: first_us },
    };
    unsafe { libc::setitimer(libc::ITIMER_REAL, &t, std::ptr::null_mut()) };
}

fn order() -> String {
    let n = N.swap(0, SeqCst).min(4);
    (0..n).map(|i| ORDER[i].load(SeqCst).to_string()).collect::<Vec<_>>().join(" ")
}

/// A sum of floats that a handler's own arithmetic must not disturb.
fn sum(until: impl Fn(u64) -> bool) -> (f64, u64) {
    let (mut s, mut i) = (0.0f64, 1u64);
    while !until(i) {
        s += 1.0 / black_box(i as f64);
        i += 1;
    }
    (s, i)
}

fn main() {
    // 1. A timer every 5 ms while the program only adds floats; the same sum again
    //    with no timer must be the same to the last bit.
    set(libc::SIGALRM, tick, 0);
    timer(5000, 5000);
    let (with, n) = sum(|_| TICKS.load(SeqCst) >= 5);
    timer(0, 0);
    let (without, _) = sum(|i| i >= n);
    println!("1 timer: {} ticks, floats {}", TICKS.load(SeqCst).min(5), if with.to_bits() == without.to_bits() { "the same" } else { "differ" });

    // 2. A store through a bad pointer: caught, and stepped over in the frame.
    set(libc::SIGSEGV, over, 0);
    SKIP.store(3, SeqCst);
    let mut r12 = 0u64;
    unsafe { asm!("mov byte ptr [rax], 1", in("rax") 0x20usize, inout("r12") r12) };
    println!("2 fault: signal {} at {:#x}, a register from the frame {:#x}", GOT.load(SeqCst), WHERE.load(SeqCst), r12);

    // 3. A division by nothing, stepped over the same way.
    set(libc::SIGFPE, over, 0);
    SKIP.store(2, SeqCst);
    GOT.store(0, SeqCst);
    unsafe { asm!("div ecx", in("ecx") 0u32, inout("eax") 7u32 => _, inout("edx") 0u32 => _, inout("r12") r12 => _) };
    println!("3 division: signal {}, at the instruction {}", GOT.load(SeqCst), WHERE.load(SeqCst) != 0);

    // 4. A read cut short (no restart asked), then one started again (asked).
    let mut pipe = [0i32; 2];
    let mut byte = 0u8;
    unsafe { libc::pipe(pipe.as_mut_ptr()) };
    set(libc::SIGALRM, note, 0);
    GOT.store(0, SeqCst);
    timer(20000, 0);
    let r = unsafe { libc::read(pipe[0], &raw mut byte as *mut libc::c_void, 1) };
    let cut = std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR);
    println!("4 read cut short: {r}, EINTR {cut}, the handler saw {}", GOT.load(SeqCst));
    set(libc::SIGALRM, note, libc::SA_RESTART);
    GOT.store(0, SeqCst);
    if unsafe { libc::fork() } == 0 {
        unsafe {
            libc::usleep(150_000);
            libc::write(pipe[1], b"x".as_ptr() as *const libc::c_void, 1);
            libc::_exit(0);
        }
    }
    timer(20000, 0);
    let r = unsafe { libc::read(pipe[0], &raw mut byte as *mut libc::c_void, 1) };
    println!("4 read started again: {r} '{}', the handler saw {}", byte as char, GOT.load(SeqCst));
    unsafe { libc::wait(std::ptr::null_mut()) };

    // 5. A handler on another stack.
    let stack = libc::stack_t { ss_sp: &raw mut OTHER as *mut libc::c_void, ss_flags: 0, ss_size: 65536 };
    unsafe { libc::sigaltstack(&stack, std::ptr::null_mut()) };
    set(libc::SIGUSR1, on_other, libc::SA_ONSTACK);
    GOT.store(0, SeqCst);
    unsafe { libc::raise(libc::SIGUSR1) };
    println!("5 on the other stack: {}", GOT.load(SeqCst));

    // 6. One signal inside another's handler.
    set(libc::SIGUSR1, outer, 0);
    set(libc::SIGUSR2, inner, 0);
    unsafe { libc::raise(libc::SIGUSR1) };
    println!("6 one inside another: {}", order());

    // 7. Held back, seen waiting, and let go.
    unsafe {
        let (mut one, mut was, mut waiting): (libc::sigset_t, libc::sigset_t, libc::sigset_t) = std::mem::zeroed();
        libc::sigemptyset(&mut one);
        libc::sigaddset(&mut one, libc::SIGUSR2);
        libc::sigprocmask(libc::SIG_BLOCK, &one, &mut was);
        libc::raise(libc::SIGUSR2);
        let ran = N.load(SeqCst);
        libc::sigpending(&mut waiting);
        libc::sigprocmask(libc::SIG_SETMASK, &was, std::ptr::null_mut());
        println!("7 held: ran {ran} while blocked, waiting {}, then {}", libc::sigismember(&waiting, libc::SIGUSR2), order());
    }

    // 8. The same signal inside its own handler: not unless the handler said so.
    for (flags, name) in [(0, "deferred"), (libc::SA_NODEFER, "at once")] {
        TICKS.store(0, SeqCst);
        set(libc::SIGUSR1, again, flags);
        unsafe { libc::raise(libc::SIGUSR1) };
        println!("8 itself inside itself, {name}: {}", order());
    }

    // 9. A handler asked for once only: the second time the signal ends the process
    //    (a child's, so that this one can say so).
    let child = unsafe { libc::fork() };
    if child == 0 {
        set(libc::SIGUSR1, note, libc::SA_RESETHAND);
        unsafe {
            libc::raise(libc::SIGUSR1);
            libc::raise(libc::SIGUSR1);
            libc::_exit(0);
        }
    }
    let mut status = 0;
    unsafe { libc::waitpid(child, &mut status, 0) };
    println!("9 once only: the child ended by signal {}", if libc::WIFSIGNALED(status) { libc::WTERMSIG(status) } else { 0 });
}
