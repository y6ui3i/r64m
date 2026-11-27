//! The primitives: 23 small C programs, each made of one thing a compiled program does
//! (a loop, a call through a pointer, the arithmetic of doubles), which r64m is held
//! to and raced on (`docs/experiments.md` names them in every race).
//!
//! The programs are g64m's and are read from its checkout, as the traces are (ADR
//! 0012; see `tests/trace.rs`): `tests/prim64/NAME.c`, built there as `NAME.elf`, and
//! what each prints, `NAME.out`. Here each has its name and what it is made of, and
//! five tests: the interpreter must print what is right, and the JIT in each of its
//! four ways must print the same, end the same, and count the same instructions.
//!
//! Without the checkout every test is shown as ignored.

use libtest_mimic::{Arguments, Failed, Trial};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// One primitive: its name (the file's, in g64m) and what its C is made of.
struct Primitive {
    name: &'static str,
    what: &'static str,
}

const fn primitive(name: &'static str, what: &'static str) -> Primitive {
    Primitive { name, what }
}

/// Every primitive. The integer ones first, in the order they were brought into
/// blocks; then those of floating point and of the XMM registers.
const PRIMITIVES: [Primitive; 23] = [
    primitive("p_loop", "loops: counting up and down, one inside another, with a sum carried round"),
    primitive("p_if", "if and else: chains of comparisons whose arms store to memory, so that they stay jumps"),
    primitive("p_ternary", "the conditional operator: smaller and larger of two, the absolute value, a value kept between two bounds, a comparison's result used as a number"),
    primitive("p_switch", "switch: twelve cases next to each other (a table of addresses), and five far apart (comparisons)"),
    primitive("p_recursion", "recursion: a function that calls itself twice, and one that goes down a tree it builds on the stack"),
    primitive("p_funcptr", "pointers to functions: one of eight called by a number from the data, and a function handed to another that calls it for every element"),
    primitive("p_pointer", "pointers: a linked list walked, a buffer read and written through moving pointers, a pointer to a pointer"),
    primitive("p_index", "arrays and structures: an element by its number, a field of an element, a table of rows and columns"),
    primitive("p_bytes", "bytes: text counted, changed and compared one character at a time"),
    primitive("p_fill", "filling and copying memory as a library does it: the string instructions with a repeat count"),
    primitive("p_shift", "bits: shifts by a constant and by a variable, masks, NOT and the negative"),
    primitive("p_rotate", "bits: rotating, the place of the lowest and of the highest set bit, one bit tested and set by its number, the count of set bits"),
    primitive("p_bitfield", "bits: a structure whose members are a few bits each, read and written"),
    primitive("p_muldiv", "multiplying and dividing by numbers not known when it is compiled: 32 and 64 bits, with and without a sign, and the remainder"),
    primitive("p_bignum", "large numbers: two of 256 bits multiplied into one of 512 with the carry handed on, and two of 256 bits added"),
    primitive("p_pixel", "pixels, one at a time: a picture of 256 by 256 put over another with its transparency"),
    primitive("p_sha", "a hash: SHA-256's rounds over blocks of 64 bytes (rotations, additions of 32 bits, bytes turned end for end)"),
    primitive("p_float", "float: the four operations and a comparison"),
    primitive("p_dbl_arith", "double: adding, taking away, multiplying and dividing, over arrays and in one chain"),
    primitive("p_dbl_cmp", "double: comparisons that decide a jump, the smaller and larger of two, and a search"),
    primitive("p_dbl_conv", "double: to and from whole numbers of 32 and 64 bits, with and without a sign, and to and from float"),
    primitive("p_ldbl", "long double: the four operations and a comparison, which are the x87's"),
    primitive("p_simd", "pixels, many at a time: p_pixel's picture, built so that the compiler works on several pixels in one XMM register"),
];

/// The four ways the JIT is run: blocks made when an address is hot or at once, and
/// each block held to the interpreter as it runs or not.
const JIT_MODES: [(&str, &[&str]); 4] = [
    ("jit", &[]),
    ("jit_check", &["--jit-check"]),
    ("jit_now", &["--jit-now"]),
    ("jit_check_now", &["--jit-check", "--jit-now"]),
];

fn g64m() -> PathBuf {
    match std::env::var_os("R64M_G64M") {
        Some(dir) => PathBuf::from(dir),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../g64m"),
    }
}

/// Runs a primitive with `--count` and what else `mode` says.
fn run(dir: &Path, p: &Primitive, mode: &[&str]) -> Result<Output, Failed> {
    Command::new(env!("CARGO_BIN_EXE_r64m"))
        .args(mode)
        .arg("--count")
        .arg(dir.join(format!("{}.elf", p.name)))
        .output()
        .map_err(|e| format!("r64m: {e}").into())
}

/// The interpreter prints what the program printed where it was compiled for.
fn interpreter_is_right(dir: &Path, p: &Primitive) -> Result<(), Failed> {
    let expected = std::fs::read(dir.join(format!("{}.out", p.name))).map_err(|e| format!("{}.out: {e}", p.name))?;
    let out = run(dir, p, &["--interpret"])?;
    if out.stdout == expected && out.status.code() == Some(0) {
        return Ok(());
    }
    Err(format!(
        "{} ({})\n  expected: {}\n  printed:  {}\n  ended with {}: {}",
        p.name,
        p.what,
        String::from_utf8_lossy(&expected).trim_end(),
        String::from_utf8_lossy(&out.stdout).trim_end(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim_end()
    )
    .into())
}

/// The JIT in one of its ways does what the interpreter does: the same output, the
/// same end, and (in what it says at the end) the same count of instructions.
fn jit_is_the_interpreter(dir: &Path, p: &Primitive, mode: &[&str]) -> Result<(), Failed> {
    let (alone, jit) = (run(dir, p, &["--interpret"])?, run(dir, p, mode)?);
    if alone.stdout == jit.stdout && alone.status == jit.status && alone.stderr == jit.stderr {
        return Ok(());
    }
    Err(format!(
        "{} ({}) with {}\n  the interpreter: {} | {} | {}\n  the JIT:         {} | {} | {}",
        p.name,
        p.what,
        mode.join(" "),
        String::from_utf8_lossy(&alone.stdout).trim_end(),
        alone.status,
        String::from_utf8_lossy(&alone.stderr).trim_end(),
        String::from_utf8_lossy(&jit.stdout).trim_end(),
        jit.status,
        String::from_utf8_lossy(&jit.stderr).trim_end()
    )
    .into())
}

/// Every program of the directory has its name here, and every name its program.
fn the_list_is_the_directory(dir: &Path) -> Result<(), Failed> {
    let mut there: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "elf"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    there.sort();
    let mut here: Vec<&str> = PRIMITIVES.iter().map(|p| p.name).collect();
    here.sort();
    if there == here { Ok(()) } else { Err(format!("in g64m: {there:?}\nhere: {here:?}").into()) }
}

fn main() {
    let args = Arguments::from_args();
    let dir = g64m().join("tests/prim64");
    let missing = !dir.is_dir();
    let mut trials = Vec::new();
    {
        let dir = dir.clone();
        trials.push(Trial::test("the_list_is_the_directory", move || the_list_is_the_directory(&dir)).with_ignored_flag(missing));
    }
    for p in &PRIMITIVES {
        let d = dir.clone();
        trials.push(Trial::test(format!("{}::interpreter", p.name), move || interpreter_is_right(&d, p)).with_ignored_flag(missing));
        for (label, mode) in JIT_MODES {
            let d = dir.clone();
            trials.push(
                Trial::test(format!("{}::{label}", p.name), move || jit_is_the_interpreter(&d, p, mode)).with_ignored_flag(missing),
            );
        }
    }
    libtest_mimic::run(&args, trials).exit();
}
