//! Programs of an x86-64 container run by r64m, held to what the real machine printed
//! for the same programs (ADR 0017, point 5).
//!
//! The cases are data: `tests/container/*.txt`, each a list of commands with the
//! standard output and the exit code that an x86-64 Linux gave. A command after
//! "$ " is a program and its arguments; one after "$$ " is given whole to the shell. A case is run three
//! ways (the interpreter, the JIT, the JIT with every block checked) and must give
//! that output and that code each time.
//!
//! They can be run only where the programs are: on Linux, with the x86-64 image a
//! file's cases were taken in as the root (r64m hands a program's paths to the kernel
//! as they are). Each file's first line names a file that is there in its image
//! (`#! needs PATH`); where that is not, the file's cases are shown as ignored. The image is not in the repository. To run them, the test and r64m are
//! built for arm64 Linux and started inside such a container, with `R64M_BIN` saying
//! where r64m is there (the place Cargo knows is the building host's).

use libtest_mimic::{Arguments, Failed, Trial};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The three ways each case is run.
const WAYS: [(&str, &[&str]); 3] = [("interpreter", &[]), ("jit", &["--jit"]), ("jit_check", &["--jit-check"])];

/// One case: a program with its arguments, what it prints, and how it ends.
struct Case {
    command: Vec<String>,
    output: Vec<u8>,
    code: i32,
}

/// The cases of one file. A line that begins with `#` before the first case is a
/// comment. An argument is as it stands in the file: `\n` there is the two
/// characters, as a shell would hand them on.
fn cases(text: &str) -> Vec<Case> {
    let mut all = Vec::new();
    let mut now: Option<Case> = None;
    for line in text.lines() {
        if let Some(script) = line.strip_prefix("$$ ") {
            assert!(now.is_none(), "a case without its end before: {line}");
            now = Some(Case { command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()], output: Vec::new(), code: 0 });
        } else if let Some(command) = line.strip_prefix("$ ") {
            assert!(now.is_none(), "a case without its end before: {line}");
            now = Some(Case { command: command.split(' ').map(String::from).collect(), output: Vec::new(), code: 0 });
        } else if let (Some(code), true) = (line.strip_prefix("? "), now.is_some()) {
            let mut case = now.take().unwrap();
            case.code = code.parse().expect("an exit code");
            all.push(case);
        } else if let Some(case) = &mut now {
            case.output.extend(line.as_bytes());
            case.output.push(b'\n');
        } else {
            assert!(line.is_empty() || line.starts_with('#'), "neither a comment nor a case: {line}");
        }
    }
    assert!(now.is_none(), "the last case has no end");
    all
}

fn r64m() -> PathBuf {
    match std::env::var_os("R64M_BIN") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_BIN_EXE_r64m")),
    }
}

/// The cases that make files make them by fixed names: one of those at a time.
static FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run(case: &Case, way: &[&str]) -> Result<(), Failed> {
    let _one = case.command.iter().any(|a| a.contains("/tmp")).then(|| FILES.lock().unwrap_or_else(|e| e.into_inner()));
    let out = Command::new(r64m()).args(way).args(&case.command).output().map_err(|e| format!("r64m: {e}"))?;
    if out.stdout == case.output && out.status.code() == Some(case.code) {
        return Ok(());
    }
    Err(format!(
        "{}\n  the real machine: {:?}, ended with {}\n  r64m:             {:?}, {}\n  and said: {}",
        case.command.join(" "),
        String::from_utf8_lossy(&case.output),
        case.code,
        String::from_utf8_lossy(&out.stdout),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim_end()
    )
    .into())
}

fn main() {
    let args = Arguments::from_args();
    let mut trials = Vec::new();
    let files = [
        ("busybox", include_str!("container/busybox.txt")),
        ("shell", include_str!("container/shell.txt")),
        ("gcc", include_str!("container/gcc.txt")),
        ("python", include_str!("container/python.txt")),
        ("redis", include_str!("container/redis.txt")),
        ("goal", include_str!("container/goal.txt")),
        ("nginx", include_str!("container/nginx.txt")),
        ("memcached", include_str!("container/memcached.txt")),
        ("node", include_str!("container/node.txt")),
        ("java", include_str!("container/java.txt")),
        ("go", include_str!("container/go.txt")),
        ("postgres", include_str!("container/postgres.txt")),
    ];
    for (file, text) in files {
        // Only where the file's programs are.
        let needs = text.lines().next().and_then(|l| l.strip_prefix("#! needs ")).expect("a file of cases begins by saying what it needs");
        let here = cfg!(target_os = "linux") && Path::new(needs).exists();
        for case in cases(text) {
            let case = std::sync::Arc::new(case);
            for (way, options) in WAYS {
                // Only some of the ways, where asked (`R64M_WAYS=jit`, or several
                // with commas): the interpreter and the checker take many times
                // as long, and a push is not held up for them (`cargo xtask`).
                if std::env::var("R64M_WAYS").is_ok_and(|ways| ways != "all" && !ways.split(',').any(|w| w == way)) {
                    continue;
                }
                let said: String = case.command[1..].join("_").chars().take(48).map(|c| if c.is_alphanumeric() || "-.".contains(c) { c } else { '_' }).collect();
                let name = format!("{file}::{said}::{way}");
                let case = case.clone();
                trials.push(Trial::test(name, move || run(&case, options)).with_ignored_flag(!here));
            }
        }
    }
    libtest_mimic::run(&args, trials).exit();
}
