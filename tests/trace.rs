//! r64m's traces held to the real CPU's (ADR 0006, ADR 0012).
//!
//! The cases and what is right for each are g64m's and are read from its checkout, which
//! is looked for beside this one or where `R64M_G64M` says: `tests/asm64/*.asm` (built
//! here with nasm), `tests/c64/*.elf` (kept built there), and for each the trace of
//! every instruction taken on an x86 CPU, `tests/expected64/NAME.trace.gz`. A case is
//! right when `r64m --trace` prints that trace, byte for byte.
//!
//! Without the checkout every case is shown as ignored, and so is one that needs what
//! r64m does not have yet: those are listed in `NOT_YET` with what they wait for.

use libtest_mimic::{Arguments, Failed, Trial};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Cases by the first letters of their names, and what each waits for.
const NOT_YET: [(&str, &str); 5] = [
    ("a_prefix", "the x87"),
    ("x_fxsave", "the x87, whose state FXSAVE keeps"),
    ("f_", "the x87"),
    ("j_x87", "the x87"),
    ("c_ld", "the x87 (long double)"),
];

fn g64m() -> PathBuf {
    match std::env::var_os("R64M_G64M") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../g64m"),
    }
}

fn run(cmd: &mut Command) -> Result<Vec<u8>, Failed> {
    let out = cmd.output().map_err(|e| format!("{cmd:?}: {e}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{cmd:?}: {}\n{}", out.status, said.trim_end()).into());
    }
    Ok(out.stdout)
}

/// The fields of the two lines that differ, as `NAME expected -> got`.
fn fields_that_differ(expected: &str, got: &str) -> String {
    let mut said = Vec::new();
    for (e, g) in expected.split(' ').zip(got.split(' ')) {
        if e != g {
            let (name, value) = e.split_once('=').unwrap_or((e, ""));
            let got = g.split_once('=').map_or(g, |(_, v)| v);
            said.push(format!("{name} {value} -> {got}"));
        }
    }
    said.join(", ")
}

/// Builds the case if it is assembly, runs it, and compares the trace.
fn case(g64m: &Path, name: &str, asm: Option<&Path>) -> Result<(), Failed> {
    let program = match asm {
        Some(asm) => {
            let built = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
            run(Command::new("nasm")
                .arg("-w-other")
                .arg("-I")
                .arg(g64m.join("tests/asm64/"))
                .args(["-f", "bin", "-o"])
                .arg(&built)
                .arg(asm))?;
            built
        }
        None => g64m.join("tests/c64").join(format!("{name}.elf")),
    };
    let expected = run(Command::new("gzip")
        .arg("-dc")
        .arg(g64m.join("tests/expected64").join(format!("{name}.trace.gz"))))?;
    let out = Command::new(env!("CARGO_BIN_EXE_r64m"))
        // The cases named x_ and cx_ use SSE: their traces have the XMM registers too.
        .arg(if name.starts_with("x_") || name.starts_with("cx_") { "--trace-xmm" } else { "--trace" })
        .arg(&program)
        .output()
        .map_err(|e| format!("r64m: {e}"))?;
    if out.stdout == expected && out.status.code() == Some(0) {
        return Ok(());
    }

    // Where the two part, and how r64m ended.
    let expected = String::from_utf8_lossy(&expected);
    let got = String::from_utf8_lossy(&out.stdout);
    let (mut e, mut g) = (expected.lines(), got.lines());
    let mut line = 1;
    let mut last = "";
    let parted = loop {
        match (e.next(), g.next()) {
            (Some(a), Some(b)) if a == b => last = a,
            (Some(a), Some(b)) => break format!("line {line}: {}", fields_that_differ(a, b)),
            (Some(_), None) => break format!("r64m stopped after line {}", line - 1),
            (None, Some(_)) => break format!("r64m went on after the trace's last line, {}", line - 1),
            (None, None) => break "the traces are the same".to_string(),
        }
        line += 1;
    };
    let before = last.split(' ').next().unwrap_or("");
    let said = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "{parted}\n  the instruction before ended at {before}\n  r64m ended with {}: {}",
        out.status,
        said.trim_end()
    )
    .into())
}

fn main() {
    let args = Arguments::from_args();
    let g64m = g64m();
    let mut names: Vec<(String, Option<PathBuf>)> = Vec::new();
    for (dir, ext) in [("tests/asm64", "asm"), ("tests/c64", "elf")] {
        let Ok(entries) = std::fs::read_dir(g64m.join(dir)) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == ext) {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                names.push((name, (ext == "asm").then_some(path)));
            }
        }
    }
    names.sort();

    let mut trials = Vec::new();
    if names.is_empty() {
        // Shown, so that a run without the checkout does not look like a run that passed.
        trials.push(Trial::test("g64m_checkout_not_found", || Ok(())).with_ignored_flag(true));
    }
    for (name, asm) in names {
        let waits = NOT_YET.iter().find(|(prefix, _)| name.starts_with(prefix));
        let g64m = g64m.clone();
        let label = match waits {
            Some((_, what)) => format!("{name} (waits for {what})"),
            None => name.clone(),
        };
        trials.push(
            Trial::test(label, move || case(&g64m, &name, asm.as_deref()))
                .with_ignored_flag(waits.is_some()),
        );
    }
    libtest_mimic::run(&args, trials).exit();
}
