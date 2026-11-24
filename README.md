# r64m

An x86-64 emulator with a JIT for arm64 hosts, in Rust: a rebuild of
[g64m](https://github.com/y6ui3i/g64m), which is written in Goose. Not a port of its
code: what g64m found out is used, and the design begins again (`docs/adr/`).

**Static programs run, interpreted.** This repository was started on 2026-11-24. So
far: the guest's memory (`src/mem.rs`), the loader of a static program with its first
stack (`src/elf.rs`), an interpreter for the integer instructions, SSE and SSE2 and the
x87 (`src/cpu.rs`, `src/cpu/sse.rs`, `src/cpu/x87.rs`) and three system calls
(`src/sys.rs`). All 39 of g64m's cases with a trace from the real CPU give that trace,
and all 23 of its primitives print what they should, on macOS and on arm64 Linux
(R003). The interpreter runs 72 million instructions a second on a MacBook Air (Apple
M1), 1.27 times g64m's interpreter and ahead of it on every primitive (R007); QEMU,
which is a JIT, runs the same primitives at 1,267 million. r64m's own JIT
([0014](docs/adr/0014-the-jit.md)) translates blocks of integer instructions (moves,
arithmetic and logic on 8 to 64 bits, with and without the carry, multiplication and
division, shifts, rotations, bit scans, conditional moves and sets, the stack, jumps)
and of the scalar SSE instructions ([0015](docs/adr/0015-sse-in-blocks.md): moves,
arithmetic, comparisons and conversions of floats and doubles, and the packed
instructions of whole numbers; the XMM registers in the host's vector registers),
joins them to one another, finds a return's block without leaving generated code, and
checks each block against the interpreter when asked (`--jit-check`). It is asked for
with `--jit` and is not yet what r64m does by itself. Of the 23 primitives (named, with what each is made of, in `tests/prim.rs`) twenty-two are
past QEMU on this Mac (1.06 to 8.56 times), thirteen of them at twice QEMU or past it,
and on nineteen r64m's JIT is level with g64m's or ahead (R023). The one behind QEMU
is long double's: the x87 is interpreted, by soft arithmetic of 80 bits. Translated
code is guarded against writes only where the guest may write it
([0016](docs/adr/0016-only-writable-code-is-guarded.md)). There is no dynamic linking,
no system call beyond `write` and `exit`, and no machine yet.

## The goal

In an x86-64 container on an arm64 host, nginx takes a request over TLS and passes it to
a Python process serving FastAPI, and `Hello, world` comes back
([0011](docs/adr/0011-the-goal.md)).

## What r64m is not

- **Not a boundary of security.** A program's memory is the host's
  ([0008](docs/adr/0008-a-programs-memory-is-the-hosts.md)): the guest and r64m are one
  process with one set of rights, and an address that no x86 program can form validly
  (bit 47 or above set) is not caught and may reach r64m's own memory. QEMU's user mode
  and FEX-Emu are the same. What a container may not do is for the host's kernel to
  enforce.
- **Not exact on a host with 16 KB pages**, where generated code is concerned. The host
  r64m is made for is arm64 Linux with 4 KB pages. macOS on Apple silicon is where it
  is developed; there the host protects 16 KB at a time, so a closed page beside an
  open one can be read by generated code, and a guard page does not guard.
  There the guest also has 2^40 bytes of address space and not 2^47
  ([0013](docs/adr/0013-macos-reserves-less.md)).
- **64-bit only**: no 16-bit or 32-bit code, no BIOS.

## Why this exists

g64m asked how far a well-worn technique (an emulator with a JIT) can get when it is
written in a language made by AI and built by AI, with QEMU's own emulation (TCG) as the
yardstick. On 2026-11-24 it runs NetHack's build in a Linux guest at twice QEMU's speed
on a MacBook Air (Apple M1), and boots that guest at twice QEMU's speed.

r64m is to do the same things in Rust and then the goal above, and both are to be tried on a Raspberry Pi
(arm64 Linux, where there is no Rosetta and QEMU's user mode is the only rival).

**Why Rust**: the work is a JIT and the memory of a guest, where speed and low-level
control matter, and Rust is a well-worn language that the tools building this know
exactly. (g64m's own record of what cost it time is the other reason: globals reached by
their offset in one block, arrays that could not be mapped, a C file for every call the
language could not make.)

## What carries over from g64m

Not its code. What does not depend on the language:

- the tests and what the real CPU answered for each (g64m's `tests/`, with expected
  output taken under KVM and by ptrace on an x86 host);
- the design the experiments arrived at (blocks, the table of blocks, the cells, the
  flags' record), which is in g64m's `docs/jit64.md`;
- the ledger of what was tried and what did not work (`docs/experiments.md` there), so
  that it is not tried again here.

g64m is also the oracle for speed and for behaviour where the real CPU cannot be asked:
r64m is right when it prints what g64m prints, and g64m is right when it prints what the
real CPU does.

## What comes first

1. Done (R001): g64m built and its tests passed on arm64 Linux as it is. Two hosts need
   no second design.
2. Done (R002, ADR 0008, `src/mem.rs`): a program's memory is a reservation of the
   host's address space, protected by the host.
3. Done: a static program loaded and run by the interpreter on that memory, its trace
   the real CPU's, for the integer instructions, SSE and the x87.
4. Done (R004 to R007): the interpreter timed, and made faster than g64m's on every
   primitive without giving up the simple form.
5. The JIT, in ADR 0014's order. Done: the encoder; runnable memory and the way in and
   out; the translator for integer instructions with `--jit-check`; a fault in a
   block; a store to translated code, and to data beside it; blocks joined; returns
   and jumps through an operand found in a table. The record of the flags is written only where
   something can read it (R013 to R015); the integer instructions the primitives use
   (R016, R017); a block goes on past a conditional jump (R018); the scalar SSE
   instructions (R020) and the packed integer ones (R021); stores to data beside
   code on 16 KB pages go straight to memory (R023).
6. Next: what the goal's programs need before they run at all (ADR 0011): dynamic
   linking, system calls, threads, signals.

One case is owed: the x87's square root when what is left over equals the root (the
rounding there is not touched by any of g64m's cases; found by breaking it on purpose).
It needs a trace from the real CPU, and so the oracle in Rust (ADR 0012).

## Rules of this repository

Everything kept here is written in English: source comments, documentation, the ledger,
commit messages, branch names, pull requests. An experiment gets a card in
`docs/experiments.md` before it runs, and its result in the same place, kept or not.

## Building and testing

```sh
cargo build --release
cargo test
target/release/r64m program            # run a static x86-64 program
target/release/r64m --trace program    # print the registers after every instruction
```

Everything here is Rust; nothing is added in another language (0012). The toolchain's
version is fixed in `rust-toolchain.toml`, and the versions of the two crates from
outside in `Cargo.lock` ([0009](docs/adr/0009-two-crates.md)).

For arm64 Linux, from the Mac, a static executable (the target is fetched once with
`rustup target add aarch64-unknown-linux-musl`):

```sh
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    cargo build --release --target aarch64-unknown-linux-musl
```

`cargo test` holds r64m's traces to the real CPU's. The cases and the traces are g64m's
and are read from its checkout, beside this one or where `R64M_G64M` says; `nasm` builds
the cases. Without the checkout those tests are shown as ignored.
