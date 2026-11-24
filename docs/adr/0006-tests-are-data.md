# 0006: Tests of behaviour are data, run by a harness

**Status**: accepted (2026-11-24, the owner)

## Context

g64m's tests of instructions are assembly files, each with its expected output in another
directory, compressed: reading a test does not say what is right. One test to a file was
found hard to read.

## Decision

Three kinds:

- a small function (a decoder, the encoding of a host instruction) is tested in its own
  file, under `#[cfg(test)]`;
- behaviour is a case under `tests/cases/`: assembly, with what must come out written in
  the file where that is short, cases of one subject together in one file; a harness
  library runs them, each under its own name. The expected output comes from the real
  CPU, as in g64m;
- what takes long (the boot, the build in the guest) runs only when asked for.

Helper work (taking expected output again, the races, the gate) is Rust in the same
repository (`cargo xtask`), not shell and Python beside it.

## Consequences

- Which harness library: to be chosen when the first case is written, with the reason
  in a decision of its own.
  (Chosen 2026-11-24: 0009.)
