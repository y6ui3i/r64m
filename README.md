# r64m

An x86-64 emulator with a JIT for arm64 hosts, in Rust: the port of
[g64m](https://github.com/y6ui3i/g64m), which is written in Goose.

**Nothing is ported yet.** This repository was started on 2026-11-24 with the skeleton
below; the first step is in "What comes first".

## Why this exists

g64m asked how far a well-worn technique (an emulator with a JIT) can get when it is
written in a language made by AI and built by AI, with QEMU's own emulation (TCG) as the
yardstick. On 2026-11-24 it runs NetHack's build in a Linux guest at twice QEMU's speed
on a MacBook Air (Apple M1), and boots that guest at twice QEMU's speed.

r64m is to do the same things in Rust, and then both are to be tried on a Raspberry Pi
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

1. g64m built and its tests passed on arm64 Linux (it has only ever run on macOS). What
   that turns up is what r64m must be built for from the start: two hosts.
2. The shape of the guest's memory. g64m holds it as an array of its own and tests every
   address; a program without a kernel can have the host's protection do that instead
   (g64m's E184). r64m begins with that decided.
3. The interpreter for a program without a kernel, held to g64m's traces.

## Rules of this repository

Everything kept here is written in English: source comments, documentation, the ledger,
commit messages, branch names, pull requests. An experiment gets a card in
`docs/experiments.md` before it runs, and its result in the same place, kept or not.

## Building

```sh
cargo build --release
cargo test
```

The toolchain's version is fixed in `rust-toolchain.toml`.
