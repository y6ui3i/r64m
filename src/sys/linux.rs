//! The system calls that are handed to the host's kernel (ADR 0017, point 1), and the
//! processes (point 2). For a Linux host only.
//!
//! A call's number is x86-64's and is made arm64's; where arm64 has only the form
//! that takes a directory (`openat` for `open`), that form is used with "the working
//! directory". A pointer is `base + g`, and nothing is not a pointer; whether the
//! memory there may be read or written is the kernel's to find, since r64m's own
//! protection of it is the guest's (ADR 0008). Only that it lies in the guest's
//! reservation is seen to here. A structure laid out differently on the two is
//! written over: `stat` is the one so far.

use super::{open_flags, Sys, EFAULT, EINVAL, ENOSYS};
use crate::cpu::{Cpu, RSP};
use crate::mem::Mem;
use libc::c_long;
use std::ffi::CString;

const EACCES: u64 = 13;
const ENOEXEC: u64 = 8;
const ENOENT: u64 = 2;
const ENAMETOOLONG: u64 = 36;
const E2BIG: u64 = 7;
const AT_FDCWD: u64 = -100i64 as u64;

/// The host's own error of the call just made.
pub fn errno() -> u64 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(5) as u64
}

/// One call of the host's kernel by its own number.
fn raw(number: c_long, a: [u64; 6]) -> Result<u64, u64> {
    match unsafe { libc::syscall(number, a[0], a[1], a[2], a[3], a[4], a[5]) } {
        n if n < 0 => Err(errno()),
        n => Ok(n as u64),
    }
}

/// The host's address of a pointer of the guest's that needs no length: a string, or
/// a structure the kernel knows the size of. Nothing stays nothing.
fn p(m: &Mem, g: u64) -> Result<u64, u64> {
    match g {
        0 => Ok(0),
        g if g >= m.size() => Err(EFAULT),
        g => Ok(m.base() as u64 + g),
    }
}

/// The same for `len` bytes, all of which must lie in the reservation.
fn buf(m: &Mem, g: u64, len: u64) -> Result<u64, u64> {
    if len > m.size() || g > m.size() - len {
        return Err(EFAULT);
    }
    p(m, g)
}

/// A string of the guest's, copied out: for the calls r64m looks at itself.
fn string(m: &Mem, at: u64, most: usize) -> Result<Vec<u8>, u64> {
    let mut bytes = Vec::new();
    loop {
        match m.read_u8(at + bytes.len() as u64).map_err(|_| EFAULT)? {
            0 => return Ok(bytes),
            _ if bytes.len() >= most => return Err(if most <= 4096 { ENAMETOOLONG } else { E2BIG }),
            b => bytes.push(b),
        }
    }
}

/// The strings a list of pointers ended by nothing points at (`argv`, `envp`).
fn strings(m: &Mem, list: u64) -> Result<Vec<CString>, u64> {
    let mut all = Vec::new();
    if list == 0 {
        return Ok(all);
    }
    loop {
        match m.read_u64(list + 8 * all.len() as u64).map_err(|_| EFAULT)? {
            0 => return Ok(all),
            _ if all.len() >= 1 << 16 => return Err(E2BIG),
            at => all.push(CString::new(string(m, at, 1 << 17)?).expect("no zero inside")),
        }
    }
}

/// `stat`'s structure as x86-64 has it, written to the guest from the host's.
fn put_stat(m: &mut Mem, at: u64, st: &libc::stat) -> Result<u64, u64> {
    let mut b = [0u8; 144];
    let fields: [(usize, usize, u64); 16] = [
        (0, 8, st.st_dev),
        (8, 8, st.st_ino),
        (16, 8, st.st_nlink as u64),
        (24, 4, st.st_mode as u64),
        (28, 4, st.st_uid as u64),
        (32, 4, st.st_gid as u64),
        (40, 8, st.st_rdev),
        (48, 8, st.st_size as u64),
        (56, 8, st.st_blksize as u64),
        (64, 8, st.st_blocks as u64),
        (72, 8, st.st_atime as u64),
        (80, 8, st.st_atime_nsec as u64),
        (88, 8, st.st_mtime as u64),
        (96, 8, st.st_mtime_nsec as u64),
        (104, 8, st.st_ctime as u64),
        (112, 8, st.st_ctime_nsec as u64),
    ];
    for (at, n, v) in fields {
        b[at..at + n].copy_from_slice(&v.to_le_bytes()[..n]);
    }
    m.write_bytes(at, &b).map_err(|_| EFAULT)?;
    Ok(0)
}

fn stat_at(m: &mut Mem, dir: u64, path: u64, to: u64, flags: u64) -> Result<u64, u64> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir as i32, p(m, path)? as *const libc::c_char, &mut st, flags as i32) } != 0 {
        return Err(errno());
    }
    put_stat(m, to, &st)
}

/// `readv`, `writev` and their like: the guest's list of pieces made the host's.
fn vector(m: &Mem, number: c_long, fd: u64, list: u64, count: u64, offset: u64) -> Result<u64, u64> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let mut pieces = Vec::with_capacity(count as usize);
    for i in 0..count {
        let at = m.read_u64(list + 16 * i).map_err(|_| EFAULT)?;
        let len = m.read_u64(list + 16 * i + 8).map_err(|_| EFAULT)?;
        pieces.push(libc::iovec { iov_base: buf(m, at, len)? as *mut libc::c_void, iov_len: len as usize });
    }
    raw(number, [fd, pieces.as_ptr() as u64, count, offset, 0, 0])
}

/// The `ioctl`s that are handed on: those of a terminal and of a descriptor whose
/// argument is nothing, a number, or a structure the two kernels lay out alike.
fn ioctl(m: &Mem, fd: u64, request: u64, arg: u64) -> Result<u64, u64> {
    match request {
        // TCGETS, TCSETS, TCSETSW, TCSETSF, TIOCGPGRP, TIOCSPGRP, TIOCGWINSZ,
        // TIOCSWINSZ, FIONREAD, FIONBIO.
        0x5401..=0x5404 | 0x540f | 0x5410 | 0x5413 | 0x5414 | 0x541b | 0x5421 => raw(libc::SYS_ioctl, [fd, request, p(m, arg)?, 0, 0, 0]),
        // FIONCLEX, FIOCLEX.
        0x5450 | 0x5451 => raw(libc::SYS_ioctl, [fd, request, 0, 0, 0, 0]),
        _ => Err(super::ENOTTY),
    }
}

/// `fcntl` for copying a descriptor, for its flags and for locks. The flags of an
/// open file (`F_GETFL`, `F_SETFL`) are numbered differently on the two kernels in
/// four places.
fn fcntl(m: &Mem, fd: u64, cmd: u64, arg: u64) -> Result<u64, u64> {
    const F_GETFL: u64 = 3;
    const F_SETFL: u64 = 4;
    let arg = match cmd {
        // F_DUPFD, F_GETFD, F_SETFD, F_DUPFD_CLOEXEC, F_GETFL.
        0 | 1 | 2 | 1030 | F_GETFL => arg,
        F_SETFL => open_flags(arg, true),
        // F_GETLK, F_SETLK, F_SETLKW: a structure that is the same on both.
        5..=7 => p(m, arg)?,
        _ => return Err(EINVAL),
    };
    let done = raw(libc::SYS_fcntl, [fd, cmd, arg, 0, 0, 0])?;
    Ok(if cmd == F_GETFL { open_flags(done, false) } else { done })
}

/// What a link in `/proc` that names the running program says: the guest's program,
/// not r64m.
fn readlink(m: &mut Mem, s: &Sys, dir: u64, path: u64, to: u64, len: u64) -> Result<u64, u64> {
    if string(m, path, 4096)? == b"/proc/self/exe" {
        let name = s.program.as_bytes();
        let n = name.len().min(len as usize);
        m.write_bytes(to, &name[..n]).map_err(|_| EFAULT)?;
        return Ok(n as u64);
    }
    raw(libc::SYS_readlinkat, [dir, p(m, path)?, buf(m, to, len)?, len, 0, 0])
}

// ---- processes ---------------------------------------------------------------------------------

const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const CLONE_PARENT_SETTID: u64 = 0x10_0000;
const CLONE_CHILD_CLEARTID: u64 = 0x20_0000;
const CLONE_CHILD_SETTID: u64 = 0x100_0000;

/// A new process: the host's `fork` of r64m whole, its blocks with it. If the parent
/// is to wait until the child has started another program or ended (`vfork`), it
/// waits at a pipe whose other end the child holds until then.
fn fork(c: &mut Cpu, m: &mut Mem, s: &mut Sys, flags: u64, stack: u64, parent_tid: u64, child_tid: u64) -> Result<u64, u64> {
    // Threads are not built: memory shared with a child that goes on beside.
    if flags & CLONE_VM != 0 && flags & CLONE_VFORK == 0 {
        return Err(ENOSYS);
    }
    let mut wait = [-1i32; 2];
    if flags & CLONE_VFORK != 0 {
        // Closed by the kernel when a program is started; put high, out of the way
        // of the descriptors the program counts on.
        let mut low = [0i32; 2];
        if unsafe { libc::pipe2(low.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(errno());
        }
        for i in 0..2 {
            wait[i] = unsafe { libc::fcntl(low[i], libc::F_DUPFD_CLOEXEC, 1000) };
            unsafe { libc::close(low[i]) };
        }
    }
    match unsafe { libc::fork() } {
        pid if pid < 0 => {
            let e = errno();
            unsafe { (libc::close(wait[0]), libc::close(wait[1])) };
            Err(e)
        }
        0 => {
            unsafe { libc::close(wait[0]) };
            if stack != 0 {
                c.reg[RSP] = stack;
            }
            if flags & CLONE_CHILD_SETTID != 0 {
                let _ = m.write_u32(child_tid, unsafe { libc::gettid() } as u32);
            }
            if flags & CLONE_CHILD_CLEARTID != 0 {
                s.clear_tid = child_tid;
            }
            Ok(0)
        }
        pid => {
            if flags & CLONE_PARENT_SETTID != 0 {
                let _ = m.write_u32(parent_tid, pid as u32);
            }
            if wait[0] >= 0 {
                unsafe { libc::close(wait[1]) };
                let mut byte = 0u8;
                // Until every copy of the other end is closed.
                while unsafe { libc::read(wait[0], &mut byte as *mut u8 as *mut libc::c_void, 1) } < 0 && errno() == libc::EINTR as u64 {}
                unsafe { libc::close(wait[0]) };
            }
            Ok(pid as u64)
        }
    }
}

/// Another program in this process: r64m started again with it, by the host's
/// `execve`, with r64m's own options as they were. What the kernel would refuse is
/// refused here first, while there is still a program to tell.
fn execve(m: &Mem, s: &Sys, path: u64, argv: u64, envp: u64) -> Result<u64, u64> {
    let name = string(m, path, 4096)?;
    if name.is_empty() {
        return Err(ENOENT);
    }
    let file = CString::new(name).expect("no zero inside");
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(file.as_ptr(), &mut st) } != 0 || unsafe { libc::access(file.as_ptr(), libc::X_OK) } != 0 {
        return Err(errno());
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(EACCES);
    }
    // A program is an ELF file or begins with "#!": a shell is told of anything else
    // and may run it itself.
    let mut magic = [0u8; 4];
    let fd = unsafe { libc::open(file.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(errno());
    }
    let got = unsafe { libc::read(fd, magic.as_mut_ptr() as *mut libc::c_void, 4) };
    unsafe { libc::close(fd) };
    if !(got == 4 && magic == *b"\x7fELF" || got >= 2 && magic[..2] == *b"#!") {
        return Err(ENOEXEC);
    }

    let given = strings(m, argv)?;
    let env = strings(m, envp)?;
    let mut args: Vec<CString> = vec![CString::new(s.exe.as_bytes()).map_err(|_| EINVAL)?];
    for option in &s.options {
        args.push(CString::new(option.as_bytes()).map_err(|_| EINVAL)?);
    }
    // The name the program is given for itself need not be its path.
    args.push(c"--argv0".to_owned());
    args.push(given.first().cloned().unwrap_or_else(|| file.clone()));
    args.push(file);
    args.extend(given.into_iter().skip(1));
    let list = |all: &[CString]| -> Vec<*const libc::c_char> {
        all.iter().map(|a| a.as_ptr()).chain([std::ptr::null()]).collect()
    };
    let (args, env) = (list(&args), list(&env));
    unsafe { libc::execve(args[0], args.as_ptr(), env.as_ptr()) };
    Err(errno())
}

// ---- the table ---------------------------------------------------------------------------------

/// Answers call `n` of x86-64's numbering if it is one handed to the host's kernel.
pub fn call(c: &mut Cpu, m: &mut Mem, s: &mut Sys, n: u64, a: [u64; 6]) -> Option<Result<u64, u64>> {
    // A call whose arguments mean the same to both kernels, with those among the
    // first that are pointers named by a mask (bit 0: the first argument).
    let same = |m: &Mem, number: libc::c_long, pointers: u8| -> Result<u64, u64> {
        let mut host = a;
        for (i, arg) in host.iter_mut().enumerate() {
            if pointers >> i & 1 != 0 {
                *arg = p(m, *arg)?;
            }
        }
        raw(number, host)
    };
    Some(match n {
        // ---- files
        0 => buf(m, a[1], a[2]).and_then(|to| raw(libc::SYS_read, [a[0], to, a[2], 0, 0, 0])),
        2 => p(m, a[0]).and_then(|path| raw(libc::SYS_openat, [AT_FDCWD, path, open_flags(a[1], true), a[2], 0, 0])),
        3 => same(m, libc::SYS_close, 0),
        4 => stat_at(m, AT_FDCWD, a[0], a[1], 0),
        5 => {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(a[0] as i32, &mut st) } != 0 { Err(errno()) } else { put_stat(m, a[1], &st) }
        }
        6 => stat_at(m, AT_FDCWD, a[0], a[1], libc::AT_SYMLINK_NOFOLLOW as u64),
        7 => {
            // poll: the time to wait in milliseconds, or for ever if negative.
            let ms = a[2] as i32;
            let wait = libc::timespec { tv_sec: (ms / 1000) as _, tv_nsec: (ms % 1000 * 1_000_000) as _ };
            let wait = if ms < 0 { 0 } else { &wait as *const libc::timespec as u64 };
            buf(m, a[0], a[1].saturating_mul(8)).and_then(|fds| raw(libc::SYS_ppoll, [fds, a[1], wait, 0, 0, 0]))
        }
        8 => same(m, libc::SYS_lseek, 0),
        17 => buf(m, a[1], a[2]).and_then(|to| raw(libc::SYS_pread64, [a[0], to, a[2], a[3], 0, 0])),
        18 => buf(m, a[1], a[2]).and_then(|from| raw(libc::SYS_pwrite64, [a[0], from, a[2], a[3], 0, 0])),
        19 => vector(m, libc::SYS_readv, a[0], a[1], a[2], 0),
        21 => p(m, a[0]).and_then(|path| raw(libc::SYS_faccessat, [AT_FDCWD, path, a[1], 0, 0, 0])),
        22 => buf(m, a[0], 8).and_then(|fds| raw(libc::SYS_pipe2, [fds, 0, 0, 0, 0, 0])),
        32 => same(m, libc::SYS_dup, 0),
        33 => {
            // dup2 of a descriptor onto itself says whether it is open; dup3 refuses.
            if a[0] == a[1] {
                raw(libc::SYS_fcntl, [a[0], 1, 0, 0, 0, 0]).map(|_| a[1])
            } else {
                raw(libc::SYS_dup3, [a[0], a[1], 0, 0, 0, 0])
            }
        }
        40 => same(m, libc::SYS_sendfile, 1 << 2),
        72 => fcntl(m, a[0], a[1], a[2]),
        73 => same(m, libc::SYS_flock, 0),
        74 => same(m, libc::SYS_fsync, 0),
        75 => same(m, libc::SYS_fdatasync, 0),
        76 => p(m, a[0]).and_then(|path| raw(libc::SYS_truncate, [path, a[1], 0, 0, 0, 0])),
        77 => same(m, libc::SYS_ftruncate, 0),
        79 => buf(m, a[0], a[1]).and_then(|to| raw(libc::SYS_getcwd, [to, a[1], 0, 0, 0, 0])),
        80 => same(m, libc::SYS_chdir, 1),
        81 => same(m, libc::SYS_fchdir, 0),
        82 => p(m, a[0]).and_then(|from| p(m, a[1]).and_then(|to| raw(libc::SYS_renameat, [AT_FDCWD, from, AT_FDCWD, to, 0, 0]))),
        83 => p(m, a[0]).and_then(|path| raw(libc::SYS_mkdirat, [AT_FDCWD, path, a[1], 0, 0, 0])),
        84 => p(m, a[0]).and_then(|path| raw(libc::SYS_unlinkat, [AT_FDCWD, path, libc::AT_REMOVEDIR as u64, 0, 0, 0])),
        86 => p(m, a[0]).and_then(|from| p(m, a[1]).and_then(|to| raw(libc::SYS_linkat, [AT_FDCWD, from, AT_FDCWD, to, 0, 0]))),
        87 => p(m, a[0]).and_then(|path| raw(libc::SYS_unlinkat, [AT_FDCWD, path, 0, 0, 0, 0])),
        88 => p(m, a[0]).and_then(|target| p(m, a[1]).and_then(|link| raw(libc::SYS_symlinkat, [target, AT_FDCWD, link, 0, 0, 0]))),
        89 => readlink(m, s, AT_FDCWD, a[0], a[1], a[2]),
        90 => p(m, a[0]).and_then(|path| raw(libc::SYS_fchmodat, [AT_FDCWD, path, a[1], 0, 0, 0])),
        91 => same(m, libc::SYS_fchmod, 0),
        92 => p(m, a[0]).and_then(|path| raw(libc::SYS_fchownat, [AT_FDCWD, path, a[1], a[2], 0, 0])),
        93 => same(m, libc::SYS_fchown, 0),
        95 => same(m, libc::SYS_umask, 0),
        137 => same(m, libc::SYS_statfs, 0b11),
        138 => same(m, libc::SYS_fstatfs, 0b10),
        217 => buf(m, a[1], a[2]).and_then(|to| raw(libc::SYS_getdents64, [a[0], to, a[2], 0, 0, 0])),
        257 => p(m, a[1]).and_then(|path| raw(libc::SYS_openat, [a[0], path, open_flags(a[2], true), a[3], 0, 0])),
        258 => same(m, libc::SYS_mkdirat, 0b10),
        260 => same(m, libc::SYS_fchownat, 0b10),
        262 => stat_at(m, a[0], a[1], a[2], a[3]),
        263 => same(m, libc::SYS_unlinkat, 0b10),
        264 => same(m, libc::SYS_renameat, 0b1010),
        265 => same(m, libc::SYS_linkat, 0b1010),
        266 => same(m, libc::SYS_symlinkat, 0b101),
        267 => readlink(m, s, a[0], a[1], a[2], a[3]),
        268 => same(m, libc::SYS_fchmodat, 0b10),
        269 => same(m, libc::SYS_faccessat, 0b10),
        280 => same(m, libc::SYS_utimensat, 0b110),
        285 => same(m, libc::SYS_fallocate, 0),
        292 => raw(libc::SYS_dup3, [a[0], a[1], open_flags(a[2], true), 0, 0, 0]),
        293 => buf(m, a[0], 8).and_then(|fds| raw(libc::SYS_pipe2, [fds, open_flags(a[1], true), 0, 0, 0, 0])),
        295 => vector(m, libc::SYS_preadv, a[0], a[1], a[2], a[3]),
        296 => vector(m, libc::SYS_pwritev, a[0], a[1], a[2], a[3]),
        316 => same(m, libc::SYS_renameat2, 0b1010),
        332 => same(m, libc::SYS_statx, 0b1_0010),
        439 => same(m, libc::SYS_faccessat2, 0b10),
        16 => ioctl(m, a[0], a[1], a[2]),
        271 => same(m, libc::SYS_ppoll, 0b1101),

        // ---- time, and what the machine is
        35 => same(m, libc::SYS_nanosleep, 0b11),
        63 => buf(m, a[0], 390).and_then(|to| {
            raw(libc::SYS_uname, [to, 0, 0, 0, 0, 0])?;
            // The machine is the guest's, whatever the host is: the fifth of six
            // names of 65 bytes.
            m.write_bytes(a[0] + 4 * 65, b"x86_64\0").map_err(|_| EFAULT)?;
            Ok(0)
        }),
        96 => same(m, libc::SYS_gettimeofday, 0b11),
        97 => p(m, a[1]).and_then(|to| raw(libc::SYS_prlimit64, [0, a[0], 0, to, 0, 0])),
        98 => same(m, libc::SYS_getrusage, 0b10),
        99 => same(m, libc::SYS_sysinfo, 1),
        100 => same(m, libc::SYS_times, 1),
        160 => p(m, a[1]).and_then(|new| raw(libc::SYS_prlimit64, [0, a[0], new, 0, 0, 0])),
        201 => {
            let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut now) };
            if a[0] != 0 && m.write_u64(a[0], now.tv_sec as u64).is_err() { Err(EFAULT) } else { Ok(now.tv_sec as u64) }
        }
        204 => same(m, libc::SYS_sched_getaffinity, 0b100),
        24 => same(m, libc::SYS_sched_yield, 0),
        228 => same(m, libc::SYS_clock_gettime, 0b10),
        229 => same(m, libc::SYS_clock_getres, 0b10),
        230 => same(m, libc::SYS_clock_nanosleep, 0b1100),
        302 => same(m, libc::SYS_prlimit64, 0b1100),
        318 => buf(m, a[0], a[1]).and_then(|to| raw(libc::SYS_getrandom, [to, a[1], a[2], 0, 0, 0])),
        // madvise: advice, which need not be taken.
        28 => Ok(0),

        // futex: the word waited on is the guest's memory, which is the host's (ADR
        // 0008), so the kernel's own futex on `base + g` is the guest's. The fourth
        // argument is a time to wait for the calls that wait (WAIT, LOCK_PI,
        // WAIT_BITSET, WAIT_REQUEUE_PI) and a number for the others.
        202 => (|| {
            let waits = matches!(a[1] & 0x7f, 0 | 6 | 9 | 11);
            let fourth = if waits { p(m, a[3])? } else { a[3] };
            raw(libc::SYS_futex, [p(m, a[0])?, a[1], a[2], fourth, p(m, a[4])?, a[5]])
        })(),

        // ---- who the process is
        39 => same(m, libc::SYS_getpid, 0),
        102 => same(m, libc::SYS_getuid, 0),
        104 => same(m, libc::SYS_getgid, 0),
        105 => same(m, libc::SYS_setuid, 0),
        106 => same(m, libc::SYS_setgid, 0),
        107 => same(m, libc::SYS_geteuid, 0),
        108 => same(m, libc::SYS_getegid, 0),
        109 => same(m, libc::SYS_setpgid, 0),
        110 => same(m, libc::SYS_getppid, 0),
        111 => raw(libc::SYS_getpgid, [0; 6]),
        112 => same(m, libc::SYS_setsid, 0),
        115 => same(m, libc::SYS_getgroups, 0b10),
        116 => same(m, libc::SYS_setgroups, 0b10),
        117 => same(m, libc::SYS_setresuid, 0),
        118 => same(m, libc::SYS_getresuid, 0b111),
        119 => same(m, libc::SYS_setresgid, 0),
        120 => same(m, libc::SYS_getresgid, 0b111),
        121 => same(m, libc::SYS_getpgid, 0),
        124 => same(m, libc::SYS_getsid, 0),
        186 => same(m, libc::SYS_gettid, 0),

        // ---- processes
        56 => fork(c, m, s, a[0], a[1], a[2], a[3]),
        57 => fork(c, m, s, 0, 0, 0, 0),
        58 => fork(c, m, s, CLONE_VM | CLONE_VFORK, 0, 0, 0),
        59 => execve(m, s, a[0], a[1], a[2]),
        61 => same(m, libc::SYS_wait4, 0b1010),
        62 => same(m, libc::SYS_kill, 0),
        234 => same(m, libc::SYS_tgkill, 0),
        247 => same(m, libc::SYS_waitid, 0b1_0100),
        _ => return None,
    })
}

/// What the process was given that a program started by `execve` keeps and r64m
/// must know of: the signals that are ignored, and those that are blocked.
pub fn inherited(s: &mut Sys) {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut set);
        for signal in 1..=64 {
            if libc::sigismember(&set, signal) == 1 {
                s.blocked |= 1 << (signal - 1);
            }
            let mut was: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, std::ptr::null(), &mut was) == 0 && was.sa_sigaction == libc::SIG_IGN {
                s.actions[signal as usize][0] = 1;
            }
        }
    }
}

/// A signal the program asks to be ignored, or to do what it does by itself again,
/// is so for the host too (but for the two r64m needs): the kernel then keeps it so
/// across `execve`. A handler is only kept (ADR 0017, "Not yet").
pub fn disposition(signal: u64, handler: u64) {
    if signal == libc::SIGSEGV as u64 || signal == libc::SIGBUS as u64 {
        return;
    }
    let to = match handler {
        0 => libc::SIG_DFL,
        1 => libc::SIG_IGN,
        _ => return,
    };
    unsafe { libc::signal(signal as i32, to) };
}
