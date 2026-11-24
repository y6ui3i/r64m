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

----

## R002: what each host lets a program do with memory (2026-11-24)

The next decision is how r64m holds the memory of a program without a kernel. g64m holds
it as an array and tests every address (is it inside, does the store's page hold
translated code): thirteen of 28 host instructions in one loop (E184). QEMU's user mode
and FEX-Emu let the host's own protection do that: the guest's memory is the host's at an
offset or at the same address, a wild address faults, and a page with translated code is
kept unwritable so that a store to it faults too. Whether r64m can do the same, and how
far, is a matter of what the two hosts allow, which is to be seen and not guessed:

1. the page's size (x86 programs expect 4 KB; a host with larger pages cannot protect a
   guest's page by itself);
2. how many bits a user address has, and how much can be reserved with no access at one
   stroke;
3. whether memory can be had at a fixed low address (an x86 program that is not
   position-independent is linked at 400000h);
4. whether a fault's handler is told the address and can go on at another place;
5. what a fault taken and gone on from costs.

- **Hypothesis**: **macOS on Apple silicon has 16 KB pages and 47 bits, refuses memory
  below 4 GB to a program built as usual, and reserves at least 2^40 bytes at one stroke;
  Docker's Linux has 4 KB pages and 48 bits, gives 400000h, and reserves 2^40 too; on
  both the handler is told the address and can go on, at a cost of some microseconds.**
- **What changed since last time**: nothing; first look.
- **Next move for each result**:
  - As guessed → the decision is written for Linux first (4 KB pages: the host's
    protection can stand for the guest's page by page), with macOS's larger pages a
    case it names
  - Linux has larger pages too, or will not reserve → the choices change: what is left
    is g64m's tests made cheaper
  - No going on from a fault → no host protection at all; the same
- **Cut-off**: one probe, in C (no crate fetched for it), run on the Mac and in the
  container of R001; it is a look, not part of r64m.

**Result: as guessed, with numbers. Linux lets the host's protection stand for a guest's
page; macOS does not, by its pages' size.**

| | macOS 27 on the M1 | arm64 Linux in the container (kernel 7.2, OrbStack) |
|---|---|---|
| Page | 16 KB | 4 KB |
| A hint is honoured up to | under 2^46 (47 bits of address) | under 2^48 (48 bits) |
| Largest reservation with no access | 2^46 bytes | 2^47 bytes |
| Memory at 10000h, 400000h, 10000000h | not given (the first 4 GB are kept empty) | given there |
| Memory at 100000000h | not given (the program itself is loaded there) | given there |
| `mprotect` of 4 KB inside a piece | refused | accepted |
| Memory both writable and runnable | refused (macOS wants `MAP_JIT`) | given |
| A fault: the address told, the handler goes on elsewhere | yes | yes |
| A fault taken and gone on from | 2.2 microseconds | 0.49 microseconds |

- **The hypothesis** held on every count.
- **2^47 bytes on Linux is all an x86 program can address**: an address of user space
  there has 47 bits. Reserved whole, no address the guest can form inside its own half
  is outside the reservation. The host has one bit more, which is where r64m itself can
  live.
- **macOS cannot protect 4 KB**: a guest's page that may not be written shares a host
  page with three that may. What the host can say there is coarser than what the guest
  asked for.
- **A fault is cheap enough to be the rare way**: half a microsecond is some 1,500 host
  instructions' time; g64m's way out of a block and back was 400 to 600. It is not cheap
  enough to be met often.
- The probe is not kept (C, in a scratch folder); what it printed is this table.

**Next.** The decision, as `docs/adr/0008`, proposed and not accepted: the owner's to
read.
