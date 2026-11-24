# 0001: A rebuild in Rust, not a port of g64m's code

**Status**: accepted (2026-11-24, the owner)

## Context

g64m is an x86-64 emulator with a JIT for arm64 hosts, written in Goose. On 2026-11-24 it
ran NetHack's build in a Linux guest at twice the speed of QEMU's own emulation and booted
that guest at twice QEMU's speed (its E182 and E185). What still held it were things of
the language, not of the design:

- its globals lie in one block in the order they are declared and are reached by their
  offset, so that what the interpreter uses most lay 48 MB in, and every global added
  moved the arrays off their cache lines (E173);
- the guest's memory can only be an array of the program's own, so every address is
  tested before it is used; thirteen of the 28 host instructions of one primitive's loop
  were such tests (E184);
- generated code cannot call the interpreter's routines freely, so one instruction left
  to the interpreter is a block left and entered again, some 400 to 600 host instructions
  (E172, E173);
- every low-level call needs a file of C beside it.

The next milestone was to have been a Docker backend written in Goose (M11). Its centre
is the guest's memory held by the host's own protection, which is the thing Goose could
not do.

## Decision

r64m is written in Rust from the start, from what g64m's 185 experiments found, and not
by carrying g64m's code across line by line. The Docker backend is not made in Goose; it
is one of the things r64m is to do.

## Consequences

- g64m stays as it is: the reference for what r64m is to do and how fast, and the place
  the tests and their expected output come from.
- What carries over is what does not depend on the language: the tests, the design the
  experiments arrived at, and the record of what did not work.
- Nothing of g64m is brought in because it was there (0007).
