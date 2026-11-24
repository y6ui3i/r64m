# Experiment ledger

A card is written before an experiment runs, and its result is added in the same place.

The experiments of g64m, which this is the port of, are in its own ledger
(`docs/experiments.md` in https://github.com/y6ui3i/g64m, E001 to E185 when this
repository was started). The numbers here begin again at R001.

## R001: g64m on arm64 Linux, before anything of r64m is written (2026-11-24)

g64m has only ever run on macOS. r64m is to run on two hosts from the start (macOS and
Linux, both arm64), and g64m is what it is held to on both. So the first thing is
whether g64m itself runs on arm64 Linux, and what of macOS it had taken for granted.

(This card was written after the run, which is not the rule. What was held before it is
what the README said the day before: "what that turns up is what r64m must be built for
from the start".)

- **Hypothesis**: **g64m does not build or does not pass its tests on arm64 Linux as it
  is: the branch of its JIT's C for a host that is not Apple's has never been run, the
  registers it uses were chosen by macOS's rules, and Goose has no compiler for arm64
  Linux.**
- **What changed since last time**: nothing; it had not been tried.
- **Next move for each result**:
  - It fails → each failure is a thing r64m is designed for from the start
  - It passes → Linux is not a second design; the reference is at hand on both hosts,
    and the design begins with the guest's memory (ADR 0001's second point)
- **Cut-off**: g64m's own suites against the Linux build; one boot.

**Result: it builds as it is and passes everything that was run. The hypothesis was wrong
on all three counts.**

- **Goose has no compiler for arm64 Linux** (its releases are Linux x64, macOS arm64 and
  x64, Windows x64), and none was needed: `goose -o g64m.c src/g64m.goose --standalone`
  on the Mac writes one file of C with the runtime in it, and `gcc -O2` in an arm64
  Alpine container compiles it with g64m's six C files (`-lm -pthread`), with no error
  and no change to a line. (`tools/linux/Dockerfile`: Alpine 3.24, gcc 15.2, 14 packages,
  174 MB.)
- **g64m's suites, run from the Mac with `build/g64m` a script that runs the Linux build
  in the container** (the tests' programs assembled on the Mac as always): `difftest64`
  39 ok (each also under `--jit` and `--jit-check`), `prim64` 23 ok, the fourteen system
  tests ok plain, under `--jit` and under `--jit-check`.
- **Linux boots under it** to the shell's prompt: 1,516,032,565 guest instructions and
  30,952 blocks, the numbers the macOS build has to the last one. 1.46 to 1.61 s through
  `docker exec` (which is 0.04 s by itself), where the macOS build took 1.33 and 1.49 s
  in the same minute.
- **Not run**: the tests that need the disk images (the boot from a disk, NetHack built
  in the guest), the network, the frame buffer.

**What it means for r64m**: Linux on arm64 is not a second design. The JIT's code is the
same; the host's part is how memory that can be run is had and how the cache of
instructions is told (macOS: `MAP_JIT` and a switch between writing and running; Linux
here: one mapping that is both, and `__builtin___clear_cache`). Not shown by this: a
Linux that refuses a mapping both writable and runnable, and any core but the M1's
(Docker's Linux runs on the same CPU).

**Next.** The guest's memory: how a program without a kernel has it from the host, with
the host's protection for what lies outside it; written as a decision before any code.
