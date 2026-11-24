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

**Result: 1.67 times slower than g64m's interpreter, which is inside the guess and
inside "leave it as it is". And one thing not looked for: on macOS r64m takes 0.17
seconds to start and end, all of it the reservation.**

On a MacBook Air (Apple M1), macOS 27. Each primitive one round, five runs of r64m and of
g64m's interpreter at the same time, the least of each, less a program that does
nothing (r64m 0.163 s, g64m 0.075 s). The count of instructions is the same from both
for every program.

| | guest instructions | r64m | | g64m, interpreter | | r64m / g64m |
|---|---|---|---|---|---|---|
| p_bignum | 44.7 M | 1.311 s | 34.1 M/s | 0.797 s | 56.1 M/s | 1.65 |
| p_bitfield | 67.6 M | 2.000 s | 33.8 M/s | 1.146 s | 59.0 M/s | 1.74 |
| p_bytes | 34.6 M | 0.876 s | 39.5 M/s | 0.532 s | 65.0 M/s | 1.65 |
| p_dbl_arith | 21.6 M | 0.708 s | 30.5 M/s | 0.440 s | 49.0 M/s | 1.61 |
| p_dbl_cmp | 37.5 M | 1.020 s | 36.8 M/s | 0.748 s | 50.1 M/s | 1.36 |
| p_dbl_conv | 33.8 M | 1.052 s | 32.2 M/s | 0.663 s | 51.0 M/s | 1.59 |
| p_fill | 7.8 M | 0.240 s | 32.5 M/s | 0.016 s | 489.9 M/s | 15.08 |
| p_float | 23.1 M | 0.733 s | 31.5 M/s | 0.450 s | 51.3 M/s | 1.63 |
| p_funcptr | 20.9 M | 0.576 s | 36.4 M/s | 0.315 s | 66.5 M/s | 1.83 |
| p_if | 23.0 M | 0.586 s | 39.3 M/s | 0.371 s | 62.1 M/s | 1.58 |
| p_index | 29.9 M | 0.916 s | 32.7 M/s | 0.495 s | 60.4 M/s | 1.85 |
| p_ldbl | 13.9 M | 0.590 s | 23.5 M/s | 0.295 s | 47.1 M/s | 2.00 |
| p_loop | 15.0 M | 0.398 s | 37.8 M/s | 0.230 s | 65.3 M/s | 1.73 |
| p_muldiv | 30.8 M | 0.845 s | 36.4 M/s | 0.470 s | 65.4 M/s | 1.80 |
| p_pixel | 26.5 M | 0.779 s | 34.0 M/s | 0.482 s | 54.9 M/s | 1.62 |
| p_pointer | 33.0 M | 0.889 s | 37.1 M/s | 0.525 s | 62.8 M/s | 1.69 |
| p_recursion | 12.2 M | 0.326 s | 37.3 M/s | 0.191 s | 63.6 M/s | 1.71 |
| p_rotate | 89.5 M | 2.326 s | 38.5 M/s | 1.437 s | 62.3 M/s | 1.62 |
| p_sha | 16.8 M | 0.514 s | 32.7 M/s | 0.287 s | 58.6 M/s | 1.79 |
| p_shift | 73.8 M | 2.167 s | 34.0 M/s | 1.283 s | 57.5 M/s | 1.69 |
| p_simd | 16.6 M | 0.608 s | 27.3 M/s | 0.492 s | 33.7 M/s | 1.24 |
| p_switch | 25.8 M | 0.692 s | 37.3 M/s | 0.438 s | 58.8 M/s | 1.58 |
| p_ternary | 39.4 M | 1.077 s | 36.6 M/s | 0.620 s | 63.5 M/s | 1.74 |
| **all** | 737.8 M | 21.229 s | **34.8 M/s** | 12.725 s | **58.0 M/s** | **1.67** |

Beside them, the same 23 primitives at 200 rounds as timed earlier today (g64m's E186
table, 147,461 M instructions in all), as instructions a second:

| | all 23 | against r64m's interpreter |
|---|---|---|
| r64m, interpreter | 34.8 M/s | 1 |
| g64m, interpreter | 58.0 M/s | 1.7 |
| QEMU (TCG, user mode) | 1,267 M/s | 36 |
| Rosetta | 1,808 M/s | 52 |
| g64m with its JIT | 2,562 M/s | 74 |

- **The hypothesis** held at its low end: 1.67 where two to five was guessed, and 74
  times to g64m's JIT where "some hundred" was. The simple form costs less than
  thought.
- **One primitive is apart: `p_fill`, 15 times.** It is `rep stos`, which g64m does as
  many repetitions in one call as it may (its E113) and r64m one to a step. Not a
  matter of the interpreter's form, and to be had back the same way when it is wanted.
- **So the interpreter stays as it is** (the card's first case), and the JIT is
  designed with it as the slow path. 34.8 M/s is the base the JIT's gain is counted
  from; twice QEMU is 73 times that.

What was not looked for:

- **On macOS the reservation is not free.** `r64m` with nothing to run takes 0.17 s,
  none of it its own: all in the kernel. A probe of `mmap` and `munmap` with no access,
  by size: 2^40 bytes 1.1 ms and 1.3 ms, 2^44 17 ms and 26 ms, 2^46 65 ms and 104 ms.
  The time goes with the size. On Linux the same `r64m` starts and ends in under 10 ms
  with 2^47 bytes. It is why the 17 unit tests take 4.5 s on the Mac and 0.01 s on
  Linux: each makes a reservation.
- It touches ADR 0008's first point ("as large as the host gives") for macOS only,
  where r64m is developed and not what it is made for: a program run there pays 0.17 s
  once, and a program that makes many processes would pay it for each. Not changed
  here: the owner's to say whether macOS reserves less (2^40 bytes is a thousand
  gigabytes and costs 2 ms).
- One number seen and not explained: `p_loop` took 0.59 s in the Linux container where
  it takes 0.40 s on the Mac itself, the same source and the same CPU. One run, not a
  measurement.

The timing was a script of its own in a scratch folder, not kept (ADR 0012: what is
kept is Rust; a `cargo xtask` for the races is owed when they are run again).

**Next.** The JIT's design, as a decision (ADR 0013).

## R005: the interpreter past g64m's (2026-11-24)

The owner's aim: r64m's interpreter faster than g64m's (58.0 M instructions a second
over the 23 primitives, R004; r64m's is 34.8). The JIT's design (ADR 0014) waits.

The card, written before the run.

- **Hypothesis**: the time is not in what the instructions do but in what is done
  again for every one of them, and three things are guessed, in this order of size:
  1. the fetch: fifteen bytes copied out for each instruction after a search of the
     list of regions (and the search again for every access to memory, ADR 0010);
  2. the decoding itself, done afresh each time (prefixes, ModRM, the constants read a
     byte at a time, each read with its own test);
  3. what is set up and passed round for each step (the `Run` built and filled, results
     handed back through several layers).
  A profile of two primitives (`p_loop`: registers and jumps; `p_index`: memory) says
  whether that is so before anything is changed.
- **What is changed from before**: R004 timed the interpreter as first written. Here it
  is changed, one thing at a time, each kept only if the 39 traces still hold and the
  race with g64m's interpreter (both at once, as in R004) shows a gain.
- **Next move for each result**:
  - The profile agrees → in the order of what it shows as largest: a memory of the last
    region found (one for code, one for data); then the fetch reading the guest's bytes
    where they lie instead of copying fifteen; then, if still behind, a memory of
    decoded instructions as g64m has (its entry by address, dropped when the page is
    written)
  - The profile shows something else largest → that first, and said
  - Past 58 M/s without a memory of decoded instructions → stop there: the simple form
    is kept (ADR 0007) and the aim is met
  - Not past it with all three → said as found, with what the profile then shows, and
    the owner's to say whether the interpreter is worth more or the JIT is next
- **Cut-off**: the three changes above and no more in this entry. `p_fill`'s `rep stos`
  (15 times slower than g64m's, R004) is counted in the total but is not what this is
  about: if it alone keeps the total behind, that is said and it gets its own change.

**Result: past g64m's interpreter by a fifth, without a memory of decoded instructions.
The simple form is kept.**

On a MacBook Air (Apple M1), macOS 27; the 23 primitives, 737.8 M instructions.

The profile first (`sample`, `p_rotate` and `p_index`): over half of all samples were in
`step` itself, which is the fetch (the search of the list and the copy of fifteen bytes)
and the setting up; a quarter in `exec`. So the first guess was the largest, as guessed.

Three changes, each raced against the build before it and against g64m's interpreter,
all three running at once (three runs, the least of each):

| | before | after | g64m beside them |
|---|---|---|---|
| 1. The last region found is remembered, one for each kind of access (`Mem::check`) | 34.2 M/s | 52.2 M/s | 56.3 M/s |
| 2. An instruction's bytes are read where they lie, not copied (`Mem::code_at`) | 46.6 M/s | 48.1 M/s | 49.3 M/s |
| 3. The small helpers put in line (`#[inline(always)]`), `check`'s search apart and cold | 48.7 M/s | 60.3 M/s | 50.4 M/s |

(The rows are not to be read down: the machine ran warmer from one race to the next, as
g64m's own column shows. Each row is right across.)

- The second profile, after the first change, put `exec` on top and showed the helpers
  (`modrm`, `alu`, `sr`, `rm_get`, `check`) as functions of their own, each called for
  every instruction. That was not one of the card's three; the card says to take what
  the profile shows largest and say so. It took the place of the memory of decoded
  instructions, which was then not needed.

Then r64m as it now is against g64m's interpreter, the two at once, five runs of each
program, the least, less a program that does nothing; three such races:

| race | r64m | g64m, interpreter | g64m's time / r64m's |
|---|---|---|---|
| 1 | 61.6 M/s | 50.8 M/s | 1.21 |
| 2 | 68.1 M/s | 56.1 M/s | 1.21 |
| 3 | **67.4 M/s** | **55.7 M/s** | **1.21** |

The middle one (the third), by program:

| | r64m | g64m, interpreter | g64m's time / r64m's |
|---|---|---|---|
| p_bignum | 0.625 s | 0.817 s | 1.31 |
| p_bitfield | 0.976 s | 1.201 s | 1.23 |
| p_bytes | 0.421 s | 0.547 s | 1.30 |
| p_dbl_arith | 0.399 s | 0.460 s | 1.15 |
| p_dbl_cmp | 0.627 s | 0.777 s | 1.24 |
| p_dbl_conv | 0.626 s | 0.706 s | 1.13 |
| p_fill | 0.120 s | 0.019 s | 0.16 |
| p_float | 0.442 s | 0.476 s | 1.08 |
| p_funcptr | 0.300 s | 0.331 s | 1.10 |
| p_if | 0.277 s | 0.384 s | 1.39 |
| p_index | 0.417 s | 0.507 s | 1.22 |
| p_ldbl | 0.415 s | 0.310 s | 0.75 |
| p_loop | 0.178 s | 0.235 s | 1.32 |
| p_muldiv | 0.428 s | 0.496 s | 1.16 |
| p_pixel | 0.370 s | 0.500 s | 1.35 |
| p_pointer | 0.438 s | 0.561 s | 1.28 |
| p_recursion | 0.152 s | 0.210 s | 1.38 |
| p_rotate | 1.130 s | 1.479 s | 1.31 |
| p_sha | 0.235 s | 0.301 s | 1.28 |
| p_shift | 1.098 s | 1.331 s | 1.21 |
| p_simd | 0.385 s | 0.505 s | 1.31 |
| p_switch | 0.351 s | 0.450 s | 1.28 |
| p_ternary | 0.527 s | 0.654 s | 1.24 |

- **The aim is met**: 1.21 times g64m's interpreter in each of three races, 67.4 M
  instructions a second where it was 34.8 (R004): 1.94 times itself.
- **Two programs are still behind.** `p_fill` (0.16): `rep stos` one repetition to a
  step, as said in R004 and left alone here. `p_ldbl` (0.75): the x87, where g64m keeps
  an 80-bit value's f64 until it is first read (its E162) and r64m makes it each time.
  Each is one known thing, neither is the interpreter's form.
- **Nothing was given up for it**: the 17 unit tests, the 39 traces and the 23
  primitives hold on macOS and, built again for Linux, in the container.
- **How the races vary**: the first race of the three final ones was a tenth slower for
  both than the next two (the machine, without a fan, had just run the earlier races).
  The ratio did not move. A ratio from two programs run at once is what is to be
  trusted here, not an instructions-a-second figure from one run.
- The profiles and the race were `sample` and a script in a scratch folder, not kept.

**Next.** The JIT's design (ADR 0014) is still the owner's to answer. Beside it, as
things that would each be a change of their own: `rep stos` and `rep movs` in one
step, and the x87's f64 kept.

## R006: the two primitives still behind g64m's interpreter (2026-11-24)

The owner's aim: `p_fill` and `p_ldbl` past g64m's interpreter too (R005 left them at
0.16 and 0.75 of its speed).

The card, written before the run.

- **Hypothesis**:
  - `p_fill`: nearly all of its 7.8 M instructions are repetitions of `rep stos`, each
    a whole step here (fetched and decoded again). Done as g64m does (its E113), all
    the repetitions of a `rep stos` or `rep movs` in one step when no trace is being
    written, it is a fill or a copy of memory and takes about no time.
  - `p_ldbl`: guessed, not known: the register forms of the x87 work out ST(0) and
    ST(1) as f64 before they look at which instruction it is (g64m did the same until
    its E162), and the division is a 128-bit division by the compiler's library
    routine where g64m does two of 64 bits. A profile says which before anything is
    changed.
- **What is changed from before**: R005 changed what every instruction pays. This
  changes two instructions' own work, and for the first time lets a step be more than
  one instruction: the count (`--count`) must still be g64m's for every primitive, a
  repetition counting as one, and with a trace every repetition is still a step and a
  line.
- **Next move for each result**:
  - Both past g64m → all 23 are, and the interpreter is left; the JIT's design is next
  - `p_fill` not past it with the whole run in one step → what is left is looked at
    (the check of the range, the copy itself) and said
  - `p_ldbl`'s profile shows something else → that, and said; if it is the x87's
    arithmetic in integers itself (which is what makes it right), it stays behind and
    that is the answer
- **Cut-off**: the string change, and for the x87 what the profile shows first and
  second, no more. Raced as in R005: against g64m's interpreter, two at once, five
  runs of each program, three races, the middle one.

**Result: `p_fill` is past g64m's interpreter; `p_ldbl` is nearer and not past (0.90
of its speed, from 0.75). The cut-off is reached; the x87 goes on under a new card.**

The profile of `p_ldbl` first (`sample`), by where the samples ended: `exec` 806, the
x87's `escape` 451, the hand-over of the x87's operand (`Run::x87`, with the `memmove`
and `memcpy` it called, 134 more) 400, **the search of the list of regions 368**,
`step` 233, the f64 of an 80-bit value (`now`) 212, `soft_add` 158, `arith_to` 142,
`soft_div` with the library's 128-bit division 186.

- The search was not guessed. R005 remembered one region for each kind of access;
  this program reads its stack and its data in turn, and the one was wrong every
  time.
- The guess (ST(0) and ST(1) made into f64 before the instruction is known) is there,
  as `now`, but fourth.

What was changed, by the card: the string instructions, and for the x87 the first and
second of the profile.

1. `rep movs` and `rep stos` going up, with 64-bit addresses, do all their repetitions
   in one step when no trace is written (`Cpu::rep_whole`): a fill (`Mem::fill`) or a
   forward copy (`Mem::copy_up`, refused where the copy would read what it wrote: then,
   and when any of it would fault, a repetition to a step as before). The count goes
   on by one for each repetition.
2. Two regions remembered for each kind of access, not one.
3. The x87's operand handed over by its size (one or two reads or writes of the
   memory) and not as a copy of a length known only when run.

Raced as the card says (r64m and g64m's interpreter at once, five runs of each
program, three races):

| race | r64m | g64m, interpreter | all 23 | `p_fill` | `p_ldbl` |
|---|---|---|---|---|---|
| 1 | 73.7 M/s | 58.6 M/s | 1.26 | 0.013 s against 0.014 s | 0.328 s against 0.290 s (0.88) |
| 2 | **72.0 M/s** | **57.0 M/s** | **1.26** | 0.013 s against 0.018 s | 0.331 s against 0.300 s (0.91) |
| 3 | 70.9 M/s | 56.0 M/s | 1.27 | 0.013 s against 0.022 s | 0.335 s against 0.307 s (0.92) |

- **`p_fill`**: 0.120 s before, 0.013 s now. It is past g64m's, though at a hundredth
  of a second for either the two cannot be told far apart: what can be said is that
  it is no longer behind.
- **`p_ldbl`**: 0.415 s before, 0.33 s now; g64m's 0.30 s. In every other program
  r64m is ahead; this is the one left.
- **All 23 together**: 1.26 times g64m's interpreter (1.21 in R005). The second
  remembered region helped more than the x87: it is every program's.
- **What holds**: the count of instructions and the output are g64m's for all 23; 23
  unit tests (six new: the fill, the forward copy, the remembered regions outliving no
  change, a `rep` in one step against one at a time in registers, flags, memory and
  count, and one that faults part of the way), the 39 traces.
- A mistake in the new tests, found by the tests: the first form ended its programs
  with SYSCALL, which writes RCX, and then compared RCX. They end with UD2 now.

**Next.** R007: the x87's own work, which the profile showed third and after.

## R007: `p_ldbl` past g64m's interpreter: the x87's own work (2026-11-24)

The card, written before the run.

- **Hypothesis**: with the search gone, the profile of `p_ldbl` (taken after R006) has
  three things of the x87's own that are work not asked for by the instruction:
  1. `now` (279 samples of some 3,300): every register form makes ST(0) and ST(1)
     into f64 before it looks at which instruction it is, and only the functions of
     the library and FXAM use them;
  2. the hand-over (`Run::x87`, 424): 112 bytes are cleared for every instruction,
     for an operand that is ten bytes at most unless it is an environment;
  3. the division (`soft_div` and the library's 128-bit division under it, 220): g64m
     does it as two divisions of 64 bits (its E161).
  Together they are over a quarter of the samples; r64m is a tenth behind.
- **What is changed from before**: R006 changed what lay round the x87 (the search, the
  copy of the operand). This changes the x87's code itself, in those three places.
- **Next move for each result**:
  - Past g64m on `p_ldbl` in the middle race of three → all 23 are past; the
    interpreter is left and the JIT's design (ADR 0014) is next
  - Still behind → said as found with the profile then; what is left is the
    arithmetic in integers that makes the x87 right, and the owner's to say whether
    one primitive at nine tenths is worth more
- **Cut-off**: these three and no more. The traces of `f_x87`, `j_x87`, `c_ld`,
  `a_prefix` and `x_fxsave` are what say the x87 is still right; the race is R006's.

**Result: `p_ldbl` is past g64m's interpreter, by a few hundredths; all 23 primitives
now are, in each of three races.**

The three changes, as the card has them: ST(0) and ST(1) are made into f64 only by the
instructions that go through f64; the operand's bytes are sixteen that the CPU clears,
112 only for an environment or a whole unit saved; the division is two divisions of 64
bits (`div128`, with a test of it against the 128-bit division on 601,728 dividends and
divisors, the awkward ones among them).

`p_ldbl` alone first, the build of R006, the new one and g64m's interpreter at once,
seven runs of each, three times:

| | R006's build | new | g64m, interpreter | g64m's time / new |
|---|---|---|---|---|
| 1 | 0.338 s | 0.292 s | 0.295 s | 1.01 |
| 2 | 0.338 s | 0.292 s | 0.303 s | 1.04 |
| 3 | 0.337 s | 0.292 s | 0.301 s | 1.03 |

Then all 23 as in R006 (two at once, five runs of each program), three races:

| race | r64m | g64m, interpreter | all 23 | the three nearest | behind |
|---|---|---|---|---|---|
| 1 | 73.3 M/s | 57.5 M/s | 1.27 | p_ldbl 1.03, p_float 1.15, p_dbl_arith 1.15 | none |
| 2 | 71.1 M/s | 56.0 M/s | 1.27 | p_ldbl 1.06, p_float 1.15, p_dbl_conv 1.15 | none |
| 3 | **71.6 M/s** | **56.3 M/s** | **1.27** | p_ldbl 1.04, p_dbl_conv 1.16, p_float 1.16 | none |

The middle one (the third), by program:

| | r64m | g64m, interpreter | g64m's time / r64m's |
|---|---|---|---|
| p_bignum | 0.601 s | 0.824 s | 1.37 |
| p_bitfield | 0.961 s | 1.191 s | 1.24 |
| p_bytes | 0.396 s | 0.532 s | 1.34 |
| p_dbl_arith | 0.391 s | 0.460 s | 1.18 |
| p_dbl_cmp | 0.624 s | 0.753 s | 1.21 |
| p_dbl_conv | 0.579 s | 0.672 s | 1.16 |
| p_fill | 0.013 s | 0.017 s | 1.31 |
| p_float | 0.414 s | 0.482 s | 1.16 |
| p_funcptr | 0.277 s | 0.325 s | 1.17 |
| p_if | 0.272 s | 0.381 s | 1.40 |
| p_index | 0.400 s | 0.504 s | 1.26 |
| p_ldbl | 0.291 s | 0.304 s | 1.04 |
| p_loop | 0.168 s | 0.232 s | 1.38 |
| p_muldiv | 0.408 s | 0.490 s | 1.20 |
| p_pixel | 0.360 s | 0.491 s | 1.36 |
| p_pointer | 0.411 s | 0.548 s | 1.33 |
| p_recursion | 0.145 s | 0.199 s | 1.37 |
| p_rotate | 1.098 s | 1.475 s | 1.34 |
| p_sha | 0.222 s | 0.294 s | 1.32 |
| p_shift | 1.046 s | 1.310 s | 1.25 |
| p_simd | 0.387 s | 0.506 s | 1.31 |
| p_switch | 0.335 s | 0.458 s | 1.37 |
| p_ternary | 0.505 s | 0.650 s | 1.29 |

- **The aim is met for every primitive**: none is behind g64m's interpreter in any of
  the three races. `p_ldbl` is ahead by 3 to 6 hundredths, which is little: it is
  right to say it is level or just past, not that it is faster.
- **All 23 together**: 71.6 M instructions a second, 1.27 times g64m's interpreter;
  34.8 M/s when first timed (R004).
- **What holds**: 24 unit tests, the 39 traces (the x87's five among them) on macOS;
  built again for Linux, 24 unit tests, 39 traces and 23 primitives in the container.
- The interpreter is still the simple form: nothing decoded is remembered.

**Next.** The JIT's design (ADR 0014), which is where QEMU's 1,267 M/s is to be met:
that figure is a JIT's, and an interpreter is not expected to come near it.

## R008: the first blocks, timed (2026-11-24)

The card, written before the run.

- **Hypothesis**: the first translator covers 35% of the primitives' instructions, and
  four primitives nearly wholly (`p_loop`, `p_if`, `p_pointer`, `p_recursion`: over
  99.5% in blocks). Every block still comes out to Rust and is found again by a look
  in a hash table before the next goes in, and every instruction that sets the flags
  writes its record (two or three host instructions more). So those four are guessed
  to run at some hundreds of millions of instructions a second: several times the
  interpreter's 72, and well under QEMU's 1,267. The primitives that are mostly not
  translated yet are guessed to be no faster than interpreted, or a little slower (the
  count of arrivals, the looking up).
- **What is changed from before**: R007 timed the interpreter alone. Here the same
  programs run with `--jit`.
- **Next move for each result**:
  - The four are at hundreds of millions and held back by the coming out and going in
    (a profile says: time in `run`, the hash table, `enter`) → blocks joined to one
    another is the next thing built (ADR 0014's first "not yet")
  - The four are held back by something else (the record of the flags, the code
    itself) → that first, and said
  - A primitive is slower with `--jit` than without by more than a tenth → why is
    found before going on: a JIT that loses to its own interpreter anywhere is not
    switched on by default
  - Whatever the four do, what the other 19 are not translated for is counted next
    (which instructions end their blocks), since 65% of the instructions are still the
    interpreter's
- **Cut-off**: one race: each primitive at one round, interpreted and with `--jit`,
  the two at once, five runs, the least, less a program that does nothing; and one
  profile of `p_loop` with `--jit`. On the Mac only. No change to the code in this
  entry.

**Result: as guessed for the four that are wholly in blocks (1.8 to 4.3 times the
interpreter, 160 to 366 M instructions a second), and the reason is the one guessed:
nineteen twentieths of their time is spent between blocks, not in them. Two primitives
are slower with the JIT than without.**

On a MacBook Air (Apple M1), macOS 27. Each primitive one round, interpreted and with
`--jit` at once, five runs, the least, less a program that does nothing.

| | in blocks | interpreted | with `--jit` | gain | with `--jit` |
|---|---|---|---|---|---|
| p_bignum | 26.5% | 0.568 s | 0.519 s | 1.09 | 86 M/s |
| p_bitfield | 9.1% | 0.888 s | 0.863 s | 1.03 | 78 M/s |
| p_bytes | 10.7% | 0.384 s | 0.433 s | 0.89 | 80 M/s |
| p_dbl_arith | 0.0% | 0.370 s | 0.384 s | 0.96 | 56 M/s |
| p_dbl_cmp | 11.5% | 0.640 s | 0.642 s | 1.00 | 58 M/s |
| p_dbl_conv | 0.0% | 0.575 s | 0.564 s | 1.02 | 60 M/s |
| p_fill | 3.1% | 0.012 s | 0.011 s | 1.09 | 709 M/s |
| p_float | 0.0% | 0.386 s | 0.404 s | 0.96 | 57 M/s |
| p_funcptr | 79.3% | 0.264 s | 0.238 s | 1.11 | 88 M/s |
| p_if | 99.7% | 0.263 s | 0.144 s | 1.83 | 160 M/s |
| p_index | 75.6% | 0.383 s | 0.155 s | 2.47 | 193 M/s |
| p_ldbl | 0.0% | 0.280 s | 0.288 s | 0.97 | 48 M/s |
| p_loop | 99.8% | 0.161 s | 0.046 s | 3.50 | 326 M/s |
| p_muldiv | 23.3% | 0.383 s | 0.330 s | 1.16 | 93 M/s |
| p_pixel | 15.1% | 0.348 s | 0.323 s | 1.08 | 82 M/s |
| p_pointer | 99.6% | 0.385 s | 0.090 s | 4.28 | 367 M/s |
| p_recursion | 100.0% | 0.137 s | 0.043 s | 3.19 | 284 M/s |
| p_rotate | 78.8% | 1.018 s | 0.512 s | 1.99 | 175 M/s |
| p_sha | 0.1% | 0.222 s | 0.228 s | 0.97 | 74 M/s |
| p_shift | 16.7% | 1.027 s | 0.930 s | 1.10 | 79 M/s |
| p_simd | 0.8% | 0.383 s | 0.384 s | 1.00 | 43 M/s |
| p_switch | 35.7% | 0.326 s | 0.376 s | 0.87 | 69 M/s |
| p_ternary | 16.7% | 0.478 s | 0.450 s | 1.06 | 88 M/s |
| **all** | 35.0% | 9.88 s | 8.36 s | **1.18** | 88 M/s |

- **The four** (`p_loop` 3.5, `p_if` 1.8, `p_pointer` 4.3, `p_recursion` 3.2 times):
  hundreds of millions a second, as guessed, and a quarter of QEMU's 1,267 at best.
- **Where their time goes** (`sample` of `p_loop` with `--jit`, 200 rounds): of some
  2,450 samples, 1,416 in `Jit::run` (the look in the hash table for the next block),
  500 in `settle` (the flags worked out from the record after every block, whether
  anything reads them or not), 387 in `enter` (the sixteen registers loaded and
  stored), and about 120 in generated code itself. A twentieth of the time is the
  program; the rest is going from one block to the next. `p_loop`'s 15 million
  instructions are 3 million entries into blocks: five instructions to an entry.
- **The two that lose**: `p_bytes` (0.384 s to 0.433 s, 13% slower) and `p_switch`
  (0.326 s to 0.376 s, 15%). `p_switch` has 36% of its instructions in blocks but
  enters a block 5.6 million times for 9.2 million instructions: 1.6 instructions to
  an entry. An entry costs more than interpreting one or two instructions. So it is
  the same thing as holds back the four, seen from the other side: blocks too short
  for what it costs to go in and out. (By the card this is found before going on: it
  is found, and it is not a second fault but the first one again.)
- **So the JIT is not what `r64m` does unless asked** (`--jit`), until no primitive
  loses by it.

**Next.** By the card's first case: blocks joined to one another, so that a block's end
goes to the next block and not out to Rust (ADR 0014's first "not yet", now counted:
95% of `p_loop`'s time). With it, the flags settled only when something outside
generated code is about to read them. Then the count of which instructions end the
blocks of the other nineteen.

## R009: blocks joined to one another (2026-11-24)

The card, written before the run.

- **Hypothesis**: 95% of `p_loop`'s time with `--jit` is between blocks (R008). If a
  block's end whose target is a constant (a jump, either side of a conditional jump,
  a call, the instruction a block stops before) goes to the target's block by a jump
  written into the code, and the flags are worked out from their record only when the
  interpreter is about to run, then a loop made of such blocks stays in generated code.
  The four primitives that are wholly in blocks are guessed to come to over a
  thousand million instructions a second (their generated code alone ran at several
  times that in R008's profile), `p_recursion` less than the others (a return's target
  is not a constant: it still comes out to Rust and is looked up). The two that lost
  (`p_bytes`, `p_switch`) are guessed to stop losing where their short blocks follow
  one another, and not where each is followed by an instruction of the interpreter's.
- **What is changed from before**: three things in the code, all part of joining:
  a jump that can be rewritten at each such end (first to its own way out, then to the
  target's block once there is one); the count of instructions kept in a host register
  while generated code runs, since Rust no longer sees each block end; the flags
  settled before the interpreter runs and not after every block. With `--jit-check`
  nothing is joined: the check is of one block at a time.
- **Next move for each result**:
  - The four past a thousand million → what the other nineteen are not translated for
    is counted, and instructions are added in the order of that count
  - The four well short of it → a profile says where the time is now (the record of
    the flags written at every instruction is the first suspect), and that is next
  - `p_bytes` or `p_switch` still slower than interpreted by more than a tenth → the
    cost of one entry and exit is taken apart (the look-up, the sixteen loads and
    stores) before more is translated
  - A joined run that does not give the interpreter's output, exit code or count →
    put right before anything is timed
- **Cut-off**: the race of R008 again (interpreted against `--jit`, five runs, both at
  once), and one profile of the slowest of the four. What a return and a jump through
  a register cost is seen and not changed here.

**Result: where blocks stay made, joining does what was guessed (`p_loop` 5,029 M
instructions a second, 1.29 times QEMU). But on macOS most primitives lose their blocks
after eight rounds, for a reason that is the host's page and not the joining: found
here, and the next entry's to put right.**

On a MacBook Air (Apple M1), macOS 27.

One round each, interpreted, with the JIT as it was (R008's build) and joined, the
three at once, five runs: the four primitives that are wholly in blocks became too
short to time (`p_loop` 0.177 s interpreted, 0.047 s before, 0.002 s joined; `p_if`
0.281, 0.155, 0.010; `p_pointer` 0.409, 0.097, 0.010; `p_recursion` 0.147, 0.041,
0.010). So they were timed at 200 rounds, beside g64m's JIT (two at once, three runs,
less a program that does nothing), with QEMU's and Rosetta's times for the same builds
from earlier today (g64m's E186 table) set beside:

| 200 rounds | guest instructions | r64m `--jit` | | g64m `--jit` | QEMU | Rosetta | in blocks |
|---|---|---|---|---|---|---|---|
| p_loop | 3,005 M | 0.598 s | 5,029 M/s | 0.232 s | 0.773 s | 0.240 s | 99.8% |
| p_recursion | 2,434 M | 2.030 s | 1,199 M/s | 0.648 s | 1.567 s | 0.810 s | 100.0% |
| p_if | 4,602 M | 62.9 s | 73 M/s | 1.694 s | 1.873 s | 0.997 s | **0.5%** |
| p_pointer | 6,595 M | 98.8 s | 67 M/s | 1.816 s | 1.900 s | 0.723 s | **0.5%** |
| p_index | 5,986 M | 86.3 s | 69 M/s | 1.217 s | 1.723 s | 0.420 s | **0.4%** |
| p_rotate | 17,822 M | 242.9 s | 73 M/s | 3.359 s | 4.403 s | 3.060 s | **0.4%** |
| p_funcptr | 4,191 M | 66.9 s | 63 M/s | 3.463 s | 9.123 s | 2.867 s | **0.4%** |

- **`p_loop`**: 5,029 M/s where it was some 330 before joining: 1.29 times QEMU, and
  2.6 times slower than g64m's JIT beside it. Past a thousand million, as guessed.
- **`p_recursion`**: 1,199 M/s, 0.77 of QEMU: less than the others, as guessed, and
  for the reason guessed. It went into generated code 144 million times for 2,434
  million instructions: every return comes out to Rust to be looked up.
- **The five others at 200 rounds are interpreted**: 0.4 to 0.5% of their
  instructions in blocks, where one round has 75 to 99.7%. The JIT gave their code
  up. Why: these programs' code is at 401000h and their data at 403000h, which on
  macOS are one host page of 16 KB. Translated code makes that host page unwritable;
  each round the program writes the line it prints into its data, which is a store to
  that page; the page's blocks are forgotten for it, and after eight times the page is
  not translated from again (`GIVEN_UP`, meant for a page whose code is really written
  over). One round is one forgetting and nothing shows: R008's race and this one's
  could not see it. `p_loop` and `p_recursion` write nothing but their stack.
- **On Linux it does not happen**: the same builds at 200 rounds in the container run
  `p_if` with 99.7% of 4,602 M instructions in blocks and no page forgotten, `p_pointer`
  99.6% of 6,595 M. (Not timed there.) It is ADR 0008's point B, met for the first
  time: on a host with 16 KB pages a page of data beside a page of code is caught by
  the code's protection.
- **The two that lost in R008** at one round: `p_switch` 0.395 s interpreted, 0.451 s
  before, 0.408 s joined (3% slower than interpreted, from 14%); `p_bytes` 0.514 s,
  0.566 s, 0.600 s: still 17% slower. By the card that is looked at before more is
  translated; it is the same host page for all that can be told yet, and is looked at
  again when that is put right.
- All 23 at one round: interpreted 61.5 M/s, joined 73.8 M/s (three programs running
  at once, and warm: the figures are for comparing across, not with other entries).

**Next.** R010: a store to data that shares a host page with translated code is made
without forgetting the code, when the guest's own page that is written holds none.
Then this entry's race again at 200 rounds, which is the length that shows it.

## R010: data beside code in one host page (2026-11-24)

The card, written before the run.

- **Hypothesis**: on macOS a store to data that lies in the same 16 KB host page as
  translated code forgets that page's blocks, and after eight such stores the page is
  given up (R009). The store is not to code: the guest's own 4 KB page that is written
  holds no translated instruction. If such a store is made by the interpreter with the
  host page opened for that one instruction and closed again, and nothing is
  forgotten, the blocks stay; a store from generated code to that page still faults
  and leaves its block each time (the host cannot tell the two guest pages apart), so
  it costs a fault and an interpreted instruction, some microseconds, where it cost
  the blocks. The five primitives that fell to 0.5% in blocks at 200 rounds are
  guessed to keep what they have at one round (75 to 99.7%), and those that write
  their data once a round to run near `p_loop`'s speed; one that writes data beside
  its code inside its loop would be held back by the faults, and which ones do is not
  known.
- **What is changed from before**: in R009 any store to a marked host page forgot its
  blocks. Now only a store that touches a guest page with translated instructions in
  it does. The runner keeps, for each guest page, how many blocks have instructions
  there.
- **Next move for each result**:
  - The five keep their blocks and run in seconds, not minutes → the race with QEMU
    for all 23 is the next entry, with what is not translated counted
  - They keep their blocks but one is slow by the faults (thousands of stores a
    second through the handler) → an instruction that faults often is left to the
    interpreter without the fault: its block is made again to end before it
  - Blocks are still forgotten → what writes to the code's own guest page is looked at
    (a program that keeps data in its text)
- **Cut-off**: R009's race at 200 rounds again for the seven, and the counts
  (`--jit-stats`) for all 23 at 200 rounds. macOS, which is where it happens; Linux is
  run for the tests only.

**Result: the blocks stay. Three of the five run in under two seconds where they took
one to four minutes, and are level with QEMU or past it; the two others and
`p_funcptr` keep their blocks too and are slow for other reasons, which are counted
here and are the next entries'.**

On a MacBook Air (Apple M1), macOS 27. 200 rounds, r64m `--jit` beside g64m `--jit`
(two at once, three runs, the least, less a program that does nothing); QEMU's and
Rosetta's times for the same builds are those taken earlier today (g64m's E186 table).

| 200 rounds | guest instructions | r64m `--jit` | | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | in blocks | entries | stores beside code | forgotten |
|---|---|---|---|---|---|---|---|---|---|---|---|
| p_loop | 3,005 M | 0.545 s | 5,511 M/s | 0.212 s | 0.773 s | 0.240 s | **1.42** | 99.8% | 1.2 M | 0 | 0 |
| p_pointer | 6,595 M | 1.860 s | 3,545 M/s | 1.413 s | 1.900 s | 0.723 s | **1.02** | 99.6% | 2.3 M | 200 | 0 |
| p_if | 4,602 M | 1.723 s | 2,672 M/s | 1.416 s | 1.873 s | 0.997 s | **1.09** | 99.7% | 0.9 M | 200 | 0 |
| p_recursion | 2,434 M | 3.656 s | 666 M/s | 0.565 s | 1.567 s | 0.810 s | 0.43 | 100.0% | 144 M | 0 | 0 |
| p_rotate | 17,822 M | 65.6 s | 272 M/s | 3.399 s | 4.403 s | 3.060 s | 0.07 | 78.7% | 103 M | 200 | 0 |
| p_index | 5,986 M | 28.0 s | 213 M/s | 1.078 s | 1.723 s | 0.420 s | 0.06 | 75.6% | 131 M | 200 | 0 |
| p_funcptr | 4,191 M | 48.1 s | 87 M/s | 3.424 s | 9.123 s | 2.867 s | 0.19 | 79.3% | 984 M | 200 | 0 |

- **The hypothesis held**: no page's blocks were forgotten; each of the five made its
  200 stores beside the code (one a round, the line it prints) and kept what it has at
  one round. `p_if` went from 62.9 s to 1.72 s, `p_pointer` from 98.8 s to 1.86 s.
- **Three primitives are now at QEMU's speed or past it** on this Mac: `p_loop` 1.42
  times, `p_if` 1.09, `p_pointer` 1.02. g64m's JIT is still ahead of r64m's on each
  (2.6, 1.2 and 1.3 times), and Rosetta ahead of both.
- **What holds the others back is not this entry's subject, and is now counted**:
  - `p_funcptr` goes into generated code 984 million times for 4,191 million
    instructions, `p_recursion` 144 million times for 2,434: a return and a call
    through a register have no constant target, so each comes out to Rust to be looked
    up in a hash table. That is the whole of `p_recursion`'s distance from QEMU (it is
    all in blocks) and most of `p_funcptr`'s.
  - `p_index` and `p_rotate` have a fifth to a quarter of their instructions not
    translated, and go in and out over a hundred million times around them.
- **`p_recursion` is not steady**: 2.03 s in R009's race and 3.66 s here, the same
  build of the same program but for this entry's change, which is not on its path (it
  writes nothing beside code). Not explained; to be timed again by itself before
  anything is concluded from it.
- **Two defects were found by this entry and put right in it**, each with a test that
  fails without the fix: an instruction run again after its store faulted began with
  the flags and registers it had itself changed (found by one case, `j_more`, giving a
  wrong exit code on macOS only); and code whose blocks were forgotten was never
  translated again. Neither was this entry's hypothesis; both were there since R008.
- All 62 programs give the interpreter's output, exit code and count in four JIT
  modes on macOS and in the Linux container; 50 unit tests.

**Next.** Two things, each its own entry: a return and a jump through a register found
without leaving generated code (what `p_recursion` and `p_funcptr` wait for); and the
count of which instructions end the blocks of the other primitives, translated in the
order of that count. The race with QEMU for all 23 follows them.

## R011: a return and a jump through an operand, without leaving generated code (2026-11-25)

The card, written before the run.

- **Hypothesis**: a return's target is not a constant, so every return comes out to
  Rust, which looks the address up in a hash table and goes in again: 144 million
  times in `p_recursion`'s 2,434 million instructions, 984 million in `p_funcptr`'s
  4,191 million (R010). If generated code looks the address up itself in a small table
  of its own (the address's low bits choose a place; the place holds an address and
  where its block's code is; equal, it jumps there; not equal, it leaves as before),
  a return is some seven host instructions and no leaving. `p_recursion`, which is
  wholly in blocks, is guessed to pass QEMU (1.567 s at 200 rounds; r64m took 2.03 s
  and 3.66 s in two races). `p_funcptr` also needs CALL and JMP through a register or
  memory translated, which they are not yet: with them it is guessed to come from
  48 s to a few seconds, still short of QEMU's 9.1 s only if what else it is not
  translated for (a fifth of its instructions) is in its loop.
- **What is changed from before**: a table of 16,384 places that generated code reads
  (its address in a host register), filled by Rust whenever it finds a block for an
  address, emptied of a block's place when the block is forgotten; RET looks in it;
  CALL and JMP through an operand (FFh, fields 2 and 4) are translated and look in it.
  With `--jit-check` the table is left empty, so that every block is still checked by
  itself.
- **Next move for each result**:
  - `p_recursion` past QEMU and steady over three races → the count of what ends the
    other primitives' blocks is next, and instructions are added by it
  - `p_recursion` not past QEMU → a profile; the first suspect is two returns whose
    addresses share a place in the table and put each other out, the second the
    record of the flags written at every instruction that sets them
  - `p_recursion` still unsteady from race to race (R010 saw 2.03 s and 3.66 s) →
    that is found before any figure of it is used
  - `p_funcptr` still far from QEMU → its count of what is not translated says what
- **Cut-off**: `p_recursion` and `p_funcptr` at 200 rounds beside g64m's JIT, three
  races of three runs each, with the counts; the other five of R010's table once, to
  see that nothing is lost. macOS.

**Result: `p_recursion` is past QEMU by 3.7 times, steady over three races, and past
g64m's JIT and Rosetta too. `p_funcptr` came from 48 s to 12.9 s and is still short of
QEMU.**

On a MacBook Air (Apple M1), macOS 27. 200 rounds, r64m `--jit` beside g64m `--jit`,
three runs each, the least, less a program that does nothing; QEMU and Rosetta as timed
earlier (g64m's E186 table).

| 200 rounds | r64m `--jit` | | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | in blocks | entries |
|---|---|---|---|---|---|---|---|---|
| p_recursion, race 1 | 0.428 s | 5,681 M/s | 0.553 s | 1.567 s | 0.810 s | 3.66 | 100.0% | 6,773 |
| p_recursion, race 2 | 0.426 s | 5,711 M/s | 0.554 s | | | 3.68 | | |
| p_recursion, race 3 | 0.425 s | 5,724 M/s | 0.552 s | | | **3.68** | | |
| p_funcptr | 12.870 s | 326 M/s | 3.435 s | 9.123 s | 2.867 s | 0.71 | 91.0% | 185 M |
| p_loop | 0.562 s | 5,352 M/s | 0.165 s | 0.773 s | 0.240 s | 1.38 | 99.8% | |
| p_if | 1.763 s | 2,610 M/s | 1.375 s | 1.873 s | 0.997 s | 1.06 | 99.7% | |
| p_pointer | 1.858 s | 3,549 M/s | 1.422 s | 1.900 s | 0.723 s | 1.02 | 99.6% | |

- **`p_recursion`**: 3.66 s (R010) to 0.43 s. It goes into generated code 6,773 times
  where it went 144 million. It is the first primitive on which r64m's JIT is faster
  than g64m's (by 1.3) and than Rosetta (by 1.9). Steady: 0.428, 0.426, 0.425 s. What
  made it unsteady in R010 (2.03 s and 3.66 s) was on the way that is no longer
  taken, the look-up in Rust's hash table after every return; it was not found, it
  went away.
- **`p_funcptr`**: 48.1 s to 12.9 s; 91.0% of its instructions in blocks where 79.3%
  were (the calls through a register are translated now). It still goes in 185
  million times: what it is not translated for is in its loop. Not past QEMU, as the
  card allowed.
- **Nothing lost** on `p_loop`, `p_if`, `p_pointer`.
- **Not done as the card had it**: `p_funcptr` was raced once and not three times, and
  `p_index` and `p_rotate` not at all. At 200 rounds a primitive that is partly
  interpreted takes a minute a run, and the race was stopped at the owner's word that
  it takes too long. From here a race is one round first, and the 200-round build only
  for what is under a tenth of a second at one round, no run over fifteen seconds.
- A mistake of the work, not of the code: the commit that brought this in said its
  unit tests had passed in the Linux container, and three had not (a limit of eight
  pieces of generated code in a process, met by tests run at once). The command that
  ran them went on to commit. Put right in the next commit, which says so.

**Next.** R012: what `p_rotate`, `p_index` and `p_funcptr` are not translated for.

## R012: what `p_rotate`, `p_index` and `p_funcptr` are not translated for (2026-11-25)

The owner's word: put the three right at one stroke.

The card, written before the run.

- **What was looked at first** (their code, disassembled; nothing run): the
  instructions in them that the translator does not read are of four kinds:
  1. shifts and rotations of 32 and 64-bit registers: SHL, SHR, ROL, ROR by a constant,
     by one and by CL (`p_rotate` 14 places, `p_index` 9, `p_funcptr` 5);
  2. operands of 8 and 16 bits: MOV of a byte or of 16 bits to memory, MOV of a
     constant byte, `test dl, dl`, `or ah, 1`, `and cx, 3ffh`, `xor byte [..], 1`
     (in all three, in the loops of two);
  3. BSR and TZCNT (`p_rotate`);
  4. NOT, CDQE, and the long NOP with prefixes that pads a loop's start.
- **Hypothesis**: with these four kinds translated the three run wholly in blocks
  (they are 78.7%, 75.6% and 91.0% in blocks now, and go in and out of generated code
  a hundred million times and more), and come from 65.6 s, 28.0 s and 12.9 s at 200
  rounds to a few seconds: near QEMU (4.40 s, 1.72 s, 9.12 s), past it for `p_funcptr`.
  The other primitives that use the same instructions gain without being aimed at.
- **What is changed from before**: the translator reads an operand's size as 8, 16, 32
  or 64 bits where it read 32 or 64; the prefixes 66h (16 bits), F3h before BSF and
  BSR, and the segment prefixes that do nothing. New records of the flags: a shift
  (its operand and count), a bit scan (its operand). A rotation changes two flags and
  leaves four, which one record cannot say: it is translated where the flags are
  written whole by a later instruction of the block before anything reads them, and
  left to the interpreter elsewhere. The flags of an 8 or 16-bit operation are had
  from the host by doing it on the operands moved to the top of a 32-bit register.
- **Next move for each result**:
  - The three in blocks (over 99%) and near QEMU → the race of all 23 against QEMU
    is the next entry, with what is still not translated counted by program
  - In blocks but far from QEMU → a profile of the slowest: the record of the flags
    written at every instruction that sets them is the first suspect
  - One not in blocks → what still ends its blocks is listed as here, and said
  - A program that no longer gives the interpreter's output, exit code or count →
    put right before anything is timed
- **Cut-off**: the four kinds above, for the three. Timed by the shorter race (one
  round; 200 rounds only where one round is under a tenth of a second; no run over
  15 s), once for all 23 so that a loss anywhere shows.

**Result: the three run wholly in blocks and are past QEMU: `p_rotate` 1.31 times,
`p_index` 1.44, `p_funcptr` 3.10. Eight primitives are now past QEMU on this Mac, and
on three r64m's JIT is level with g64m's or ahead of it.**

On a MacBook Air (Apple M1), macOS 27. The shorter race: one round first, and the
200-round build where one round is under a tenth of a second, r64m `--jit` beside g64m
`--jit`, three runs, the least, less a program that does nothing, no run over 15 s.
QEMU and Rosetta as timed earlier (g64m's E186 table).

The three this entry is about, at 200 rounds:

| | before (R010, R011) | now | | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | in blocks |
|---|---|---|---|---|---|---|---|---|
| p_rotate | 65.6 s | 3.368 s | 5,292 M/s | 3.359 s | 4.403 s | 3.060 s | **1.31** | 100.0% (78.7%) |
| p_index | 28.0 s | 1.195 s | 5,009 M/s | 1.076 s | 1.723 s | 0.420 s | **1.44** | 100.0% (75.6%) |
| p_funcptr | 12.9 s | 2.944 s | 1,423 M/s | 3.317 s | 9.123 s | 2.867 s | **3.10** | 100.0% (91.0%) |

All the primitives that are wholly or nearly in blocks, at 200 rounds:

| | r64m `--jit` | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | g64m's time / r64m's |
|---|---|---|---|---|---|---|
| p_recursion | 0.440 s | 0.540 s | 1.567 s | 0.810 s | 3.56 | 1.23 |
| p_funcptr | 2.944 s | 3.317 s | 9.123 s | 2.867 s | 3.10 | 1.13 |
| p_loop | 0.444 s | 0.162 s | 0.773 s | 0.240 s | 1.74 | 0.36 |
| p_index | 1.195 s | 1.076 s | 1.723 s | 0.420 s | 1.44 | 0.90 |
| p_pointer | 1.377 s | 1.406 s | 1.900 s | 0.723 s | 1.38 | 1.02 |
| p_rotate | 3.368 s | 3.359 s | 4.403 s | 3.060 s | 1.31 | 1.00 |
| p_if | 1.510 s | 1.404 s | 1.873 s | 0.997 s | 1.24 | 0.93 |
| p_fill | 2.506 s | 1.447 s | 2.883 s | 0.497 s | 1.15 | 0.58 |
| p_shift | 1.910 s | 0.704 s | 0.893 s | 0.680 s | **0.47** | 0.37 |

- **The hypothesis held**: all three are 100% in blocks and past QEMU. `p_funcptr`
  was guessed to pass it and did by three times; the two others were guessed to come
  near it and passed it.
- **None is at twice QEMU yet** but `p_recursion` and `p_funcptr`.
- **`p_shift` is wholly in blocks now and is half QEMU's speed**: the one program in
  blocks that loses to QEMU. It is made of shifts, and every shift writes its record
  of the flags (three or four host instructions) whether anything reads them or not.
  The first suspect named in the card, now with a program that shows it.
- **`p_loop` is at 0.36 of g64m's JIT** though it is past QEMU: the same record,
  written at every addition of a loop that g64m runs without it.
- **The other fourteen are still mostly the interpreter's** (0 to 59% in blocks, 42
  to 113 M instructions a second): the floating-point ones (SSE and the x87 are not
  translated at all), and integer ones that wait for instructions not yet in the list
  (`p_sha` is 96.8% in blocks and still at 98 M/s: the few that are left are in its
  loop). Of all 23 primitives' instructions 51.8% are in blocks.
- 62 programs give the interpreter's output, exit code and count in four JIT modes on
  macOS and in the Linux container; 58 unit tests on both.

**Next.** Two things, by what this shows: the record of the flags not written where
nothing can read it (what `p_shift` and `p_loop` wait for, and every block pays); and
the instructions that end the blocks of the fourteen, counted by program.

## R013: the record of the flags not written where nothing reads it (2026-11-25)

The card, written before the run.

- **Hypothesis**: every instruction that sets the flags writes its record (two to four
  host instructions) whether or not anything reads the flags before they are set
  again. `p_shift` is wholly in blocks and at 0.47 of QEMU, `p_loop` past QEMU and at
  0.36 of g64m's JIT (R012), and the record is guessed to be why, though no profile
  has said so. If the translator looks at the guest's code after an instruction that
  sets the flags, and finds that on every way from it the flags are written whole
  before anything reads them, the record is not written. `p_shift`, whose shifts
  follow one another, is guessed to gain most (to QEMU's speed or past it); a loop
  whose last compare is read by its own jump still writes that one record, since the
  way out of the loop may read the flags, so `p_loop` is guessed to gain little by
  this alone.
- **What is changed from before**: the translator reads on from a flag-setting
  instruction (through the rest of its block, and past the block's end along jumps
  whose targets are constants) until it finds an instruction that writes all the
  flags (dead: no record), or one that reads them, or one it does not know, or a
  return (live: the record is written as before). What it read to decide is part of
  what the block rests on: a store there forgets the block. With `--jit-check` every
  record is written, since the check compares the flags after each block.
  A fault in the stretch where the flags are dead finds them stale: nothing reads
  them there by the same proof, but what would be shown to a signal handler is not
  what the CPU would show. (No signal is delivered yet.)
- **Next move for each result**:
  - `p_shift` to QEMU's speed or past it, `p_loop` little changed → the second half:
    the record of a loop's last compare made only on the way out that needs it, which
    is its own entry's worth of change and is done here only if this result asks
  - `p_shift` little changed → the record was not the reason: a profile of `p_shift`
    says what is, before anything more is built on the guess
  - A program that no longer gives the interpreter's output, exit code or count →
    put right before anything is timed
- **Cut-off**: this one change; the shorter race for the nine primitives that are
  wholly in blocks. One profile if the guess fails.

**Result: as guessed on both counts. `p_shift` came from 0.47 of QEMU to level with it
(1.91 s to 0.88 s); `p_loop` hardly moved. Three others gained without being aimed at,
and `p_index` is the third primitive at twice QEMU.**

On a MacBook Air (Apple M1), macOS 27. The shorter race, 200 rounds, r64m `--jit`
beside g64m `--jit`; QEMU and Rosetta as timed earlier.

| 200 rounds | before (R012) | now | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | g64m's time / r64m's |
|---|---|---|---|---|---|---|---|
| p_shift | 1.910 s | 0.879 s | 0.709 s | 0.893 s | 0.680 s | 1.02 (0.47) | 0.81 |
| p_index | 1.195 s | 0.777 s | 1.041 s | 1.723 s | 0.420 s | **2.22** (1.44) | 1.34 |
| p_pointer | 1.377 s | 1.143 s | 1.371 s | 1.900 s | 0.723 s | 1.66 (1.38) | 1.20 |
| p_rotate | 3.368 s | 3.108 s | 3.392 s | 4.403 s | 3.060 s | 1.42 (1.31) | 1.09 |
| p_loop | 0.444 s | 0.436 s | 0.169 s | 0.773 s | 0.240 s | 1.77 (1.74) | 0.39 |
| p_if | 1.510 s | 1.546 s | 1.366 s | 1.873 s | 0.997 s | 1.21 (1.24) | 0.88 |
| p_funcptr | 2.944 s | 2.937 s | 3.321 s | 9.123 s | 2.867 s | 3.11 | 1.13 |
| p_recursion | 0.440 s | 0.446 s | 0.549 s | 1.567 s | 0.810 s | 3.51 | 1.23 |
| p_fill | 2.506 s | 2.505 s | 1.474 s | 2.883 s | 0.497 s | 1.15 | 0.59 |

- **The record was the reason for `p_shift`**: twice as fast with it gone where
  nothing reads it. No program in blocks loses to QEMU now.
- **`p_loop` as guessed**: its loop ends in a compare read by its own jump, whose way
  out of the loop may read the flags, so that record is still written every time
  round. 0.39 of g64m's JIT.
- **r64m's JIT is ahead of g64m's on five of the nine** (`p_index`, `p_pointer`,
  `p_rotate`, `p_funcptr`, `p_recursion`) and behind on four.
- A fault where the flags are dead finds them as they were before the instruction
  whose record was left out. A test holds this as it is; it is to be met again when
  signals are delivered to the guest.
- 62 programs in four JIT modes and 62 unit tests on macOS and in the Linux container.

**Next.** The card's first case: the second half, as R014.

## R014: a loop's last record made only on the way out that needs it (2026-11-25)

The card, written before the run.

- **Hypothesis**: a loop ends `cmp; jne` (or `sub; jne`, `test; je`, `add; jne`). The
  jump is answered by the host's flags; the way back into the loop writes the flags
  again; the way out may read them. So the record is needed on one way only, and R013
  writes it on both because it writes it at the compare. If an instruction that sets
  the flags and is followed at once by a jump that the host's flags answer writes no
  record itself, and the jump writes one on each of its two ways only where the flags
  are not dead, a loop's body loses those three host instructions. `p_loop` is guessed
  to gain a tenth to a quarter (its loop is some ten host instructions), `p_if` and
  the others with compares in their loops something. It is not guessed to bring
  `p_loop` to g64m's speed: g64m is 2.6 times faster there, which three instructions
  in ten do not explain.
- **What is changed from before**: for ADD, SUB, AND, OR, XOR, CMP and TEST of 32 or
  64 bits on a register with a register or a constant, followed at once by such a
  jump. What the record needs must still be there at the jump: for CMP and TEST the
  operands are; for the others the result is, and two new kinds of record say "this
  is the result and the second operand", from which the first is had again. Not where
  the two operands are one register (the second is then lost).
- **Next move for each result**:
  - `p_loop` gains as guessed and is still far from g64m → a profile of `p_loop`'s
    loop as generated (what the host instructions are, against g64m's for the same
    loop), which is its own entry
  - `p_loop` gains nothing → the record on the way out was not what it costs; the same
    profile, sooner
  - A program wrong → put right first
- **Cut-off**: this one change, the shorter race for the nine.

**Result: nothing gained. `p_loop` is 0.436 s as before; the guess of a tenth to a
quarter was wrong. The record was already gone from its loop.**

| 200 rounds | before (R013) | now | g64m `--jit` | QEMU |
|---|---|---|---|---|
| p_loop | 0.436 s | 0.436 s | 0.216 s | 0.773 s |
| p_if | 1.546 s | 1.584 s | 1.405 s | 1.873 s |
| p_shift | 0.879 s | 0.922 s | 0.751 s | 0.893 s |
| p_index | 0.777 s | 0.769 s | 1.081 s | 1.723 s |
| p_pointer | 1.143 s | 1.138 s | 1.408 s | 1.900 s |
| p_rotate | 3.108 s | 3.138 s | 3.307 s | 4.403 s |
| p_funcptr | 2.937 s | 2.934 s | 3.406 s | 9.123 s |
| p_recursion | 0.446 s | 0.439 s | 0.536 s | 1.567 s |

Nothing moved beyond what two races differ by (`p_shift` 0.88 and 0.92 s: the same).

- **Why the guess was wrong**: R013 had already taken the record out of `p_loop`'s
  loop. Its last instruction is `sub rax, 1`, and both ways of the jump after it
  write the flags before anything reads them (the loop's head by its XOR, the way on
  by an ADD): R013's reading follows both ways of a jump that the host's flags answer,
  so it found the flags dead and left the record out. This entry's case (dead one way,
  live the other) is not in `p_loop`. The card's hypothesis rested on R013's own
  guess about `p_loop` ("still writes that one record"), which was not looked at
  before it was built on.
- The change is kept: it is right (63 unit tests, 62 programs, both hosts) and it is
  the general form of what R013 does for the case of both ways dead. No primitive
  among the nine is the worse for it. That it gains something somewhere is not shown.
- **What `p_loop`'s loop is, looked at** (`R64M_JIT_DUMP`, the block's code given to
  the assembler to read back). Five guest instructions (`mov rcx, rsi; xor rcx, rax;
  add rdx, rcx; sub rax, 1; jne`) are nine of the host's:

  ```
  mov  x1, x6
  eor  x1, x1, x0
  tst  x1, x1          ; the host's flags of the XOR, which nothing asks for
  adds x2, x2, x1
  mov  x26, #1         ; the constant one, put in a register
  subs x0, x0, x26
  b.ne taken           ; taken every time round
  ...
  taken: add x27, x27, #5
         b   start     ; and a second jump taken every time round
  ```

  No record, no leaving generated code. Two jumps taken a round, one instruction to
  set flags that are not used, one to load a constant an instruction could carry.
  600 million rounds in 0.436 s is 2.3 cycles a round; g64m's is about one.

**Next.** R015: the three things that listing shows.

## R015: three things in a loop's host code (2026-11-25)

The card, written before the run.

- **Hypothesis**: `p_loop`'s loop is nine host instructions with two jumps taken a
  round (R014's listing), at 2.3 cycles a round; g64m's JIT runs it at about one. Three
  of the nine are not asked for by the guest's five:
  1. a jump taken to a second jump taken: where a conditional jump's target is its own
     block's start and no record is to be written on that way, the conditional jump
     can go there itself (the count of instructions added before it, once, for both
     ways);
  2. `tst` after XOR and OR to give the host's flags, where the flags are dead and no
     jump of the block asks the host's;
  3. a constant put in a register for ADD, SUB and CMP, where it is under 4,096 and
     the host's instruction can carry it.
  With them the loop is six instructions and one jump taken. `p_loop` is guessed to
  come from 0.436 s to under 0.3 s (the two jumps are guessed to be most of it), still
  behind g64m's 0.17 to 0.22 s. The others gain where they have such loops, constants
  and logic; by how much is not guessed.
- **What is changed from before**: those three, in the translator. With `--jit-check`
  a block does not jump to its own start (the check is of one pass through a block).
- **Next move for each result**:
  - `p_loop` under 0.3 s → the nine are raced again and set beside QEMU; what the
    fourteen primitives not in blocks wait for is the next entry
  - `p_loop` not under 0.3 s → the listing again, and the cycles counted for what is
    left (the host's counters, as g64m's ledger did), before another guess
  - A program wrong → put right first
- **Cut-off**: these three; the shorter race for the nine; one listing of the loop
  after.

**Result: `p_loop` 0.436 s to 0.379 s, twice QEMU by the race, and not under 0.3 s as
guessed. The loop is six host instructions and one jump taken, as meant; it takes 1.7
cycles a round, which is not explained.**

On a MacBook Air (Apple M1), macOS 27. The shorter race, 200 rounds, r64m `--jit`
beside g64m `--jit`; QEMU and Rosetta as timed earlier.

| 200 rounds | before (R014) | now | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | g64m's time / r64m's |
|---|---|---|---|---|---|---|---|
| p_recursion | 0.439 s | 0.440 s | 0.540 s | 1.567 s | 0.810 s | 3.56 | 1.23 |
| p_funcptr | 2.934 s | 2.932 s | 3.360 s | 9.123 s | 2.867 s | 3.11 | 1.15 |
| p_index | 0.769 s | 0.712 s | 1.042 s | 1.723 s | 0.420 s | 2.42 | 1.46 |
| p_loop | 0.436 s | 0.379 s | 0.216 s | 0.773 s | 0.240 s | **2.04** | 0.57 |
| p_pointer | 1.138 s | 1.143 s | 1.405 s | 1.900 s | 0.723 s | 1.66 | 1.23 |
| p_rotate | 3.138 s | 3.297 s | 3.347 s | 4.403 s | 3.060 s | 1.34 | 1.02 |
| p_if | 1.584 s | 1.527 s | 1.411 s | 1.873 s | 0.997 s | 1.23 | 0.92 |
| p_shift | 0.922 s | 0.774 s | 0.714 s | 0.893 s | 0.680 s | 1.15 | 0.92 |
| p_fill | 2.536 s | 2.506 s | 1.466 s | 2.883 s | 0.497 s | 1.15 | 0.58 |

- **Four of the nine are at twice QEMU or past it** (`p_recursion`, `p_funcptr`,
  `p_index`, `p_loop`); all nine are past QEMU; six are level with g64m's JIT or ahead
  of it.
- **The loop as it now is** (the same listing as R014's):

  ```
  mov  x1, x6
  eor  x1, x1, x0
  adds x2, x2, x1
  subs x0, x0, #1
  add  x27, x27, #5
  b.ne start
  ```

- **The guess of under 0.3 s was wrong**, and the card says to count before guessing
  again. Counted with the host's counters (`/usr/bin/time -l`, the whole process):

  | `p_loop`, 200 rounds | host instructions retired | cycles | for a round of the loop |
  |---|---|---|---|
  | r64m `--jit` | 3,640 M | 1,025 M | 6.1 instructions, 1.7 cycles |
  | g64m `--jit` | 5,302 M | 973 M | 8.8 instructions, 1.6 cycles (its start included) |

  r64m retires a third fewer instructions than g64m and takes as many cycles. Six
  instructions whose longest chain is one deep take 1.7 cycles a round where one
  would be expected. Why is not known: no guess is made here. (g64m's cycles include
  its start, some 0.08 s in the kernel that the race takes off as "a program that
  does nothing": with that off, g64m's loop is the faster by about a quarter, which
  is what the race's 0.216 s against 0.379 s overstates and the cycles understate.)
- 62 programs in four JIT modes and 65 unit tests on macOS and in the Linux container.

**Next.** What the fourteen primitives that are not in blocks wait for (0 to 59% in
blocks, R012): the instructions that end their blocks, counted by program; SSE first
among them by how many programs it holds back. The 1.7 cycles are left as found.

## R016: what the interpreter still runs under the JIT (2026-11-25)

The card, written before the run.

- **What was counted first** (`--jit-why`, new: every instruction the interpreter runs
  while the JIT is on, by its prefixes and opcode; all 23 primitives at one round).
  The fourteen primitives that are not in blocks have 41 to 100% of their instructions
  interpreted, and the commonest instructions the interpreter runs are ones the
  translator already has: `cmp rax, imm8` (19.0 M), `jne` (15.9 M), `add r/m64, r64`
  (14.7 M), `mov r/m32, r32` (14.5 M), `movzx` (10.3 M), `or` (10.2 M). They are
  interpreted because of where blocks may begin: only where a jump arrives. When a
  block ends before an instruction that is not translated, the interpreter runs that
  one, and then every instruction after it up to the next jump, translated or not.
  Behind those in the count come the ones that are truly missing: the arithmetic with
  the A register and a constant (`48 3d`, CMP RAX with a constant: 8.5 M), SSE (some
  55 M over six primitives), CMOV and SETcc (5.8 M), IMUL (5.7 M), the x87 (10 M, one
  primitive), IDIV, BSWAP, the string instructions.
- **Hypothesis**: if the instruction after one that the interpreter ran in a block's
  stead is taken as a place where a block may begin, the integer primitives that are
  now a quarter in blocks (`p_bignum`, `p_bytes`, `p_muldiv`, `p_pixel`, `p_ternary`,
  `p_bitfield`) come to more than half without any new instruction, and gain by it.
  With the A-register forms, CMOV, SETcc and IMUL added they are guessed to be over
  90% in blocks, and past QEMU as the nine are. The floating-point primitives do not
  move until SSE is translated, which is not in this entry.
- **What is changed from before**: (1) where a block may begin; (2) ADD, OR, AND, SUB,
  XOR, CMP and TEST of the A register with a constant (05h and its like, A8h, A9h);
  (3) CMOVcc and SETcc, which read the flags: translated where the host's flags
  answer, as a conditional jump is, and counted as reading them elsewhere; (4) IMUL
  with two and three operands, its flags a record of its operands.
- **Next move for each result**:
  - The six integer primitives over 90% in blocks and past QEMU → SSE is the next
    entry, and with it the six floating-point primitives
  - In blocks and not past QEMU → the count again for the slowest, and the listing of
    its loop, before more is added
  - (1) alone makes a primitive slower (short blocks between interpreted
    instructions, as R008 saw) → said, and (1) is kept only with what makes those
    blocks longer
  - A program wrong → put right first
- **Cut-off**: these four. The count and the shorter race after (1) alone, and again
  after all four. SSE, the x87, IDIV, BSWAP and the strings are counted and not done.

**Result: the share of instructions in blocks went from 51.8% to 81.8%, three more
primitives are past QEMU (`p_fill` 2.74 times, `p_ternary` 1.13; `p_loop` and `p_index`
further), and four that are over 93% in blocks are still far behind it. Being in
blocks is not the measure: one instruction of the interpreter's in a loop is.**

On a MacBook Air (Apple M1), macOS 27.

After the first change alone (a block may begin after what the interpreter ran in a
block's stead), one round, three at once, interpreted against the JIT before and with:
all 23 took 10.48 s interpreted, 7.38 s before, **10.17 s with**: slower, as the card's
third case had it. 76.1% of instructions were in blocks (51.8% before), but each
instruction that stood in cost three looks in hash tables and a going out and in for
the short block after it (`p_dbl_conv` 0.70 s to 1.13 s, `p_ternary` 0.57 s to 0.70 s).
So it was kept only with the rest, and with the three tables made one with a cheap
hash.

After all four, the shorter race (r64m `--jit` beside g64m `--jit`; QEMU and Rosetta
as timed earlier), the primitives that are over 90% in blocks, at 200 rounds:

| 200 rounds | in blocks | r64m `--jit` | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's |
|---|---|---|---|---|---|---|
| p_recursion | 100.0% | 0.439 s | 0.646 s | 1.567 s | 0.810 s | 3.57 |
| p_funcptr | 100.0% | 2.932 s | 3.405 s | 9.123 s | 2.867 s | 3.11 |
| p_fill | 10.2% | 1.050 s | 1.510 s | 2.883 s | 0.497 s | **2.74** (1.15) |
| p_index | 100.0% | 0.656 s | 1.093 s | 1.723 s | 0.420 s | 2.63 |
| p_loop | 100.0% | 0.334 s | 0.218 s | 0.773 s | 0.240 s | 2.32 |
| p_pointer | 100.0% | 1.146 s | 1.465 s | 1.900 s | 0.723 s | 1.66 |
| p_rotate | 100.0% | 3.251 s | 3.455 s | 4.403 s | 3.060 s | 1.35 |
| p_if | 100.0% | 1.540 s | 1.473 s | 1.873 s | 0.997 s | 1.22 |
| p_shift | 100.0% | 0.768 s | 0.757 s | 0.893 s | 0.680 s | 1.16 |
| p_ternary | 100.0% | 0.714 s | 0.593 s | 0.807 s | 0.473 s | **1.13** (interpreted before) |
| p_switch | 95.8% | 8.691 s | 3.070 s | 6.167 s | 2.847 s | 0.71 |
| p_bytes | 96.4% | 6.884 s | 1.183 s | 1.373 s | 0.573 s | 0.20 |
| p_pixel | 96.0% | 5.058 s | 0.538 s | 0.523 s | 0.393 s | 0.10 |

(`p_fill` is 10% in blocks by count because its work is `rep stos`, which the
interpreter does in one step: what changed for it is that the loop round the `rep` is
now blocks.) Eleven primitives are past QEMU, five of them at twice or past it.

- **The hypothesis held for the share and failed for the speed.** The six integer
  primitives were guessed to be over 90% in blocks and past QEMU. In blocks: `p_bytes`
  96.4%, `p_pixel` 96.0%, `p_switch` 95.8%, `p_bitfield` 93.2%, `p_muldiv` 86.7%,
  `p_bignum` 81.2%, `p_ternary` 100%. Past QEMU: `p_ternary` alone. A program 96% in
  blocks goes in and out of generated code a million times a round for the 4%, and
  that is all its time.
- **What the 4% is**, counted again (`--jit-why`): `p_bytes` and `p_bignum` ADC with a
  constant (`48 83 /2`) and MUL; `p_pixel` a MOVZX from a second byte (AH and its
  like); `p_switch` JBE where the host's flags do not answer; `p_bitfield` shifts of
  16 and 8 bits; `p_muldiv` DIV and IDIV; `p_sha` BSWAP. Then SSE, for six
  primitives, and the x87 for one.
- **`p_sha` is 99.0% in blocks and runs at 98 M instructions a second**, for a reason
  that is not an instruction: it makes 30,721 stores a round to data that shares a
  16 KB host page with its code, each a fault, an instruction interpreted, and the
  page opened and closed (R010's way, which was made for one store a round). The
  profile is `mprotect`. It is what R010's card named as its second case and what ADR
  0008's point B allowed for; on a host with 4 KB pages it does not happen. It is not
  put right here.
- 62 programs in four JIT modes and 69 unit tests on macOS and in the Linux container.

**Next.** The integer instructions the count names (ADC and SBB, MUL, DIV and IDIV,
the shifts of 8 and 16 bits, MOVZX from a second byte, BSWAP, a jump the host's flags
do not answer) as R017. Then two larger things, each a decision before it is a change:
SSE in blocks; and stores to data beside code on a host with 16 KB pages.

## R017: the integer instructions the count names (2026-11-25)

The card, written before the run.

- **Hypothesis**: five primitives are 81 to 99% in blocks and held back by one or two
  instructions in their loops that the interpreter runs (R016's count): ADC with a
  constant and MUL (`p_bignum`, `p_bytes`), shifts of 8 and 16 bits (`p_bitfield`),
  MOVZX from a second byte (`p_pixel`), DIV and IDIV (`p_muldiv`), BSWAP (`p_sha`).
  With those translated the five are guessed to be wholly in blocks; `p_bytes`,
  `p_pixel` and `p_bitfield` to pass QEMU; `p_bignum` and `p_muldiv` to come near it
  (their multiplications and divisions are a host instruction each, as QEMU's are).
  `p_sha` is not guessed to move: what holds it is its stores beside its code (R016).
  `p_switch` is not in this: its instruction is a jump the host's flags do not answer,
  which is another kind of change.
- **What is changed from before**: ADC and SBB of 32 and 64 bits, where the host's
  flags say what the carry is (the carry is put into the host's and ADCS or SBCS does
  the rest; the record has the carry in it); MUL and IMUL with one operand; DIV and
  IDIV, which leave for the interpreter where they would fault or where the dividend
  does not fit the host's division (the upper half not the lower's extension); SHL,
  SHR and SAR of 8 and 16-bit registers by a constant; MOVZX and MOVSX from AH and its
  like; BSWAP.
- **Next move for each result**:
  - Wholly in blocks and as guessed → blocks that go on past a conditional jump is
    the next entry (`p_switch`, and every block's length), then the two decisions
    R016 left (SSE; stores beside code on 16 KB pages)
  - In blocks and far from QEMU → the count and the listing for that one
  - A program wrong → put right first
- **Cut-off**: these six kinds. The shorter race for the integer primitives.

**Result: the five are wholly in blocks and past QEMU, `p_bignum` and `p_muldiv` by
more than was guessed. Sixteen of the 23 primitives are past QEMU on this Mac, seven of
them at twice or past it.**

On a MacBook Air (Apple M1), macOS 27. The shorter race, 200 rounds, r64m `--jit`
beside g64m `--jit`; QEMU and Rosetta as timed earlier.

| 200 rounds | before (R016) | now | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | g64m's time / r64m's |
|---|---|---|---|---|---|---|---|
| p_muldiv | interpreted in part | 0.915 s | 0.923 s | 4.397 s | 0.653 s | **4.81** | 1.01 |
| p_bignum | interpreted in part | 1.142 s | 1.741 s | 2.590 s | 0.887 s | **2.27** | 1.52 |
| p_bytes | 6.884 s | 0.816 s | 1.083 s | 1.373 s | 0.573 s | 1.68 | 1.33 |
| p_pixel | 5.058 s | 0.489 s | 0.482 s | 0.523 s | 0.393 s | 1.07 | 0.99 |
| p_bitfield | interpreted in part | 1.033 s | 1.075 s | 1.100 s | 0.890 s | 1.06 | 1.04 |
| p_ternary | 0.714 s | 0.763 s | 0.531 s | 0.807 s | 0.473 s | 1.06 | 0.70 |
| p_switch | 8.691 s | 8.683 s | 3.090 s | 6.167 s | 2.847 s | 0.71 | 0.36 |
| p_sha | 96 M/s at one round | the same | | | | far behind | |

Where all 23 stand against QEMU now (QEMU's time over r64m's, 200 rounds):
`p_muldiv` 4.81, `p_recursion` 3.57, `p_funcptr` 3.11, `p_fill` 2.73, `p_index` 2.43,
`p_loop` 2.28, `p_bignum` 2.27 (seven at twice or past it); `p_bytes` 1.68,
`p_pointer` 1.66, `p_rotate` 1.35, `p_if` 1.22, `p_shift` 1.16, `p_pixel` 1.07,
`p_bitfield` 1.06, `p_ternary` 1.06 (eight more past it); `p_switch` 0.71; `p_sha`
far behind; and the six floating-point ones (`p_dbl_arith`, `p_dbl_cmp`,
`p_dbl_conv`, `p_float`, `p_ldbl`, `p_simd`) mostly interpreted. 84.4% of the
primitives' instructions are in blocks.

- **The hypothesis held**, and `p_bignum` and `p_muldiv`, guessed only to come near
  QEMU, are at 2.27 and 4.81 times it. On ten of the sixteen r64m's JIT is level with
  g64m's or ahead of it.
- **One of the six kinds was not in at first, and the tests did not say so**: the
  shifts of 8 bits have opcodes of their own (C0h, D0h), which the reader did not
  know, so they stayed the interpreter's. Every test of them passed, because the
  interpreter does them right: a test that compares results cannot tell who computed
  them. `p_bitfield` staying at 97.7% in blocks and ten seconds showed it; the count
  (`--jit-why`) named the instruction. The tests of small shifts now also ask that
  they are in a block.
- A division at a block's start that left for the interpreter came back to its own
  block and went round for ever: found by a test that did not end. A block now has a
  way out that says "this instruction is the interpreter's this time".
- **What is left among the integer primitives**: `p_switch` (a jump the host's flags
  do not answer: `cmp; je; jbe`, where the first jump ends the block and the second
  begins the next with no flags of the host's known); `p_sha` (its stores beside its
  code, R016).
- 62 programs in four JIT modes and 72 unit tests on macOS and in the Linux container.

**Next.** Three things, each a decision before it is a change: a block that goes on
past a conditional jump (what `p_switch` waits for, and ADR 0014's second "not yet");
SSE in blocks (six primitives); stores to data beside code on a host with 16 KB pages
(`p_sha`, and whatever real program keeps its hot data next to its code).

## R018: a block that goes on past a conditional jump (2026-11-25)

The card, written before the run.

- **Hypothesis**: a block ends at its first conditional jump. So `cmp; je; jbe` is two
  blocks, and the second begins with a jump for which no flags of the host's are
  known: it is the interpreter's every time (`p_switch`: 1.08 million such jumps a
  round, 0.71 of QEMU). If a block goes on past a conditional jump, with the way taken
  out of line after the block's end and the way not taken continuing in the block, the
  second jump is answered by the same flags and nothing leaves generated code.
  `p_switch` is guessed to pass QEMU (it is all in blocks but for that jump). Every
  other program's blocks become longer and fewer, with fewer jumps from block to
  block; what that gains is not guessed beyond "something, nowhere a loss".
- **What is changed from before**: a conditional jump that the host's flags answer and
  that does not go to its own block's start no longer ends the block, while the block
  has room. The way taken is a piece of code after the block's end (the count of the
  instructions run so far, the record if that way needs it, a way out that can be
  joined); the way not taken is the next instruction. The reading that finds flags
  dead follows the same rule, so that it and the translator agree about which jumps
  are inside a block. Not with `--jit-check` (the check is of a block run from start
  to end, and such a block has more than one end).
- **Next move for each result**:
  - `p_switch` past QEMU and no primitive the worse → SSE is the next entry
  - `p_switch` not past QEMU → the count and the listing for it
  - A primitive slower by more than a twentieth → why, before this is kept: longer
    blocks are made once and the ways taken are now further from the code they leave
  - A program wrong → put right first
- **Cut-off**: this one change. The shorter race for the sixteen integer primitives
  that are in blocks, set beside R017's.

**Result: `p_switch` is past QEMU by 2.11 times (8.68 s to 2.92 s) and `p_if` gained a
fifth. One primitive is the worse by a twentieth, `p_loop`, and why is found only in
part. Kept.**

On a MacBook Air (Apple M1), macOS 27. The shorter race, 200 rounds, r64m `--jit`
beside g64m `--jit`; QEMU and Rosetta as timed earlier.

| 200 rounds | before (R017) | now | g64m `--jit` | QEMU | Rosetta | QEMU's time / r64m's | g64m's time / r64m's |
|---|---|---|---|---|---|---|---|
| p_muldiv | 0.915 s | 0.885 s | 0.953 s | 4.397 s | 0.653 s | 4.97 | 1.08 |
| p_recursion | 0.439 s | 0.423 s | 0.573 s | 1.567 s | 0.810 s | 3.71 | 1.35 |
| p_funcptr | 2.932 s | 2.854 s | 3.333 s | 9.123 s | 2.867 s | 3.20 | 1.17 |
| p_fill | 1.056 s | 1.004 s | 1.466 s | 2.883 s | 0.497 s | 2.87 | 1.46 |
| p_index | 0.710 s | 0.642 s | 1.103 s | 1.723 s | 0.420 s | 2.69 | 1.72 |
| p_bignum | 1.142 s | 1.083 s | 1.733 s | 2.590 s | 0.887 s | 2.39 | 1.60 |
| p_loop | 0.339 s | 0.363 s | 0.222 s | 0.773 s | 0.240 s | 2.13 | 0.61 |
| p_switch | 8.683 s | 2.923 s | 3.068 s | 6.167 s | 2.847 s | **2.11** (0.71) | 1.05 |
| p_pointer | 1.146 s | 1.110 s | 1.427 s | 1.900 s | 0.723 s | 1.71 | 1.29 |
| p_bytes | 0.816 s | 0.820 s | 1.113 s | 1.373 s | 0.573 s | 1.67 | 1.36 |
| p_if | 1.540 s | 1.205 s | 1.365 s | 1.873 s | 0.997 s | 1.55 (1.22) | 1.13 |
| p_rotate | 3.251 s | 3.192 s | 3.348 s | 4.403 s | 3.060 s | 1.38 | 1.05 |
| p_shift | 0.768 s | 0.728 s | 0.755 s | 0.893 s | 0.680 s | 1.23 | 1.04 |
| p_pixel | 0.489 s | 0.475 s | 0.525 s | 0.523 s | 0.393 s | 1.10 | 1.11 |
| p_ternary | 0.763 s | 0.732 s | 0.568 s | 0.807 s | 0.473 s | 1.10 | 0.78 |
| p_bitfield | 1.033 s | 1.041 s | 1.063 s | 1.100 s | 0.890 s | 1.06 | 1.02 |

All sixteen integer primitives that are in blocks are past QEMU, eight of them at
twice or past it; on fourteen of the sixteen r64m's JIT is level with g64m's or ahead.

- **The hypothesis held for `p_switch`**: wholly in blocks (95.8% before), three times
  as fast, past QEMU and past g64m's JIT.
- **"Nowhere a loss" did not hold: `p_loop` is 6% slower**, and steadily (the build
  before against this one, the two at once, three races of five runs: 0.335, 0.337,
  0.337 s against 0.356, 0.357, 0.357 s; `p_ternary` and `p_pixel` the same to the
  millisecond in the same races). By the card that is to be explained before the
  change is kept. What was found:
  - the loop's own block is the same six host instructions as before, and is the
    first block made in both builds, so it lies at the same place in the piece;
  - the host's counters say the same instructions retired (3,635 M) and 6.5% more
    cycles (1,022 M to 1,088 M): 66 million cycles more over 600,000 rounds of the
    outer loop, some 110 cycles a round;
  - what changed is the two blocks of the outer loop, which are longer now and reach
    the inner loop by a way taken that lies after their end;
  - placing every block's start on a boundary of 4 to 128 bytes changes nothing
    (0.346 to 0.353 s at each).
  So the cycles are lost where the inner loop is entered and left, not in it, and not
  by where blocks begin. What costs 110 cycles there is not known (the host's
  prediction of the inner loop's jump after a different way in is the suspect, and
  not shown). The change is kept: it is right, it is what `p_switch` and `p_if` gain
  by, and the loss is 6% of one primitive that stays at twice QEMU. That the card's
  "why" is answered only in part is said here and not hidden.
- 62 programs in four JIT modes and 75 unit tests on macOS and in the Linux container.

**Next.** SSE in blocks: the six floating-point primitives, which are 60 to 90%
interpreted. A decision first (which registers of the host hold the guest's, and how
x86's rules for what is not a number are kept), then the entry.

## R019: what the host's arithmetic does where x86's is particular (2026-11-25)

A look at the host before a decision (ADR 0015, SSE in blocks), as R002 was before
ADR 0008.

The card, written before the run.

- **Hypothesis**: x86's SSE and arm64's floating point are both IEEE 754 and give the
  same bits for every result that is a number. They are guessed to differ in four
  places, each of which the interpreter does x86's way by hand already
  (`src/cpu/sse.rs`, held to the real CPU's traces):
  1. a result that is not a number, of operands that were (infinity less infinity,
     nothing by nothing, the root of a negative): x86 gives the one with the sign set
     (FFF8000000000000h), arm64 is guessed to give it with the sign clear;
  2. two operands that are not numbers: x86 gives the first, made quiet; arm64 is
     guessed to prefer a signalling one wherever it stands;
  3. the smaller and the larger of two: x86 gives the second operand when either is
     not a number or the two are equal (zeros of both signs among them); arm64's own
     FMIN and FMAX are guessed to do otherwise;
  4. a float to an integer that does not fit, or is not a number: x86 gives the most
     negative integer for all of them; arm64 is guessed to give the largest, the most
     negative, or zero, as the case is.
- **What is changed from before**: nothing; nothing of r64m is run. A probe of the
  host's own instructions with chosen operands.
- **Next move for each result**:
  - As guessed → ADR 0015 is written for them: where a result can be particular the
    block does not trust the host's instruction alone
  - A fifth difference shows (a result that is a number and differs) → the decision is
    not written until it is understood: it would mean no host instruction can stand
    for x86's
  - The host agrees with x86 somewhere it was guessed not to → that case needs nothing
    special, and the decision is the simpler by it
- **Cut-off**: one probe, in Rust (a test that is not kept), on the Mac; the host's
  instructions are the same on Linux.

The result (MacBook Air, Apple M1; the host's own instructions, by a probe in Rust
that is not kept).

**As guessed, on all four counts; no fifth difference.**

| | x86 (the interpreter, held to the traces) | arm64 |
|---|---|---|
| 1. infinity less infinity, 0 times infinity, 0/0, infinity/infinity, the root of -1 | FFF8000000000000h | 7FF8000000000000h: the sign is clear |
| 2. quiet 1 + quiet 2 | quiet 1 | quiet 1 |
| 2. quiet 1 + signalling 2 | quiet 1 | signalling 2, made quiet: the signalling one is preferred |
| 2. signalling 1 + quiet 2, signalling 1 + signalling 2 | 1, made quiet | 1, made quiet |
| 2. a number and one that is not, either order | that one, made quiet | the same |
| 3. FMIN, FMAX of +0 and -0 | the second operand | -0 for the smaller, +0 for the larger, whichever the order |
| 3. FMIN, FMAX with one that is not a number | the second operand, as it is | the one that is not a number, made quiet |
| 4. to 32 bits: 3e9, 1e19, infinity | 80000000h | 7FFFFFFFh |
| 4. to 32 bits: not a number | 80000000h | 0 |
| 4. to 64 bits: 1e19, infinity / not a number | 8000000000000000h | 7FFFFFFFFFFFFFFFh / 0 |

Multiplying gave what adding gave in every row of 2. Results that are numbers had the
bits IEEE says: 0.1 + 0.2, 1/3, the smallest denormal halved (0), an overflow
(infinity), the root of 2.

Two things the probe showed besides, and both are used in ADR 0015:

- **The smaller and the larger by a comparison and a choice are x86's rule exactly.**
  `fcmp a, b; fcsel r, a, b, mi` (and `gt` for the larger) gave the second operand for
  zeros of either order, the second operand when the first was not a number, and the
  second, not made quiet, when it was the one: all eight rows as `far64` has them.
- **The host's flags after `fcmp`**: less 1000, equal 0110, greater 0010, unordered
  0011 (N Z C V). x86's UCOMISD gives ZF PF CF = 000 greater, 001 less, 100 equal,
  111 unordered. Above, above or equal, below, below or equal and parity each have one
  arm64 condition that is true on exactly the same of the four (GT, GE, LT... see the
  ADR); equal and not equal do not, since x86's ZF is set when unordered and the
  host's Z is not.

What is not shown: single precision (the same instructions with another size; the
tests of the translation will hold it to the interpreter), and the rounding other than
to nearest, which the interpreter does not do for arithmetic either.
