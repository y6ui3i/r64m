//! r64m: an x86-64 emulator with a JIT for arm64 hosts. A rebuild of g64m in Rust.
//!
//! So far: the memory of a program without a kernel (`mem`). Nothing runs yet.

mod cpu;
mod elf;
mod jit;
// Some of it waits for the calls that map and unmap.
#[allow(dead_code)]
mod mem;
mod profile;
mod sys;

use std::io::Write;

/// What `r64m` says of itself.
fn banner() -> String {
    format!(
        "r64m {}: a rebuild of g64m in Rust; runs static programs, interpreted",
        env!("CARGO_PKG_VERSION")
    )
}

/// How the JIT was asked for: the same for every thread of the guest's.
#[derive(Clone, Copy)]
struct Mode {
    jit: bool,
    check: bool,
    hot: u8,
}

static MODE: std::sync::OnceLock<Mode> = std::sync::OnceLock::new();

/// How far the guest went in one step.
enum Went {
    Next,
    /// To a SYSCALL, which is to be answered.
    Syscall,
    /// Nowhere: a signal waits to be delivered first.
    Signal,
}

/// The guest goes on by one step: an instruction of the interpreter's, or as far as
/// blocks take it.
fn one(c: &mut cpu::Cpu, mem: &mut mem::Mem, blocks: &mut Option<jit::run::Jit>) -> Result<Went, cpu::Stop> {
    match blocks {
        None if c.kick != 0 => Ok(Went::Signal),
        None => cpu::step(c, mem).map(|step| match step {
            cpu::Step::Next => Went::Next,
            cpu::Step::Syscall => Went::Syscall,
        }),
        Some(blocks) => match blocks.run(c, mem) {
            jit::run::Event::Syscall => Ok(Went::Syscall),
            jit::run::Event::Signal => Ok(Went::Signal),
            jit::run::Event::Stop(stop) => Err(stop),
            jit::run::Event::Differs(what) => {
                eprintln!("r64m: --jit-check: {what}");
                std::process::exit(3);
            }
        },
    }
}

/// The guest cannot go on as it is: a fault of its own. If it has a handler for the
/// signal that is, the handler is entered and it goes on there; if not, it ends, with
/// what stopped it said and as that signal ends a process (ADR 0019, point 2).
fn stopped(stop: cpu::Stop, c: &mut cpu::Cpu, mem: &mut mem::Mem, system: &mut sys::Sys) {
    if sys::fault(c, mem, system, &stop) {
        return;
    }
    let what = match &stop {
        cpu::Stop::Fault(f) => format!("{:?} of {:#x} that is not allowed", f.access, f.addr),
        cpu::Stop::Divide => "a division that cannot be done".to_string(),
        cpu::Stop::Undefined => "not an instruction".to_string(),
        cpu::Stop::Protection => "an instruction the CPU refuses".to_string(),
        cpu::Stop::NotWritten(bytes) => format!("an instruction not written yet: {bytes}"),
        cpu::Stop::Again => unreachable!("an instruction to be run again is run again where it stands"),
    };
    eprintln!("r64m: at {:#x} after {} instructions: {what}", c.rip, c.steps);
    sys::die_of(&stop)
}

/// A thread of the guest's other than its first (ADR 0019): the same loop, with the
/// thread's own registers, its own handle on the memory, and its own blocks.
fn thread(mut c: cpu::Cpu, mut mem: mem::Mem, mut system: sys::Sys) {
    let mode = *MODE.get().expect("set before any thread is started");
    let mut blocks = mode.jit.then(|| {
        jit::run::Jit::new(mode.check, mode.hot).unwrap_or_else(|e| fail(format_args!("no memory for a thread's generated code: {e}")))
    });
    sys::attend(&mut c);
    loop {
        mem.sync();
        match one(&mut c, &mut mem, &mut blocks) {
            Ok(Went::Next) => {}
            Ok(Went::Signal) => sys::deliver(&mut c, &mut mem, &mut system),
            Ok(Went::Syscall) => {
                match sys::call(&mut c, &mut mem, &mut system) {
                    Some(sys::Exit::Group(code)) => std::process::exit(code),
                    // Its code is nobody's: a process ends with its first thread's.
                    Some(sys::Exit::Thread(_)) => {
                        sys::thread_ends(&mut mem, &system);
                        return;
                    }
                    None => {}
                }
                if let (Some((start, end)), Some(blocks)) = (system.changed.take(), &mut blocks) {
                    blocks.guest_changed(start, end, &mut mem);
                }
            }
            Err(stop) => stopped(stop, &mut c, &mut mem, &mut system),
        }
    }
}

/// r64m's own environment, as the guest's.
fn environment() -> Vec<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    std::env::vars_os()
        .map(|(k, v)| [k.as_bytes(), b"=", v.as_bytes()].concat())
        .collect()
}

/// The program a file that begins with "#!" names on its first line, and the one
/// argument that may follow it.
fn shebang(img: &[u8]) -> Option<(String, Option<String>)> {
    let line = img.strip_prefix(b"#!")?;
    let line = String::from_utf8_lossy(line.split(|&b| b == b'\n').next()?).into_owned();
    let line = line.trim();
    let (program, rest) = match line.split_once([' ', '\t']) {
        Some((program, rest)) => (program, rest.trim()),
        None => (line, ""),
    };
    if program.is_empty() {
        return None;
    }
    Some((program.to_string(), (!rest.is_empty()).then(|| rest.to_string())))
}

fn fail(what: std::fmt::Arguments) -> ! {
    eprintln!("r64m: {what}");
    std::process::exit(2)
}

/// One line of a trace: the registers and flags after an instruction, in the form of
/// g64m's `--trace` and of the oracle that single-steps the real CPU.
fn trace_line(out: &mut impl Write, c: &cpu::Cpu) -> std::io::Result<()> {
    const NAMES: [&str; 16] = [
        "RAX", "RCX", "RDX", "RBX", "RSP", "RBP", "RSI", "RDI", "R8", "R9", "R10", "R11", "R12",
        "R13", "R14", "R15",
    ];
    write!(out, "RIP={:016X}", c.rip)?;
    for (name, v) in NAMES.iter().zip(c.reg) {
        write!(out, " {name}={v:016X}")?;
    }
    write!(out, " F={:08X}", c.rflags)
}

/// What `--trace-xmm` adds to a line: MXCSR without its six status bits, and each XMM
/// register as `X3=high:low`, each only when it is not what it was on the line before.
struct SeenXmm {
    mxcsr: u32,
    xmm: [u64; 32],
}

impl SeenXmm {
    fn line(&mut self, out: &mut impl Write, c: &cpu::Cpu) -> std::io::Result<()> {
        if c.mxcsr & 0xffc0 != self.mxcsr {
            self.mxcsr = c.mxcsr & 0xffc0;
            write!(out, " MXCSR={:04X}", self.mxcsr)?;
        }
        for i in 0..16 {
            let (lo, hi) = (c.xmm[i * 2], c.xmm[i * 2 + 1]);
            if (lo, hi) != (self.xmm[i * 2], self.xmm[i * 2 + 1]) {
                (self.xmm[i * 2], self.xmm[i * 2 + 1]) = (lo, hi);
                write!(out, " X{i}={hi:016X}:{lo:016X}")?;
            }
        }
        Ok(())
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    // Started by the kernel for an x86-64 program (registered with `binfmt_misc`
    // under this name, with the flag that keeps the program's own first argument:
    // R049): what follows is the program's path, the first argument it was given,
    // and the rest. No option can be given this way, so this is the way with the
    // JIT.
    let by_kernel = std::path::Path::new(&args.remove(0)).file_name().is_some_and(|n| n == "r64m-binfmt");
    if by_kernel && args.len() >= 2 {
        let path = args.remove(0);
        let name = args.remove(0);
        args.splice(0..0, ["--jit".to_string(), "--quiet".to_string(), "--argv0".to_string(), name, path]);
    }
    // The options as they were given, for a program that starts another (`execve`
    // is r64m started again with them, ADR 0017).
    let own_options: Vec<String> = args.iter().take_while(|a| a.starts_with("--") && *a != "--argv0").cloned().collect();
    // For measuring: how often an address is arrived at before its block is made.
    let jit_hot = args.iter().take_while(|a| a.starts_with("--")).position(|a| a.starts_with("--jit-hot=")).map(|i| args.remove(i));
    let jit_hot: Option<u8> = jit_hot.and_then(|a| a["--jit-hot=".len()..].parse().ok());
    // The options, in any order, before the program.
    let mut option = |name: &str| {
        let found = args.iter().take_while(|a| a.starts_with("--")).position(|a| a == name);
        found.map(|i| args.remove(i)).is_some()
    };
    let count = option("--count");
    let jit_check = option("--jit-check");
    let jit_stats = option("--jit-stats");
    // Each system call said as it is made and as it answers (src/sys.rs).
    if option("--quiet") {
        sys::QUIET.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if option("--calls") {
        sys::SAY_CALLS.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // What each thread's blocks hold, said as they grow (src/jit/run.rs).
    if option("--jit-memory") {
        jit::run::SAY_MEMORY.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // Where r64m's own time goes: a sampler (src/profile.rs).
    let profile = option("--profile");
    // What the interpreter still ran, by instruction: for deciding what to translate.
    let jit_why = option("--jit-why");
    // For the tests: every block made the first time its address is arrived at.
    let jit_now = option("--jit-now");
    // For measuring: every block with ordered loads and stores, as a process with
    // more than one thread has them.
    let jit_ordered = option("--jit-ordered");
    let jit = option("--jit") || jit_check;
    let trace_xmm = args.first().is_some_and(|a| a == "--trace-xmm");
    let trace = trace_xmm || args.first().is_some_and(|a| a == "--trace");
    if trace {
        args.remove(0);
    }
    // The name the program is given for itself, if it is not to be its path: as
    // `execve` hands it on.
    let mut argv0 = None;
    if args.first().is_some_and(|a| a == "--argv0") && args.len() >= 2 {
        args.remove(0);
        argv0 = Some(args.remove(0));
    }
    let mut seen = SeenXmm { mxcsr: 0x1f80, xmm: [0; 32] };
    let mut mem = mem::Mem::reserve()
        .unwrap_or_else(|e| fail(format_args!("the host gave no address space for the guest: {e}")));
    let Some(path) = args.first() else {
        println!("{}", banner());
        println!(
            "guest memory: 2^{} bytes reserved, host pages of {} KB",
            mem.size().trailing_zeros(),
            mem.host_page() / 1024
        );
        println!("usage: r64m [--count] [--jit | --jit-check] [--jit-now] [--jit-stats] [--jit-memory] [--calls] [--quiet] [--profile] [--trace | --trace-xmm] program [arguments]");
        return;
    };

    let mut img = std::fs::read(path).unwrap_or_else(|e| fail(format_args!("{path}: {e}")));
    // A file that begins with "#!" names the program that is to run it: that one is
    // loaded, with its one argument if it has one, and then the file's own name.
    let mut args = args.clone();
    for _ in 0..4 {
        let Some((program, argument)) = shebang(&img) else { break };
        if let Some(argument) = argument {
            args.insert(0, argument);
        }
        args.insert(0, program);
        argv0 = None;
        img = std::fs::read(&args[0]).unwrap_or_else(|e| fail(format_args!("{}: {e}", args[0])));
    }
    let path = &args[0].clone();
    let mut loaded = elf::load(&mut mem, &img).unwrap_or_else(|e| fail(format_args!("{path}: {e}")));
    // A program that names an interpreter begins there (ADR 0017).
    let mut entry = loaded.entry;
    if let Some(name) = elf::interpreter(&img) {
        let name = String::from_utf8_lossy(&name).into_owned();
        let img = std::fs::read(&name).unwrap_or_else(|e| fail(format_args!("{path}: its interpreter {name}: {e}")));
        entry = elf::load_interpreter(&mut mem, &img, &mut loaded).unwrap_or_else(|e| fail(format_args!("{name}: {e}")));
    }
    let mut guest_args: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
    if let Some(name) = &argv0 {
        guest_args[0] = name.as_bytes();
    }
    // The environment is r64m's own, but for a trace: the real CPU's traces were
    // taken with none, and the stack pointer is in every line.
    let own_env: Vec<Vec<u8>> = if trace { Vec::new() } else { environment() };
    let guest_env: Vec<&[u8]> = own_env.iter().map(|e| e.as_slice()).collect();
    let rsp = elf::first_stack(&mut mem, &loaded, &guest_args, &guest_env)
        .unwrap_or_else(|e| fail(format_args!("{path}: {e}")));

    let mut c = cpu::Cpu::new(entry, rsp);
    c.rep_whole = !trace;
    let mut system = sys::Sys::new(if trace { sys::Output::Nowhere } else { sys::Output::Host }, loaded.brk);
    // Itself, for `execve`: on Linux by the link the kernel keeps, which is r64m in
    // whatever container this is; its path is that of another's files (R049).
    system.exe = if cfg!(target_os = "linux") {
        "/proc/self/exe".to_string()
    } else {
        std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/proc/self/exe".to_string())
    };
    system.options = own_options;
    system.program = std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.clone());
    let mut lines = std::io::BufWriter::new(std::io::stdout().lock());
    if jit && trace {
        fail(format_args!("a trace is of the interpreter: not with --jit"));
    }
    let mut blocks = if jit {
        // With the check every repetition is a step, as the check counts them.
        c.rep_whole = !jit_check;
        let mut made = jit::run::Jit::new(jit_check, if jit_now { 1 } else { jit_hot.unwrap_or(jit::run::HOT) })
            .unwrap_or_else(|e| fail(format_args!("no memory for generated code: {e}")));
        if jit_why {
            made.why = Some(Default::default());
        }
        made.timed = jit_stats;
        made.ordered = jit_ordered;
        Some(made)
    } else {
        None
    };
    if profile {
        profile::start().unwrap_or_else(|e| fail(format_args!("--profile: {e}")));
    }
    MODE.set(Mode { jit, check: jit_check, hot: if jit_now { 1 } else { jit_hot.unwrap_or(jit::run::HOT) } }).ok();
    system.thread = Some(thread);
    sys::attend(&mut c);
    let code = loop {
        mem.sync();
        match one(&mut c, &mut mem, &mut blocks) {
            Ok(Went::Next) => {}
            Ok(Went::Signal) => sys::deliver(&mut c, &mut mem, &mut system),
            Ok(Went::Syscall) => {
                // The exit that ends a program is not a line of the trace: there is
                // nothing after it to show.
                match sys::call(&mut c, &mut mem, &mut system) {
                    Some(sys::Exit::Group(code)) => break code,
                    // The first thread ends and others go on: only it ends, and the
                    // process with the last of them, as the kernel has it.
                    Some(sys::Exit::Thread(code)) if sys::threads_started() => sys::end_thread(&mut mem, &system, code),
                    Some(sys::Exit::Thread(code)) => break code,
                    None => {}
                }
                // Blocks made from pages the call unmapped, mapped over or protected
                // otherwise are of what is no longer there (ADR 0016, point 2).
                if let (Some((start, end)), Some(blocks)) = (system.changed.take(), &mut blocks) {
                    blocks.guest_changed(start, end, &mut mem);
                }
            }
            Err(stop) => {
                lines.flush().ok();
                stopped(stop, &mut c, &mut mem, &mut system);
            }
        }
        if trace {
            let mut line = trace_line(&mut lines, &c);
            if trace_xmm {
                line = line.and_then(|()| seen.line(&mut lines, &c));
            }
            line.and_then(|()| writeln!(lines))
                .unwrap_or_else(|e| fail(format_args!("the trace: {e}")));
        }
    };
    lines.flush().ok();
    if profile {
        let hot = profile::report(jit::is_generated, main as *const () as usize, |pc| blocks.as_ref().and_then(|b| b.where_is(pc)));
        // The code of the blocks met most often, as words an assembler can be given,
        // with where each of the guest's instructions begins in them.
        for &(rip, count) in hot.iter().take(40) {
            if let Some((words, starts)) = blocks.as_ref().and_then(|b| b.code_of(rip)) {
                eprintln!("r64m: profile: block {rip:#x} {count} starts {starts:x?}");
                eprintln!("r64m: profile: words {}", words.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" "));
            }
        }
    }
    if count {
        eprintln!("r64m: {} instructions", c.steps);
    }
    if let Some(why) = blocks.as_ref().and_then(|b| b.why.as_ref()) {
        let mut by_count: Vec<_> = why.iter().collect();
        by_count.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        for (key, n) in by_count.into_iter().take(40) {
            let bytes: Vec<String> = key.iter().map(|b| format!("{b:02x}")).collect();
            eprintln!("r64m: why: {n:>12} {}", bytes.join(" "));
        }
    }
    if let (true, Some(blocks)) = (jit_stats, &blocks) {
        let s = &blocks.stats;
        eprintln!(
            "r64m: jit: {} blocks, entered {} times, {} of {} instructions in blocks, {} jumps joined, {} checked, {} left by a fault, {} stores made beside code, {} pages' blocks forgotten, emptied {} times",
            s.blocks, s.entered, s.in_blocks, c.steps, s.joined, s.checked, s.faults, s.beside, s.forgotten, s.emptied
        );
        // (Timed only when this is asked for: the clock is read twice for each.)
        eprintln!(
            "r64m: jit: {:.3} s making blocks, {:.3} s in generated code",
            s.making as f64 / 1e9,
            s.running as f64 / 1e9
        );
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::{banner, shebang};

    #[test]
    fn the_program_a_script_names() {
        assert_eq!(shebang(b"#!/bin/sh\necho hi\n"), Some(("/bin/sh".to_string(), None)));
        // One argument, which is all the rest of the line as the kernel has it.
        assert_eq!(shebang(b"#! /usr/bin/env  python3 -u \n"), Some(("/usr/bin/env".to_string(), Some("python3 -u".to_string()))));
        assert_eq!(shebang(b"#!/bin/busybox sh"), Some(("/bin/busybox".to_string(), Some("sh".to_string()))));
        assert_eq!(shebang(b"\x7fELF"), None);
        assert_eq!(shebang(b"#!\n"), None);
    }

    #[test]
    fn the_banner_names_the_version() {
        assert!(banner().starts_with("r64m 0.1.0"));
    }
}
