# 0012: Rust only; g64m is read, not built upon

**Status**: accepted (2026-11-24, the owner)

## Context

g64m is Goose with six files of C beside it for what Goose could not call. With r64m
begun as a rebuild (0001), two questions were open: whether work goes on in Goose and C
as well, and where r64m's tests come from, since the cases and the real CPU's answers
to them are g64m's (`tests/asm64`, `tests/c64`, `tests/expected64`).

## Decision

- **Work goes on in Rust alone**: away from C and from Goose. Nothing is added to r64m
  in another language: no C glue, no shell or Python beside it (0006 already says
  helpers are Rust). g64m is not developed further. The one exception the owner named:
  if r64m is ever built for WebAssembly, the page around it (HTML and what a page
  needs) is what it has to be.
- **g64m is for reference.** Its checkout is read: the cases, the traces, the ledger,
  the design. r64m's tests read the cases and traces from that checkout (beside this
  one, or where `R64M_G64M` says) and are not copies of them.

## Consequences

- One copy of each case and each trace, in g64m. r64m's own repository does not test
  itself without that checkout: the tests are then shown as ignored, not as passed.
- The cases stay assembly for nasm, which is a tool r64m's tests run, as they run
  `gzip`: not code of r64m's.
- (R038, the owner 2026-11-25.) A program written here for r64m to run, where a
  test needs what a shell or Python cannot make, is Rust too: the crate `guest`,
  built for x86-64 Linux, held to what the real machine prints for the same file.
  What a compiler under test is given stays a few lines of C inside a shell
  command: no `.c` file.
- 0006's `tests/cases/` in this repository waits: a case that g64m does not have is
  the first that goes there.
- New expected output can only come from the real CPU. g64m's oracle
  (`tools/oracle/step64.c`, run on an x86 host) took what there is. It is C: when a
  trace has to be taken again or a new one taken, the oracle is written again in Rust
  here (ptrace through `libc`, built on the x86 host), not changed there.
- `tools/linux/Dockerfile` installs a C compiler: it is the record of R001, which built
  g64m, and builds nothing of r64m's.
