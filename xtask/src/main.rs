//! r64m's tools (ADR 0012: Rust here too), started as `cargo xtask COMMAND`:
//!
//!   cargo xtask test [ARGUMENTS]       the unit tests, the primitives and the traces
//!   cargo xtask guest                  the guest's own programs, built and run
//!   cargo xtask images build [DIR]     the two images that are built here
//!   cargo xtask images load DIR        the same, taken from files `build DIR` left
//!   cargo xtask container [--ways W] [--jobs N] [SUITE ...]
//!                                      programs of x86-64 images, held to the real machine
//!   cargo xtask ci [--ways W]          all of the above that this host can run
//!   cargo xtask install                r64m made Docker's runner of x86-64 images here
//!   cargo xtask uninstall              and taken away again
//!   cargo xtask status                 whether it is, and what else is registered
//!
//! The same commands are what the CI runs (.github/workflows/ci.yml): what passes
//! here passes there.
//!
//! Where things run. On macOS `test` is the host's `cargo test`. On Linux everything
//! is built for arm64 Linux with musl, as r64m itself is for a container: the
//! container tests start r64m inside an x86-64 image, where there is no C library
//! of the host's to link to. The primitives and the traces need g64m's checkout
//! (beside this one, or where `R64M_G64M` says) and, for the traces, `nasm`.

use std::path::{Path, PathBuf};
use std::process::{Command, exit};

/// What r64m is built for to run in a container, and what the guest's programs are.
const LINUX: &str = "aarch64-unknown-linux-musl";
const GUEST: &str = "x86_64-unknown-linux-musl";

/// Where a suite's image comes from.
enum Image {
    /// Docker Hub's, by its digest: the very image the cases were taken in on hal.
    Hub(&'static str),
    /// Built from `tools/images/NAME/Dockerfile` and tagged `r64m-test-NAME:amd64`.
    Built(&'static str),
}

/// The files of cases (`tests/container/*.txt`) and the image each was taken in.
/// A file is chosen by its name as a filter of the tests' names.
const SUITES: [(&str, Image, &[&str]); 10] = [
    ("gcc", Image::Built("gcc"), &["busybox::", "shell::", "gcc::"]),
    ("python", Image::Hub("python@sha256:e48f59cdba69a3cbd9ed2cf0f348d90d45a8d350d98134cf1070bc106ee9ed90"), &["python::"]),
    ("redis", Image::Hub("redis@sha256:7958cfbb9d152f87dadfbc89f1b0b15938f0b81ff8982feb327476f4911935c3"), &["redis::"]),
    ("goal", Image::Built("goal"), &["goal::"]),
    // nginx:alpine, memcached:alpine, node:22-alpine, eclipse-temurin:21-jre-alpine,
    // golang:1.23-alpine and postgres:17-alpine as they were on 2026-11-26 (R047).
    ("nginx", Image::Hub("nginx@sha256:54845972134f849c623fe5cfbedee7e1d35a5708c84a4f36bf9471a9b7aa673d"), &["nginx::"]),
    ("memcached", Image::Hub("memcached@sha256:4c212724c5f8a4db867cf61626dff2d4447a0474d5a103093fe61d663e535f64"), &["memcached::"]),
    ("node", Image::Hub("node@sha256:12f54cafa58d0e96554ae05871e29663ff7241836377dc9a7916449b8adb062c"), &["node::"]),
    ("java", Image::Hub("eclipse-temurin@sha256:c788d7d9ddc1cf0cf38a1735e14792eb2ff4dc6886e9658771095b7e36a69523"), &["java::"]),
    ("go", Image::Hub("golang@sha256:a7ecaac5efda22510d8c903bdc6b19026543f1eac3317d47363680df22161bd8"), &["go::"]),
    ("postgres", Image::Hub("postgres@sha256:e80acae3f1109e8e4533618b594918936f2858c63afc7204c4773148a673add8"), &["postgres::"]),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask is a folder of the repository").to_path_buf()
}

fn fail(what: impl std::fmt::Display) -> ! {
    eprintln!("xtask: {what}");
    exit(1)
}

/// `cargo` in the repository, with the linker a build for another target needs.
fn cargo() -> Command {
    let mut c = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    c.current_dir(root());
    for target in ["AARCH64_UNKNOWN_LINUX_MUSL", "X86_64_UNKNOWN_LINUX_MUSL"] {
        let name = format!("CARGO_TARGET_{target}_LINKER");
        if std::env::var_os(&name).is_none() {
            c.env(name, "rust-lld");
        }
    }
    c
}

/// Says the command and runs it; whether it ended well.
fn ran(c: &mut Command) -> bool {
    eprintln!("xtask: {c:?}");
    c.status().unwrap_or_else(|e| fail(format_args!("{c:?}: {e}"))).success()
}

fn must(c: &mut Command) {
    if !ran(c) {
        fail(format_args!("failed: {c:?}"));
    }
}

/// The arguments that make a build one for arm64 Linux, where the host is Linux.
fn for_linux() -> &'static [&'static str] {
    if cfg!(target_os = "linux") { &["--target", LINUX] } else { &[] }
}

/// The unit tests, the primitives and the traces: not the container cases, which
/// run where a file of the image they name is, and on a Linux host with a shell
/// and a compiler of its own would take the host's programs for the image's (the
/// CI's first run did), nor the guest's, which `guest` builds first.
fn test(more: &[String]) -> bool {
    ran(cargo().args(["test", "--release", "--bin", "r64m", "--test", "prim", "--test", "trace"]).args(for_linux()).args(more))
}

/// The guest's programs built for x86-64 Linux, and the tests that run them. Only
/// Linux can run them; elsewhere the tests show every case as ignored.
fn guest() -> bool {
    must(cargo().args(["build", "--release", "--manifest-path", "guest/Cargo.toml", "--target", GUEST]));
    ran(cargo().args(["test", "--release", "--test", "guest"]).args(for_linux()))
}

fn tag(name: &str) -> String {
    format!("r64m-test-{name}:amd64")
}

fn built() -> impl Iterator<Item = &'static str> {
    SUITES.iter().filter_map(|(_, image, _)| match image {
        Image::Built(name) => Some(*name),
        Image::Hub(_) => None,
    })
}

/// `build`: the images that are made here, and with a folder each one saved there
/// as a file. `load`: taken from such files. (The CI builds them on an x86-64
/// machine, where their own programs run to build them, and runs them on arm64.)
fn images(args: &[String]) {
    let dir = args.get(1).map(|d| root().join(d));
    match args.first().map(String::as_str) {
        Some("build") => {
            for name in built() {
                must(Command::new("docker").args(["build", "--platform", "linux/amd64", "-t", &tag(name)]).arg(root().join("tools/images").join(name)));
                if let Some(dir) = &dir {
                    std::fs::create_dir_all(dir).unwrap_or_else(|e| fail(format_args!("{}: {e}", dir.display())));
                    must(Command::new("docker").args(["save", "-o"]).arg(dir.join(format!("{name}.tar"))).arg(tag(name)));
                }
            }
        }
        Some("load") => {
            let dir = dir.unwrap_or_else(|| fail("images load DIR"));
            for name in built() {
                must(Command::new("docker").args(["load", "-i"]).arg(dir.join(format!("{name}.tar"))));
            }
        }
        _ => fail("images build [DIR] | images load DIR"),
    }
}

/// The value of an option given as `--name VALUE`, taken out of the arguments.
fn option(args: &mut Vec<String>, name: &str) -> Option<String> {
    let at = args.iter().position(|a| a == name)?;
    if at + 1 >= args.len() {
        fail(format_args!("{name} needs a value"));
    }
    args.remove(at);
    Some(args.remove(at))
}

/// r64m and the container tests built for arm64 Linux and put in one folder, which
/// a container is given as `/r`.
fn stage() -> PathBuf {
    must(cargo().args(["build", "--release", "--target", LINUX]));
    let mut c = cargo();
    c.args(["test", "--release", "--target", LINUX, "--no-run", "--test", "container", "--message-format=json"]);
    eprintln!("xtask: {c:?}");
    let out = c.stderr(std::process::Stdio::inherit()).output().unwrap_or_else(|e| fail(format_args!("cargo: {e}")));
    if !out.status.success() {
        fail("the container tests were not built");
    }
    // The line of Cargo's that names the test's file: `"executable":"PATH"` in the
    // message about the target named `container`.
    let text = String::from_utf8_lossy(&out.stdout);
    let test = text
        .lines()
        .filter(|l| l.contains("\"name\":\"container\""))
        .find_map(|l| l.split("\"executable\":\"").nth(1)?.split('"').next().map(PathBuf::from))
        .unwrap_or_else(|| fail("Cargo did not say where the container tests were built"));
    let dir = root().join("target/stage");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(format_args!("{}: {e}", dir.display())));
    for (from, to) in [(root().join("target").join(LINUX).join("release/r64m"), "r64m"), (test, "container")] {
        std::fs::copy(&from, dir.join(to)).unwrap_or_else(|e| fail(format_args!("{}: {e}", from.display())));
    }
    dir
}

/// One file of cases in its image; whether every case passed, and what was said.
fn suite(stage: &Path, image: &str, filter: &str, ways: &str) -> (bool, String) {
    let mut c = Command::new("docker");
    c.args(["run", "--rm", "--platform", "linux/amd64", "-e", "R64M_BIN=/r/r64m", "-e"]);
    c.arg(format!("R64M_WAYS={ways}"));
    c.arg("-v").arg(format!("{}:/r:ro", stage.display()));
    c.args(["--entrypoint", "/r/container", image, filter]);
    let said = format!("xtask: {c:?}\n");
    let out = c.output().unwrap_or_else(|e| fail(format_args!("docker: {e}")));
    let text = said + &String::from_utf8_lossy(&out.stdout) + &String::from_utf8_lossy(&out.stderr);
    // A file whose cases were all passed over (its image is not what it needs)
    // passes nothing: that is not a pass.
    let some = text.lines().any(|l| l.starts_with("test result: ok.") && !l.contains(" 0 passed"));
    (out.status.success() && some, text)
}

fn container(mut args: Vec<String>) -> bool {
    let ways = option(&mut args, "--ways").unwrap_or_else(|| "all".to_string());
    let jobs: usize = option(&mut args, "--jobs").map_or(1, |n| n.parse().unwrap_or_else(|_| fail("--jobs N")));
    if let Some(unknown) = args.iter().find(|a| !SUITES.iter().any(|(name, ..)| name == a)) {
        let names: Vec<&str> = SUITES.iter().map(|s| s.0).collect();
        fail(format_args!("no suite named {unknown}: there are {}", names.join(", ")));
    }
    let stage = stage();
    let mut work = Vec::new();
    for (name, image, filters) in &SUITES {
        if !args.is_empty() && !args.iter().any(|a| a == name) {
            continue;
        }
        let image = match image {
            Image::Hub(image) => image.to_string(),
            Image::Built(dir) => {
                let image = tag(dir);
                if !Command::new("docker").args(["image", "inspect", &image]).output().is_ok_and(|o| o.status.success()) {
                    fail(format_args!("no image {image}: `cargo xtask images build` makes it"));
                }
                image
            }
        };
        work.extend(filters.iter().map(|filter| (image.clone(), *filter)));
    }
    let work = std::sync::Mutex::new(work.into_iter());
    let failed = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| {
                loop {
                    let Some((image, filter)) = work.lock().unwrap().next() else { break };
                    let (ok, text) = suite(&stage, &image, filter, &ways);
                    // Whole, so that two suites' lines are not mixed.
                    print!("{text}");
                    if !ok {
                        failed.lock().unwrap().push(filter);
                    }
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap();
    if !failed.is_empty() {
        eprintln!("xtask: failed: {}", failed.join(" "));
    }
    failed.is_empty()
}

// ---- Docker's runner of x86-64 images ---------------------------------------------
//
// The kernel that runs the containers (the VM's, with Docker on a Mac) starts a
// program it cannot run itself by what is registered with `binfmt_misc`. `install`
// registers r64m there for x86-64 programs (R049); an entry made later is asked
// first, so QEMU's or Rosetta's, if there is one, is left as it is and comes back
// to work when r64m's is removed. The list can only be written from inside that
// kernel's world with every right: by a container started `--privileged`. The
// container's image is made here of two files and nothing else: r64m, and this
// program built for arm64 Linux, which does the writing.

/// The image of the two files, and the entry's name in the kernel's list.
const HELPER: &str = "r64m-install";
const ENTRY: &str = "r64m";
const BINFMT: &str = "/proc/sys/fs/binfmt_misc";

/// The line that registers r64m: programs that begin as a 64-bit little-endian ELF
/// file for x86-64, a program or a shared object (the mask passes over the
/// system's own byte and the low bit of the type). Flag P: r64m is given the
/// program's path and then the first argument the program was started with (it
/// knows this order by its name, `r64m-binfmt`). Flag F: the kernel opens r64m's
/// file now and holds it, so every container finds it whatever its own files are.
const RULE: &str = r":r64m:M::\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00\x3e\x00:\xff\xff\xff\xff\xff\xfe\xfe\x00\xff\xff\xff\xff\xff\xff\xff\xff\xfe\xff\xff\xff:/r64m-binfmt:PF";

/// Builds r64m and this program for arm64 Linux and makes the image of them.
fn helper() {
    must(cargo().args(["build", "--release", "--target", LINUX]));
    must(cargo().args(["build", "--release", "--manifest-path", "xtask/Cargo.toml", "--target", LINUX]));
    let dir = root().join("target/install");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| fail(format_args!("{}: {e}", dir.display())));
    for (from, to) in [(root().join("target").join(LINUX).join("release/r64m"), "r64m-binfmt"), (root().join("xtask/target").join(LINUX).join("release/xtask"), "xtask")] {
        std::fs::copy(&from, dir.join(to)).unwrap_or_else(|e| fail(format_args!("{}: {e}", from.display())));
    }
    std::fs::write(dir.join("Dockerfile"), "FROM scratch\nCOPY r64m-binfmt xtask /\nENTRYPOINT [\"/xtask\"]\n").unwrap_or_else(|e| fail(format_args!("{}: {e}", dir.display())));
    must(Command::new("docker").args(["build", "--quiet", "--platform", "linux/arm64", "--tag", HELPER]).arg(&dir));
}

/// Runs this program in the image, with every right, for one of the three commands.
fn in_the_kernels_world(command: &str) -> bool {
    helper();
    ran(Command::new("docker").args(["run", "--rm", "--privileged", "--platform", "linux/arm64", HELPER, command]))
}

/// The kernel's list, mounted if this container does not see it yet.
fn list() -> PathBuf {
    let dir = PathBuf::from(BINFMT);
    if !dir.join("register").exists() && !(mount_the_list() && dir.join("register").exists()) {
        fail(format_args!("{BINFMT} is not to be had: this must run in a container started with --privileged, on a kernel with binfmt_misc"));
    }
    dir
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn mount_the_list() -> bool {
    // mount("binfmt_misc", BINFMT, "binfmt_misc", 0, null): the kernel's call 40, made
    // by hand: this crate has no dependencies.
    let (source, kind) = (c"binfmt_misc", c"binfmt_misc");
    let target = std::ffi::CString::new(BINFMT).expect("no zero in it");
    let answer: i64;
    unsafe {
        std::arch::asm!("svc 0", in("x8") 40u64, inlateout("x0") source.as_ptr() as i64 => answer, in("x1") target.as_ptr(), in("x2") kind.as_ptr(), in("x3") 0u64, in("x4") 0u64, options(nostack));
    }
    answer == 0
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
fn mount_the_list() -> bool {
    false
}

fn write_to(file: &Path, what: &str) {
    use std::io::Write;
    let done = std::fs::OpenOptions::new().write(true).open(file).and_then(|mut f| f.write_all(what.as_bytes()));
    done.unwrap_or_else(|e| fail(format_args!("{}: {e}", file.display())));
}

/// What the kernel says of r64m's entry and which others there are.
fn say_the_list(dir: &Path) {
    match std::fs::read_to_string(dir.join(ENTRY)) {
        Ok(entry) => println!("r64m is registered:\n{}", entry.trim_end()),
        Err(_) => println!("r64m is not registered"),
    }
    let mut others: Vec<String> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n != ENTRY && n != "register" && n != "status").collect();
    others.sort();
    println!("the other entries: {}", if others.is_empty() { "none".to_string() } else { others.join(", ") });
}

/// The three commands as the container runs them.
fn in_the_container(command: &str) {
    let dir = list();
    let entry = dir.join(ENTRY);
    if matches!(command, "registers" | "unregisters") && entry.exists() {
        write_to(&entry, "-1");
    }
    if command == "registers" {
        write_to(&dir.join("register"), RULE);
    }
    say_the_list(&dir);
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        fail("test | guest | images | container | ci | install | uninstall | status (see xtask/src/main.rs)");
    }
    let command = args.remove(0);
    let ok = match command.as_str() {
        "test" => test(&args),
        "guest" => guest(),
        "images" => {
            images(&args);
            true
        }
        "container" => container(args),
        "install" => in_the_kernels_world("registers"),
        "uninstall" => in_the_kernels_world("unregisters"),
        "status" => in_the_kernels_world("says"),
        "registers" | "unregisters" | "says" => {
            in_the_container(&command);
            true
        }
        "ci" => {
            let ways = option(&mut args, "--ways").unwrap_or_else(|| "jit".to_string());
            // Each is run even if one before failed: one run says all that is wrong.
            let mut ok = test(&[]);
            ok &= guest();
            if Command::new("docker").arg("version").output().is_ok_and(|o| o.status.success()) {
                ok &= container(vec!["--ways".to_string(), ways]);
            } else {
                eprintln!("xtask: no Docker here: the container tests were not run");
            }
            ok
        }
        other => fail(format_args!("no command {other}")),
    };
    exit(if ok { 0 } else { 1 })
}
