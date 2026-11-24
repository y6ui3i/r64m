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

## R003: r64m on arm64 Linux as it is (2026-11-24)

The card, written before the run.

- **Hypothesis**: r64m, never built for anything but macOS, builds for arm64 Linux and
  does there what it does on the Mac: the 17 unit tests pass (among them what the host
  itself refuses, asked of a forked child), the 39 cases give the real CPU's traces and
  the 23 primitives print what they should. One thing is expected to differ and is what
  the run is for: the reservation is 2^47 bytes and the host's page is the guest's, so
  the test of a shared host page has nothing to do there.
- **What is changed from before**: the host. Until now everything ran on macOS 27 on the
  M1 (16 KB pages, 2^46 bytes). Now the same source built for
  `aarch64-unknown-linux-musl` (the target fetched by rustup, linked with the
  toolchain's own `rust-lld`), run in the container of R001 (Alpine 3.24, kernel 7.2
  under OrbStack, 4 KB pages).
- **Next move for each result**:
  - All the same → the JIT is begun on a memory known to behave on the host r64m is
    made for
  - A unit test of the memory fails → the host's side of `mem.rs` is wrong for Linux
    (the order of `mmap` and `mprotect`, what a fixed mapping over a reservation does):
    put right before anything is built on it
  - A trace or a primitive differs → something in the interpreter leans on the host
    (the library's functions in the x87, a float's conversion): found by the first line
    that differs
  - It does not build or link → the way of building for Linux is what has to be
    decided (a container with Rust in it is the other way, and a larger download)
- **Cut-off**: one build and one run of each of the three. The cases are the ones
  already assembled on the Mac (the container has no nasm, and none is fetched for
  this), so what is run on Linux is r64m, not the harness around it.

**Result: all the same. r64m does on arm64 Linux what it does on the Mac.**

| | macOS 27 on the M1 | arm64 Linux in the container (Alpine 3.24, kernel 7.2, OrbStack) |
|---|---|---|
| What `r64m` says it reserved | 2^46 bytes, host pages of 16 KB | 2^47 bytes, host pages of 4 KB |
| Unit tests (memory, loader) | 17 pass | 17 pass |
| Cases whose trace is the real CPU's | 39 of 39 | 39 of 39 (123,226 lines) |
| Primitives that print what they should | 23 of 23 | 23 of 23 |

- **The hypothesis** held, the expected difference included: 2^47 bytes, and the host's
  page is the guest's.
- **The host's side of `mem.rs` is right on Linux** as written for macOS: a fixed
  mapping over part of the reservation, `mprotect` to the page, and what the host
  itself refuses (asked of a forked child) are the same.
- **Nothing in the interpreter leans on the host**: the x87's use of the library's
  functions and the floats' conversions give the same 123,226 lines under musl as under
  macOS's library.
- **Building**: `rustup target add aarch64-unknown-linux-musl` (146 MB as installed)
  and `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld cargo build --release
  --target aarch64-unknown-linux-musl` on the Mac give a static executable; no C
  compiler and no container with Rust in it is needed.
- **What was not run on Linux**: the harness `tests/trace.rs` itself. The container has
  no nasm, so the cases were the ones the harness had assembled on the Mac, and the
  comparison there was a loop of `cmp`. The unit tests were the test executable built
  for Linux, run as it is.
- **The container sees only some of the Mac's folders** (OrbStack): not the checkout
  under `Documents`, and a scratch folder only when mounted at another path (`/w`). The
  builds and cases were copied to a scratch folder for the run.

**Next.** The first translated block, on Linux and macOS both from its first day.

## R004: how fast the simple interpreter is, before a JIT (2026-11-24)

The card, written before the run.

- **Hypothesis**: r64m's interpreter is the simple form (ADR 0007): every instruction
  decoded each time, every flag worked out at once, a search of the list of regions for
  every access to memory (ADR 0010). g64m's interpreter remembers what it decoded and
  runs the common instructions through a short front (its E110, E118). So r64m's is
  guessed to be two to five times slower than g64m's interpreter on the primitives, and
  some hundred times slower than g64m's JIT.
- **What is changed from before**: nothing in r64m; this is the first time it is timed.
  One thing is added to say what was run: `r64m --count` prints how many instructions
  the program took, which must be g64m's count for the same program.
- **Next move for each result**:
  - Within about three times g64m's interpreter → the interpreter stays as it is, and
    the JIT is designed with it as the slow path for what is not translated
  - Much slower than that (ten times or more) → where the time goes is looked at before
    the JIT (a profile of one primitive): a slow path that slow would show in every
    program's start, and the cheap things (a memory of the last region, of the last
    decoded instruction) are tried first
  - Faster than g64m's interpreter → said as found; nothing changes but what is
    expected of the JIT's gain
  - In every case the numbers are the base the JIT's gain is counted from
- **Cut-off**: the 23 primitives as kept built in g64m (`tests/prim64/*.elf`, one round
  each, 4 to 90 million instructions), each timed five times under r64m and under
  g64m's interpreter, the two running at the same time, the least taken, less the time
  of a program that does nothing. g64m's JIT, QEMU and Rosetta are not run again: their
  times for the same primitives at 200 rounds were taken earlier today (g64m's E186
  table) and are set beside as instructions per second. On the Mac only.
