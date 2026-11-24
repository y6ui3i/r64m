# 0009: Two crates from outside: `libc` in the program, `libtest-mimic` for the tests

**Status**: accepted (2026-11-24, the owner: both may be fetched)

## Context

r64m is to stay small enough to be read whole (0007), and each thing taken from outside
is something that cannot be read whole. Two needs cannot be met by Rust's standard
library:

- ADR 0008 needs the host's `mmap`, `mprotect`, `sigaction` and the layout of what a
  signal handler is handed (the faulting address, the saved registers). The standard
  library offers none of them.
- ADR 0006 needs a harness that turns each case found in a file into a test with its
  own name, which `cargo test` can list, filter and run in parallel. The built-in
  harness only knows tests that are written as functions.

## Decision

**`libc`** is the program's only dependency. It is the declarations of the host's C
library and nothing else: no code of its own runs. Looked at and not taken:

- declaring the calls by hand (`unsafe extern "C"`): the calls are easy, but the layouts
  of `sigaction`, `siginfo_t` and `ucontext_t` differ between macOS and Linux and
  between C libraries, and a layout written wrongly is silent. `libc` is where those
  layouts are kept right;
- `nix` and `rustix`: safe wrappers over the same calls. More to read than what they
  would wrap here, and r64m's use (a fixed mapping inside a reservation, a handler that
  rewrites the saved registers) is where their safety does not reach.

**`libtest-mimic`** is a dependency of the tests only (`[dev-dependencies]`): the
executable does not contain it. It gives a `main` for a test program that takes a list
of named tests made at run time and behaves as the built-in harness does. Looked at and
not taken:

- `datatest-stable`: one test for each *file*. 0006 wants the cases of one subject
  together in one file, so the list has to be made by r64m's own code;
- a `main` of r64m's own: forty lines for running and counting, but then the filter,
  `--list`, running in parallel and the output that tools read are each to be written
  again.

Versions are fixed by `Cargo.lock`, which is kept in the repository. A third crate
needs a decision of its own.

## What came with them

`Cargo.lock` names 24 packages besides r64m, 6.47 MB as downloaded (the estimate given
to the owner beforehand was about 2.5 MB: wrong, it counted only what is built on this
host).

- `libc` 0.2.191: alone. The program's own tree is this one crate.
- `libtest-mimic` 0.8.2 brings 22: `clap` for its command line (`clap`, `clap_builder`,
  `clap_lex`, `strsim`), clap's derive macro and what a macro needs (`clap_derive`,
  `syn`, `quote`, `proc-macro2`, `heck`, `unicode-ident`), colour on a terminal
  (`anstream`, `anstyle`, `anstyle-parse`, `anstyle-query`, `colorchoice`, `utf8parse`,
  `is_terminal_polyfill`, `once_cell_polyfill`), `escape8259` for its JSON output, and
  three that are for Windows only and are fetched but never built here
  (`anstyle-wincon`, `windows-sys`, `windows-link`).

## Consequences

- The executable depends on the host's C library and on nothing else from outside.
- Building the tests builds a macro's tool chain (`syn`), which is most of the time a
  first `cargo test` takes.
- If the 22 come to cost more than the four things they save from being written, the
  harness is replaced by a `main` of r64m's own; the cases, being data, do not change.
