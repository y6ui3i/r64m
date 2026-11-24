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
| [0002](0002-main-waits-for-a-version-that-works.md) | `main` is not updated until r64m does what g64m does |
| [0003](0003-64-bit-only.md) | 64-bit only: no 16-bit, no 32-bit, no BIOS |
| [0004](0004-one-executable.md) | One executable |
| [0005](0005-loaders-in-host-code.md) | A program and a kernel are loaded by host code |
| [0006](0006-tests-are-data.md) | Tests of behaviour are data, run by a harness |
| [0007](0007-simple-first.md) | The simple form first; what g64m added is added again only when counted |
| [0008](0008-a-programs-memory-is-the-hosts.md) | A program's memory is the host's, with the host's protection |
