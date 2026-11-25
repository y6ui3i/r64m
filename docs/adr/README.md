# Decisions

One file for each decision that shapes r64m: what was decided, why, and what follows from
it. A decision is written when it is made, numbered in order, and not rewritten
afterwards: one that is changed gets a new file that names the one it replaces.

The ledger (`../experiments.md`) says what was measured. These say what was decided. A
decision that rests on a measurement names the experiment (R-numbers are this
repository's, E-numbers are g64m's, in its `docs/experiments.md`).

Each file has: **Status** (accepted, or replaced by which), **Context**, **Decision**,
**Consequences**.

| | |
|---|---|
| [0001](0001-a-rebuild-in-rust.md) | A rebuild in Rust, not a port of g64m's code |
| [0002](0002-main-waits-for-a-version-that-works.md) | `main` is not updated until r64m does what g64m does (the bar changed by 0018) |
| [0003](0003-64-bit-only.md) | 64-bit only: no 16-bit, no 32-bit, no BIOS |
| [0004](0004-one-executable.md) | One executable |
| [0005](0005-loaders-in-host-code.md) | A program and a kernel are loaded by host code |
| [0006](0006-tests-are-data.md) | Tests of behaviour are data, run by a harness |
| [0007](0007-simple-first.md) | The simple form first; what g64m added is added again only when counted |
| [0008](0008-a-programs-memory-is-the-hosts.md) | A program's memory is the host's, with the host's protection |
| [0009](0009-two-crates.md) | Two crates from outside: `libc` in the program, `libtest-mimic` for the tests |
| [0010](0010-the-interpreter-asks-the-list.md) | The interpreter asks the list of regions; generated code does not |
| [0011](0011-the-goal.md) | The goal for 64 bits: HTTPS through nginx to FastAPI, in an x86-64 container |
| [0012](0012-rust-only-g64m-for-reference.md) | Rust only; g64m is read, not built upon |
| [0013](0013-macos-reserves-less.md) | On macOS the reservation is 2^40 bytes |
| [0014](0014-the-jit.md) | The JIT: blocks for arm64, the simple form first |
| [0015](0015-sse-in-blocks.md) | SSE in blocks: the host's vector registers, the interpreter for what is not a number |
| [0016](0016-only-writable-code-is-guarded.md) | Only code the guest may write is guarded against writes |
| [0017](0017-the-containers-way.md) | The container's way: a program's system calls are the host kernel's |
| [0018](0018-one-thing-well.md) | r64m does one thing: it runs an x86-64 program's instructions; it is not a machine |
