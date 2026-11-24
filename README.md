# r64m

An x86-64 emulator with a JIT for arm64 hosts, in Rust: a rebuild of
[g64m](https://github.com/y6ui3i/g64m), which is written in Goose. Not a port of its
code: what g64m found out is used, and the design begins again (`docs/adr/`).

**Static programs of integer instructions run, interpreted.** This repository was
started on 2026-11-24. So far: the guest's memory (`src/mem.rs`), the loader of a static
program with its first stack (`src/elf.rs`), an interpreter for the integer instructions
(`src/cpu.rs`) and three system calls (`src/sys.rs`). Of g64m's 39 cases with a trace
from the real CPU, the 26 that use neither the x87 nor SSE give that trace. There is no
JIT, no x87, no SSE, no dynamic linking and no machine yet.

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
3. Done for the integer instructions: a static program loaded and run by the
   interpreter on that memory, its trace the real CPU's.
4. The x87 and SSE, to the same traces; then the primitives (`tests/prim64` of g64m),
   which print what they compute.

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

`cargo test` holds r64m's traces to the real CPU's. The cases and the traces are g64m's
and are read from its checkout, beside this one or where `R64M_G64M` says; `nasm` builds
the cases. Without the checkout those tests are shown as ignored.
