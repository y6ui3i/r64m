//! The guest's own test programs (`guest/`, Rust built for x86-64 Linux) run by r64m
//! and held to what the real machine printed for the same file (ADR 0012, R038).
//!
//! These are the cases a shell or Python cannot make: a handler that changes its own
//! frame, a handler on another stack, a fault stepped over. Each program is
//! statically linked, so it needs no image: only Linux, r64m, and the program built:
//!
//!   CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
//!     cargo build --release --manifest-path guest/Cargo.toml --target x86_64-unknown-linux-musl
//!
//! What is right for each is in `tests/guest/NAME.txt`: the program's output on an
//! x86-64 Linux, then "? " and its exit code. A program is run three ways (the
//! interpreter, the JIT, the JIT with every block checked). Where there is no Linux,
//! or the programs are not built, every case is shown as ignored. `R64M_GUEST` says
//! where the programs are if not where Cargo puts them, and `R64M_BIN` where r64m is.

use libtest_mimic::{Arguments, Failed, Trial};
use std::path::{Path, PathBuf};
use std::process::Command;

const WAYS: [(&str, &[&str]); 3] = [("interpreter", &["--interpret"]), ("jit", &[]), ("jit_check", &["--jit-check"])];

/// The programs, and what the real machine printed for each.
const PROGRAMS: [(&str, &str); 2] = [("signals", include_str!("guest/signals.txt")), ("unused", include_str!("guest/unused.txt"))];

fn programs() -> PathBuf {
    match std::env::var_os("R64M_GUEST") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("guest/target/x86_64-unknown-linux-musl/release"),
    }
}

fn r64m() -> PathBuf {
    match std::env::var_os("R64M_BIN") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_BIN_EXE_r64m")),
    }
}

/// The output and the exit code a file of what is right holds.
fn right(text: &str) -> (String, i32) {
    let mut output = String::new();
    let mut code = None;
    for line in text.lines().filter(|l| !l.starts_with('#')) {
        match line.strip_prefix("? ") {
            Some(c) => code = Some(c.parse().expect("an exit code")),
            None => output += &format!("{line}\n"),
        }
    }
    (output, code.expect("a file of what is right ends with the exit code"))
}

fn run(program: &Path, way: &[&str], output: &str, code: i32) -> Result<(), Failed> {
    let out = Command::new(r64m()).args(way).arg(program).output().map_err(|e| format!("r64m: {e}"))?;
    let got = String::from_utf8_lossy(&out.stdout);
    if got == output && out.status.code() == Some(code) {
        return Ok(());
    }
    // The lines that differ, side by side.
    let mut said = Vec::new();
    let (mut a, mut b) = (output.lines(), got.lines());
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) if x == y => {}
            (x, y) => said.push(format!("  the real machine: {}\n  r64m:             {}", x.unwrap_or("(no line)"), y.unwrap_or("(no line)"))),
        }
    }
    Err(format!("{}\n  r64m ended with {}: {}", said.join("\n"), out.status, String::from_utf8_lossy(&out.stderr).trim_end()).into())
}

fn main() {
    let args = Arguments::from_args();
    let dir = programs();
    let mut trials = Vec::new();
    for (name, text) in PROGRAMS {
        let program = dir.join(name);
        let here = cfg!(target_os = "linux") && program.exists();
        let (output, code) = right(text);
        for (way, options) in WAYS {
            let (program, output) = (program.clone(), output.clone());
            trials.push(Trial::test(format!("{name}::{way}"), move || run(&program, options, &output, code)).with_ignored_flag(!here));
        }
    }
    libtest_mimic::run(&args, trials).exit();
}
