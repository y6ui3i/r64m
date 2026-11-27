//! What a call does not use, it does not look at (R059; `tests/guest.rs`). The kernel
//! takes six registers with every call and reads as many as the call has arguments,
//! or fewer where a command says so; the C library leaves the others as they were.
//! R056's hang was r64m refusing a `futex` wake for what its fifth register held.
//!
//! Each call here is made twice: the registers it does not use hold zero, and then
//! all ones. A line says how each of the two ended: "ok", or the error's number.
//! Wherever x86-64 Linux is done rightly the two are the same.

use std::arch::asm;

const ONES: u64 = !0;

fn call(number: u64, a: [u64; 6]) -> i64 {
    let answer: i64;
    unsafe {
        asm!("syscall", inlateout("rax") number as i64 => answer, in("rdi") a[0], in("rsi") a[1], in("rdx") a[2], in("r10") a[3], in("r8") a[4], in("r9") a[5], lateout("rcx") _, lateout("r11") _, options(nostack));
    }
    answer
}

/// The call with the registers past its own arguments holding `rest`.
fn with(number: u64, used: &[u64], rest: u64) -> i64 {
    let mut a = [rest; 6];
    a[..used.len()].copy_from_slice(used);
    call(number, a)
}

fn said(answer: i64) -> String {
    if answer < 0 { answer.to_string() } else { "ok".to_string() }
}

/// What the call leaves open, to be closed before the next.
#[derive(Clone, Copy, PartialEq)]
enum Leaves {
    Nothing,
    Descriptor,
    TwoAt(u64),
}

fn twice(name: &str, number: u64, used: &[u64], leaves: Leaves) {
    let mut line = format!("{name}:");
    for rest in [0, ONES] {
        let answer = with(number, used, rest);
        line += &format!(" {}", said(answer));
        if answer >= 0 {
            match leaves {
                Leaves::Nothing => {}
                Leaves::Descriptor => {
                    call(3, [answer as u64, 0, 0, 0, 0, 0]);
                }
                Leaves::TwoAt(at) => {
                    let two = unsafe { *(at as *const [i32; 2]) };
                    call(3, [two[0] as u64, 0, 0, 0, 0, 0]);
                    call(3, [two[1] as u64, 0, 0, 0, 0, 0]);
                }
            }
        }
    }
    println!("{line}");
}

fn main() {
    use Leaves::{Descriptor, Nothing, TwoAt};
    // Somewhere for answers, a word to wait on, a page of its own, and files.
    let mut space = [0u64; 512];
    let to = space.as_mut_ptr() as u64;
    let word = 0u32;
    let word = &word as *const u32 as u64;
    let none = b"/dev/null\0".as_ptr() as u64;
    let root = b"/\0".as_ptr() as u64;
    let short = [0u64, 1];
    let short = short.as_ptr() as u64;
    let page = call(9, [0, 8192, 3, 0x22, ONES, 0]) as u64;
    let file = call(2, [none, 2, 0, 0, 0, 0]) as u64;
    let me = call(39, [0; 6]) as u64;
    let thread = call(186, [0; 6]) as u64;
    assert!((page as i64) > 0 && (file as i64) >= 0);

    // ---- calls with a fixed number of arguments: the registers past them.
    twice("read of nothing", 0, &[file, to, 0], Nothing);
    twice("write", 1, &[file, to, 1], Nothing);
    twice("open", 2, &[none, 0, 0], Descriptor);
    twice("close of no descriptor", 3, &[ONES], Nothing);
    twice("fstat", 5, &[file, to], Nothing);
    twice("lseek", 8, &[file, 0, 1], Nothing);
    twice("mprotect", 10, &[page, 4096, 3], Nothing);
    twice("brk", 12, &[0], Nothing);
    twice("rt_sigprocmask", 14, &[0, 0, to, 8], Nothing);
    twice("access", 21, &[root, 0], Nothing);
    twice("sched_yield", 24, &[], Nothing);
    twice("madvise", 28, &[page, 4096, 0], Nothing);
    twice("dup", 32, &[file], Descriptor);
    twice("nanosleep", 35, &[short, 0], Nothing);
    twice("getpid", 39, &[], Nothing);
    twice("socket", 41, &[1, 1, 0], Descriptor);
    twice("wait4 with no child", 61, &[ONES, 0, 1, 0], Nothing);
    twice("kill with no signal", 62, &[me, 0], Nothing);
    twice("uname", 63, &[to], Nothing);
    twice("fsync", 74, &[file], Nothing);
    twice("getcwd", 79, &[to, 4096], Nothing);
    twice("umask", 95, &[0o22], Nothing);
    twice("gettimeofday", 96, &[to, 0], Nothing);
    twice("getrusage", 98, &[0, to], Nothing);
    twice("sysinfo", 99, &[to], Nothing);
    twice("times", 100, &[to], Nothing);
    twice("getuid", 102, &[], Nothing);
    twice("getgid", 104, &[], Nothing);
    twice("geteuid", 107, &[], Nothing);
    twice("getegid", 108, &[], Nothing);
    twice("getppid", 110, &[], Nothing);
    twice("getpgid", 121, &[0], Nothing);
    twice("getsid", 124, &[0], Nothing);
    twice("sigaltstack", 131, &[0, to], Nothing);
    twice("gettid", 186, &[], Nothing);
    twice("time", 201, &[0], Nothing);
    twice("sched_getaffinity", 204, &[0, 128, to], Nothing);
    twice("clock_gettime", 228, &[1, to], Nothing);
    twice("clock_getres", 229, &[1, to], Nothing);
    twice("tgkill with no signal", 234, &[me, thread, 0], Nothing);
    twice("openat", 257, &[(-100i64) as u64, none, 0], Descriptor);
    twice("eventfd2", 290, &[0, 0], Descriptor);
    twice("epoll_create1", 291, &[0], Descriptor);
    twice("pipe2", 293, &[to, 0], TwoAt(to));
    twice("prlimit64", 302, &[0, 7, 0, to], Nothing);
    twice("getrandom", 318, &[to, 8, 0], Nothing);

    // ---- calls where a command says how many arguments there are.
    // open, openat: no mode unless a file may be made.
    twice("open, no mode", 2, &[none, 0], Descriptor);
    // ioctl FIOCLEX and fcntl F_GETFD, F_GETFL: no third.
    twice("ioctl FIOCLEX", 16, &[file, 0x5451], Nothing);
    twice("fcntl F_GETFD", 72, &[file, 1], Nothing);
    twice("fcntl F_GETFL", 72, &[file, 3], Nothing);
    // mremap in place: no new address.
    twice("mremap in place", 25, &[page, 4096, 4096, 0], Nothing);
    // futex: a wake has three, a wait four (R056).
    twice("futex wake", 202, &[word, 129, 1], Nothing);
    twice("futex wait on another value", 202, &[word, 128, 1, 0], Nothing);
    // epoll_ctl: taking a descriptor out names no event; shmctl: removing has
    // nothing to say. Made anew for each of the two, with the pointer the call does
    // not use at nothing and then at nowhere.
    let epoll = call(291, [0; 6]) as u64;
    let event = [1u32, 0, 0];
    let mut line = "epoll_ctl, taken out:".to_string();
    for rest in [0, ONES] {
        let added = call(233, [epoll, 1, file_for_epoll(to), event.as_ptr() as u64, 0, 0]);
        let answer = call(233, [epoll, 2, file_for_epoll(to), rest, rest, rest]);
        line += &format!(" {} {}", said(added), said(answer));
    }
    println!("{line}");
    let mut line = "shmctl, removed:".to_string();
    for rest in [0, ONES] {
        let id = call(29, [0, 4096, 0o1600, 0, 0, 0]);
        let answer = if id >= 0 { call(31, [id as u64, 0, rest, rest, rest, rest]) } else { id };
        line += &format!(" {} {}", said(id), said(answer));
    }
    println!("{line}");

    // And the last: the process ends with its code whatever else the registers hold.
    call(231, [7, ONES, ONES, ONES, ONES, ONES]);
}

/// A pipe's reading end, made once and kept where `to` points past what the calls
/// above write: a descriptor `epoll` can watch (a file like `/dev/null` it refuses).
fn file_for_epoll(to: u64) -> u64 {
    let at = (to + 2048) as *mut [i32; 3];
    unsafe {
        if (*at)[2] == 0 {
            assert_eq!(call(293, [at as u64, 0, 0, 0, 0, 0]), 0);
            (*at)[2] = 1;
        }
        (*at)[0] as u64
    }
}
