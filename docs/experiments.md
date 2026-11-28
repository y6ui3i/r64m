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

## R020: the scalar SSE instructions in blocks (2026-11-25)

ADR 0015 was accepted as proposed. This is its first part (point 8): the guest's XMM
registers in the host's v0 to v15, and the scalar instructions of points 2 to 6.

The card, written before the run.

- **Hypothesis**: the four floating-point primitives that are SSE (p_float,
  p_dbl_arith, p_dbl_cmp, p_dbl_conv) run at the interpreter's speed because a block
  ends at their first SSE instruction. With the scalar instructions translated they
  run mostly in blocks (over 90% of their instructions) and pass QEMU. The integer
  primitives pay for sixteen more loads going in and sixteen more stores coming out,
  and are guessed not to show it (entries are rare where blocks are joined).
- **What is changed from before** (R018): `enter` loads and stores v0 to v15 and keeps
  the host's d8 to d15; the translator reads the SSE opcodes of ADR 0015's points 2 to
  6; a record of the flags for UCOMIS and COMIS; `--jit-check` compares the XMM
  registers and MXCSR. Nothing of the integer translation is changed.
- **Next move for each result**:
  - The four are mostly in blocks and past QEMU → the packed integers (p_simd), a card
    of its own, by a count of what is executed
  - One of them is still mostly the interpreter's → `--jit-why` says which
    instructions; those are added here if they are scalar SSE that was missed, and
    named as the next card's if they are something else (x87, packed)
  - In blocks but not past QEMU → the code of its inner loop is read
    (`R64M_JIT_DUMP`) before anything is changed
  - An integer primitive is slower by more than the races differ among themselves
    (3%) → the XMM registers are loaded only for blocks that use them, the ADR's own
    "Not yet"
- **Cut-off**: one shorter race (one round first; the 200-round build only where one
  round is under 0.1 s; no run over 15 s), after all 62 programs give the
  interpreter's results in the four modes on both hosts.

The result (MacBook Air, Apple M1; three at once: this build, R018's build from its
commit, g64m with its JIT; the least of three, less a program that does nothing; QEMU's
times are R008's).

**As guessed for the four; the integer primitives showed the loads after all, in one
place, and the card's next move for that was taken here.**

| 200 rounds | in blocks | r64m | R018 | g64m's JIT | QEMU | QEMU / r64m |
|---|---|---|---|---|---|---|
| p_float | 100% | 2.602 s | over 15 s | 3.925 s | 8.283 s | 3.18 |
| p_dbl_arith | 100% | 2.842 s | over 15 s | 4.077 s | 7.847 s | 2.76 |
| p_dbl_cmp | 100% | 1.524 s | over 15 s | 2.094 s | 8.367 s | 5.49 |
| p_dbl_conv | 100% | 2.689 s | over 15 s | 1.799 s | 22.810 s | 8.48 |

At one round the four went from 0.40 to 0.58 s to 0.008 to 0.017 s. The sixteen
integer primitives that were in blocks are each within 0.4% of R018's build (0.998 to
1.004). Twenty of the 23 primitives are now past QEMU, twelve of them at twice QEMU
or past it, and on seventeen of the twenty r64m's JIT is level with g64m's or ahead
(behind: p_loop, p_ternary, and p_dbl_conv by 1.5 times, which is not looked into
here). p_ldbl (x87), p_simd (packed integers: 40% in blocks, 0.417 s to 0.311 s a
round) and p_sha are where they were.

All 62 programs give the interpreter's output, exit code and count in the four JIT
modes, on macOS and in the Linux container; 86 unit tests on both, eleven of them new:
every scalar instruction beside the interpreter on R019's operands in both sizes, from
a register and from memory, each asserted to be in a block.

What happened on the way, in its order:

1. **The cut-off conversion was built without a way out**, not as ADR 0015's point 5
   had it. p_dbl_conv was 86% in blocks at first: a third of its conversions do not
   fit 32 bits (it is `(i32)(x * 1.5)`), and each left for the interpreter. x86's one
   answer for all that does not fit is the smallest integer, which a comparison and a
   conditional choice make in the block. The ADR has a note.
2. **The integer primitives were not slower, but for p_fill: 7.6%** (1.130 s for
   1.050 s). It has a tenth of its instructions in blocks and comes and goes 160,000
   times a round.
3. **A first guess at the cure was wrong.** The XMM registers were loaded and stored
   only once some block of the program used them. p_fill was still 10% slower. A
   second guess (the host's d8 to d15 kept only then) changed nothing either. Sampling
   the two builds showed the way in and out at 18% of the time for 7% before: p_fill
   has a block that uses an XMM register, so "some block uses them" was true for it,
   and every entry paid. This should have been asked of the program before the first
   cure was written, not after the second.
4. **The cure that worked is the card's own**: a block that uses the XMM registers
   begins with a test of a word in the way in's frame, and loads them (and keeps the
   host's d8 to d15) if no block has since the way in; the way out stores them only
   then. A block without SSE begins as before. p_fill: 1.074 s for 1.069 s.

Two things found about the measuring:

- **The races since R017 timed to the nearest 50 ms at worst.** The scratch script
  waited for each program with a timeout, and Python's wait with a timeout polls,
  sleeping up to 50 ms between looks. It showed here as the new and the old build
  taking the same time to the millisecond on most lines. The wait is now without a
  timeout, with a timer that kills. R017's and R018's ratios stand to about 5% on a
  run of a second and are not otherwise put right; this race's are to the
  millisecond. One more reason for the race to be `cargo xtask`, which is still owed.
- **The primitives have their names in the repository now** (`tests/prim.rs`, asked
  for by the owner): each with what its C is made of, and five tests each, the
  interpreter against what is right and the JIT's four ways against the interpreter.
  What the scratch script `jitall` did for them is done by `cargo test`.

## R021: the packed integer instructions in blocks (2026-11-25)

ADR 0015's second part (point 8): by a count of what is executed.

The card, written before the run.

- **Hypothesis**: p_simd has 40% of its instructions in blocks and is 30 times behind
  QEMU a round (0.311 s for 0.010 s) because a dozen or so packed integer opcodes end
  its blocks. Each works on whole numbers, where the host's vector instruction gives
  x86's bits in every case (nothing is "not a number", nothing saturates differently
  unless the opcode says so), so each is translated straight, with no way out. With
  those the count names, p_simd is over 90% in blocks and past QEMU.
- **What is changed from before** (R020): the translator reads the packed opcodes the
  count names; the encoder gains their forms. Nothing else.
- **Next move for each result**:
  - Over 90% in blocks and past QEMU → the stores beside code on 16 KB pages (p_sha),
    which needs a decision first
  - An opcode the count names has no host instruction that is the same in every case
    (a shuffle by a constant, a shift of the whole register by bytes) → it is written
    as several, or left to the interpreter and named here with its count
  - In blocks but not past QEMU → the inner loop's code is read (`R64M_JIT_DUMP`)
    before anything is changed
  - A primitive that was in blocks is slower by more than 1% (the race is to the
    millisecond now) → looked into before the commit
- **Cut-off**: one count, one shorter race (three at once: this build, R020's, g64m's
  JIT), after every test passes on both hosts.

The count (p_simd, one round, the instructions the interpreter ran, by opcode): PMULUDQ
2,359,296; PSHUFD 1,966,088; PSRLQ by a constant 1,441,792; PSRLD 1,179,664; PADDD
917,504; PUNPCKLDQ 802,816; PSLLD 393,224; SHUFPS 393,216; PSUBD 262,152; PCMPGTD
131,072; PADDQ 32,769; PUNPCKHDQ 16,384. The shuffles' constants in its code: PSHUFD
by 08h (twelve places), D8h and 00h; SHUFPS by DDh. The C cases execute a few hundred
of the same kinds (PUNPCKLBW, PADDB, PCMPEQ among them) and nothing else that is
packed.

What was translated: those twelve, and with each its like in the other sizes where
the host's instruction differs only in a field (PADD, PSUB, PCMPEQ, PCMPGT, PUNPCKL
and PUNPCKH of every size; the shifts of 16, 32 and 64 bits). All straight: none has
a way out. PMULUDQ is three of the host's (two narrowings and a widening multiply).
A shuffle by any constant is right: PSHUFD is one instruction where all four are the
same element and else four moves of an element; SHUFPS by DDh and by 88h is one (the
odd, the even elements of both), else four moves. A shift by the element's width or
more is x86's nothing, or the sign everywhere, made without the host's shift. The
shifts of the whole register by bytes are left to the interpreter: one place in
p_simd's code, not in its count.

The result (MacBook Air, Apple M1; three at once: this build, R020's, g64m with its
JIT; the least of three, less a program that does nothing; QEMU's times are R008's).

**As guessed.**

| | in blocks | r64m | R020 | g64m's JIT | QEMU | QEMU / r64m |
|---|---|---|---|---|---|---|
| p_simd, one round | 99.98% (40.2%) | 0.003 s | 0.299 s | | | |
| p_simd, 200 rounds | | 0.473 s | over 15 s | 1.649 s | 2.060 s | 4.35 |

Twenty-one of the 23 primitives are past QEMU, thirteen of them at twice QEMU or past
it, and on eighteen of the twenty-one r64m's JIT is level with g64m's or ahead. Not
past QEMU: p_ldbl (the x87, not in blocks by ADR 0015) and p_sha (stores beside code).

The other primitives are within 0.5% of R020's build but for p_fill, 1.3% to 1.9%
slower in four races (the card's line is 1%). Looked into: its generated code is the
same word for word in the two builds, and the interpreter alone is the same speed
(1.004); the difference is 15 ms over 32 million entries, a cycle and a half each, in
compiled Rust that this change did not touch (the runner's loop), so it is where the
compiler put that code. Not chased. (The commit of the translation was made before
the race, not after the look as the card said; the look changed nothing in it.)

All 62 programs give the interpreter's output, exit code and count in the four JIT
modes, on macOS and in the Linux container; 89 unit tests on both, three of them new:
every opcode from a register, from memory, with itself and with the upper eight
registers on operands chosen for signs and carries; both shuffles by each of the 256
constants; every shift by 16 counts from nothing to 255.

## R022: p_sha with only writable code marked (2026-11-25)

A look before a decision (ADR 0016), as R002 and R019 were.

p_sha's four segments are at 400000h (read), 401000h (read and run: its code),
402000h (read) and 403000h (read and write: its data). All four are in one 16 KB page
of the host's. A block made from the code marks that host page as holding translated
code, so that nothing may write it; every store to the data at 403000h then faults,
leaves generated code, and is made by the interpreter with the page opened for one
instruction (R010): 30,721 times a round. Every program linked with 4 KB pages has
its code's end and its data's beginning this close.

The card, written before the run.

- **Hypothesis**: the mark is not needed for p_sha at all. The guest may not write the
  page its code is on (it is mapped to be read and run), so no store of a correct
  program changes that code, and a block made from it rests on nothing that can
  change. If only code from pages the guest may write is marked, p_sha's stores go
  straight to memory, and p_sha (0.129 s a round, where QEMU would take 0.002 s) is
  past QEMU.
- **What is changed from before** (R021): a build that is not kept, in which a block
  marks its host pages only if one of the guest's pages it lies in may be written by
  the guest.
- **Next move for each result**:
  - p_sha past QEMU, the 62 programs right → ADR 0016 proposes this, with what it
    gives up (a store by generated code to a page the guest may not write, where that
    page shares a host page with one it may: on x86 a fault, here a store that also
    leaves a block stale)
  - p_sha in blocks without faults but not past QEMU → what is left is read from the
    block's code before the ADR is written; the ADR may still propose this
  - A program is not right → what rests on the mark besides stores to code is
    understood first
- **Cut-off**: one build, one race of p_sha and of the primitives that store beside
  code (`--jit-stats` says which), the 62 programs once.

The result (MacBook Air, Apple M1; the build that is not kept, R021's, g64m with its
JIT, three at once; QEMU's time is R008's).

**As guessed.**

| p_sha | stores made beside code | left by a fault | one round | 200 rounds | g64m's JIT | QEMU | QEMU / r64m |
|---|---|---|---|---|---|---|---|
| R021 | 30,721 a round | | 0.119 s | over 15 s | | | |
| only writable code marked | 0 | 0 | 0.002 s | 0.278 s | 0.323 s | 0.437 s | 1.57 |

All 62 programs give the interpreter's output, exit code and count in the four JIT
modes with that build (macOS; the cases that write their own code have it on pages
they may write, and are marked as before). Nineteen of the 23 primitives made one
store beside code each (the first to their data) and p_sha 30,721; p_fill, raced
beside, was 2.8% faster and p_loop the same.

So with this rule 22 of the 23 primitives would be past QEMU on this Mac. The build is
not kept; ADR 0016 proposes the rule.

## R023: ADR 0016 as built (2026-11-25)

ADR 0016 was accepted as proposed.

The card, written before the run.

- **Hypothesis**: the rule built to be kept gives what R022's build gave: p_sha with no
  store beside code and about 1.5 times QEMU, the other primitives where they were
  (p_fill perhaps a little faster), and every program right on both hosts.
- **What is changed from before** (R021): a block is guarded (its host pages marked,
  its pages counted for "is this store to code?") only if the guest may write one of
  the guest's pages it lies in or rests on. The tests of stores to code and beside it
  have their code on a page the guest may write, where those cases still are; a new
  test holds that code on a page it may not write is not marked.
- **Next move for each result**:
  - As R022 → the JIT's list in the README is done but for the x87; the next work is
    the goal's (ADR 0011): what a real program needs before it runs at all
  - p_sha differs from R022's build by more than 2% → the two builds' blocks are
    compared before anything else
  - A test of the old cases cannot be made to hold on a writable page → the case is
    looked at: it may have rested on the mark for something else
- **Cut-off**: one race of all primitives (three at once: this build, R021's, g64m's
  JIT), after every test passes on both hosts.

The result (MacBook Air, Apple M1; three at once: this build, R021's, g64m with its
JIT; the least of three, less a program that does nothing; QEMU's times are R008's).

**As R022.**

| 200 rounds | r64m | R021 | g64m's JIT | QEMU | QEMU / r64m |
|---|---|---|---|---|---|
| p_sha | 0.281 s | over 15 s | 0.327 s | 0.437 s | 1.55 |
| p_fill | 1.103 s | 1.131 s | 1.558 s | 2.883 s | 2.61 |

p_sha is within 1% of R022's build (0.278 s). The other primitives are within 1% of
R021's (0.990 to 1.003); p_fill is 2.5% faster, as it was in R022.

Twenty-two of the 23 primitives are past QEMU on this Mac (1.06 to 8.56 times),
thirteen of them at twice QEMU or past it, and on nineteen of the twenty-two r64m's
JIT is level with g64m's or ahead (behind: p_loop, p_ternary, p_dbl_conv). The one
not past QEMU is p_ldbl: the x87, which is not put in blocks (ADR 0015).

All 62 programs give the interpreter's output, exit code and count in the four JIT
modes, on macOS and in the Linux container; 90 unit tests on both. The tests of a
store to code and of a store beside it had their code on a page the guest may write
already and hold as they were; one new test holds that code on a page it may not
write is not marked, with its data in the same host page written 300 times and no
fault, and that a block resting on code it only read is guarded if that code may be
written.

Not built, by the ADR's point 2: forgetting a page's blocks when the guest makes it
writable. There is no system call that can do it yet; it goes in with the first.

## R024: what a container's programs ask of the kernel (2026-11-25)

The owner chose the container's way to the goal (ADR 0011) and to a race with QEMU on
a build: x86-64 programs of a container image run as processes on the host's own
arm64 Linux kernel, with no guest kernel and no machine. A look before the decision
that orders the work (ADR 0017).

The card, written before the run.

- **Hypothesis**: what r64m lacks for this is counted in system calls, and the list is
  short where it starts. A dynamically linked "Hello, world" in Alpine asks for fewer
  than twenty different system calls; compiling and linking one C file with Alpine's
  GCC (the driver, `cc1`, `as`, `collect2`, `ld`: five programs, started by `vfork`
  or `fork` and `execve`) for fewer than sixty; and none is of a kind that cannot be
  handed on to an arm64 kernel (`ptrace`, `seccomp`, `io_uring`, `modify_ldt`,
  `arch_prctl` beyond setting FS).
- **What is changed from before**: nothing of r64m is run. The real machine (hal, an
  x86-64 Linux) runs the two in an Alpine container with GCC that is on it already,
  under `strace -f -c`.
- **Next move for each result**:
  - As guessed → ADR 0017 orders the work by these lists: what hello needs first,
    then what the compile adds
  - A call of a kind that cannot be handed on shows → it is a point of the ADR for
    the owner
  - Far more calls than guessed → the ADR orders by count of use, and the first
    milestone is still hello
- **Cut-off**: one count of each, on hal.

The result (hal, an x86-64 Linux 7.0; Alpine with GCC 14.2 in a container; the calls
counted by `strace -f -c` on the container's first process; `exit_group` does not
return and is not counted).

**As guessed: fewer than guessed for both, and none of a kind that cannot be handed
on.**

| | different calls | which, by how often |
|---|---|---|
| hello, dynamically linked | 8 (and `exit_group`) | brk 2, mprotect 2, execve, arch_prctl, set_tid_address, mmap, ioctl, writev |
| `gcc -O2 h.c -o h2` | 33 | mmap 835, lseek 655, munmap 611, readv 537, stat 129, open 120, read 93, readlink 89, rt_sigaction 77, close 70, fstat 68, writev 64, fcntl 56, access 45, mprotect 26, rt_sigprocmask 20, brk 20, ioctl 14, prlimit64 9, execve 8, wait4 6, set_tid_address 6, arch_prctl 6, unlink 5, pipe2 4, clone 4, sysinfo 3, umask 2, lstat 2, getrusage 2, getcwd 2, rt_sigreturn, chmod |

Hello opens no file: with musl the dynamic loader and the C library are one file,
which the kernel (here: r64m's loader) maps as the program's interpreter. The compile
is eight programs started by `clone` (musl's `posix_spawn`) and `execve`.

What differs between the two kernels, for the handing on: x86-64 has `open`, `stat`,
`lstat`, `access`, `readlink`, `unlink`, `chmod` where arm64 has only their forms
that take a directory (`openat` and the rest); `stat`'s structure is laid out
differently; `arch_prctl` is x86's own (r64m sets FS itself).

Seen besides, on this Mac, with what is on it already (nothing fetched):

- r64m built for arm64 Linux runs inside the x86-64 Alpine container as it is
  (`docker run --platform linux/amd64 ... /r/r64m --jit p_loop.elf`): an arm64
  program is the kernel's own to run wherever its file is.
- The Docker VM's kernel has 4 KB pages: nothing of 0008's point B applies there.
- That container's own x86-64 programs are run by Rosetta, and a user-mode QEMU
  (`qemu-x86_64`) is in an arm64 image from g64m's work. Both yardsticks are at hand;
  how each is put to the same container's programs is not shown yet.

## R025: hello, dynamically linked, in the container (2026-11-25)

ADR 0017 was accepted as proposed. Its first step.

The card, written before the run.

- **Hypothesis**: with a loader that takes a program not at a fixed address and its
  interpreter, and R024's eight system calls (`brk`, `mmap`, `mprotect`, `arch_prctl`,
  `set_tid_address`, `ioctl`, `writev`, and the exit), a C "Hello, world" compiled by
  Alpine's GCC and linked dynamically prints its line and ends with 0 under r64m in
  the x86-64 Alpine container on this Mac, by the interpreter and with `--jit` and
  `--jit-check`. musl's loader is 600 KB of compiled C that r64m has never run: it is
  guessed to need no instruction r64m lacks.
- **What is changed from before** (R023): `src/elf.rs` takes `ET_DYN` and loads the
  file `PT_INTERP` names; the first stack says where both are; `src/sys.rs` answers
  the eight, memory from r64m's own list and the rest by the host's kernel (Linux);
  a change of mapping or protection forgets the blocks made from those pages (ADR
  0016, point 2).
- **Next move for each result**:
  - It prints and ends with 0 in all three ways → a few of busybox's programs that
    need no file (`echo`, `true`, `false`, `expr`) as the first cases of the Rust
    test, held to what hal prints; then step 2 (the shell, a pipe)
  - An instruction r64m lacks → written in the interpreter, with a case, before
    anything else
  - A system call beyond the eight → answered if it is of R024's list for the
    compile; else looked at
  - Right by the interpreter and not with `--jit-check` → the block that differs is
    the finding: a translation that the primitives never met
- **Cut-off**: hello in the three ways, and the busybox cases, in the container.

The result (this Mac's Docker VM: arm64 Linux 7.2, 4 KB pages; the x86-64 Alpine
container with GCC 14.2 that was on it; r64m built for arm64 Linux, static).

**As guessed: hello ran the first time it was tried, in all three ways.**

`./h` (C, compiled by the container's GCC, dynamically linked against musl) printed
`Hello, world` and ended with 0 by the interpreter, with `--jit` and with
`--jit-check`: 31,517 instructions, most of them the dynamic linker's. No instruction
was lacking and no block differed from the interpreter.

busybox then asked for six calls beyond the eight (it gives up privileges and blocks
signals as it starts): `getuid`, `getgid`, `setuid`, `setgid`, `rt_sigprocmask`, and
`fcntl` for `printf`. Answered: the identities by the host; the set of blocked signals
kept as the program's own and blocked on the host without SIGSEGV and SIGBUS, which
r64m needs; what each signal is to do (`rt_sigaction`) kept and given back, no
handler called; `fcntl` for copying a descriptor and for its flags, with the four
flags of an open file that are numbered differently on the two kernels renumbered.

Twelve cases of busybox with no file but its own (`echo`, `true`, `false`, `expr`
twice, `seq` twice, `basename`, `dirname`, `printf` twice, `factor`) give what hal
printed for the same commands in alpine:3.22, each in the three ways: 36 of 36
(`tests/container.rs`, run inside the x86-64 container; shown as ignored on macOS and
wherever the dynamic linker's file is not).

What was built: `ET_DYN` at a place of r64m's choosing and the interpreter at another
(Linux's, brought under 2^40); the first stack with where the interpreter is and who
the process is, for a program that has one (a static program's stack is as it was:
the traces have the stack pointer in every line); the environment handed on, but for
a trace; `mmap` downwards from under the interpreter, at a fixed address over what is
there, and of a file by reading it into the guest's own memory (private mappings
only); and ADR 0016's point 2: after a call that unmaps, maps over or protects pages,
the blocks made from them are dropped.

99 unit tests on macOS and in the Linux container (nine new: the loader with a
program that can be put anywhere and its interpreter, and each call of r64m's own);
the 62 programs in the four JIT modes on both; the 116 of the primitives and the 39
traces on macOS.

Not shown, and owed to the steps that will meet them: a mapping of a file that is
shared and written; an address freed by `munmap` given out again (they are not: the
reservation is 2^39 bytes below the interpreter, and a long-lived program could use
it up); a handler called; `set_tid_address`'s word cleared at a thread's end.

## R026: the shell, a pipe, files (2026-11-25)

ADR 0017's second step.

The card, written before the run.

- **Hypothesis**: with the calls for files handed to the host's kernel (the x86-64
  forms without a directory made into arm64's with one; `stat`'s structure written
  over), `fork` and `clone` as the host's `fork`, `execve` as r64m started again with
  the new program, and `wait4`, busybox's shell runs commands joined by pipes, and
  its programs read and write files, as on the real machine. What busybox asks for
  beyond R024's lists is not known and is found by running: each call r64m names as
  not known is added if it is of a kind already decided (handed on, or memory), and
  the list is recorded.
- **What is changed from before** (R025): `src/sys.rs` gains the file calls, the
  process calls, time, identity and the terminal's `ioctl`s; r64m takes `--argv0` (a
  program's own name for itself is not always its path) and reads a `#!` line;
  `mremap`.
- **Next move for each result**:
  - The cases are as hal's in the three ways → step 3: GCC compiling and linking one
    file (R024's 33 calls, most of them here by then)
  - A call that is neither handed on as it is nor memory (a structure that differs,
    as `stat`'s does, or something the kernel cannot be asked on the guest's behalf)
    → named in the result, and built if small
  - A child that does not behave (a `fork` with blocks, a pipe that does not close)
    → the finding; `--jit-check` in the child says whether it is the JIT's
  - A case differs from hal's only in what the host is (`uname -m`, the clock) → it
    is not a case; said so
- **Cut-off**: the cases chosen before the run (files read and listed, files
  written and removed, the shell with a loop, a pipe of three programs, a command's
  exit code, a script with `#!`), in the container.

The result (this Mac's Docker VM, the x86-64 Alpine 3.22.8 container with GCC; what
is right is what hal printed for the same commands in the same image's contents).

**As guessed, and with no call left unknown: every case as the real machine has it
the first time the whole was run, in the interpreter and with `--jit`. `--jit-check`
found one thing, which was the check's own.**

Twenty-four commands given to busybox's shell: a file read (`cat`, `sed`, `wc`,
`md5sum`, `od` of `/dev/zero`), directories listed, a file written, read back and
removed, a directory made, entered and removed, loops, `&&` and `||`, a command's
exit code, a command substitution, pipes of two and of three programs, `awk`, a
script with `#!` made and run, a shell in a shell, `uname -m` (x86_64, by r64m's
answer), `readlink /proc/self/exe` (the program, not r64m). All 24 give hal's output
and exit code by the interpreter, with `--jit`, with `--jit-check`, and with
`--jit-check --jit-now`. With R025's twelve: 36 cases, 108 of 108 in
`tests/container.rs` (three ways each).

**What `--jit-check` found.** `md5sum` stopped with "the block of 64 instructions at
55555E7B54h: RFLAGS 286h -> 287h". MD5's rounds are a long straight run, and the 64th
instruction of a block (the most a block holds) was `ror edx, 11`. A rotation is
translated only where its flags are written over before anything reads them, and
that was found true by reading on: the next instruction but one is an `add`. But the
`add` is in the next block, and the check compares the flags at each block's end. So
the result was right (with `--jit` the sum is hal's) and the check cried out for
flags nothing reads. Under the check a rotation's flags must now be written over
inside its own block, or the rotation is the interpreter's; a test holds it (a block
of 63 moves, a rotation, an addition). The primitives never had a rotation at a
block's 64th place.

What was built: a table of 130 or so of x86-64's calls, each made arm64's: the same
call where there is one, the form that takes a directory where arm64 has only that
(`open`, `stat`, `lstat`, `access`, `readlink`, `unlink`, `rename`, `mkdir`, `rmdir`,
`chmod`, `chown`, `link`, `symlink`, `pipe`, `dup2`, `poll`, `getrlimit`, `time`);
`stat`'s structure written over from the host's; lists of pieces (`readv`) made the
host's; `uname` with the machine said to be x86_64; `/proc/self/exe` answered with
the program; `fork`, `vfork` and `clone` without shared memory as the host's `fork`
(for `vfork` the parent waits at a pipe that the kernel closes when the child starts
a program); `execve` as r64m started again with its own options and `--argv0`, after
the checks the kernel would make (there, runnable, a file, an ELF or a `#!`); a `#!`
line read by r64m; a signal that is to be ignored or left alone made so on the host,
and what the process was given (ignored and blocked signals) read at the start;
`mremap`.

102 unit tests on macOS (new: the rotation, the `#!` line, `mremap`), and 101 in the
Linux container, where they were run before the test of `mremap` was added; the 62
programs in the four JIT modes on both; the 116 of the primitives
and the 39 traces on macOS.

Not shown: the calls of the table that no case reaches (the cases reach those a
shell and its tools use; the rest are there by their kind and are met, or not, by
GCC next); `stat`'s structure has no test of its own beyond `ls -l` and `wc -c`
being right; `vfork`'s wait (busybox's shell forks); two cases at once in one
directory (the test runs those that make files one at a time).

## R027: GCC compiling and linking one file (2026-11-25)

ADR 0017's third step.

The card, written before the run.

- **Hypothesis**: the container's GCC 14.2 compiles and links a C file under r64m,
  and what it makes is byte for byte what it makes on the real machine: the object
  file of `gcc -O2 -c` and the linked program have hal's MD5 sums, and the program
  runs. R024 counted 33 different calls for this, all in R026's table but `sysinfo`
  and `getrusage`'s like, which are there by their kind and not yet reached. `cc1` is
  25 MB of compiled C++ that r64m has never run: an instruction r64m lacks is the
  likeliest thing to be met, and the interpreter says which (`not written yet`).
- **What is changed from before** (R026): nothing until something is met.
- **Next move for each result**:
  - The sums are hal's in the three ways → step 4's ground: NetHack's sources in the
    container and its build by `make` under r64m, first for being right (the sums
    of its object files), then timed beside user-mode QEMU and Rosetta
  - An instruction r64m lacks → written in the interpreter with a case of its own
    (its flags and results from the real CPU, by hal), before going on
  - A call not known or a structure that differs → added by its kind; named here
  - The sums differ → the first object that differs is found (`cc1`'s assembly
    output, then `as`'s), and `--jit-check` says whether a block is wrong
  - Right by the interpreter and too slow to be used (`cc1` under the interpreter
    is guessed at minutes) → said; the race is the JIT's anyway
- **Cut-off**: one C file compiled and linked in the three ways, and a second file
  that is not trivial (a few hundred lines with floating point) the same.

The result (this Mac's Docker VM, the x86-64 Alpine 3.22.8 container with GCC 14.2;
what is right is what hal made and printed with the same image's contents).

**As guessed, and with nothing met: GCC compiled the first time it was tried, and
what it makes is byte for byte the real machine's.**

Nothing was changed in r64m for this step: no instruction was lacking in `cc1`, `as`,
`collect2` or `ld`, and no call was unknown.

| made under r64m | MD5 | hal's |
|---|---|---|
| `gcc -O2 -c h.c` | aa5ffb9a19610afd34b034d8dd385322 | the same |
| `gcc -O2 h.c -o h` | 38b2bef03df1db4b08dd7ece61f8276e | the same |
| `gcc -O2 -c big.c` | c3b65411dceaae2f2887a27dc9b411b0 | the same |
| `gcc -O2 big.c -o big -lm` | 85dd6a0f2a2b0c28d539c3e043b2653e | the same |
| `gcc -O0 -g big.c -o big0 -lm` | 7ba7966e95679e9cc155267997410ebd | the same |

`big.c` is 140 lines written for this: structures, bodies moved by one another's
gravity, `qsort`, 8 by 8 matrices, `sin`, `cos`, `exp`, `log`, `pow`, `atan2`, text
cut into words, `malloc` and `realloc`, `printf` of doubles in four forms. The three
programs made were run under r64m and print what they print on hal, to the last
digit. All of it the same by the interpreter, with `--jit` and with `--jit-check`.

Compiling hello is 95 million instructions in three programs (`cc1` 89.9 million,
`as` 3.5, the driver 1.2). The five builds and three runs together took 9.8 s with
`--jit`, 33.2 s by the interpreter and 173 s with every block checked (one run each,
with Docker's own start; not a race: that is step 4).

Six cases are kept in `tests/container/gcc.txt` with hal's output: an object file's
sum, a linked hello's sum and its run, a program with the mathematics library built
with `-O0 -g` (its sum, its output, its exit code), two files compiled and put
together through `ar`, the compiler's version, the preprocessor. With R025's and
R026's: 42 cases, 126 of 126 in the three ways.

A mistake in the test, found by these cases failing: the cases that make files are
run one at a time, and which those are was told by "/tmp/" in the command; the new
ones say `cd /tmp`. Five failed with two ways at once in one directory. Told by
"/tmp" now.

`big.c` is not kept: the repository takes no C (ADR 0012), and whether a C file that
is only what a compiler under test is given counts is the owner's to say. The kept
cases write their few lines of C from the shell.

102 unit tests on macOS and in the Linux container (`mremap`'s there too now); the
62 programs in the four JIT modes on both; the 116 of the primitives and the 39
traces on macOS.

## R028: NetHack built by make under r64m (2026-11-25)

ADR 0017's fourth step, its first half: the build is right before it is raced.

NetHack 3.6.7's sources are g64m's oracle's copy on hal (`nethack-367-src.tgz`, 5.6 MB,
SHA-256 98cf67df...2aacb2; copied from there, not fetched). The image is the Alpine
3.22.8 with GCC 14.2 of the steps before, with Alpine's `ncurses-dev` added (fetched
from Alpine's mirror with the owner's word, 2026-11-25) and the sources unpacked and
set up for Linux. The same image is put on hal, so that both build the same thing.

The card, written before the run.

- **Hypothesis**: `make all` in that tree runs to its end under r64m with `--jit`
  (some hundred compilations, an archive, three programs of NetHack's own that are
  built and then run to make its data, and the link), and every file it makes has
  the MD5 sum of the one hal makes, but for those that hold the time of the build.
  It is guessed to take several times what Rosetta takes for the same `make`: each
  compilation is a new process whose blocks are made from nothing.
- **What is changed from before** (R027): nothing until something is met.
- **Next move for each result**:
  - It ends and the sums are hal's → timed three times beside Rosetta (the same
    container, one after the other and then at once), and where r64m's time goes is
    counted (`--jit-stats` of one compilation: translated, run, interpreted) before
    anything is built to make it less. The race with QEMU waits for the owner's
    switch of the Docker VM from Rosetta to QEMU
  - It stops (a call, an instruction, a signal that was to be delivered: `make`
    waits for its children and may want SIGCHLD) → that is built, by its kind
  - A file differs that should not → the first one in the build's order is the
    finding; `--jit-check` on that one compilation says whether it is a block
- **Cut-off**: one whole build compared file by file; three timed builds of each.

The result (the image as built: the test for the Linux text console taken out of
`unixtty.c`, since the kernel's headers are not in the image; Alpine's fortified
headers off, since the curses interface does not compile with them; the Guidebook
said to be made, since its making needs nroff. hal builds the same image's tree.)

**The build is right. And it is 27 times the real machine's time: far too slow, as
the owner said while it ran.**

`make all` under `r64m --jit` ended with 0. Of the 284 files it made (137 object
files, the three programs of NetHack's own that make its data, the levels, the
archive of data, `nethack` itself) 281 have hal's MD5 sums. The three that have not
are the three that hold the time of the build: `include/date.h`, `src/version.o`,
`src/nethack`. Nothing was changed in r64m to get there.

| `make all` | time |
|---|---|
| hal (i7-3770K, the real machine, in the container) | 111 s |
| r64m `--jit` (MacBook Air M1, the Docker VM) | 3,008 s |

One compilation, to see where it goes (`cc1` on `apply.c` after the preprocessor:
10,831 lines, 7.07 thousand million instructions; the assembly made is the same):

| `cc1 -O2 apply.i` | time | |
|---|---|---|
| Rosetta | 2.0 s | |
| r64m `--jit` | 47 s | 150 million instructions a second |
| r64m, the interpreter | 128 s | 55 million |

With `--jit`: 168,160 blocks made, the 16 MB of generated code filled and emptied
once, 97.4% of the instructions run in blocks, and generated code entered 92 million
times: once for every 77 instructions. The primitives are entered a few thousand
times in thousands of millions. What the interpreter ran in the blocks' stead, by
opcode (`--jit-why`):

| | times | what it is |
|---|---|---|
| `mov r, fs:[...]` (64 48 8B) | 27.1 million | the stack protector's word read at a function's start |
| `sub r, fs:[...]` (64 48 2B) | 27.1 million | and compared at its end |
| `jne` after it (0F 85) | 24.8 million | |
| `bt r/m64, r64` (48 0F A3) | 16.2 million | a bit of a set tested (GCC's sets are bits) |
| `jnc`, `jc` after it (73, 72) | 14.2 million | |
| `pslldq` (66 0F 73 /7) | 4.9 million | an XMM register moved by bytes |
| `xchg ax, ax` (66 90) | 3.1 million | a no-operation of two bytes |
| `bt`, `bts`, `btr` in other forms | 4.5 million | |

None of these is in any primitive. Every function compiled with the stack protector
(Alpine's GCC does so by default, and was itself built so) leaves generated code
twice. That is the first thing: R029.

## R029: the instructions that end cc1's blocks (2026-11-25)

The card, written before the run.

- **Hypothesis**: `cc1` leaves generated code once in 77 instructions because of a
  handful of instructions no primitive has (R028's table), the stack protector's two
  above all. With an operand relative to FS or GS, `bt`, `bts`, `btr`, `btc` of a
  register (by a register and by a constant), the no-operation of two bytes, and the
  XMM shifts by bytes translated, generated code is entered at most a tenth as often
  (under 9 million times for `apply.c`) and `cc1` takes under a third of its 47 s.
  It will not be near Rosetta's 2 s by this alone: 168,160 blocks are made in one
  compilation, and what making them costs is not known yet.
- **What is changed from before** (R028): the translator reads those; the bit tests
  leave the host's flags saying what the carry is, and are translated where the
  other flags are written over before anything reads them, as a rotation is. The
  room for generated code is 64 MB for 16 (it was filled once).
- **Next move for each result**:
  - Entered a tenth as often, time under a third → what is left is counted again
    (`--jit-why`, and the time of making blocks apart from running them) and that is
    the next card; then the whole build again
  - Entered a tenth as often and the time hardly less → the time is not in the
    leaving: the making of blocks is measured next (a count of blocks made by time)
  - Not a tenth → the table says what else; added if it is of the same kind
- **Cut-off**: `cc1` on `apply.i` once with `--jit-stats` and once with `--jit-why`,
  after every test passes on both hosts and the 42 cases in the container.

The result (this Mac's Docker VM; `cc1 -O2` on `apply.i` as in R028; then the whole
build, once each).

**The instructions were as guessed and were not where the time was. The time was in
r64m's own keeping, and that was mine: R025's.**

| `cc1 -O2 apply.i`, r64m `--jit` | entered | blocks | in blocks | time |
|---|---|---|---|---|
| R028 | 92.1 million | 168,160 (emptied once) | 97.4% | 47 s |
| with the instructions translated | 14.0 million | 99,756 | 99.2% | 48 s |
| and a change of mapping elsewhere costing nothing | 14.0 million | 99,756 | 99.2% | 4.9 s |

Rosetta: 2.0 s. The assembly `cc1` writes is the same each time.

1. **The instructions.** FS- and GS-relative operands, `bt`/`bts`/`btr`/`btc` of a
   register (translated where only the carry is asked for before the flags are
   written over, as a rotation is; a jump on the carry asks the host's flags),
   `pslldq`/`psrldq`, and `xchg ax, ax`. Generated code was entered 6.6 times less
   often, not ten, and the time did not move: the card's second line. So the time
   was not in the leaving.
2. **Where it was.** `--jit-stats` was given two clocks: 0.35 s making blocks, 4.4 s
   in generated code (1,600 million instructions a second: as the primitives), of
   48 s. The rest was `Jit::guest_changed`, built in R025 for ADR 0016's point 2:
   after every call that maps, unmaps or protects memory it walked every place
   known (hundreds of thousands) to find blocks made from the pages changed. `cc1`
   maps memory many thousand times, nearly always for data. Now the pages anything
   is known of are kept in an ordered set, and a change that touches none of them is
   seen so at once. A test holds both sides (a change elsewhere keeps the block; the
   code's page mapped anew drops it).
   - What should have caught it: R025 and R026 ran programs of a few million
     instructions and no time was looked at. A walk through everything, in a path
     that every `mmap` takes, was written without a count of how often `mmap` is
     called. R024 had the count (835 in one small compile).

The whole build, `make all` of NetHack 3.6.7, once each:

| | time | files that differ from hal's |
|---|---|---|
| hal (i7-3770K, the real machine) | 111 s | |
| Rosetta (this Mac, the same container) | 168 s | the three that hold the time |
| r64m `--jit`, R028 | 3,008 s | the same three |
| r64m `--jit`, now | 256 s | the same three |

r64m takes 1.52 times Rosetta's time for the build, and builds the same files. One
run each: not yet a race (three runs, and at once, with QEMU, is the next card; QEMU
needs the Docker VM switched from Rosetta by the owner).

What is left in one compilation, by the last `--jit-why`: `bt` in the forms not
translated (where the other flags are read, 4.9 million) with the `jnc` after it;
`movhps` (0F 16); the string instructions with a repeat count; and 14 million
entries. And of the 4.9 s, about 2 are neither generated code nor the making of
blocks.

106 unit tests on macOS and 105 in the Linux container (the last was added after);
the 62 programs in the four JIT modes on both; the 42 container cases in three ways.

## R030: NetHack's build, r64m beside user-mode QEMU (2026-11-25)

ADR 0017's fourth step, its second half. The owner switched the Docker VM (OrbStack)
from Rosetta to QEMU for it: an x86-64 program in a container is now run by
OrbStack's own `qemu-x86_64` 9.2.2, started by the kernel for every program (a
`binfmt_misc` handler), as Docker does it anywhere without Rosetta. r64m is run as
before, by its own name, and starts itself again for each program.

The card, written before the run.

- **Hypothesis**: on `make all` of NetHack 3.6.7 in the same container r64m with
  `--jit` (256 s once, R029; Rosetta 168 s) is faster than QEMU. Guessed from the
  primitives, where r64m is 1 to 8 times QEMU; but a build is a few hundred
  processes that each translate from nothing, and there QEMU's translator, which is
  quick to make code that is slow, may do better than on a long loop. So: guessed
  faster, by less than on the primitives.
- **What is changed from before** (R029): nothing of r64m. The yardstick is there.
- **Next move for each result**:
  - r64m faster in all three pairs → said, with by how much; then what is left of
    one compilation (R029's list) is what makes it more
  - Level (within 10%) or slower → where r64m's time goes in a short process is the
    next card (blocks made and never run again, the 16 arrivals before a block is
    made, the table), measured before anything is changed
  - QEMU's build is not right (its files' sums not hal's) → said; the race is then
    of builds that are not the same thing
- **Cut-off**: three builds each, one at a time and in turn (QEMU, r64m, QEMU, ...),
  the least and the middle of each given; then one pair at once. Each build's files
  compared with hal's. If one build is over 20 minutes, one of each and not three.

The result (MacBook Air, Apple M1; OrbStack's VM with 8 cores given, arm64 Linux 7.2,
4 KB pages; the x86-64 Alpine 3.22.8 container of R028; QEMU is OrbStack's
`qemu-x86_64` 9.2.2 as the kernel's handler; `make all` with one job; the seconds are
`make`'s own, by the container's clock to the second).

**As guessed, and by more than guessed: r64m builds NetHack in 0.45 of QEMU's time,
2.2 times as fast, in every pair.**

| `make all`, in turn | 1 | 2 | 3 | least | middle |
|---|---|---|---|---|---|
| QEMU 9.2.2, user mode | 590 s | 592 s | 594 s | 590 s | 592 s |
| r64m `--jit` | 266 s | 270 s | 272 s | 266 s | 270 s |
| QEMU / r64m | 2.22 | 2.19 | 2.18 | 2.22 | 2.19 |

Both at once, one pair: r64m 292 s, QEMU 624 s (2.14; QEMU had the machine to itself
for its second half, so this pair says only that neither was much slowed by the
other).

Every one of the eight builds made the 284 files with hal's sums but for the three
that hold the time of the build. So the two do the same work, and QEMU does it right.

Beside them, from R029 and R028, one run each: Rosetta 168 s; hal, the real machine
(an i7-3770K), 111 s. r64m takes 1.6 times Rosetta's time.

One compilation alone (`cc1 -O2` on `apply.i`, once): QEMU 9.6 s, r64m 3.9 s, Rosetta
2.0 s: r64m 2.4 times QEMU there. The whole build's ratio is a little less than one
long compilation's: the short processes (the driver, `as`, the shell, `make` itself)
are where r64m starts from nothing each time.

So the aim (twice QEMU) is met on this, the first real work measured in a container.
It is not met on ten of the 23 primitives (R023), and `p_ldbl` is behind QEMU.

Not measured: `make -j`; a second run of Rosetta (the VM is now on QEMU: it is
switched back by the owner); how much of r64m's 266 s is generated code and how much
its own keeping, over the whole build (for one compilation it was 2.75 s of 4.9 s,
R029).

## R031: where a compilation's time goes outside generated code (2026-11-25)

The first principle (ADR 0018) asks that r64m's time be spent behaving as x86-64. In
one compilation (`cc1 -O2` on `apply.i`, 4.9 s with the clocks on, 3.9 s without) the
clocks of R029 say 2.75 s is in generated code and 0.27 s making blocks. The rest,
between one and two seconds, is not accounted for.

The card, written before the run.

- **Hypothesis**: the rest is two things of about the same size: the runner's work
  each time generated code is entered (14 million times: a look in the map of
  places, the block's first run from Rust, joining), and the interpreter for the 56
  million instructions that are still its own, each of which under the JIT also
  keeps a copy of the registers and asks the map. Which is the larger is not known,
  and neither is whether something else is there, as `guest_changed` was.
- **What is changed from before** (R030): a sampler in r64m (`--profile`): a timer
  that interrupts a thousand times a second of CPU time and notes where r64m was;
  at the end the places are printed with their counts, and how many were in
  generated code. Nothing else.
- **Next move for each result**:
  - One function of r64m's own keeping is a large share → that is cut first, and
    the profile taken again
  - The interpreter's own instructions are the share → the instructions `--jit-why`
    names are translated, by count
  - It is spread thin over many places → said; then the gain is in entering less
    often (the table's size, calls and returns joined), measured by entries
- **Cut-off**: one profile; after each change one more and the time without the
  profile, three runs, least.

The result (this Mac's Docker VM, on QEMU as in R030; `cc1 -O2` on `apply.i`; the
time is CPU time by the sampler's own count, a thousand to the second, the least of
three, the two builds in turn).

**As guessed in kind, and by the card's second line: the interpreter was the larger
share, with nothing hidden as `guest_changed` was. The time outside generated code
went from 1.51 s to 0.85 s; the whole build from 266 s to 203 s.**

The first profile (3.90 s of CPU):

| | share |
|---|---|
| generated code | 61% |
| the interpreter (`exec`, `step`, the flags worked out, the look in the list of regions) | about 17% |
| the runner's loop and the way in and out | about 7% |
| making blocks (reading, translating, writing, joining) | about 5% |
| the guest's `mmap` and `munmap` on the host | about 3% |
| the set of known pages (R029's) | 1% |

What was changed, in the order it was found:

1. **A bit test is always in its block.** It changes the carry and leaves the other
   flags, which a record could not say; so it was translated only where the other
   flags could be shown to be written over, and `cc1` tests bits where that cannot
   be shown 4.9 million times (and the `jnc` after it as often). A record can now
   say "and the carry was given since" (two bits above its kind; with no record
   before, the kind is "the flags as RFLAGS has them"). Six host instructions more
   where the flags are not dead, none where they are. Generated code was entered 6.6
   million times for 14.0.
2. **`movhps`, `movhpd` from memory and `movlhps`** (half a million).
3. **A block is made after four arrivals, not sixteen.** What was left to the
   interpreter was then mostly not instructions it alone knows but the sixteen runs
   of each block before it was made. Measured, three runs each, CPU milliseconds:

   | arrivals | `apply.i` (7.07 thousand million instructions) | hello's `cc1` (90 million) |
   |---|---|---|
   | 1 | 3016 | 141 |
   | 2 | 3008 | 108 |
   | 3 | 3010 | 103 |
   | 4 | 3058 | 101 |
   | 8 | 3261 | 102 |
   | 16 | 3516 | 107 |

   Making a block costs 2.6 microseconds, about what interpreting it twenty times
   does; 16 was g64m's number, taken over unmeasured (ADR 0014 said so).
4. The set of known pages is written only when a place is new, not at every arrival.

| `cc1 -O2 apply.i`, CPU | all | in generated code | outside it |
|---|---|---|---|
| R030's build | 3.87 s | 2.36 s | 1.51 s |
| now | 3.05 s | 2.20 s | 0.85 s |

The whole build, `make all` of NetHack, twice: 211 s and 203 s, where R030 had 266 s
to 272 s; QEMU's 590 s is 2.9 times the least. Every file as hal's but the three with
the time. (Rosetta: 168 s, one run, R029.)

What `--jit-why` names now is thin: the string instructions with a repeat count
(1.5 million, each a leaving and a coming back), `dec` and `push` through an operand
(FF /1, /6: a quarter of a million each), `jo`, `movlps`, `shufpd`, the LOCK-prefixed
`or` and `cmpxchg`; the rest of the list is ordinary instructions run by the
interpreter after one of these, until the next jump.

Not done: the timings are of one file and one build; the primitives were not raced
again after the change of arrivals (their loops run thousands of millions of times
and are made in the first microseconds either way; the 62 programs give the same
results).

106 unit tests and the 62 programs in the four JIT modes on both hosts; the 42
container cases in three ways.

## R032: what Python and a server ask for beyond a build (2026-11-25)

ADR 0017's fifth step is threads, signals delivered and sockets, for the goal's
programs (0011: nginx, Python with FastAPI). How they are built is a decision (0017
left it: "threads share blocks: how?"). A look before it, as R024 was before 0017.

The card, written before the run.

- **Hypothesis**: three things are guessed, each to be shown or not by the real
  machine's count (hal, `strace -f`, with the images on it: `python:3.13-slim`,
  which is Debian with glibc, and `redis:alpine`, a server with threads):
  1. Python printing a line starts no thread and is delivered no signal; what it
     asks beyond the build's calls is small (and glibc asks the CPU what it is,
     which musl does not: `cpuid`);
  2. a Python program that starts threads uses `clone` (or `clone3`) with shared
     memory, `futex`, and `set_robust_list`, and nothing else new: so a guest thread
     can be a host thread with the guest's `futex` handed on as it is;
  3. a server that waits for connections uses `epoll` and sockets, whose calls are
     handed on but for `epoll`'s structure, which is packed on x86-64 and not on
     arm64; and a signal reaches a handler only when the server is told to stop.
- **What is changed from before**: nothing of r64m is changed for the count. Then
  each program is tried under r64m as it is (the Python image copied from hal to
  this Mac, not fetched), to see what stops it first.
- **Next move for each result**:
  - As guessed → ADR 0019 proposes: a guest thread is a host thread with its own CPU
    state and its own blocks; signals are noted by the host's handler and delivered
    between blocks; and the order is by what the goal's programs meet first
  - Python's line needs threads or a delivered signal → the order changes: that
    first
  - A call of a kind not decided (a structure that differs, memory shared between
    processes) → named in the ADR as a point
- **Cut-off**: four counts on hal (Python's line, Python's threads, Python's own
  HTTP server answering one request, redis-server starting and answering one PING),
  and one try of each under r64m.

The result (hal: `strace -f -c` of each program's process in its container; then each
tried under r64m in this Mac's Docker VM, the images copied from hal).

**As guessed on all three counts.**

| on hal | different calls | what is new beyond the build's |
|---|---|---|
| Python 3.13 printing a line | 38 | `futex` (1), `getrandom`, `set_robust_list`, `rseq`, `gettid`; no `clone`, and no signal but the shell's own |
| Python starting four threads | 40 | `clone` 4 and `clone3` 1, `futex` 111, `set_robust_list` 5, `rseq` 5, `madvise` |
| Python's HTTP server answering one request | 55 | `socket`, `bind`, `listen`, `accept4`, `connect`, `sendto`, `recvfrom`, `getsockname`, `getpeername`, `setsockopt`, `shutdown`, `epoll_create1`, `poll`; a thread |
| redis-server started, one PING, stopped | 57 | `epoll_pwait` 16, `epoll_ctl` 7, `clone` 5 (threads), `fork`, `prctl`, `setitimer`, `membarrier`, `rt_sigreturn` 4: a timer's signal reaches a handler |

Under r64m as it was, Python stopped twice before its line, neither time for a
thread or a signal:

1. "CPU ISA level is lower than required": glibc will not start on a CPU that does
   not say MMX, which every x86-64 CPU has, and r64m's CPUID did not say it. It says
   it now. **MMX's registers and instructions are not written**: owed, when a
   program uses them (compiled x86-64 code uses SSE2 in their place).
2. "The futex facility returned an unexpected error code": glibc calls `futex` with
   one thread. It is handed on: the word waited on is the guest's memory, which is
   the host's, so the kernel's futex at `base + g` is the guest's futex.

Then `python3 -c 'print("Hello, world"); ...'` (a line, pi, 2 to the 100th, a third,
a sorted set) prints what it should and ends with 0 by the interpreter, with `--jit`
and with `--jit-check`: 46.6 million instructions, glibc's loader and CPython's start.
`set_robust_list` and `rseq` are answered ENOSYS, which glibc takes.

The threads program stops where it was guessed to: `clone3` and then `clone` with
shared memory are refused, and Python says "can't start new thread". That is ADR
0019's to decide.

## R033: threads (2026-11-25)

ADR 0019 was accepted as proposed (A, B, C). Its first step: a guest thread is a host
thread. Beginning it, a thing the proposal had missed was seen and added as point D
(proposed): x86 orders memory between threads more strictly than arm64, and plain
loads and stores in blocks do not keep that order. This card builds threads without
it; whether a program is right for the right reason with more than one thread is not
claimed until D is decided.

The card, written before the run.

- **Hypothesis**: with `clone` with shared memory starting a host thread that has its
  own CPU state and its own blocks, the list of regions shared under a lock, and the
  LOCK-prefixed instructions made atomic in the interpreter, R032's Python program
  (four threads that each sum, joined, the sums printed sorted) prints what hal
  printed, and redis-server starts, answers PING and stops, in the interpreter and
  with `--jit`. Run twenty times each, since what is wrong with threads is wrong
  only sometimes.
- **What is changed from before** (R032): `Mem` is a thread's view of a shared list;
  `Sys` keeps what is the process's (the end of the heap, where mappings go, what
  signals do) behind a lock; `clone`; the exits (`exit` ends a thread, `exit_group`
  the process); the interpreter's LOCK. Nothing of the blocks' code.
- **Next move for each result**:
  - Right twenty times of twenty in both ways → sockets and `epoll` (0019's order),
    and D's cost measured beside
  - Wrong or hung sometimes → the instruction or call is found before anything is
    added; a hang's threads are looked at where they wait
  - redis needs what is not built (a signal delivered: its timer) → said; it waits
    for 0019's third step and Python's threads stand alone for this card
- **Cut-off**: the two programs, twenty runs each way.

The result (this Mac's Docker VM, on QEMU for its own x86-64 programs; the Python and
redis images copied from hal; what is right is what hal printed).

**As guessed for Python's threads: twenty of twenty, by the interpreter and with
`--jit`. redis waits for sockets, by the card's third line.**

| Python's four threads, each summing, joined, the sums printed sorted | right |
|---|---|
| `--jit` | 20 of 20 |
| the interpreter | 20 of 20 |

Nine Python cases are kept (`tests/container/python.txt`, with hal's output): a line;
arithmetic of floats and of large integers; a hash, JSON and a regular expression;
four threads started and joined; a pool of eight threads over 2,000 tasks; a lock
taken 160,000 times by eight threads, the count exact; a child process run and its
output read; an exit code. All nine as hal's in the three ways: 27 of 27 in the
Python container, and the 42 cases of before in the Alpine one (126 of 126).

What was built:

- `clone` with shared memory starts a thread of the host's with the registers the
  guest gave the child, its own blocks, and a handle of its own on the memory; its
  id is the host's; at its end the word the guest named is cleared and the waiter
  woken. `exit` ends a thread, `exit_group` the process.
- The list of regions is one for all threads: each has a copy and takes it anew when
  the shared one's number has moved; a change is made holding the shared one.
- What is the process's for system calls (the heap's end, where mappings go, what
  each signal is to do) is behind a lock that is never held across a call that waits.
- **The LOCK prefix means something now.** The interpreter writes the operand only if
  it is still what it read (the host's compare-and-swap) and runs the instruction
  again if not; `xchg` with memory the same. A test: four threads each do 50,000
  `lock add`, `lock inc` and `lock sub` on the same words, and every one is counted.
- A store by one thread into guarded code is counted with the memory; the other
  threads drop their guarded blocks when they next come by.

Found on the way:

1. **A handle on the memory freed the whole reservation when it was dropped.** With
   one thread there is one handle. A test with four threads ended r64m with a fault
   the moment the first thread's handle went. The reservation now goes with the last
   handle. (In a program's run a thread ends by the kernel's `exit` and drops
   nothing: the test met what a run would not have.)
2. **`--jit-check` cannot hold a block to the interpreter when there are threads**:
   what another thread writes between the two runs makes them differ rightly ("RDX
   31h -> 35h" in Python's own counter). With more than one thread blocks are no
   longer checked.
3. **`madvise` "not needed"** was answered "done" and nothing was done; an
   allocator (redis's) tests that the pages read as nothing afterwards, found that
   they did not, and said so. It is handed to the kernel now: the pages are the
   host's. (A mapping of a file, which r64m holds as a copy, would read as nothing
   where Linux reads the file again: not met.)
4. **The test's data was wrong once**: a case's line was written out by the shell's
   `echo` on hal, which turned `\2` into a byte. The lines are taken from the file
   they were typed in now. r64m was right.

redis-server stops at its first socket (`socket`, `epoll_create1`: not known yet):
ADR 0019's second step.

**Not claimed**: that a program with threads is right for the right reason. Blocks
load and store plainly, and arm64 does not order plain loads and stores between
threads as x86 does (ADR 0019, point D, proposed). The cases above pass because they
share memory through locks, which are LOCK-prefixed and so atomic and ordered.

109 unit tests and the 62 programs in the four JIT modes on both hosts.

## R034: memory ordered between threads as x86 orders it (2026-11-25)

ADR 0019's point D was accepted (「X86_64 に合わせて」).

The card, written before the run.

- **Hypothesis**: once a process has a second thread, its blocks can load with
  `ldapr` and store with `stlr` (the host's load that sees what was released and
  store that releases), and the interpreter the same, at a cost that is paid only
  then: guessed at between a tenth and a half more time for a program with threads,
  nothing for one without. The address of such an access must be in one register, so
  a host register is freed for it (the way out's address moves to the thread's
  state; every leaving for Rust then loads it), which every program pays for: guessed
  not to show.
- **What is changed from before** (R033): the encoder's two forms; x24 is the
  address of an ordered access and no longer the way out; every load and store of
  the guest's memory in a block goes through one place that writes it plain or
  ordered; a thread's blocks are dropped and made anew, ordered, when the memory is
  first shared; the interpreter's reads and writes of the guest's memory are ordered
  when it is shared. An ordered access that the host refuses (not aligned, on a host
  without the newer rule for that) leaves the block for the interpreter, as any
  fault does.
- **Next move for each result**:
  - The tests pass, programs with one thread are within 1% and those with threads
    cost what was guessed → sockets and `epoll` (0019's order)
  - A program with one thread is slower by more than 1% → the way out's address goes
    back to a register and another is freed (the count of instructions: it is
    written to memory at each leaving anyway)
  - Threads cost more than half again → the processor's own x86 order is looked at
    where the host has it, a card of its own
- **Cut-off**: all tests on both hosts; the primitives raced against R033's build
  (three at once, least of three); the Python cases with threads timed plain and
  ordered, five runs each.

The result (this Mac's Docker VM; CPU time by the sampler, milliseconds).

**It is right by every test, a program with one thread pays nothing, and a program
with threads pays far more than was guessed: the card's third line.**

What was built: `ldapr` and `stlr` in the encoder; every load and store of the
guest's memory in a block goes through one place (`Emit::mem`, `Emit::fmem`), which
writes it plain or, ordered, as the host's address in x24 and the ordered
instruction (a vector register by way of general ones); a thread's blocks are
dropped and made ordered when the memory is first shared; the interpreter's accesses
are followed by a full barrier when it is shared; an ordered access the host refuses
(16 bytes not aligned and across a line of 16) leaves the block for the interpreter,
which the tests met and passed through. `--jit-ordered` makes every block ordered,
to measure; the unit tests can be run so (`R64M_ORDERED`).

All 109 unit tests pass plain and with every block ordered, on macOS and in the Linux
container; the 62 programs in the four JIT modes on both; the Python cases 27 of 27
and the Alpine ones 126 of 126.

| CPU ms, least of the runs | plain | ordered | |
|---|---|---|---|
| `cc1 -O2 apply.i` (one thread), every block ordered by `--jit-ordered` | 3,225 | 4,947 | 1.53 |
| Python summing squares (one thread), the same | 678 | 1,063 | 1.57 |
| Python's four threads summing | 123 | 203 | 1.65 |
| Python's eight threads taking one lock 160,000 times | 251 | 983 | 3.9 |

(The last counts the time eight threads spend waiting for one another too, and an
interpreted LOCK now costs barriers; it is not all loads and stores.)

A program with one thread: the way out's address was first moved out of x24 for
every block, and `cc1` took 3,196 for 3,117 (least of six), 2.5% more: over the
card's 1%. Plain blocks keep it in x24 again and only ordered ones load it: 3,164
for 3,157.

So ordering by the instructions costs a program with threads half again and more.
The card's next move for that is the processor's own x86 order. A first look, made
here because it is one call: **this VM's kernel takes it.** `prctl(PR_SET_MEM_MODEL,
TSO)` (the Asahi kernel's call, 4D4D444Ch) answers 0 in an arm64 container, and the
model read back is 1. Whether memory is then ordered as x86 orders it, and what a
thread's blocks should do with it, is R035.

## R035: the processor's own x86 order (2026-11-25)

Apple's processors can order memory as x86 does, for a thread that asks (it is how
Rosetta is fast). R034 found that this VM's kernel takes the asking (`prctl`
4D4D444Ch, the Asahi kernel's `PR_SET_MEM_MODEL`, with 1 for TSO). Taking it and
doing it are two things.

The card, written before the run.

- **Hypothesis**: a test of two threads on plain loads and stores, in a program of
  the host's own (not the guest's): one thread writes a counter to x and then to y,
  for ever rising; the other reads y and then x. x86's order forbids the reader to
  find x behind y. Without the mode the reader is guessed to find it so within some
  thousands of millions of looks on this machine (if it never does, the test shows
  nothing and is said to); with the mode asked for by both threads, never in the same
  number of looks. Then a guest's threads can each ask for the mode when the memory
  is first shared, keep their plain blocks, and pay nothing; where the kernel
  refuses (another host), R034's ordered blocks are used as now.
- **What is changed from before** (R034): first nothing, a probe that is not kept.
  Then, if the probe says so: each thread asks for the mode (the first thread when
  the second is made, every other as it starts); a thread that was granted it makes
  plain blocks and its interpreter leaves the barrier out.
- **Next move for each result**:
  - Found without the mode, never with it → built as above; the Python cases with
    threads timed again (they should cost what plain cost in R034)
  - Never found either way → the mode is not shown to do anything here; r64m keeps
    the ordered blocks, and the mode waits for a test that can see it
  - Found with the mode too → the mode is not what it says in this VM: not used
- **Cut-off**: the probe, each way for at most a minute; then the build and the
  timings of R034's table.

The result (this Mac's Docker VM: OrbStack's arm64 Linux 7.2 on an Apple M1).

**As guessed, and plainly: without the mode x was behind y in half of all looks; with
it, never. A guest's threads now ask for it and keep their plain blocks.**

The probe (a program of the host's own, not kept: one thread stores a rising count to
x and then to y, another loads y and then x, with plain stores and loads; 15 seconds
each, twice):

| | looks | x found behind y |
|---|---|---|
| without the mode | 13,310,168,064 | 6,926,587,365 |
| with the mode | 543,300,608 | 0 |
| without, again | 13,243,520,000 | 7,003,202,331 |
| with, again | 567,986,176 | 0 |

Half of all looks without it: the second load is made before the first is done, and
nothing puts that right. So the order that arm64 does not keep is not a rare thing
here; a block of plain loads would have shown it to any program that shares memory
without a lock. (With the mode the reader makes a twentieth as many looks in the
time: the processor does over again what it did out of order, as an x86 does. That
is the cost of two threads reading and writing the same words, on x86 too.)

What was built: when a guest's `clone` first shares the memory, the thread that
shares it asks the kernel for the mode, and each new thread asks as it starts. If
every one is given it, blocks stay plain and the interpreter leaves its barrier out.
If any is refused (a host without it: a Raspberry Pi; a kernel that does not offer
it), all threads go to R034's ordered blocks. `R64M_NO_X86_ORDER` refuses on
purpose, to test that way on a host that has the mode.

| CPU ms, least of five | not ordered (R033) | ordered by instructions (R034) | the processor's order |
|---|---|---|---|
| Python's four threads summing | 119 | 199 | 136 |
| Python's eight threads taking one lock 160,000 times | 248 | 972 | 302 |

The mode costs 1.14 and 1.22 times what no order at all cost, where the
instructions cost 1.67 and 3.9.

Both ways are held: the nine Python cases in the three ways with the mode (27 of 27)
and with it refused (27 of 27); the threads program twenty of twenty each way; the
109 unit tests plain and ordered and the 62 programs on both hosts; the Alpine cases
126 of 126.

Not shown: the mode on any other host (Docker Desktop's VM, a Mac running Linux
itself); that every program's order is right with it (the probe is one pattern, the
one arm64 is known to break most easily); a guest thread started while another
host thread of r64m's own exists (there is none).

## R036: sockets and epoll (2026-11-25)

ADR 0019's second step.

The card, written before the run.

- **Hypothesis**: the socket calls are the kernel's as they stand (addresses,
  options and flags are numbered alike on x86-64 and arm64), with three things to
  write over: `epoll`'s event (12 bytes packed on x86-64, 16 on arm64), a message's
  header (it holds pointers of the guest's: to an address, to a list of pieces, to
  control data), and the calls arm64 has only in a newer form (`epoll_create`,
  `epoll_wait`, `select`, `eventfd`, `alarm`). With these, Python's HTTP server
  answers a request from a client in the same process, a TCP echo between two
  threads works, `asyncio` runs a small server and client, and redis-server starts,
  answers PING, stores and gives back a value, and stops: all as hal has them, in
  the interpreter and with `--jit`. redis sets a timer whose signal reaches a
  handler; no handler is called yet (0019's third step), so redis may stop there.
- **What is changed from before** (R035): `src/sys/linux.rs` gains those calls.
- **Next move for each result**:
  - All as hal's → signals delivered (0019's third step)
  - redis stops for its signal and the rest is right → said; redis is the third
    step's first case
  - A call or a structure not foreseen → added by its kind and named in the result
- **Cut-off**: the cases chosen here, in the two images, three ways each.

The result (this Mac's Docker VM; the Python and redis images from hal; what is right
is what hal printed).

**As guessed, and redis did not stop for its signal: every case as the real machine
has it, in the three ways.**

Seven Python programs with sockets (kept in `tests/container/python.txt`): a pair of
sockets; TCP between two threads, with an option set and the port read back;
Python's HTTP server answering `Hello, world` to a client in the same process; `epoll`
through a selector, empty and then ready; `asyncio`'s server and client; `select` on a
pipe; a datagram. And one session with redis 8 (`tests/container/redis.txt`): the
server started as a daemon, asked until it answers, `SET` and `GET`, `INCRBY` and
`INCR`, a list, `SHUTDOWN`, no answer after.

| in its image, three ways each | cases | right |
|---|---|---|
| Python on Debian | 16 | 48 of 48 (and 48 of 48 with the processor's order refused) |
| redis on Alpine (with busybox's own) | 13 | 39 of 39 |
| Alpine with GCC | 42 | 126 of 126 |

What was built, all in the table of calls: the socket calls as they stand (`socket`,
`connect`, `accept`, `accept4`, `bind`, `listen`, `sendto`, `recvfrom`, the names and
the options, `shutdown`, `socketpair`); `sendmsg` and `recvmsg` with a header of the
host's made beside the guest's, since it holds three pointers; `epoll_ctl` and
`epoll_wait`/`epoll_pwait` with the event written over (12 bytes for 16);
`epoll_create`, `eventfd`, `select` and `alarm` by the newer calls arm64 has;
`pselect6` with the pointer inside its last argument; the timers (`setitimer`,
`timerfd`); `prctl` for a thread's name and four settings that are numbers;
`membarrier`.

Nothing was met that was not foreseen. No instruction was lacking in redis or in
Python's networking.

redis's timer signal did not end it: its handler is recorded and not called, the
kernel's own action for that signal while no handler of the host's is set would be to
end the process, and it did not come to that in this session (the timer is for a
watchdog that is off by default). It is still 0019's third step's to deliver.

A mistake in the tests' data, found by running the cases in a third image: the
shell's cases said they need `/etc/alpine-release`, which redis's image has too with
another version in it. They need the image with GCC.

109 unit tests and the 62 programs in the four JIT modes on both hosts.

## R037: signals delivered (2026-11-25)

ADR 0019's third step (its point B).

The card, written before the run.

- **Hypothesis**: with the host's handler noting a signal for the thread it came to,
  generated code leaving at the next block's start, and the guest's handler entered
  with Linux's frame for x86-64 and left by `rt_sigreturn`, these are as on the real
  machine: a C program that counts a timer's signals and one that catches its own
  fault and goes on past it (compiled by the container's GCC); the shell's `trap`
  and `kill`; Python's `signal.alarm` with a handler, a `KeyboardInterrupt` from a
  signal sent to itself, and a child's end noticed. A call that a signal cuts short
  is started again where the guest asked for that, and answers EINTR where it did
  not.
  The look at each block's start (a load and a branch) is guessed to cost the
  primitives under 2%; p_loop, whose loop is six instructions, the most.
- **What is changed from before** (R036): `rt_sigaction` gives the host's signal
  r64m's handler; a block begins with the look; `rt_sigreturn`, `sigaltstack`,
  `rt_sigsuspend`, `pause`; a fault of the guest's own is a signal for it, or ends
  the process as that signal does.
- **Next move for each result**:
  - The cases are as hal's and the look costs under 2% → the goal's own programs
    (nginx, FastAPI): what must be fetched is asked for
  - The look costs more → only blocks that a cycle must pass through look (those
    with a jump backward, a return, or a jump through an operand), and it is
    measured again
  - A case differs → the frame is compared word for word with one the real machine
    makes (a handler that prints its own frame, run on hal)
- **Cut-off**: the cases chosen here, three ways each; the primitives raced against
  R036's build, three at once, least of three.

The result (this Mac's Docker VM; what is right is what hal printed; the primitives
raced on macOS, three at once, least of three).

**Signals are delivered as on the real machine in every case tried. The look is not
cheap where code is branches: it costs four primitives 3 to 7%, over the card's 2%,
and the card's cure did not help.**

What is right:

- A C program of eight cases (compiled on hal by the container's GCC, the same file
  run under r64m; not kept, as `big.c` was not): a timer every 5 ms while the program
  only counts; a store through a bad pointer caught and left by `siglongjmp`; the
  same stepped over by the handler adding 3 to RIP in its frame and changing RBX
  there; a division by nothing caught; a `read` cut short (EINTR) and one started
  again (`SA_RESTART`) with a child writing later; a handler on another stack; one
  signal raised inside another's handler (order 1 2 3); one held back, seen pending,
  and let go. All eight lines as hal's by the interpreter, with `--jit` and with
  `--jit-check`; twenty runs of twenty with `--jit`, ten of ten by the interpreter.
- Six commands for the shell (a trap for a signal to itself; a trap that ends a busy
  loop; a child killed and waited for, 143; a child's end noticed; a signal
  ignored; a trap in a subshell) and six for Python (a timer's handler; an interrupt
  from a signal to itself; a timer while the program counts; a child ended by
  SIGTERM, -15; a signal sent from another thread and handled by the first; a child
  that faults, -11): kept in the tests' data, all as hal's in the three ways.
- In their images, three ways each: Python 22 cases, 66 of 66; Alpine with GCC 48,
  144 of 144; redis 13, 39 of 39. 110 unit tests plain and ordered and the 62
  programs on both hosts.

What was built: `rt_sigaction` gives the host's signal r64m's handler, which notes it
for its thread and sets the thread's word; a block that a cycle must pass through
begins by looking at the word and leaves if it is set; the frame (the restorer's
address, the context with every register, what the signal was, floating point's
state above) and the handler's entry; `rt_sigreturn`; `sigaltstack`; `tkill`,
`rt_sigpending`, `pause`; a call cut short started again if the handler asked; a
fault of the guest's own as its signal, with the address and the kind, or the end of
the process as that signal ends one.

Found on the way:

1. **A call that waits with its own set of blocked signals must deliver by that
   set.** The shell blocks SIGCHLD, waits in `rt_sigsuspend` with it open, and
   counts on its handler having run when the call comes back. The handler was
   entered only when the signal was no longer blocked by the thread's own set, which
   was after, and the shell waited for ever. Now what arrives during
   `rt_sigsuspend`, `ppoll`, `pselect6` or `epoll_pwait` is delivered by the call's
   set, and the first handler's frame holds the thread's own set to put back: as the
   kernel does it.
2. **A signal that arrives just before a call is made** was delivered after the
   call, which may wait. The word is looked at as the call begins now, and the
   handler is entered first. **A few instructions' width is still open** (between
   that look and the kernel taking the call): to be closed by making that stretch
   one that the host's handler recognises and starts again.
3. `raise` is `tkill`, and `sigpending` asks the kernel and r64m both.

Not explained: one run of Python's "child ended by SIGTERM" under `--jit-check` took
68 seconds where others take 10. It gave the right answer. It may be the open width
of 2.

**The cost of the look.** Every block, at first; then only blocks that a cycle must
pass through (a jump or call to an address not after itself, a return, a jump
through an operand), and the way out it needs left off the others:

| new / R036's build | every block | only where a cycle passes |
|---|---|---|
| p_if | 1.067 | 1.070 |
| p_bignum | 1.056 | 1.054 |
| p_ternary | 1.043 | 1.042 |
| p_switch | 1.032 | 1.039 |
| p_funcptr | 1.031 | 1.030 |
| p_loop, p_recursion, p_rotate, p_shift | 0.965 to 0.971 | 0.975 to 0.98 |
| the other fourteen | 0.986 to 1.020 | |

Narrowing it changed nothing: in p_if thirteen blocks of seventeen are ones a cycle
passes through (the code is short runs ended by jumps back, and returns), read from
the two builds' generated code. So the two instructions (a load of the word, a
branch) are the cost, where blocks are short. (Four primitives came out 2 to 3%
faster, which is where the compiler happened to put code: the race's noise is of
that size, and the slower ones are slower by more.)

The next thing to try, a card of its own: the look as one instruction on a register,
with no load. The host's handler can set a bit of a register of the interrupted
thread when it interrupts generated code (the kernel hands it the registers); a block
then tests that bit and branches. The count of instructions (x27) has room for it.

Not measured: NetHack's build and the container cases' time with the look.

## R038: the guest's test programs in Rust (2026-11-25)

The cases a shell or Python cannot make (a handler that changes its own frame, a
handler on another stack, a fault stepped over) were tried with a C program that was
not kept: the repository takes no C (ADR 0012). The owner chose that such programs be
written in Rust and built for x86-64 Linux (2026-11-25), which needs Rust's library
for that target fetched (`rustup target add x86_64-unknown-linux-musl`).

The card, written before the run.

- **Hypothesis**: a Rust program built for `x86_64-unknown-linux-musl` (statically
  linked, position-independent, with no interpreter) does R037's cases and a few
  more (floating point's state across a handler; a handler that asks to be entered
  once only), prints the same lines on hal, where it runs as it is, and under r64m
  by the interpreter, with `--jit` and with `--jit-check`. It is also the first
  program r64m loads that is position-independent with no interpreter, and the
  first built by Rust: its start-up (the library's own handler for a fault on the
  stack, on another stack) is guessed to ask for nothing r64m lacks.
- **What is changed from before** (R037): a second crate in the repository, `guest`,
  built only for x86-64 Linux and only by the tests' preparation; a test that runs
  its programs under r64m against what hal printed. Nothing of r64m unless something
  is met.
- **Next move for each result**:
  - As hal's in the three ways → the cases are the repository's; then the look made
    cheaper, or the goal, as the owner chooses
  - A line differs → it is a case R037's C program did not have, and is the finding
  - The program does not start → the loader or a call is looked at first
- **Cut-off**: the one program, on hal and under r64m in the three ways, twenty runs
  with `--jit`.

The result (hal for what is right; r64m in the arm64 Linux container of this Mac's
Docker VM: the program is statically linked and needs no image).

**As guessed: the same eleven lines as hal's the first time, in the three ways, and
twenty runs of twenty with `--jit`. Nothing was changed in r64m.**

`guest/src/bin/signals.rs`, built for `x86_64-unknown-linux-musl` (578 KB,
position-independent, no interpreter):

1. a timer every 5 ms while the program sums floats, whose handler does floating
   point of its own: five ticks, and the sum the same to the last bit as without;
2. a store through a bad pointer, stepped over by the handler adding the
   instruction's length to RIP in its frame; the address it was told; a register it
   changed in the frame;
3. a division by nothing stepped over the same way;
4. a `read` cut short, and one started again with a child writing later;
5. a handler on another stack;
6. one signal inside another's handler: 1 2 3;
7. one held back, seen waiting, let go;
8. a signal raised inside its own handler: after it (10 20 11), and at once where the
   handler said so (10 11 20);
9. a handler asked for once only: the second signal ends the child, by signal 10.

Cases 1 (floating point across a handler), 8 and 9 are more than the C program of
R037 had; `siglongjmp` out of a handler, which that had, is the C library's and is
left to Python's and the shell's cases.

`tests/guest.rs` runs it against `tests/guest/signals.txt` (hal's lines): 3 of 3 on
Linux; shown as ignored on macOS and where the program is not built. Rust's library
for the target was fetched as the owner agreed: 220 MB unpacked, where "some tens"
had been said.

It is the first position-independent program with no interpreter that r64m loads,
and the first built by Rust; its start (the library's handler for a fault on the
stack, on another stack; `poll` on its three descriptors) asked for nothing new.

## R039: the look as one instruction on a register (2026-11-25)

R037's look (a load of the thread's word and a branch, at the start of every block a
cycle passes through) costs code that is all branches 3 to 7%.

The card, written before the run.

- **Hypothesis**: the look can be one instruction with no load: a test of the top bit
  of x27 (the count of instructions, which never comes near it) and a branch. The
  host's handler, which is handed the interrupted thread's registers, sets that bit
  when what it interrupted is generated code; the way in seeds it from the thread's
  word, and a signal that arrives while the way in is between seeding it and
  jumping is given the bit by the handler too (the handler knows that stretch). The
  word stays for everything that is not generated code. With this the five
  primitives R037 slowed (p_if 1.07, p_bignum 1.055, p_ternary 1.04, p_switch 1.04,
  p_funcptr 1.03 of R036's time) are within 2% of R036's build, and every signal
  case is as before.
- **What is changed from before** (R038): the way in; the host's handler; the look's
  one instruction; the count read without its top bit. Nothing of what is
  delivered or how.
- **Next move for each result**:
  - Within 2% and the signal cases right twenty times of twenty → done; the goal's
    own programs next
  - Still slower by more → the cost is not the look's instructions but that a block
    has a way out more at its end, or where the code falls: the two builds' code for
    p_if is compared again before anything else
  - A signal case fails or hangs → the stretch the handler does not know is found
    (a signal lost between two places is a hang, not a wrong answer)
- **Cut-off**: all tests on both hosts; the guest's signal program and the container
  cases with signals twenty times with `--jit`; the primitives raced against R036's
  and R037's builds, three at once, least of three.

The result (the primitives on macOS, three at once, least of three, against R036's
build; the signal cases in the Linux containers).

**The look is one instruction and every signal case is as before. Three of the five
slowed primitives are within 2 to 3% now; two are where they were, and the reason is
read from the code: one instruction more in a loop of twenty is that much.**

| now / R036's build | R037 (a load and a branch) | R039 (a test of a bit) |
|---|---|---|
| p_if | 1.070 | 1.03 |
| p_switch | 1.039 | 1.014 |
| p_funcptr | 1.030 | 1.017 |
| p_bignum | 1.054 | 1.045 |
| p_ternary | 1.042 | 1.043 |
| the other seventeen | | 0.95 to 1.03, as before |

For p_ternary the two builds' blocks were compared word for word: past the look they
are the same instructions (but for the distance of the jump back, which now goes to
the look). Its loops are 16 to 28 host instructions, and the look is one more: 4%.
So this is what a look in every round of a loop costs where the loop is short, and
no cheaper look than one instruction that is not taken is to be had; p_if had two
and went from 7% to 3% by losing one. Narrowing which blocks look did nothing in
R037 for the same reason: the hot blocks are the loops.

What was built: a block's look is `tbnz x27, #63` (x27 counts instructions and
never comes near its top bit). The way in clears the count and then sets the bit
from the thread's word; the host's handler, handed the interrupted thread's
registers, sets the bit itself if the thread was in generated code or in the four
instructions of the way in after the clearing (found once, by reading the way in's
own code for the two instructions that bound them), and sets the word always. The
count is read without its top bit.

Right as before: the guest's signal program thirty runs of thirty with `--jit` and 3
of 3 ways; the container cases 66, 144 and 39 in their images; 111 unit tests plain
and ordered and the 62 programs on both hosts.

Not done, and what it would be: a loop that looked for nothing would need the host's
handler to change the code the thread is in so that it leaves at the block's end
(each thread's blocks are its own, so it could). That is a rewrite of jumps from
inside a signal's handler, with a list of every block's ways out that the handler
can read; for 1 to 4% of the tightest loops it is not begun. QEMU looks at the start
of every block too.

Not measured: NetHack's build with the look (its code is not tight loops).

## R040: TLS with what is at hand, before the goal's own programs (2026-11-26)

The goal (ADR 0011) needs nginx and FastAPI, which are downloads the owner is asked
for. One part of it needs nothing new: the image `python:3.13-slim` has OpenSSL 3, and
OpenSSL chooses its code by CPUID. So far only `hashlib.sha256` has been through it.

The card, written before the run.

- **Hypothesis**: with what r64m's CPUID says now (SSE2, no AES-NI, no PCLMULQDQ, no
  SSSE3, no AVX), OpenSSL takes its plain paths and a TLS 1.3 handshake completes: a
  key and a self-signed certificate made by `openssl req` (RSA 2048 and P-256), a
  Python `ssl` server and client in one process over loopback, `Hello, world` back,
  with the cipher the real machine would also agree to when it is told the same
  CPUID (OPENSSL_ia32cap). What stops, if anything, is an instruction on a plain
  path not yet written (MMX is the known one), not a system call.
- **What is changed from before** (R039): nothing in r64m; the programs are new.
- **Next move for each result**:
  - It completes, with `--jit` and without → the cases go into `python.txt` (their
    expected lines from hal when it can be reached; until then marked as not yet
    held to the real machine) and the next step is the goal's programs
  - An instruction is missing → it is written, held to the real CPU by a trace
    (hal), and the run repeated
  - A system call is refused → it is added as the others were
  - It completes but is slow (a handshake over a second) → the time is recorded and
    which CPUID bits would change it listed; nothing is switched on in this
    experiment
- **Cut-off**: the handshake and one request, plain and `--jit`, in the Linux
  container; no timing race.

The result (the Linux container on the Mac; the image `python:3.13-slim`, OpenSSL
3.5.7).

**It completes, with nothing added to r64m.** `openssl req` makes an RSA 2048 key and
a P-256 key with their certificates; a Python `ssl` server and client in one process
agree on TLS 1.3 with `TLS_AES_256_GCM_SHA384`, the certificate is verified, and
`Hello, world` comes back. No instruction was missing and no system call that
mattered was refused (`set_robust_list` and `rseq` are still answered ENOSYS, as
before). The whole script (both keys, both handshakes) took 45 s interpreted and
1.6 s with `--jit`, so the handshake is far under the card's second.

Two cases are in `tests/container/python.txt` (72 of that file's cases pass now: the
66 before and these two three ways). **They are not yet held to the real machine**:
hal was out of reach (the VPN) on the day, and the expected line is what r64m and
user-mode QEMU both printed. That is owed, and is said in the file.

Not looked at: which paths OpenSSL took. r64m's CPUID says no AES-NI, PCLMULQDQ,
SSSE3 or AVX, so AES-GCM here is the table code; what switching those on would be
worth is a question for when a request's time is measured, and each instruction it
brings must first be held to the real CPU.

Next: the goal's own programs, nginx and FastAPI, which need downloads the owner is
asked for.

## R041: the goal's own programs: nginx over TLS to FastAPI (2026-11-26)

The owner allowed the downloads: Debian's `nginx` and PyPI's `fastapi` and `uvicorn`,
put on `python:3.13-slim` as a local image `r64m-goal:amd64`.

The card, written before the run.

- **Hypothesis**: in that image, with every x86-64 program run by r64m (`--jit`),
  uvicorn serves a FastAPI app on 127.0.0.1:8000, nginx (a master and one worker)
  listens with TLS on 8443 and passes to it, and a client gets status 200 and
  `Hello, world` with the handshake complete. What stops on the way is of these
  kinds, guessed in the order of likelihood: a system call not yet in the table
  (nginx: `socketpair`, `setsid`, `rt_sigsuspend` in the master, `accept4`,
  `epoll` with `EPOLLRDHUP`/`EPOLLET`, `sendfile`, `writev`, `eventfd`, `prctl`,
  `setuid`/`setgid`, `pread`/`pwrite`; uvicorn: `epoll`, `getrandom`,
  `socketpair`); a difference in a structure passed (as `epoll_event` was); a
  signal between master and worker; an instruction (pydantic-core is Rust built
  for x86-64-v1, but MMX is unwritten).
- **What is changed from before** (R040): the programs. r64m only as each stop
  asks.
- **Next move for each result**:
  - `Hello, world` comes back → the request is repeated 100 times, then with the
    client outside the container (curl on the Mac through a published port, which
    is ADR 0011's self-check); the case becomes a test; then the time of a request
    beside QEMU's is the next experiment
  - A stop of a kind above → fixed as its kind was before, each one noted here
  - A hang → `--profile` and the system-call trace say where each process waits,
    before anything is changed
  - Something that needs a decision (a call that cannot be handed to the host as
    it is) → stop and ask
- **Cut-off**: the first complete request, or a stop that needs a decision. No
  timing in this experiment beyond noting it.

The result (the Linux container on the Mac, every x86-64 program run by r64m with
`--jit`; nginx 1.26.3, FastAPI 0.165.0, uvicorn 0.54.0, Python 3.13.18; the downloads
were 13.2 MB from Debian and the PyPI wheels, and the image is 153 MB, 35 MB more
than the one it is built on).

**The goal's request completes: nginx takes it over TLS, passes it to FastAPI, and
`Hello, world` comes back with status 200.** One thing stopped it, of the first kind
the card guessed:

- `ioctl(FIOASYNC)`, which nginx's master makes on the pair of sockets it keeps with
  a worker, was refused, so no worker started. It is handed on now, with
  `fcntl(F_SETOWN/F_GETOWN)` (the next call nginx makes there) and `fadvise64`
  (asked for and answered ENOSYS before, harmlessly).

Nothing else: no structure laid out differently, no signal lost, no instruction
missing. `clone3`, `set_robust_list` and `rseq` are answered ENOSYS as before and
the programs go on without them.

What was seen, in the card's order:

- One request from a Python client inside the container that checks the
  certificate: `200 nginx/1.26.3 Hello, world`.
- A hundred, each a new connection and handshake: all 200, 2.0 s with the client's
  own start.
- From outside the container (ADR 0011's self-check): `curl` on the Mac through a
  published port, with the certificate given: status 200, the certificate verified,
  `Hello, world`; 14 ms a request after the first (21 ms). A path that is not there
  is FastAPI's 404 through nginx.
- nginx and uvicorn end when told to (SIGTERM), and the shell that waits for them
  goes on.

The case is `tests/container/goal.txt`, one command that does all of it with twenty
requests: it passes interpreted, with `--jit` and with `--jit-check` (six minutes for
the three; nearly all of it is the two slow ways starting Python). The other
container cases are as before (GCC 144, Python 72, redis 39), and the tests on macOS.
**Not yet held to the real machine**: hal was out of reach, and the expected lines are
what r64m and user-mode QEMU both printed. (A first form of the case hung, and not
by r64m: it waited for nginx's master by `kill -0`, and in the test's container the
daemon's parent is the test program, which never collects it. It waits for the pid
file now.)

Not done, and owed:

- the case and R040's two held to hal;
- the time of a request beside QEMU's, which is the next experiment (the aim is
  twice QEMU on real work in a container), and with it what CPUID should say for
  OpenSSL;
- the image as Docker would run it, with r64m registered for x86-64 programs
  (`binfmt_misc`) and not named on the command line: the owner's word is needed,
  since it changes the whole Docker VM;
- ADR 0011 says the images are the ordinary ones, unchanged: this one is
  `python:3.13-slim` with nginx added. The official nginx image beside a Python
  one, as two containers, has not been tried.

## R042: the goal's request beside QEMU (2026-11-26)

The aim is twice QEMU on real work in a container. R030/R031 measured a build; this is
the goal's own work.

The card, written before the run.

- **Hypothesis**: with the same image and the same client, r64m (`--jit`) is at
  least twice as fast as user-mode QEMU (OrbStack's, 9.2.2) on each of three
  measures: (a) from the container's start to the first answer (Python starting,
  FastAPI imported: mostly new code, where making blocks counts); (b) a request on
  a new connection, servers warm (a TLS handshake each: nginx's RSA 2048 signature,
  where QEMU says it has every extension and OpenSSL takes its AVX2/ADX/AES-NI
  paths, and r64m says none and it takes the plain ones); (c) a request on a
  connection kept open, servers warm (nginx's proxying and uvicorn's Python, and
  system calls). My guess before measuring: (c) is past 2, (a) near it, (b) the one
  most likely under.
- **What is changed from before** (R041): nothing in r64m. The key and certificate
  are made once outside and given to both; the client is one native program on the
  Mac (Python, through a published port) for both, so its own time is in both and
  pulls the ratios toward 1: the times are given beside the ratios.
- **Next move for each result**:
  - All three at 2 or more → recorded; the next is hal and the form Docker runs
  - (a) under 2 → `--jit-stats` on the Python process: making against running, as
    R031 did for the build
  - (b) under 2 → `--profile` on nginx's worker during handshakes; if the time is in
    the plain bignum and AES code, the extensions OpenSSL would use are listed with
    what each asks of r64m, as the next experiment's card (none is switched on here)
  - (c) under 2 → `--profile` on the worker and on uvicorn, and the share of time in
    system calls
- **Cut-off**: three launches of the two side by side; in each, the start timed,
  300 requests to warm, then 300 on new connections and 1,000 on one kept open; the
  least of the three for each measure. One profile for each measure under 2, no
  fix in this experiment.

The result (the Mac; OrbStack's QEMU 9.2.2 against r64m `--jit`, the two side by
side; the client a native Python program on the Mac through a published port; the
least of three launches).

**Twice QEMU at the start, not at a request: 1.15 on a new connection and 1.6 on one
kept open. My guess of (c) past 2 was wrong; (b) under was right, and the reason is
not the one I gave.**

| by the client's clock | QEMU | r64m | QEMU / r64m |
|---|---|---|---|
| (a) the container's start to the first answer | 3.48 s | 1.54 s | 2.26 |
| (b) a request on a new connection | 17.4 ms | 15.1 ms | 1.15 |
| (c) a request on a connection kept open | 4.74 ms | 2.92 ms | 1.62 |

The client's own time is in both columns, so the servers' CPU time was read too
(each process's user and system time from `/proc`, one launch, in ticks of 10 ms):

| CPU time for a request | QEMU | r64m | QEMU / r64m |
|---|---|---|---|
| (b) nginx's worker (the handshake) | 11.3 ms | 11.6 ms | 0.97 |
| (b) uvicorn | 4.8 ms | 3.0 ms | 1.6 |
| (c) nginx's worker | 0.35 ms | 0.25 ms | 1.4 |
| (c) uvicorn | 3.5 ms | 2.1 ms | 1.7 |

System time is a few percent of each: the time is the guest's code, not calls.

What the card asked for under 2, and one step it did not name:

- **OpenSSL's extensions are not the reason** (the added step: both sides run again
  with `OPENSSL_ia32cap` set so that OpenSSL takes only its plain paths; if QEMU's
  handshake had slowed much, its lead there would be the extensions). QEMU's
  worker: 12.1 ms a handshake against 11.3 with them; r64m's as before. So r64m is
  no faster than QEMU on the same plain code.
- **The handshake's time is spent leaving blocks.** `--profile` on a single nginx
  process over a thousand handshakes: 19% of the samples in generated code, about
  62% in the interpreter, the rest going in and out. `--jit-why` says what the
  interpreter is given: 1.7% of the instructions, and of those INC and DEC of a
  register (FFh /0 /1: not translated at all), the conditional jump after each
  (interpreted too, since nothing begins a block there), MUL, MOV and SHR with a
  67h prefix (OpenSSL's hand-written code pads with it), then ADC, SBB, XCHG,
  PMOVMSKB, MOVLPD, LOCK XADD, PREFETCH. They are the loops of the multi-word
  arithmetic: INC and DEC keep the carry, which is why hand-written code uses
  them and why no compiler's output among the primitives did. Each one costs
  about 30 ns (out, the flags settled, one instruction, in again).
- **uvicorn**: 70% in generated code. What leaves: LEAVE (C9h: 9.7 million times in
  2,000 requests, more than everything else together), PMOVMSKB, the conditional
  jumps after them, REP STOS and REP MOVS, LOCK CMPXCHG, XCHG, INC/DEC, PUSH of
  an operand.

A mistake of mine on the way, kept: the first profile had `--jit-stats` on, whose
clock read at every entry of a block was itself 41% of the samples (the clock is
slow in this VM). The numbers above are from runs without it.

Next (R043): these instructions translated, the commonest first, and the same
three measures again.

## R043: the instructions that leave blocks in the goal's work (2026-11-26)

R042: a handshake in nginx is 62% interpreter for 1.7% of its instructions, and
uvicorn leaves blocks ten million times in 2,000 requests for LEAVE alone.

The card, written before the run.

- **Hypothesis**: with these translated, the servers' CPU time for a request is at
  least twice QEMU's on both: nginx's worker 5.6 ms or less a handshake (11.6 now,
  QEMU 11.3), uvicorn 1.75 ms or less on a kept connection (2.1 now, QEMU 3.5).
  The handshake is the surer of the two: what leaves is nearly all of four kinds.
  1. **INC and DEC** (FEh/FFh /0 /1) of a register, 32 and 64 bits. They write
     every flag but the carry. The record is an addition's or a subtraction's of
     one with "the carry was given" above the kind (R031's bits), the carry taken
     from the host's flags where they say it, else from the record before if that
     one says its carry was given, else the instruction is the interpreter's as
     now. Where every flag is dead after, a plain addition.
  2. **ADC and SBB where the host's flags do not say the carry** (the top of a
     loop whose carry came round through a DEC and a jump): taken from the record
     if it says its carry was given, else the interpreter's as now.
  3. **A 67h prefix** on an instruction with no memory operand, where it does
     nothing (OpenSSL pads with it).
  4. **LEAVE**.
  Then by their counts, as far as each is simple: PUSH of an operand, XCHG of two
  registers, PREFETCH, PMOVMSKB, MOVLPD. Not in this experiment: LOCK CMPXCHG and
  LOCK XADD in blocks, REP STOS and REP MOVS.
- **What is changed from before** (R042): the translator only; each new form with
  a unit test against the interpreter (`--jit-check` does the rest on the
  programs), and the encodings it needs tested against clang as the others are.
- **Next move for each result**:
  - Both at 2 or more → the three measures by the client's clock again, recorded;
    then the primitives raced against R039's build (nothing there should move)
  - The handshake still under 2 → `--jit-why` and `--profile` again: what is left
    (the carry coming round by a way the record does not say; MUL's flags; the
    way in and out between blocks) decides the next card
  - uvicorn still under 2 → the same two, and the share of REP and LOCK
  - A test fails under `--jit-check` → that form is taken out until it is right
- **Cut-off**: all tests on both hosts; the container cases; one launch for the
  CPU times, three for the client's clock.

The result (the Mac, as R042: OrbStack's QEMU 9.2.2 beside r64m `--jit`).

**The handshake is 2.7 times QEMU by the servers' CPU time and a request on a kept
connection 2.06; by the client's clock the start is 2.67, a new connection 2.24, a
kept one 1.63.** Both of the card's numbers are met as it stated them (CPU time);
the kept connection by the client's clock is still under 2, with the client's own
time and the port's in both sides of it.

| CPU time for a request | QEMU | R042 | R043 | QEMU / R043 |
|---|---|---|---|---|
| nginx's worker, a new connection (the handshake) | 11.1 ms | 11.6 ms | 4.2 ms | 2.7 |
| uvicorn, a new connection | 4.6 ms | 3.0 ms | 2.4 ms | 1.9 |
| nginx's worker, a kept connection | 0.38 ms | 0.25 ms | 0.26 ms | 1.5 |
| uvicorn, a kept connection | 3.80 ms | 2.1 ms | 1.84 ms | 2.06 |

| by the client's clock (least of three launches) | QEMU | R043 | QEMU / R043 | R042's ratio |
|---|---|---|---|---|
| the container's start to the first answer | 3.90 s | 1.46 s | 2.67 | 2.26 |
| a request on a new connection | 19.96 ms | 8.90 ms | 2.24 | 1.15 |
| a request on a connection kept open | 4.56 ms | 2.79 ms | 1.63 | 1.62 |

(QEMU's own times moved between the two days' runs, 17.4 to 20.0 ms for a new
connection: the ratios are of runs side by side and are what is trusted.)

What was built, in the order it was found to matter:

1. **INC and DEC** (FFh /0 /1) of a register or memory, at 32 and 64 bits. The
   record is an addition's or a subtraction's of one, with "the carry was given"
   and the carry above the kind; the host's flags after it are a kind of their own
   (`IncDec`: zero, sign and overflow are x86's, the carry is not), so a jump on
   the carry after one is still the interpreter's.
2. **A carry that the host's flags do not say** (for ADC, SBB, INC, DEC) is taken
   from the record where the record says it was given, from RFLAGS where there is
   no record, and from the two operands where the record is a 64-bit addition's;
   with any other record the instruction is left to the interpreter from inside
   the block.
3. **After an instruction that a block left to the interpreter, the next address
   is looked up again.** This was not on the card and was the larger half: the
   first measurement after 1, 2, 4 and 5 gave 8.5 ms a handshake, not 5.6. What
   was left: an ADC that begins a block after a long run of code was cut in two at
   64 instructions, between an ADD and its ADC; and after each, the interpreter
   went on to the next jump, which in OpenSSL's squaring is about sixty
   instructions away (found by marking, in `--jit-why`, which interpreted
   instruction began each run and what record it met; the marks are not kept in
   the code). With this and the 64-bit addition's carry: 4.2 ms.
4. **A 67h prefix** where the instruction has no memory operand.
5. **LEAVE**, **PUSH of an operand** (FFh /6), **PREFETCH** and its neighbours.

Not done of the card's list: XCHG of two registers, PMOVMSKB, MOVLPD (each a few
hundred to two thousand a handshake now; they begin most of what is still
interpreted, with XCHG of memory, LOCK XADD and LOCK CMPXCHG).

**A fault of R037's found on the way, and not this experiment's**: under
`--jit-check`, a shell in a loop without calls did not see a SIGTERM, and went round
for ever. Two runs in a hundred with R042's build when four ran at once, none alone.
The checker ran the interpreter on the thread's own `Cpu` and then wrote the copy it
had kept over it, and with it the word the host's handler had set meanwhile. It runs
on the copy now. A hundred runs of four at once with the fix: all end. (This is very
likely the one run of 68 seconds noted and not explained in R037.) It was met
because a suite hung while four ran at once; I first took it for this experiment's
doing, and the old build beside the new one said otherwise.

A mistake of mine, kept: the first build translated an INC whose flags are dead
without leaving the host's flags as declared, and eleven of the 62 programs stopped
at the translator's own check under `--jit` while every unit test passed: no unit
test had an INC with nothing reading its flags. One does now.

Tests: 116 unit tests plain and ordered on both hosts (five new); the 62 programs
four ways; container cases GCC 144, Python 72, redis 39, the goal 3; the guest's
signal program; prim 116 and trace 39 on macOS. The primitives against R039's
build, three at once, least of three: all within 3% either way (three that showed 4
to 5% slower in one round were 0.98 to 1.02 in two more).

Still under 2 and next: a kept connection by the client's clock (what the client
and the port cost is not measured: a floor is wanted, a native arm64 server of the
same kind), uvicorn on a new connection (1.9), nginx's worker on a kept one (1.5).

### R040 and R041 held to the real machine (2026-11-26, later the same day)

Owed since both: the expected lines of the two TLS cases and of the goal's case were
what r64m and QEMU agreed on. hal was reached, the image `r64m-goal:amd64` copied to it
(`docker save`, 153 MB; `python:3.13-slim` was there already), and the cases' own
command lines run there natively: `TLSv1.3 TLS_AES_256_GCM_SHA384 Hello, world` twice,
and `200 nginx Hello, world`, `0`, `20`, exit code 0. **The same lines as the test
data, which now says so.** A scratch folder `/tmp/r041-1126` is left on hal.

## R044: where a request on a kept connection spends its time (2026-11-26)

R043 left a kept connection at 1.63 times QEMU by the client's clock (2.06 by
uvicorn's CPU time, 1.5 by nginx's worker's).

The card, written before the run.

- **Hypothesis**: of uvicorn's 1.84 ms a request, a quarter is still not generated
  code, in two parts I cannot yet tell apart: (1) instructions the interpreter is
  given (R042's list for uvicorn after LEAVE: PMOVMSKB, REP STOS and REP MOVS,
  LOCK CMPXCHG, XCHG, and the jumps after them), and (2) the way between blocks
  through the run loop (`Jit::run` and `enter` were 9% of R042's samples: returns
  and jumps through an operand that miss the table). If (1) is the larger, the
  instructions at the top of the list are translated; if (2), the table.
  The aim for this experiment: uvicorn 1.5 ms or less and the client's clock at
  1.9 or more.
- **What is changed from before** (R043): nothing until the profile is read.
  `--profile` and `--jit-why` on uvicorn alone over 4,000 kept requests, and on a
  single nginx process the same; `--jit-stats` without its timers for the counts.
- **Next move for each result**:
  - Interpreted instructions lead → the commonest translated, each with a unit
    test and, for SSE, a trace from hal
  - The run loop leads → what misses the table is counted (returns, indirect
    calls) and the table or the joining is changed
  - Generated code is over 85% and neither leads → the request is at what blocks
    as they are can do; the client's floor is measured before anything else, and
    the quality of the code (R-numbers to come) is another matter
- **Cut-off**: one profile each; then the change the profile asks for, the CPU
  times and three launches by the client's clock.

The result (the Mac, as before).

**Neither part leads: uvicorn is 80% generated code, and what the profile could ask
for was small. uvicorn's request went from 1.84 to 1.78 ms; the aim of 1.5 is not
met, and a kept connection is 1.67 times QEMU by the client's clock.**

The profile of uvicorn over 4,000 kept requests (its start is in it, about a fifth
of the samples): generated code 80%; the interpreter about 7% (`exec`, `settle`,
`step`, the SSE unit, the string instructions); the run loop and the way in 5%;
system calls 4%. So both parts of the hypothesis are there and neither is a
quarter: together they are an eighth.

What was built from it: **PMOVMSKB** (the commonest instruction still interpreted:
647 a request, and it is in every string function of glibc's that looks for a
byte: four host instructions gather the sixteen top bits), **MOVLPS and MOVLPD from
memory**, **PMINUB and PMAXUB**. Encodings against clang as the others; one unit
test more (117).

| | QEMU | R043 | R044 | QEMU / R044 |
|---|---|---|---|---|
| uvicorn's CPU time, a kept connection | 3.66 ms | 1.84 ms | 1.78 ms | 2.06 |
| nginx's worker's, a kept connection | 0.35 ms | 0.26 ms | 0.27 ms | 1.3 |
| nginx's worker's, a handshake | 10.8 ms | 4.2 ms | 4.1 ms | 2.6 |
| by the client's clock: the start | 3.70 s | 1.46 s | 1.29 s | 2.87 |
| a new connection | 19.65 ms | 8.90 ms | 8.43 ms | 2.33 |
| a kept connection | 4.17 ms | 2.79 ms | 2.49 ms | 1.67 |

(The kept connection's three launches were 3.36, 2.49 and 2.72 ms for r64m and
4.29, 4.20, 4.17 for QEMU: r64m's is the noisier, and the least is what is given.)

What still leaves blocks in uvicorn, for each request: conditional jumps that begin
a block or follow an interpreted instruction 735, REP STOS 372, REP MOVS 341, LOCK
CMPXCHG 190, XCHG with memory 271, SYSCALL 66. About 1,900 events at some 40 ns:
4% of the request. Each is a piece of work of its own (a compare-and-swap in
blocks; the string instructions as loops; a jump that reads RFLAGS where there is
no record), and all of them together do not reach 1.5 ms.

So by the card's third branch: the request is near what blocks as they are can do,
and the two things to know next are (1) the client's and the port's own time, which
is in both sides of the client's clock (a native arm64 nginx and FastAPI under the
same client would give it; that is a download to ask for), and (2) what the 80%
is: the quality of the code for CPython's interpreter loop (jumps through a
register, calls and returns, the flags at each compare), which no experiment here
has looked at with a profile of the guest's own addresses.

Tests: 117 unit tests plain and ordered on both hosts; the 62 programs four ways;
container cases GCC 144, Python 72, redis 39, the goal 3; prim 116 and trace 39.

## R045: what the generated code for uvicorn's request is (2026-11-26)

R044: 80% of uvicorn's time is generated code, and nothing has yet said which code
or what is slow in it.

The card, written before the run.

- **Hypothesis**: the samples in generated code gather in few blocks (the twenty
  hottest hold 40% or more), and they are CPython's evaluation loop and what it
  calls for every bytecode: reference counts, type checks, the dispatch. In their
  host code the cost that is not the guest's own work is, in my guess of the order:
  (1) the end of a block that jumps through a register or returns (the table's
  lookup, a dozen instructions for one of the guest's), (2) the record of the
  flags written where the next block may read it (three moves for a compare whose
  jump is in the same block already skips them; a compare before a call or a
  return does not), (3) the count of instructions added at each end, (4) the look
  for a signal.
- **What is changed from before** (R044): `--profile` says, for a sample in
  generated code, which block and which of the guest's instructions it was in (the
  main thread's blocks), and the guest's base in the host; nothing else. The
  guest's addresses are named from the image's own files (`/proc/PID/maps` of the
  running r64m, and `nm`).
- **Next move for each result**:
  - Ends of blocks lead → how a return finds its block is the next experiment (a
    prediction beside the call, as hardware does it)
  - The record leads → what is known dead across a call or a return (the ABI
    says the flags do not live across them) is the next: a decision for an ADR,
    since it is a guess about the guest that a hand-written program may break
  - The guest's own work leads (loads, stores, compares that are one host
    instruction each) → the request is where it can be with this design, and that
    is said
  - Spread thin over thousands of blocks → the per-block costs (3, 4) are
    measured by taking each out in a build of its own
- **Cut-off**: one profile; the ten hottest blocks read instruction by instruction.
  No change to the translator in this experiment.

The result (uvicorn alone under `--profile`, 6,000 kept requests; 8,327 samples, 6,867
in generated code, 6,622 of them in the main thread's blocks).

**My hypothesis of where the samples gather was wrong, and what they show instead is
plainer: 60% of the samples in generated code are on the first host instruction of
a block.** Not in few blocks: 2,486 blocks were met; the twenty hottest hold 16.5%,
the hundred hottest 32%, a thousand 76%.

A sample falls on the instruction the processor is waiting to finish, and the first
instruction after a jump that was not foreseen is where it waits. So the time is not
the guest's work inside blocks, nor the record of the flags, nor the count: it is
arriving. Read in the hottest blocks (CPython's evaluation loop, at
`_PyEval_EvalFrameDefault`+0xfb and on; a stub of the PLT that is one `jmp` through
memory; the loader's `__tls_get_addr`):

- Inside, the guest's instructions are one to three host instructions each and
  hardly sampled: a load is `add`, `ldr`; a compare and its jump are `adds`,
  `b.eq`. In the hottest block 112 of 158 samples are on its first word, 7 on a
  load, and 26 on the end's lookup.
- Every end that is a jump through a register or memory, and every return, is the
  table's lookup and a `br`: the host's predictor sees one indirect jump, and for a
  return it has no stack to predict from as it has for the real thing. The lookup's
  second load is the next most sampled place (137 of the 1,495 samples in the forty
  hottest blocks; the table is 256 KB, more than the first-level cache).
- The first word is the look for a signal (`tbnz`), never taken; it is sampled
  because it is first, not for what it is.

What was added to r64m for this, and stays: `--profile` says for each sample in
generated code which block, which of the guest's instructions and which host
instruction, and prints the forty hottest blocks' code. The guest's addresses were
named by asking the same Python for the address of a known function (r64m gives a
file's pages as memory of its own, so the host's list of mappings has no names: a
list of the guest's mappings from r64m itself is owed).

By the card's first branch: how a return finds its block is the next experiment.

## R046: a return predicted by the host's own stack of returns (2026-11-26)

R045: the time in generated code is arriving at blocks, and a return is an indirect
jump the host cannot foresee.

The card, written before the run.

- **Hypothesis**: the host predicts a `ret` from the `bl` instructions before it,
  whatever register the `ret` names and whether or not that register came from the
  `bl`. So: the guest's CALL is made a host `bl` (to the way out for the function,
  which is joined to its block as now), and the host instruction after that `bl`
  is a landing for the guest's return address: a way out to that address, joined
  to its block like any other. The table of blocks gives that landing for the
  return address, not the block itself; and the guest's RET looks its address up
  as now and goes there by `ret` in place of `br`. When the guest returns where
  its call said, the host foresaw it; when it does not (a return address written
  by hand, `longjmp`), the jump is only not foreseen, as every return is now.
  Nothing rests on the host's stack or on x30 holding anything. With this
  uvicorn's request is 1.5 ms or less (1.78 now), and the primitives that call
  (p_recursion, p_funcptr) are faster, none slower by more than 2%.
- **What is changed from before** (R045): CALL (direct and through an operand) and
  RET in the translator; the table's entry for an address that is some call's
  return; the landings forgotten with the block that holds them.
- **Next move for each result**:
  - 1.5 ms or less → the three measures beside QEMU again, and the primitives
  - Faster but not 1.5 → the samples on first words again: what is left is jumps
    through a register that are not returns (CPython's dispatch, the PLT), and
    the table's size is the next thing to try
  - No change → the host does not predict as supposed, or the first word is
    sampled for another reason (the instruction cache): a program of two blocks
    that only call and return is timed both ways before anything else
  - Slower → taken out, and the same small program says why
- **Cut-off**: all tests on both hosts; uvicorn's CPU time one launch; the client's
  clock three; the primitives against R044's build.

The result (the Mac; QEMU 9.2.2 beside r64m `--jit`; the primitives on macOS against
R043's build).

**The goal's work is a fifth faster: uvicorn's request 1.29 ms (1.5 was the card's),
a kept connection 1.84 times QEMU by the client's clock, a new one 2.34, the start
2.98. One primitive pays for it: p_recursion takes 2.6 times as long, and the reason
is the host's own: its stack of returns is shorter than a recursion 200 deep.**

| | QEMU | R044 | R046 | QEMU / R046 |
|---|---|---|---|---|
| uvicorn's CPU time, a kept connection | 3.16 ms | 1.78 ms | 1.29 ms | 2.45 |
| nginx's worker's, a kept connection | 0.26 ms | 0.27 ms | 0.17 ms | 1.5 |
| nginx's worker's, a handshake | 10.1 ms | 4.1 ms | 3.4 ms | 2.97 |
| by the client's clock: the start | 3.17 s | 1.29 s | 1.06 s | 2.98 |
| a new connection | 16.88 ms | 8.43 ms | 7.20 ms | 2.34 |
| a kept connection | 3.59 ms | 2.49 ms | 1.95 ms | 1.84 |

(R044's column and its QEMU were measured while a container left over from R043's
hundred runs of the signal case was still going round on one core, which I found
two hours later and stopped; this table's QEMU and R046 are without it. That is why
QEMU's own times are lower here. The ratios in R044 and R045 were of runs side by
side under the same load; they are not measured again.)

What was built:

- The guest's CALL ends its block with a host `bl` over a landing: the instruction
  after the `bl` is a way out to the return address, joined to the block there
  like any other; what the `bl` jumps to is the way to the function (or, for a
  call through an operand, the lookup). The guest's RET looks its address up as
  before and goes there by `ret`.
- The table gives the landing for an address that is some call's return address.
- **A return address has one landing.** The first build had one for each block that
  held the call, and blocks overlap (one from a function's start, one from the
  top of its loop): the table held the landing of the block made last while the
  other block's `bl` ran, so every return was foreseen wrongly, and p_funcptr
  took 2.1 times as long, p_recursion 3.1. Found by the samples (all on the
  landing) and by switching the `ret` and the `bl` off one at a time: the `ret`
  alone was the cost. Now a block whose call another block already holds has its
  `bl` made a jump to that block's. p_funcptr is back within 4%.
- A program on the Mac itself (assembler, in the scratch folder) timed the idea
  apart from r64m: a return by `br` 1.7 ns, by `ret` after a `bl` 0.73, with a
  landing that jumps on 1.4; writing x30 in between, or loading the address from
  memory, changes nothing.

**What is slower, and stays so in this build**: p_recursion, 0.42 to 1.08 s. Its
`fib` is as before; its `depth` goes 200 calls down and comes 200 back, and the
host remembers only the last few dozen calls, so most of the returns on the way
back are foreseen wrongly, where the indirect jump before had one target and was
always right. A real x86-64 has the same short memory and the same cost, so this
is how such a program runs on hardware; but the primitive was 2.9 times QEMU and
is not now (not measured beside QEMU again). The other 21 that are raced are
within 2% (p_funcptr 3.6% slower: two jumps more for each call).

Not done: the landing is a jump to the block at the return address; the block could
go on there itself, as it does past a conditional jump, and the two jumps would be
none. And a return that is not predicted well could be told apart only by counting,
which nothing here does.

Tests: 118 unit tests plain and ordered on both hosts (one new: the one landing,
and its forgetting with the code); the 62 programs four ways; the guest's signal
program; container cases GCC 144, Python 72, redis 39, the goal 3; prim 116, trace
39.

## R047: six other images (2026-11-26)

For ADR 0020's point 3 (compatible). The owner allowed the downloads: the x86-64
images `nginx:alpine`, `node:22-alpine`, `postgres:17-alpine`,
`eclipse-temurin:21-jre-alpine`, `memcached:alpine`, `golang:1.23-alpine`, from
Docker Hub.

The card, written before the run.

- **Hypothesis**: each image, given one piece of its own work under r64m (`--jit`),
  either does it as the real machine does or stops for one of a few kinds of
  reason, and I guess which for each before running:
  - nginx (musl): serves a file over HTTP; as the goal's nginx did. Runs.
  - memcached (musl, threads, epoll): set and get over TCP. Runs.
  - Go (its own runtime: threads by `clone` with its own stacks, signals for
    preemption, `sigaltstack`, no libc): builds and runs a program. Stops at a
    system call or a flag of `clone` the table does not have, or at preemption's
    signals (`tgkill` to its own threads, many a second).
  - PostgreSQL (many processes, System V or POSIX shared memory, semaphores,
    `fsync`): `initdb`, start, one query, stop. Stops at shared memory (`shmget`
    or a shared writable mapping, which is a known gap) .
  - Node (V8 makes code and runs it; `mprotect` between writing and running; the
    code on pages the guest writes): a script that computes and prints. Stops,
    or runs slowly, where V8's generated code meets ADR 0016's guard; or at
    `pkey` or `memfd` calls.
  - a JVM (the same, more so; many threads; signals used for null checks:
    SIGSEGV on purpose): `java -version`, then a class run from source. Stops at
    the faults it takes on purpose, or at its use of `mprotect` and `madvise`.
- **What is changed from before** (R046): nothing in r64m until an image stops;
  then the smallest thing that lets it go on, each noted. What needs a decision
  (shared memory between processes is one) is brought to the owner, not built.
- **Next move for each result**:
  - Runs and prints what QEMU prints → the same command on hal; the case is
    written with hal's lines
  - A call not in the table, a flag, a structure → added as the others were
  - A stop of a kind that needs design (shared memory, a fault taken on
    purpose inside generated code, code made and run by the guest at speed) →
    written down with what it would take; the image is listed as not yet
  - Runs but differs from hal → found by the trace or by halving the work, before
    any guess
- **Cut-off**: each image tried; the small fixes made; the list of what runs and
  what does not, with reasons. Not in this experiment: speed, the network under
  load, memory.

The result so far (2026-11-26; the Linux container on the Mac, `--jit`; the downloads
were the six images, 69, 13, 168, 214, 298 and 246 MB as Docker counts them unpacked).

**Four of the six run and print what the real machine prints: nginx, memcached, Node
and the JVM. Go and PostgreSQL stop at the same thing, memory shared through a
mapping, which is a decision and not built.** My guesses: nginx and memcached right;
Node, the JVM, Go and PostgreSQL each stopped for a reason I had not guessed.

| image | what it was asked | result | what stopped it on the way |
|---|---|---|---|
| `nginx:alpine` | a page over HTTP, a 404, quit | as hal | nothing |
| `memcached:alpine` | set, get, incr, delete over TCP | as hal | nothing |
| `node:22-alpine` | arithmetic, a hash, JSON; an HTTP server and client; four worker threads | as hal | `mremap` |
| `eclipse-temurin:21-jre-alpine` | its version; `keytool` makes an RSA key and lists it | as hal | `getcpu`, CPUID, and a fault of r64m's own |
| `golang:1.23-alpine` | `go run` of a program with goroutines | compiles; the linker stops | a shared writable mapping of its output file |
| `postgres:17-alpine` | `initdb`, start, a query, stop | stops in `initdb` | `signalfd` (added); then shared memory (not tried) |

What was found and mended, each the smallest thing:

- **`mremap` of an address that is not mapped answered ENOMEM; the kernel says
  EFAULT.** musl finds the end of the first thread's stack by asking to grow one
  page after another downwards until EFAULT, and V8 asks musl. Node went round for
  ever, one call a page. Found with `--calls`, which was added for this: every
  system call said as it is made, and what it answered on a line of its own.
- **`getcpu` was refused**, and a JVM that is refused it calls the old fixed page of
  system calls at FFFFFFFFFF600800h, which no process of r64m's has. Handed on.
- **CPUID did not say CLFLUSH**, which a JVM will not start without. Said, with
  the line's size; the instruction was there already and does nothing.
- **HLT was "not written"**; it is a protection fault, as it is for any program
  (musl's `abort` ends on one when the signal has not ended the process, which is
  the case for the first process of a container).
- **A fault of r64m's own, in about four runs of ten of `keytool`**: a store by the
  interpreter into the guest's memory, refused by the host. One thread of the JVM
  writes new methods into the pages another is running from; that other's blocks
  are made from the page, the page is marked as holding translated code and the
  host made to refuse stores to it (ADR 0016); and the writing thread, which
  looked at the list a moment before, stores. Not a rare window: the writer is
  always between its look and its store. Bringing the list up to date before every
  store changed nothing (tried first, and taken out). Now a store into memory
  that other threads share is made by a routine that can be refused: the handler
  of faults sends it on at an ending that answers "refused", the list is looked at
  again, and the caller meets the answer as it meets any store beside code. Twenty
  runs of twenty since. (For the exchange that LOCK instructions make, the host is
  asked first by an addition of nothing; between the asking and the exchange the
  window is still there, and is not closed.) r64m now says where a fault of its
  own was before it ends, which is how this one was placed.
- `signalfd` and `signalfd4` handed on (the host already blocks what the guest
  blocks). Not yet seen working: PostgreSQL stops later.

The cases are `tests/container/{nginx,memcached,node,java}.txt`, with hal's lines;
`goal.txt` now asks for FastAPI's own files and not for nginx, which the official
image has too.

**What needs a decision: mappings that are shared.** r64m gives a file's pages as
memory of its own, read in when mapped (R025): right for a mapping that is private,
which is what loaders and compilers make. Go's linker maps its output file to write
it (`MAP_SHARED`, writable); PostgreSQL shares memory between its processes
(`MAP_SHARED` with no file, kept across `fork`, and POSIX shared memory, which is a
file in `/dev/shm` mapped the same way). On Linux, where the host's pages are the
guest's size, the host's own shared mapping can be put at the place in the guest's
memory; on macOS (16 KB pages) it cannot in general. It is a change to what the
memory is (ADR 0008, 0013), and so is asked.

Still to do in this experiment: the JVM's case three ways (running); Go and
PostgreSQL after the decision.

## R048: mappings that are shared, on Linux (2026-11-26)

For ADR 0020's point 3. The owner's word on R047's question: 「共有マッピングを作る」.

The card, written before the run.

- **Hypothesis**: on Linux, where the host's page is the guest's, a mapping the guest
  asks to be shared can be the host's own shared mapping put at its place in the
  guest's memory (`mmap` with `MAP_SHARED | MAP_FIXED` on the reservation; with the
  guest's descriptor, or none), and the list of regions needs to know nothing more
  of it: protection, unmapping and mapping over it go as for any region, a child
  made by `fork` shares it because the host's kernel does, and what is written
  reaches the file because it is the file's pages. With that, and System V's
  segments attached the same way (`shmat` at the place), Go's linker writes its
  program and PostgreSQL does `initdb`, starts, answers a query and stops, each as
  the real machine. What else stops them I guess to be small: a call or two
  (`fallocate`, `msync`, `shmget` and its three, `semget` if this build uses it).
- **What is changed from before** (R047): `Mem` gains one way to map (shared, Linux
  only; on macOS the answer stays ENOSYS for a writable file and a private copy
  for the rest, as now); `mmap` uses it for every `MAP_SHARED`; `msync`; the four
  calls of System V shared memory. ADR 0008 gets a note.
- **Next move for each result**:
  - Both run as hal → their cases, with hal's lines; all tests; then the network
    under load and memory, as the owner asked
  - Go runs, PostgreSQL stops elsewhere → what it stops at is found with `--calls`
    and mended if it is a call; if it is of another kind it is written down
  - A program that ran before stops (a shared read-only mapping that was a copy
    and is now the file: a JVM's archive, a locale's) → shared mappings that are
    not writable go back to being copies, and that is said
- **Cut-off**: the two images; all tests on both hosts; the container cases.

The result (the Linux container on the Mac; hal for the expected lines).

**Go and PostgreSQL run and print what the real machine prints. All six of R047's
images now do. The same six under OrbStack's QEMU print the same too: this was
catching up with QEMU, not passing it.**

| image | what it was asked | result |
|---|---|---|
| `golang:1.23-alpine` | a program with goroutines, a channel, a hash: compiled, linked and run | as hal |
| `postgres:17-alpine` | `initdb`, start, a table made, filled and summed, stop | as hal |

What was built, as the card had it:

- `Mem::map_shared`: the host's own shared mapping at the place in the guest's
  memory, of a file or of none; refused where the host's page is not the guest's.
  From then on a region like any other. `mmap` uses it for every `MAP_SHARED` on
  Linux; on macOS all is as before (a private copy, and ENOSYS for a file to be
  written).
- System V shared memory: `shmget` and `shmctl` handed on; `shmat` attaches the
  segment at a place in the guest's memory (the kernel puts it there, `SHM_REMAP`);
  `shmdt` unmaps the place.
- `msync` and `sync_file_range` handed on. (`fallocate` was there.)

Of my guesses of what else would stop them: `shmget` and its three, yes;
`sync_file_range`, not guessed (PostgreSQL goes on without it but says so);
`semget`, not used by this build. Nothing that ran before stopped: the card's third
branch did not come.

A fault of the test's own, found by its failing: the three ways of a case that
listens on a port (nginx's 80, Node's 8099) ran at once and met on the port.
They begin in `/tmp` now, which is how a file's cases are told to be run one at a
time.

**Open**: Node's case of four worker threads failed once under `--jit-check` in one
whole run of its file (with the 36 busybox cases at the same time), and then passed
eleven times of eleven, eight of them alone. What it printed that once was not
kept. With more than one thread `--jit-check` checks nothing and runs as `--jit`
does, so this is a fault that shows rarely with threads under load, and it is not
found.

Tests: 119 unit tests on Linux (one new: a shared mapping is the file's pages, and
a child's), 118 on macOS, plain and ordered; the 62 programs four ways; the guest's
signal program; container cases GCC 144, redis 39, nginx 3, memcached 3, Node 9,
the JVM 6; Go's and PostgreSQL's by `--jit` (their three ways each are owed: the
run's output was lost to a mistake in my command).

## R049: r64m in QEMU's place in the host's Docker (2026-11-26)

For ADR 0020's point 1: started by `docker run` with nothing naming r64m. The owner's
picture: 「Docker コマンド → OrbStack → r64m → イメージの起動」, and of where r64m sits:
「QEMU の代わり」.

The card, written before the run.

- **Hypothesis**: the kernel of OrbStack's VM runs x86-64 programs by what is
  registered with `binfmt_misc` (now `qemu-x86_64`, flags POCF). If r64m is
  registered for the same programs (flags PF: the program's own first argument
  kept, and r64m's file held open by the kernel so that every container finds it
  whatever its files are), an entry made later is asked first, and
  `docker run --platform linux/amd64 IMAGE` runs the image under r64m with no
  word of r64m in the command. r64m needs two small things for it: to know it was
  started by the kernel (by the name it is registered under), where it takes the
  path and the first argument in the kernel's order and uses the JIT; and to start
  itself again for `execve` by `/proc/self/exe`, since its own path means nothing
  inside another container. Then the six images of R047, the goal's request and
  redis run as their cases have them, by `docker run` alone.
- **What is changed from before** (R048): the two things in r64m; one line written
  to the VM's `binfmt_misc`, and taken away at the end (by `echo -1`), unless the
  owner says to leave it. While it is there every x86-64 container of the VM runs
  under r64m; the only other container running is arm64 and is not touched.
- **Next move for each result**:
  - The images run → the cases are run as Docker runs them (the test program
    itself an x86-64 container's command), and what differs from running r64m by
    name is written down; then the owner is asked whether it stays registered
  - The kernel still chooses QEMU → the order of entries is found out; QEMU's is
    not removed or disabled without asking (it is OrbStack's own)
  - Something that worked by name does not work this way (a set-user-ID program
    without flag C, a program run from a descriptor, `docker exec`) → written
    down, with what flag or change it would take
- **Cut-off**: the images and the goal by `docker run`; the entry removed, and
  QEMU seen to run x86-64 again.

The result (OrbStack's VM on the Mac, arm64 Linux 7.2).

**It is in QEMU's place: with one line written to the VM's `binfmt_misc`,
`docker run --platform linux/amd64 IMAGE` runs the image under r64m, with no word
of r64m in the command.** An entry made later is asked before `qemu-x86_64`, which
was left as it is.

Run that way, with what the real machine printed beside:

| | by `docker run` alone |
|---|---|
| the six images of R047 and R048, each its case | as hal, all six |
| the goal's case (uvicorn, nginx over TLS, twenty requests) | `200 nginx Hello, world`, 20 answered |
| redis: set, get, shutdown | as before |
| the official nginx by its own default command, a port published | answers `curl` on the Mac; `docker exec` runs a shell in it; `docker stop` ends it in 0.13 s |

What r64m needed:

- Started under the name `r64m-binfmt` it takes what the kernel gives in the
  kernel's order (the program's path, the first argument the program was given,
  the rest: flag P), uses the JIT, and says nothing of its own on the program's
  standard error (`--quiet`: a call that is not known is answered and not named;
  Node's, the JVM's and Go's output each had three to six such lines of r64m's
  before).
- For `execve` it starts itself by `/proc/self/exe`: the kernel holds r64m's file
  open (flag F) and a container has no path to it.

What is different from running r64m by name, and stays so:

- `ps` in the container shows r64m's own command line for every process, the
  program's after it.
- A program whose file has set-user-ID is not given that user (QEMU's entry has
  flag C for it; r64m's does not, and has not been tried with it). `su` as root to
  another user works, which is what PostgreSQL's case does.
- The entry is gone when the VM is started again, and r64m's file is held as it
  was when registered: a new build is registered anew. (A scratch script does the
  two; the tool for it in Rust is owed with the others: ADR 0012.)

**Not done of the cut-off**: the entry was not taken away at the end, and QEMU was
not seen to run x86-64 again afterwards. The entry is still registered as this is
written (every x86-64 container of the VM runs under r64m until it is removed by
`echo -1` to the entry, or the VM restarts). Whether it stays is the owner's to
say.

## R050: the goal in unchanged images, two containers on Docker's network (2026-11-26)

For ADR 0020's points 1 and 3. The owner: 「登録したまま」 (R049's entry stays), and of
the network: 「負荷を掛けて NW が足かせになるならそこも見直そうか」; of memory: 「メモリの問題も
あるけど」.

The card, written before the run.

- **Hypothesis**: with r64m registered, the goal's request completes in the
  ordinary images unchanged, each in a container of its own, by `docker run` alone:
  `python:3.13-slim` installs FastAPI and uvicorn with its own `pip` and serves;
  the official `nginx:alpine`, given a configuration and a certificate as files,
  takes TLS and passes to the other by its name on a network Docker made. What is
  new to r64m in that is little: a name looked up through Docker's resolver (UDP),
  and `pip` (a long Python program, its own TLS to the outside). Under load from
  several connections at once the two servers' time is the guest's code as before
  (R042: system time a few percent), so the network is not what holds it back; and
  each process of r64m holds tens of megabytes more than the program it runs,
  most of it the tables each process has of its own (R024's note for M11).
- **What is changed from before** (R049): nothing in r64m unless something stops.
  The same downloads as were allowed for R041 (`fastapi`, `uvicorn` from PyPI),
  made by the container itself this time.
- **Next move for each result**:
  - The request completes → the rate under load (1, 4 and 16 connections kept
    open, from the Mac), each server process's user and system time, and its
    resident memory beside the same program's on hal
  - System time is a large share under load (a third or more) → what calls are
    made for each request is counted with `--calls`, and how r64m hands each on
    is looked at: that is the owner's "looked at again"
  - Memory for a process is what the hypothesis says or more → which table holds
    it is measured (the reservation is not resident; the tables are), and a card
    for making them smaller follows; nothing is changed here
  - Something stops → mended if small, as in R047
- **Cut-off**: the request; one run of each load; the memory of each process
  once. QEMU beside it is not measured here: the entry stays registered by the
  owner's word, and QEMU cannot be chosen while it is.

The result (the Mac, r64m registered in OrbStack's VM as R049 left it; hal for the
same programs run natively).

**ADR 0020's point 1 holds: in the ordinary images unchanged, by `docker run` alone,
the goal's request completes.** `python:3.13-slim` installs FastAPI and uvicorn
with its own `pip` when it starts (under r64m) and serves; the official
`nginx:alpine`, given a configuration and a certificate as files, takes TLS and
passes to the other container by its name on a network Docker made; `curl` on the
Mac gets 200, the certificate verified, `Hello, world`.

One thing stopped it: **`sendmmsg` was not known**, and glibc's lookup of a name
sends its two questions with it (the lookup failed, and `pip` with it). It and
`recvmmsg` are done now by the calls for one message, one after another.

**Under load the network is what holds the work back, and it is not r64m's doing:
the same on the real machine.**

| kept connections from the client | requests a second | uvicorn, user + system | nginx, user + system |
|---|---|---|---|
| 1 | 624 | 1.11 + 0.08 ms | 0.11 + 0.05 ms |
| 4 | 848 | 0.99 + 0.08 ms | 0.12 + 0.10 ms |
| 16 | 772, then falling to 480 | 1.2 to 1.9 + 0.1 ms | 0.3 + 0.1 ms rising to 0.4 + 1.5 ms |

uvicorn is one process and is the limit at about 900 a second. With time nginx's
system time grows to three quarters of its time and the rate halves. I took it
first for r64m's `epoll` (the calls counted with `--calls`: two waits a request,
nothing going round), then for each worker warming up (the same load three times:
it got worse, not better). What it is: nginx opens a new connection to the other
container for every request, each closed one waits a minute in the kernel
(TIME_WAIT: 12,700 of them after the first load, 19,800 four loads later), and
finding a free port for the next gets slower. **hal, natively, with the same two
containers and sixteen connections: 1,504 requests a second falling to 808 as its
waiting sockets go from 12,000 to 28,300.** So it is the configuration and the
kernel; a connection to the other server that is kept (nginx's `keepalive`) is the
remedy, and it is a line of configuration, not of r64m. System time in uvicorn
stays under a tenth throughout.

**Memory: a thread of the guest's costs r64m about 18 MB of its own, and that is
what grows.**

| resident memory | under r64m | natively on hal |
|---|---|---|
| uvicorn after its start (2 threads) | 79.6 MB | 47.2 MB |
| uvicorn under 16 connections (5 threads) | 148 MB | 48.2 MB |
| an nginx worker | 18.8 MB, most of it its parent's (the parent's own: 0.9 MB) | 6.6 MB, most of it the file's |
| a shell that waits | 0.45 MB of its own | |

Of uvicorn's 148 MB the guest's own memory is 55 MB, near what the real machine has;
r64m's own is 92.5 MB, of which 25 MB is generated code. FastAPI runs a function
that is not `async` in a pool of threads, the pool grew to five under load, and each
thread has its own blocks and its own tables (ADR 0019, point A): the same 28,000
blocks translated once for each. The JIT is not making blocks over (`--jit-stats`:
28,164 blocks after 1,000 requests, 28,186 after 8,000, never emptied). Two other
costs seen: a file's pages are a copy in every process (15 MB of libraries here,
which natively are shared), and the code a block is made from is read again by
each thread.

By the card: the network needs nothing of r64m; memory is as the hypothesis said
and more with threads, and what would make it smaller is a decision of design
(blocks shared by the threads of a process; a file's pages mapped, not copied,
which R048's shared mapping shows the way to on Linux): a card of its own, not
begun.

Not measured: QEMU beside any of this (r64m stays registered, by the owner's word,
and QEMU cannot be chosen while it is).

Tests after `sendmmsg`: 118 unit tests on macOS. The container suites were run
before it (Python 72, the goal 3, Go 6 three ways each; PostgreSQL's three ways are
still owed: the run printed nothing, for a reason not yet looked at).

## R051: where a thread's 18 MB is (2026-11-26)

The owner: 「メモリを先に」 (memory before the CI). ADR 0020 sets no bar for memory;
its point 3 asks that it be measured, and R050 measured it: about 18 MB of r64m's
own for each thread of the guest's, of which generated code is about 5 MB.

The card, written before the run.

- **Hypothesis**: most of a thread's 18 MB is not generated code but what is kept
  about each block (three vectors a block, the map of places, the maps of pages):
  some 13 MB for 28,000 blocks, near 480 bytes a block, where the code itself is
  180. If so, a good part can be had back without touching ADR 0019's point A
  (each thread its own blocks): kept smaller, the same design.
- **What is changed from before** (R050): r64m says, when asked (`--jit-memory`),
  how many bytes each part holds for a thread: code written, the blocks' own
  vectors, each map, the table. R050 had only the process's total from outside.
- **Next move for each result**:
  - What is kept about blocks is more than half of a thread's cost → it is made
    smaller in place (one array for all blocks' instruction starts, nothing kept
    that is only needed while a block is made), and the goal's memory measured
    again beside R050's
  - Code is more than half, or what is kept cannot be made much smaller → nothing
    is saved short of sharing: the design is written as an ADR for the owner
    (blocks of a process shared by its threads), and nothing built before the word
  - The parts do not add up to what the process holds from outside (a third or
    more unexplained) → what else is a thread's is looked for in the process's
    map (`smaps`): the host's stacks, the allocator's own, a copy of the list of
    regions
  - Either way the ADR for sharing is proposed with the numbers this gives: what
    sharing would save beyond what is saved in place, and a file's pages mapped
    in place of copied (15 MB a process in R050)
- **Cut-off**: the measurement by the count of this session's first hour; what is
  saved in place the same day. Sharing is not begun on this card.

**Result (2026-11-26): the 18 MB was not a thread's cost. It was a leak: what a
thread of the guest's held of r64m's was never given back when the thread ended.
Fixed; and a thread's own record of its blocks made smaller. uvicorn under load:
100 MB to 92 MB at the same moment, and flat from there where it grew 20 MB with
every round before.**

R050 read its numbers wrongly. It saw 148 MB with five threads against 80 MB with
two and divided. The pool of threads FastAPI runs a plain function in lets a thread
end when it has been idle and starts another when it is needed: the count of
threads stayed five while many more than five had been started.

What `--jit-memory` said (uvicorn alone in the R041 image, 16 connections kept from
the Mac, 1,300 requests a second), for the first thread, 28,000 blocks of 315,000
of the guest's instructions:

| part | bytes before | after |
|---|---|---|
| generated code | 7.4 MB | 7.4 MB |
| the record of each block (120 bytes, room for 32,768) | 3.9 MB | 3.9 MB |
| where each instruction begins | 6.9 MB | 2.5 MB |
| jumps made to come to a block | 0.9 MB | 0.9 MB |
| landings of returns | 0.7 MB | 0.2 MB |
| the map of places (41,000 known) | 1.4 MB | 1.4 MB |
| the rest (the table, the map of landings, the allocator's own) | 1.5 MB | 1.5 MB |
| in all | 22.7 MB | 17.9 MB |

So the hypothesis held for the first thread: what is kept about blocks was twice
the code. But a thread of the pool makes few blocks (under 2,000; 0.33 MB of code
resident each): sixteen of them added 22 MB to the process, 1.4 MB each, the table
(0.26 MB) and the guest's own stack among it.

**The leak.** With the load given in rounds, 14 seconds idle between them (the pool
lets its threads go after 10):

| | before | after |
|---|---|---|
| resident after rounds 1, 2, 3, 4 | 101, 121, 142, 163 MB | 100.4, 100.4, 100.5, 99.8 MB |
| pieces of generated code mapped | 17, 32, 47, 62 | as many as threads |

A thread's `exit` was answered by the kernel's `exit` at once (R033), from deep
inside the thread's loop: its blocks and their 64 MB mapping, its copy of the list
of regions and the host's stack were never given back. And a process has 1,024
places for pieces of code: a Python program that starts and joins 1,200 threads
one after another stopped with "too many pieces of generated code in one process".
A server left running would have stopped the same way after its thousandth thread.
Now a thread other than the first clears and wakes as before (with every signal
blocked first) and then returns from its function: everything it held is dropped,
and the host's C library ends the thread and frees its stack. The first thread
still ends by the kernel's call: the process's code is the one it gave. A case is
added to the Python suite (1,200 threads, one after another).

**Made smaller in place**: where each instruction of a block begins is kept as two
32-bit numbers (the guest's address as bytes after the block's first) in a slice
with no room to spare, in place of a 32-bit and a 64-bit number in a vector with
room; a block's landings likewise without spare room.

| uvicorn's resident memory | R050's build | now | natively on hal (R050) |
|---|---|---|---|
| after its start (2 threads) | 79.0 MB | 73.3 MB | 47.2 MB |
| under 16 connections (17 threads) | 101.1 MB | 91.8 MB | 48.2 MB |

By the card: what is kept was more than half and was made smaller in place. On
sharing blocks between threads the numbers say no for now: ADR 0019's price
(translated twice, held twice) is 1.4 MB for a thread of this pool, of which the
code is 0.33 MB; no ADR is proposed. What is left over the real machine is the
first thread's 18 MB and the guest's memory (a file's pages copied in every
process, 15 MB here): mapping a file's pages in place of copying is the next thing
that would save tens of megabytes, in every process.

**Found beside it, not fixed: Node's workers fail one run in ten when several
containers run at once.** `node` with four worker threads (the case in the Node
suite), 40 runs five at a time by `--jit`: 9 failed with R050's build and 2 with
this one, all the same way: "at 0x7ff800270b ... an instruction the CPU refuses",
which is HLT in the guest's C library (musl crashes itself so when it finds its
own state broken), after some 130 million instructions. The same address in both
builds: not of this card's making. R047 saw it once under `--jit-check` and left
it as not understood. Something a thread of r64m's does to memory another thread
is using; a card of its own is owed, and it is ADR 0020's point 3.

Tests: 118 unit tests on macOS, 119 on Linux; 62 programs four ways (248 runs);
primitives 116, traces 39; the container suites three ways each: Python 75 (the
new case among them), the goal 3, the JVM 6, Go 6, nginx 3, memcached 3, redis 3; Node 8
of 9 (the failure above). hal could not be reached (the new case's expected line
is what the program says it prints). The comparison with QEMU was not run (r64m
stays registered).

## R052: Node's workers fail one run in ten under load (2026-11-26)

The owner: 「Node の不具合を先に」. For ADR 0020's point 3 (the Node image is on its
list, and the case is in its suite).

What is known before the card (R051, and the guest's C library disassembled): the
address the run stops at is the HLT at the end of musl's `abort`. The guest called
`abort` and said nothing first; `abort` raised SIGABRT, then took the handler away
and sent the signal to itself again, and still ran on to the HLT. So there are two
things: why Node aborts, and why a SIGABRT with no handler, sent by a thread to
itself, does not end the process there as it does on the real machine.

The card, written before the run.

- **Hypothesis**: a call of the kernel's that r64m answers gives, now and then
  when threads end or start beside one another, an error the real machine never
  gives (libuv calls `abort` without a word when a call on a thread, a lock or its
  own descriptors fails): the last calls of the thread that aborts, said by
  `--calls`, show one answered with an error just before the signals of `abort`.
- **What is changed from before** (R051): the same case run 40 times, five at a
  time, with every call said; and r64m says, when it stops at an instruction the
  CPU refuses, the return addresses on the guest's stack, so that who called
  `abort` can be read from the program's file.
- **Next move for each result**:
  - A call answered with an error stands just before → that call's answer is made
    the real machine's, a case that fails often is written from it (the call made
    by many threads at once), and the 40 runs repeated
  - No call fails, and the caller of `abort` is a check of the program's own
    state (a lock's, a queue's) → the memory is what is wrong: the instructions
    that are to be atomic and are not (R033's list) are looked for in the code
    around the caller, with ordered blocks on and off
  - Saying the calls makes the failure go away (0 of 40) → the caller's address
    alone, without `--calls`, and the question is put again from that
  - Either way: a SIGABRT with no handler sent by `tkill` to the caller ends the
    process by that signal, as a small test in the guest's suite
- **Cut-off**: the 40 runs with calls said, and the reading of them, by this
  session's first hour; no second round of the same 40 without a changed condition.

**Result (2026-11-26): the hypothesis was wrong in its detail and the cause is
found and fixed. No call answered wrongly: `clone` wrote the new thread's id
where the parent asked after the thread had begun to run. The kernel writes it
before.**

- With every call said, 0 of 40 failed (the saying changes the timing). By the
  card, the stack at the stop alone: 2 of 60 failed, both with the same caller of
  `abort`: libuv's `uv_mutex_unlock`, from `NodePlatform::RegisterIsolate`, in a
  worker's thread just started. A release of a lock was refused.
- musl keeps a thread's id in the thread's own block, and has the kernel write it
  (`CLONE_PARENT_SETTID`). A lock that checks its holder writes that id into the
  lock's word. r64m started the host's thread, which ran the guest at once, and
  the parent wrote the id when the thread had told it: a thread that took a lock
  in that moment wrote nothing into it. The lock was then held by nobody, and its
  release, once the id was there, was refused; or two threads held it.
- The same explains the stop at HLT: `abort` sends the signal to the thread's id,
  and that was still nothing. A SIGABRT with no handler does end a process under
  r64m: tried by hand from the first thread and another, with signals blocked,
  and after a handler was set and taken away (all four end with 134).
- Shown by making it worse: with the parent held back 50 ms before its write, 9
  of 30 runs failed; with the fix and the same 50 ms, 0 of 30, side by side.
- A small program for the GCC suite: 2,000 threads, four at a time, each takes and
  releases a checked lock. The build before never finishes it (a lock nobody
  holds is waited for for ever); the fix prints no refusal.
- Now the new thread writes its id itself, before it tells the parent and before
  it runs any of the guest.
- The Node case at the end, 60 runs each, three and three at a time: the build
  before 59, the fix 60. (That alone would not tell them apart; the two above do.)

R047's one failure of this case and R051's one in ten are this. Any C library's
threads were open to it: a program whose first act in a new thread is to take a
lock that checks its holder.

Tests: 118 unit tests on macOS, 119 on Linux; 62 programs four ways; primitives
116, traces 39; the suites three ways each: GCC 21 (the new case among them),
Python 75, the goal 3, the JVM 6, Go 6, Node 9, nginx 3, memcached 3, redis 3. One
of Python's (a timer that counts how far a loop got in 50 ms, under the checker)
failed once while ten suites ran together and passes alone, three times of three:
the load's, not r64m's. hal could not be reached: the new case's expected line is
what the program says it prints. Left in: at an instruction the CPU refuses, r64m
says the thread and the top of the guest's stack.

**Added the same day**: hal reached. The two cases new in R051 and R052 (1,200
threads one after another in Python; 2,000 threads and a checked lock in C), run
there in the same images as the suites give them, print what the cases expect and
end with 0. The cases' notes now say so.

## R053: a file's pages mapped, not copied (2026-11-26)

The owner: 「作るか」, of the mapping of files. ADR 0020 sets no bar for memory;
this is for what R050 and R051 measured: every process holds a copy of its own of
each file it maps privately (a C library, Python's modules of C: 15 MB in
uvicorn; an nginx worker holds 18.8 MB where the real machine's holds 6.6).

The card, written before the run.

- **Hypothesis**: a private mapping of a file can be the host's own (`MAP_PRIVATE`
  of the guest's descriptor, put at the place in the guest's memory), as R048's
  shared one is: the kernel then keeps one copy of the file's pages for all
  processes and gives a process its own only of the pages it writes, as it does
  for the same program on the real machine. The pages no longer count as the
  process's own (anonymous) memory, a program's start reads less, and nothing a
  program can see changes but what Linux itself does differently from a copy:
  a page wholly past the file's end is a fault (SIGBUS), not zeroes.
- **What is changed from before** (R052): `mmap` of a file without `MAP_SHARED`
  goes the way of R048's shared mapping with the host's flag private, where the
  host's page is 4 KB (Linux); elsewhere it is the copy it was. Then, if that
  holds, the program's own file and its interpreter's, which r64m's loader reads
  whole and copies (Node's is 130 MB).
- **Next move for each result**:
  - The suites pass three ways and uvicorn's own memory falls by about the 15 MB
    → the loader's segments mapped the same way, measured on Node and on nginx's
    workers
  - A suite fails → what the case does with the mapping (a write through it, a
    protection changed, code translated from it and then changed) is made a unit
    test of `Mem`, and fixed there
  - The memory does not fall (the pages are written anyway: relocations) → said
    with the numbers, and the copy kept: nothing is changed for no gain
- **Cut-off**: `mmap` and its measurement today; the loader only if the first holds.

**Result (2026-11-26): mapped, in `mmap` and in the loader. A process's own memory
falls by what the files were: an idle Node holds 64 MB where it held 260 (the real
machine 49); uvicorn's own memory 50 MB where it was 72. A start is no faster.**

- `mmap` of a file without `MAP_SHARED` is the host's private mapping at the place
  (`Mem::map_file`, beside R048's shared one), where the host's page is 4 KB;
  elsewhere, and with `--copy-files`, the copy as before. A descriptor the host
  will not map is answered with the host's own error.
- The loader maps the pages a segment's file bytes fill, when the segment lies in
  memory as in the file, clears what follows them in their last page when the
  segment is larger in memory, and gives the rest as the guest's own. r64m itself
  reads the file through a mapping of its own and gives that back when the
  program is loaded: until now it read the whole file and kept it (Node's is 130
  MB: held once by r64m and once more as the guest's copy).

| resident memory, in MB | before | files copied (`--copy-files`) | files mapped | natively on hal |
|---|---|---|---|---|
| an idle Node: all, of which the process's own (anonymous) | 260, 257 | 134, 133 | 64, 21 | 49, 7 |
| uvicorn after its start | 73, 72 | 73, 72 | 69, 50 | 47 (R050) |
| uvicorn under 16 connections | 92, 91 | 92, 91 | 87, 69 | 48 (R050) |
| an nginx worker's own (of eight) | | 12.7 | 4.8 | |

What is no longer the process's own is the file's pages, which the kernel holds
once for every process that maps the file and can give up when memory is short.
The whole count falls less than the process's own because the file's pages that
are touched are still counted for each process that touches them.

A start, by the clock around `docker run`, nine runs each way, one after the
other: `node -e 0` 378 ms copied and 350 mapped; `python3 -c pass` 295 and 296;
`import fastapi` 786 and 774; busybox's `true` 214 and 197. The container's own
start is most of it: the hypothesis's "a start reads less" is true and is not
seen.

By the card: the first held and the loader was done. What Linux does differently
from a copy is now r64m's too: a page wholly past a file's end faults; a file
changed under a mapping is seen in pages not yet written.

Beside it: PostgreSQL's cases pass three ways (R050 left them owed: "the run
printed nothing"; that run had been started through the image's entrypoint script,
as R051's first runs of the other Alpine images were).

Tests: 118 unit tests on macOS, 119 on Linux (a private mapping of a file among
them: read, written without reaching the file, the file's later change seen in a
page not written, protected, unmapped, a descriptor that is none refused); 62
programs four ways; primitives 116, traces 39; every container suite three ways
by `cargo xtask container`: 258 runs, PostgreSQL's among them.

## R054: the kept connection, the one of point 2's four under 2 (2026-11-26)

The owner: 「維持接続を 2 倍に」. For ADR 0020's point 2: by the client's clock a
request on a kept connection is 1.95 ms under r64m and 3.59 under QEMU (R046):
1.84. Twice needs 1.79 ms or less: 8% of r64m's.

What is known (R046's table): the two servers' CPU time for a request is 1.29 ms
(uvicorn) and 0.17 (nginx's worker), 1.46 together; the client sees 1.95. Under
QEMU 3.16 and 0.26, 3.42 together; the client sees 3.59. So r64m has 0.49 ms in a
request that is not the servers' CPU, where QEMU has 0.17. R044 took that part for
the client's and the port's own, the same on both sides; these numbers say it is
not the same.

QEMU cannot be run beside r64m while r64m is registered (the VM's entry for QEMU
names no file). The work is measured against r64m's own build of before, the two
side by side; the race with QEMU needs r64m's entry switched off for its minutes
and on again, which is asked of the owner when there is something to race.

The card, written before the run.

- **Hypothesis**: about 0.3 ms of a kept request under r64m is waiting that r64m
  adds: a hand-over between threads or processes (nginx to uvicorn and back;
  uvicorn's loop to its pool's thread and back, which is how FastAPI runs a
  function that is not `async`) that is slower under r64m than its CPU time
  shows: a call that sleeps where it need not, a wake that comes late, or time in
  the kernel that the sampler does not count. Taking it away is the 8% and more.
- **What is changed from before** (R046): each thread's user and system time is
  read from the kernel for 2,000 kept requests (not the sampler's count), with
  the client's clock for the same requests, so that the request's wall time is
  divided into uvicorn's loop, its pool's thread, nginx, and what is none of them;
  and the system calls of one request are said with the time each took.
- **Next move for each result**:
  - A part of the wall time is in neither server's CPU and a call is seen to wait
    (a timeout, a sleep, a wake that the other side had to poll for) → that call's
    handling is made the kernel's own and the request measured again
  - System time is a large part (a quarter of the CPU or more) → which calls, and
    what r64m does around each (copies, the list of regions, signals' masks)
  - Neither: all of it is user time in generated code → R044's list of what still
    leaves blocks (LOCK CMPXCHG, XCHG with memory, short REP MOVS and STOS: 4% by
    its count) is built, the largest first, and the request measured after each
- **Cut-off**: the division of the time today; no more than three changes built on
  it before the race is asked for.

**Result (2026-11-26): the hypothesis was wrong; by the card's third branch three
things were built, and a kept request is 13.5% shorter by the client's clock
against the build before (the card needed 8%). The race with QEMU is not run yet.
Beside it: the JVM fails one run in six when four run at once, in the builds
before this one too; not understood.**

Where the time is (2,000 kept requests, each thread's time from the kernel; r64m
alone on the machine, not beside QEMU as in R046):

| | ms a request |
|---|---|
| the client's clock | 1.61 to 1.73 |
| uvicorn's loop (one thread), on a CPU | 1.13 to 1.20, of which system 0.05 |
| uvicorn's pool's thread | 0.05 |
| nginx's worker | 0.14 |
| the rest (the client, the port, waiting for a CPU 0.17) | 0.30 |

Nothing waits that need not: it is uvicorn's own thread in generated code. (R046's
1.95 was with QEMU's servers on the same machine at the same moment.) A request is
1.38 million of the guest's instructions; generated code was gone into 1,505 times
and the interpreter ran 1,050 instructions for it: REP STOS 185, REP MOVS 158,
LOCK CMPXCHG 108, XCHG with memory 150, and conditional jumps 320, most of them
after one of those.

Built:

- **CMPXCHG of memory** at 32 and 64 bits, with LOCK or without: the host's
  compare-and-swap (CASAL), then the comparison's flags and the A register.
- **XCHG with memory** at 32 and 64 bits: the host's swap (SWPAL).
- **REP MOVS and REP STOS** going up, of fewer than 64 elements: a loop in the
  block that does not touch the host's flags; with more, with the direction flag
  set, or begun already by the interpreter, it is the interpreter's (which does a
  long one as the host's own copy). The count of instructions goes on as the
  interpreter's does, a fault part of the way too.
- Encodings held to clang's (266 now).

Three things found on the way:

- **A real fault, in the new work**: a comparison, a long REP MOVSB, and the jump
  on the comparison. The scan that decides whether a record of the flags is
  needed took the jump to be answered by the host's flags, as it is while the
  block runs on; but a long repetition leaves the block, and the interpreter then
  ran the jump with flags nobody had recorded. uvicorn stopped at its start with a
  read of address 8. The scan now takes a repetition for what may leave. A unit
  case (it fails without the change).
- The checker (`--jit-check`) ran a repetition that the interpreter does element
  by element (its places overlap) as one instruction of the block: with p_fill's
  millions it never ended. Such a pass through a block is now not compared; and
  the checker's copy works the flags out from the record before it steps (a pass
  not compared left them not worked out, and the next comparison differed for
  nothing). The checker's message names the block's bytes.
- **The same hazard is older than this card and is not closed**: any instruction
  of a block that faults at a store (a store to a page that holds translated
  code, which the interpreter then makes) leaves the block the same way, and a
  jump after it in the same block was taken to be answered by the host's flags.
  Not seen to fail; owed a card.

| a kept request, eight pairs one after the other | the build before (R053) | this one | |
|---|---|---|---|
| the client's clock, median | 1.713 ms | 1.482 ms | 13.5% shorter |
| uvicorn's loop on a CPU, median | 1.19 ms | 0.99 ms | 17% |
| the interpreter's instructions a request | 1,050 | 400 | |
| times generated code is gone into | 1,505 | 952 | |

If QEMU's side is what it was in R046, the ratio is near 2.1; only the race says.

**The JVM under load.** The JVM's case of making a key failed in the full run of
the suites and then one run in six of the same case run alone again and again
beside another. The JVM's own report: SIGSEGV in `CodeHeap::allocate`,
`NativeJump::patch_verified_entry`, `ciEnv::register_method`: where it writes its
own code. Run alone, one at a time: 16 of 16 pass. Four at once, 60 seconds each
at most:

| build | passed | SIGSEGV | hung |
|---|---|---|---|
| R052's | 31 of 32 | 0 | 0 (one ended with r64m's own code 2) |
| R053's (before this card) | 37 of 48 | 10 | 1 |
| R053's with `--copy-files` | 26 of 32 | 4 | 2 |
| this card's, REP in blocks | 20 of 24 | 2 | 2 |
| this card's, without REP in blocks | 28 of 32 | 3 | 1 |
| this card's, without, files copied, the program read not through a mapping | 31 of 32 | 1 | 0 |

So it is not of this card's making, and it is there since R053 at the latest. A
first round with each of the three new translations switched off in turn said
"REP" (24 of 24 without it); the next round without it failed 4 of 32: the first
was chance. That was two rounds too many of one kind of test (the counts are too
small to tell builds apart at one failure in six), and several runs were started
with no limit of time and waited for while they hung: an hour was lost so. What
is needed is the JVM's report in full for one failure (the address it could not
write and what was mapped there), which was not got: a card of its own, and it
is ADR 0020's point 3.

Tests: 120 unit tests on macOS, 121 on Linux, plain and ordered; 62 programs four
ways; primitives 116, traces 39; the container suites by the JIT, all; three ways
before the last change of the scan's neighbours: all but two runs (the JVM's, as
above, and Python's timer under the checker while five suites ran at once, as in
R052).

## R055: the JVM is sent a SIGSEGV where it writes its own code, under load (2026-11-26)

The owner: 「JVM の不具合を先に」. For ADR 0020's point 3. What R054 left: four
JVMs at once, one run in six ends with SIGSEGV in the JVM's own writing of code
(`CodeHeap::allocate`, `NativeJump::patch_verified_entry`,
`ciEnv::register_method`), or hangs; since R053's build at the latest; alone, 16
of 16 pass. R054 counted runs of whole builds against each other and could not
tell them apart.

The card, written before the run.

- **Hypothesis**: the fault the JVM is sent is r64m's own doing: a store to a
  page r64m itself has closed to writing because some thread's blocks were
  translated from it (ADR 0016), met by a thread that does not take it for that:
  its copy of the list of regions is older than the mark, or the page was opened
  and closed by another thread between this thread's fault and its second try.
  The JVM writes code that other threads are running all the time; a C program
  does not.
- **What is changed from before** (R054): no build is counted against another.
  r64m says each fault it hands the guest as a signal (`--faults`: the thread,
  the instruction, the address, what the list of regions says of that address in
  this thread's copy and in the shared one, and whether the page is marked as
  code), and the JVM writes its own report to standard error: one failure is
  read whole.
- **Next move for each result**:
  - The address is in a region the guest may write, marked as code or with the
    two lists differing → the hypothesis: the store is tried again with the list
    renewed and the page opened, in place of the signal; then 48 runs four at
    once
  - The address is not the guest's to write at all (no region, or read only by
    the guest's own asking) → the fault is true to the guest's memory as r64m has
    it, and the question is who unmapped or protected it: the calls of that run
    are said (`--calls`) for the address
  - No failure in 48 runs with the saying on → the saying changes the timing: the
    line is kept in memory and said only at the end
- **Cut-off**: every run has a limit of 60 seconds; one round of 48 runs to
  capture; after the fix one round of 48. No third round of the same kind.

**Result (2026-11-26): the hypothesis held, and the fault is r64m's: a store to a
page closed for translated code, opened for the store, and closed again by another
thread before the store was made. Fixed: SIGSEGV in 1 of 100 runs where it was in
about 15 of 100. Two things are left and not understood: that one, and a hang at
the JVM's end in about 5 of 100.**

- Two failures were caught in 12 runs with `--faults` and the JVM's report on
  standard error. Both: a write (the JVM's C2 compiler thread, in
  `ciEnv::register_method` and `CodeHeap::allocate`) to a page the guest may read,
  write and run, marked as holding translated code, in this thread's list and in
  the shared one alike. So the guest was sent a fault for memory it may write.
- The way: a store to such a page that is not this thread's own code (R010's
  "beside") opens the page, has the interpreter run the instruction again, and
  closes it. If the second run faulted too, the fault went to the guest. Another
  thread that makes a block from the page in that moment closes it. The
  instruction is now begun again in that case; and the first question (is the
  page marked?) is asked of the list as it is now, not of this thread's copy as
  it was when it last looked.
- After: 48 runs four at once, no SIGSEGV, 4 hung (60 seconds); then 12 (1 hung)
  and 40 (1 SIGSEGV, no hang) while looking at the hang: 100 runs, 1 SIGSEGV, 5
  hung. Before (R054's table, the builds since R053): 23 SIGSEGV and 7 hung in
  216.
- **The hang** (one caught): every thread asleep, no CPU time going on, and the
  JVM's work done: the key was made and said so. It is at the JVM's end, and it
  was there before the fix (R054: 7 of 216). Which call each thread waits in was
  not got: the round that asked for it met no hang in 40 runs.
- **The one SIGSEGV after the fix** was in a run without `--faults`: nothing is
  known of it. Another way to the same end, or another thing.

By the card's cut-off (one round after the fix; no third of the same kind) it
stops here. Owed, as a card of its own: the hang at the end (the calls the
threads wait in: `/proc/PID/task/*/syscall` of a hung run), and the residue, with
`--faults` on in every run.

Left in: `--faults` (each fault handed to the guest, with what both lists say of
the address).

Tests: 120 unit tests on macOS, 121 on Linux; 62 programs four ways; the
container suites by the JIT, all. No unit test of the race itself: it needs two
threads and the moment between them.

## R056: the JVM hangs at its end, under load (2026-11-27)

The owner: 「終了時の固まりを追う」. For ADR 0020's point 3. What R055 left: four
JVMs at once (keytool making an RSA key), about 5 runs in 100 never end: the key
is made and said so, every thread is asleep, no CPU time goes on. It was there
before R055's fix (7 of 216). Nothing is known of where the threads wait: R055's
round that asked met no hang in 40 runs, and its one caught hang was removed
before it was asked.

The card, written before the run.

- **Hypothesis**: a wake is lost, and it is r64m's: a thread waits on a word in
  the guest's memory (a join on a thread's id word, or a lock of the JVM's)
  whose waking r64m does in the kernel's place or carries for the guest: the
  clearing and waking at a thread's end (`thread_ends`, R051), or a signal the
  JVM's end depends on. The other possibility is r64m's own end: a thread in
  r64m's exit (`std::process::exit`) that waits on something of r64m's.
- **What is changed from before** (R055): the asking is part of every run, not a
  round of its own: a run that passes its 60 seconds is asked, before it is
  removed, for each host thread's state and the call it waits in with its
  arguments (`/proc/1/task/*/syscall`, `stat`), the guest's base, and its output
  so far. Eight at once in place of four, to meet it sooner.
- **Next move for each result**:
  - Some thread waits in `futex` on an address inside the guest's memory → which
    word (a thread's id word, or a lock), what it holds, and who should have
    woken it: the path of that wake in r64m is read and a small guest program
    made that takes it many times
  - Some thread waits on an address of r64m's own, or is in `exit_group`'s way →
    r64m's end: which lock, and who holds it
  - Threads wait in other calls (a read, a wait for a signal) → what the JVM's
    end waits for by that call, and whether r64m delivered it (`--calls` of the
    last second)
  - No hang in 96 runs → 0 of 96 where 5 in 100 was seen says eight at once
    hides it (under 3 in 100 at 95%): back to four at once with the same asking,
    48 runs, and no more waiting after that: the end of the JVM is read by its
    calls instead
- **Cut-off**: every run 60 seconds; 96 runs to capture, stopped at the second
  hang caught; after a fix, one round of 96.

**Result (2026-11-27): the hypothesis held: a wake was lost, and r64m lost it, in
the `futex` call itself. Fixed: no hang in 96 runs of eight at once, where 3 of
104 hung. One run in 96 still ends with SIGSEGV; that is another thing, and now
there is a record of it.**

- Beside r64m, the same load (eight at once): the real machine, 96 of 96 end (hal;
  the owner: 「halでもやってみるか」); user-mode QEMU on the Mac, 48 of 48 (the
  owner: 「qemuでもやっていいよ」; r64m's entry in the VM was removed for the two
  minutes and put back). So it is r64m's.
- Round a (40 runs): two passed 60 seconds. One was not a hang: its threads had
  moved two seconds later, and it had not said its first line; a slow run. The
  other was one: the VM thread in a sleep it repeats, the rest in `futex`.
- Round b (64 runs, with the words read from the process's memory): two hangs,
  the same. In each, two or three threads wait on one word in the C library's own
  memory (musl's `__lock`, likely the allocator's), expecting 80000002h and
  80000003h (held, two and three counted), and the word holds 2: not held, two
  counted. It was released and nobody was woken. Everything else follows: a
  thread asleep there holds a mutex of the JVM's that four more wait for.
- The cause: r64m took the fifth argument of every `futex` call for an address
  and answered EFAULT when it was none of the guest's. FUTEX_WAKE does not use
  it: the register holds what was left there. musl's `__wake` and glibc's
  `futex_wake` do not look at the answer. So a wake was dropped whenever R8
  happened to hold a large number at a release that had waiters: a matter of
  which code ran before, and of there being waiters at all, which is why load
  brings it out. It has been so since the call was written; R053 is only where
  it was first counted.
- A program that makes the call with the unused registers set says so at once
  (now a case in `tests/container/gcc.txt`): the real machine `wake 0`, r64m
  before `wake -1` (EFAULT), after `wake 0`. The same under glibc 2.41 (the
  owner: 「glibcは大丈夫なのかな？」): before -1, after 0. Not a matter of which C
  library; Go makes the call itself and was open to it too (not tried).
- After: 96 runs, eight at once: no hang, 95 end well, one SIGSEGV. The count by
  itself is weak (0 of 96 against 3 of 104); what carries it is the state read
  from the hung processes and the call shown refused.
- **The SIGSEGV that is left** (one in round b, one after; both with `--faults`):
  a write to a page that both lists say the guest may write, with no mark of
  translated code (prot 7 in `CodeHeap::allocate`, prot 3 in `memset`). So the
  host refused a store the list allows: R055's kind, by a way R055's fix does
  not cover (it asks the list once, after the fault; the mark can be gone by
  then while the host's page was still closed at the store). Owed as its own
  card.
- Not looked at: `shmctl`'s third argument is taken for an address for every
  command, and IPC_RMID does not use it. The same kind; programs pass a null
  there. Owed with the survey below.

What the owner asked next (「マルチスレッド関連を強化するか」), in this order: the
SIGSEGV above; a survey of every call for arguments it does not use, the real
machine beside r64m with those registers set; cases of contention (locks,
condition variables, semaphores, threads made and ended; musl, glibc, Go) with a
limit of time each, for CI.

## R057: a store the list allows, refused by the host: the JVM's SIGSEGV that is left (2026-11-27)

The owner: 「マルチスレッド関連を強化するか」; the first of the three things R056
names. For ADR 0020's point 3. What R055 and R056 left: one JVM in a hundred,
eight at once, is sent SIGSEGV for a write to a page that both lists say it may
write, with no mark of translated code (two caught with `--faults`: prot 7 in
`CodeHeap::allocate`, prot 3 in `memset`).

The card, written before the run.

- **Hypothesis**: the page was closed (marked, the host's page read only) when
  the store was made, and open again when the thread asked: another thread that
  stores beside code opens the page for its one instruction, by taking the mark
  off, and puts it back. A thread whose store was refused a moment before asks
  in that window, finds no mark, and hands the fault to the guest (`Jit::run`:
  the question is asked once, after the fault). R055 closed the opposite window
  (closed again before the second try), not this one.
- **What is changed from before**: no waiting for a capture; the two records are
  the observation. The condition is made: a unit test with two threads, one
  that marks and unmarks a page in a loop, one that stores to it through
  `Jit::run`, which must never be handed a fault. Then the fix: a write the host
  refuses where the list, as it is now, lets the guest write is r64m's own
  doing whatever the mark says at that moment, and the instruction is begun
  again (with a bound, so that a refusal that stays is still said).
- **Next move for each result**:
  - The test hands a fault before the fix and none after → the fix is kept; 96
    JVM runs of eight at once, and the suites
  - The test hands no fault before the fix → the window is not this one: the
    calls that write the guest's memory for it (`read` into a closed page: the
    kernel answers EFAULT) are looked at next, by the JVM's report of which call
  - SIGSEGV again in the 96 after → its `--faults` line says whether it is the
    same kind (list allows) or a true one
- **Cut-off**: the test runs five seconds at most; one round of 96 after.

**Result (2026-11-27): the hypothesis held. A test of two threads hands the guest
the fault without the fix and never with it; 96 JVM runs of eight at once after:
96 end well, no SIGSEGV, no hang.**

- The test (`a_store_refused_while_another_thread_had_the_page_closed_is_made`):
  one thread marks a page of data and takes the mark off, over and over; the
  other stores to the page 300,000 times through `Jit::run`. With the new arm
  switched off the run was handed `Fault { Write }` (one run of two; the other
  met no refusal at the wrong moment). With it, six runs of six pass, each with
  20 to 80 refusals, about half of them found with the mark gone.
- The fix, in `Jit::run`: a write the host refuses where the list as it is now
  lets the guest write, in a memory more than one thread has, is begun again,
  whatever the mark says at that moment; after a thousand in a row for one
  instruction the fault is said as before (nothing known makes that happen).
- The owner asked beside it: 「マルチスレッドでも…一方が Fault や例外になっても他方
  には影響がないは確認しなくて良い？」. There was only what the JVM's runs imply.
  Now a case (`tests/container/gcc.txt`): three threads take 20,000 faults each
  (a read where nothing is, a write to a page that may only be read, a division
  by zero) and return from the handler, on stacks of their own for handlers,
  while four threads work beside them and take a lock; the handler counts a
  signal in a thread that made none and one with another thread's address. The
  real machine (hal, twice): none of either, and the four results. r64m by the
  interpreter, the JIT and the JIT checked: the same lines.
- Not covered: the calls that have the kernel write the guest's memory (`read`
  into a page closed for translated code is answered EFAULT by the host's
  kernel, if nothing opens it first). Not seen to fail; to be looked at with the
  survey of calls R056 owes.
- Tests after R056 and R057 together: 121 unit tests; the ten suites of
  container cases by the interpreter, the JIT and the JIT checked (264 runs of
  88 cases, the two new ones among them): all pass. (The three ways had not
  been run since before R055.)

### R049's tool, in Rust (2026-11-27)

The owner: 「もう公開してもいいかな？ … Docker に組み込む手順がないか」 and
「戻し方も書こうか」. Not an experiment: what R049 left owed ("a scratch script
does the two; the tool for it in Rust is owed"). `cargo xtask install`,
`uninstall` and `status`, and the README's section for them.

- The list is written by this same tool, built for arm64 Linux and started in a
  container with every right, in an image made of r64m and the tool and nothing
  else (`FROM scratch`): no shell, no image fetched. The list is mounted by the
  kernel's call made by hand (xtask has no dependencies).
- r64m's file is the image's own: the kernel holds it open (flag F) after the
  container and its files are gone. (The scratch script copied it to a volume
  first; that was not needed.)
- Tried on the Mac (OrbStack): install → a container's own command line is
  r64m's; uninstall → it is the program's alone, QEMU runs it; install again →
  r64m's; the JVM's image by `docker run` answers its version. Not tried: Docker
  Desktop, a Linux host with no VM.

### Before the repository is made public: what its history holds (2026-11-27)

The owner: 「監査を進める」. Not an experiment: the look that comes before a
repository of the owner's is made public. Every commit of every branch (162; `main`
and `port`, no tags), as one text of all their changes, and what GitHub holds
beside them.

- Files that ever were: 72 paths, all source, tests, documents, three
  Dockerfiles, the CI's file. None of the kinds that hold secrets (no `.env`,
  key, certificate store, database, state file); no blob over 300 KB.
- Searched for: access keys and tokens by their forms (AWS, GitHub, Slack,
  Google, `sk-`, private key blocks, JWTs): none. The words password, secret,
  token, credential, API key, bearer: one line, a case that reads `/etc/passwd`
  in a container. The JVM's case gives `keytool` a store password, `changeit`,
  for a store it makes and throws away.
- Who and where: every commit is by the owner's GitHub no-reply address. No
  e-mail address, no home directory's path, no address of a machine but
  127.0.0.1, no hardware address. The owner's name is in LICENSE. `hal` is named
  as the real machine many times, by that name only; one line says it was out
  of reach "(the VPN)".
- The ledger quotes the owner's words, in Japanese, where a card begins by them
  (twenty places). They are instructions about the work; the owner's to
  say whether they stay.
- GitHub: no pull request, no issue, no release, no wiki, no secret set for
  Actions. The CI's past runs and their logs become readable with the
  repository; they hold what the tests print.
- `main` is 29 commits behind `port` (it has R046's state): what a visitor
  sees first. Whether it is brought up before the change is the owner's word.

## R058: two unit tests never end, once, on the CI (2026-11-27)

The first pull request's run (`e88778a`, arm64 Linux, the pull request's own run)
stood for 24 minutes in the unit tests and was cancelled; the push's run of the same
commit had passed in 4. What had not ended: `mem::tests::the_host_is_as_open_as_the_list_to_the_host_page`
and `mem::tests::the_reservation_is_large_and_closed`. Both ask the host by a child
(`host_refuses`: `fork`, the child reads or writes the address and dies of it or
does not, the parent waits). The test added today
(`a_store_refused_while_another_thread_had_the_page_closed_is_made`, R057) had
ended in that run; it runs beside them, with a thread that changes protections
without pause.

The card, written before the run.

- **Hypothesis**: the child does not end. It is made by `fork` from one thread of
  a process with many: it has that thread alone and the others' state as it stood.
  When its access is refused, r64m's handler of faults runs in it (put in place
  by any test that made generated code), and something on that path, or in the C
  library's `fork`, waits for what a thread that does not exist in the child
  held. Today's test makes such moments many (a thread always inside a change of
  the list and of the host's protections), which is why it shows now.
- **What is changed from before**: nothing was looked for before; it was seen
  once. The condition is made: the unit tests' file for arm64 Linux run in a
  container with the filter `mem::tests` and today's test together, again and
  again, each run with 20 seconds at most; a run that passes them is asked for
  its processes and what each waits in. The same count with `mem::tests` alone.
- **Next move for each result**:
  - A child is found that waits (in a lock of the C library's, or spinning in
    the handler) → `host_refuses` is made not to depend on the process's state:
    the child ends by the kernel's own doing, with nothing of r64m's or the
    library's run in it after `fork`; and whether r64m itself forks so (`fork`
    for the guest, `sys/linux.rs`) is read for the same hazard
  - The parent is found in `fork` itself → the same, by the lock it waits for
  - No hang in 300 runs with the test and 300 without → not this: the CI's
    runner (4 cores, another kernel) is the condition missing; the two tests are
    then run there alone by a workflow run asked for by hand, with a limit
- **Cut-off**: 300 runs each way, 20 seconds a run at most.

**Result (2026-11-27): not reproduced, and not explained. What the two tests
depended on that is not theirs is taken away; whether that was the cause is not
known.**

- 300 runs of `mem::tests` with today's test in an arm64 Linux container (the
  Mac's VM): none hangs. The whole file of unit tests, 12 runs on four cores:
  none; each ends in 2 to 3 seconds. The count without today's test was not
  made: with none in 300 there is nothing to compare.
- What differs on the CI's runner and could not be made here: what the host does
  with a process that dies of a fault. Here the kernel writes a file `core` or
  nothing; with cores allowed, each run of the tests left one of 226 MB (the
  child is a copy of the whole test process) and was no slower. An Ubuntu host
  pipes the dying process to a program, and the kernel waits for it. That is a
  guess at the cause, not a finding: the runner was not looked into.
- The children of `host_refuses` died as the process they were copied from would
  have: by r64m's handler of faults when another test had put it in place (it
  says a line and steps aside), and then with a core taken if the host takes
  one. Now the child sets the two signals to their default and says it is not to
  be dumped, before the access: it ends by the kernel alone. With cores allowed,
  no file is left any more.
- The CI's job says how its runner takes a crash before the tests
  (`core_pattern`, the limit): the next run shows whether the guess has a
  footing.
- If it happens again with this in: the card's last move (the tests run on the
  runner by hand, with a limit and the processes listed).

### R058, what the runner said (2026-11-27)

The step added to the CI printed, on the pull request that carried it:
`|/usr/lib/systemd/systemd-coredump %P %u %g %s %t 9223372036854775808 %h %d`, and a
limit of 0. So on the runner a process that dies of a fault is piped to a program,
whatever the limit (my understanding of the kernel: a limit of 0 does not stop a
pipe; not tried). The guess has a footing; that this is what stood for 24 minutes
is still not shown. With R058's change the children are not dumped at all.

## R059: arguments a call does not use, every call (2026-11-27)

The owner: 「総点検を始める」; the second of the three things R056 names. For ADR
0020's point 3. R056's fault was one of a kind: r64m looked at a register the
kernel does not look at for that call, and refused the call by it.

The card, written before the run.

- **Hypothesis**: there are more of the kind. Read through, the calls' table
  (`src/sys.rs`, `src/sys/linux.rs`, 173 arms) shows two: `epoll_ctl` reads the
  event its fourth argument points at for EPOLL_CTL_DEL too, where the kernel
  ignores the pointer, and `shmctl` takes its third for an address for IPC_RMID,
  which has none. Everything else either hands all six registers to the host's
  kernel as they are (which ignores what it ignores) or reads an argument only
  under the flag or command that gives it meaning. Reading can miss one.
- **What is changed from before** (R056): not one call found by a hang, but all
  of them asked at once: a program of the guest's (`guest/src/bin/unused.rs`)
  makes each call twice, the registers the call does not use holding zero and
  then all ones, and says whether the two answers are the same; the calls whose
  arguments depend on a command are made with that command and a pointer that
  points nowhere. What the real machine prints (hal) is what is right.
- **Next move for each result**:
  - r64m differs from hal on a line → that call is put right (the argument is
    looked at only where the kernel looks at it), and the line stays as a case
  - hal itself says "differs" for a call → my table of what the call uses is
    wrong there: put right, not r64m
  - No line differs beyond the two found by reading → those two are put right
    and the program stays as the case for the kind
- **Cut-off**: one program, one run on hal, one on r64m three ways; no waiting.

**Result (2026-11-27): three faults, one of them not of the kind looked for. All
put right; r64m prints the real machine's 55 lines by the interpreter, the JIT
and the JIT checked.**

- The real machine: 55 lines, every call ending the same with zero and with all
  ones in the registers it does not use; the process ends with its code. So the
  table of what each call uses is right as far as it goes.
- r64m, before: the first 53 lines the same, and then r64m itself died of a
  fault (139), by all three ways, at `epoll_ctl`.
  1. **The last address of all was allowed** (`Mem::search`): an access at
     FFFF_FFFF_FFFF_FFFFh had its end cut off at the top, which left a range of
     no bytes, which nothing refuses; r64m then read at one byte before the
     guest's memory and on into it, and the host's fault was r64m's own. Any
     access of the guest's own to that address did the same by the interpreter.
     Not the kind looked for; found because the pointer that points nowhere was
     that address. A unit test now holds the highest addresses to a fault.
  2. **`epoll_ctl`, EPOLL_CTL_DEL**: the event was read through the fourth
     argument, which the kernel ignores for that command. Now not looked at.
  3. **`shmctl`, IPC_RMID (and SHM_LOCK, SHM_UNLOCK)**: the third argument was
     taken for an address. Now not looked at.
- The two that reading had found were the two the run found; reading had not
  found the first, and the run had no other.
- The program stays as a case (`tests/guest/unused.txt`), run three ways with the
  guest's other program.
- What it does not cover: the calls it does not make (about 120 of the 173
  arms, most of which hand all six registers to the host's kernel untouched);
  `clone`'s arguments under its flags; and the calls that have the kernel write
  the guest's memory where a page is closed for translated code (R057's note).
- Tests: 122 unit tests; six container suites by the JIT (PostgreSQL, which
  removes its segment, among them).

## R060: the Go case never ends, once, on the CI (2026-11-27)

The pull request that carries R059: its CI run stood for 41 minutes after the JVM's
suite had passed, in the suite that comes next, Go's (`go version`, then `go run`
of a program with goroutines), and was cancelled. The same code had passed that
suite on the Mac. Go's toolchain is many processes with many threads each; it
makes its own `futex`, `epoll` and `clone` calls, without a C library.

The card, written before the run.

- **Hypothesis**: one of two. (a) R059's change is at fault: Go takes a
  descriptor out of its `epoll` with a pointer to an event, as it always did, and
  something in the three changes answers it differently. (b) It was there before:
  a wake lost or a signal not delivered under load, of R056's kind but by a
  path the JVM does not take, met now by chance on a runner with four cores.
- **What is changed from before**: nothing was looked for before. The condition is
  made on the Mac: the case's `go run` in the same image, four at once in
  containers held to four cores together, 60 seconds each at most, with R059's
  build and with the build before it (R057's); a run that passes its time is
  asked for every process and thread of its container and the call each waits
  in, and the words those calls wait on (R056's way).
- **Next move for each result**:
  - Hangs with R059's build and none in as many runs with R057's → (a): the three
    changes are taken out one at a time, by the call the hung threads wait in
  - Hangs with both → (b): the state read says which wake or signal; the fix is
    made where it is, and R059 is not held back by it longer than that takes
  - No hang in 48 runs of each → the runner is the condition missing (four
    cores, its kernel): the suite is run there by hand with a limit of time on
    each case and the same asking, and until then the CI's cases get a limit so
    that a hang is a failure in minutes, not a job that stands
- **Cut-off**: 48 runs of each build, 60 seconds a run.

**Result (2026-11-27): not reproduced; neither (a) nor (b) is shown. The CI's next
hang will say where it is: every case has a limit of time now, and a case that
passes it fails with every thread's call.**

- A first round with each container held to one core counted four hangs that
  were none: Go's toolchain compiles its library on a first `go run`, which one
  core does not finish in 60 seconds; the threads had moved between two looks.
  A wrong condition of mine, not a result.
- With four cores each and 150 seconds, two at once: 24 runs of R059's build,
  all end well. The CI's own second run of the same code (started by the card's
  push) passed in 4 minutes.
- R057's build was not run beside it: with none in 24 of the new build there is
  nothing for it to be compared with (a count that changes no move).
- So one hang in three CI runs of this code, none in 24 on the Mac. Whether
  R059's changes have any part in it is not known. Go takes descriptors out of
  its `epoll` by a pointer to a real event, which R059 no longer reads; the
  kernel does not read it either.
- What is done instead of waiting: `tests/container.rs` gives each case a limit
  (300 seconds by the JIT, 1,800 by the other two ways; `R64M_CASE_SECONDS` says
  another) and, past it, fails the case with every process and thread of the
  container and the call each is in with its arguments (the cases run inside the
  container, so it is all there to read), then ends the case's processes. Tried
  with a limit of 3 seconds on the Go case: the failure lists `go`'s threads in
  `futex` and `epoll_pwait` as they stood.
- Owed: the same limit for the unit tests' step (R058's kind), which has only
  the job's 120 minutes.

## R061: the CI's four minutes (2026-11-27)

The owner: 「CI時間かかりすぎだな 時間削れる？」, and to the plan: 「そうしよう」. Not
for a point of ADR 0020: for the work itself, now that every change waits for
the job `linux` before it can be merged.

Where the four minutes go (the last run that passed on `main`): setting up 30
seconds (checkouts, `nasm`, `rustup`); unit tests, primitives and traces 69
(building, mostly); the guest's programs 2; loading the two built images 9; the
container cases 135, ten suites one after another, eight of them pulling their
image first.

The card, written before the run.

- **Hypothesis**: the container cases take 135 seconds because they wait for
  one another; the runner has four cores and the suites share nothing but the
  two built images. Four at once (`--jobs 4`, which `cargo xtask container` has
  had since it was made) brings the step to 50 or 60 seconds and the job to
  under three minutes.
- **What is changed from before**: one word in the workflow's step, for a push
  and a pull request (the JIT alone). The weekly run of three ways stays one at
  a time: a case of Python's that times 50 milliseconds failed once under the
  checker when many suites ran together (R054), and the weekly run is not waited
  for.
- **Next move for each result**:
  - The step is under 70 seconds and passes on the pull request's own run → kept;
    the next cut (the tests and the cases as two jobs side by side, which needs
    `main`'s required check changed to two) is put to the owner with this number
  - It is not faster by a third or more → pulls or the runner's disk are the
    wait, not the cores: the step's log says which, and the images are kept
    between runs instead
  - A case fails that passes alone → fewer at once (2), and the case named
- **Cut-off**: the pull request's own run and one re-run; R060's limit of time
  holds each case.

**Result (2026-11-27): the container cases 135 → 82 seconds on the runner, the
job 4 minutes → 3 minutes 16. Kept. Not the 50 to 60 I said.**

- On the Mac (images at hand): 64 seconds one at a time, 33 four at once, all
  suites passing both ways.
- On the runner, this pull request's own run: the step 82 seconds, every suite
  passing; the job 3 minutes 16 seconds (it was 4 minutes 0 to 4 minutes 13 in
  the five runs before).
- Why not 50 to 60: a step cannot be shorter than its longest suite with its
  pull, and the JVM's alone is 40 seconds of cases (R060's log) after its image
  is pulled; the staging build is in the step too. My number took the Mac's
  ratio for the runner's.
- What is left of the job: setting up 30 seconds, the tests' build and run 70,
  the cases 82, loading images 10. The two long steps do not need each other:
  as two jobs side by side the wait would be about two minutes. That needs
  `main`'s required check to become two, which is the owner's to say.

## R062: the CI as two jobs side by side (2026-11-27)

The owner: 「2ジョブに分ける」. What R061 left: of the job's 3 minutes 16, the
tests (70 seconds, with 30 of setting up) and the container cases (82, with
loading images) do not need each other.

The card, written before the run.

- **Hypothesis**: as two jobs a pull request waits for the longer one: about two
  minutes (the cases: runner start, `rustup`, images, a build of r64m and the
  test, 82 seconds of cases), where it waits 3 minutes 16 now.
- **What is changed from before**: the job `linux` becomes `tests` and `cases`;
  `main`'s required check becomes those two (the owner's word, with the
  question). The tests' job gets ten minutes at most and the cases' twenty (two
  hours for the weekly three ways), where there were two hours for all: R058's
  kind of hang ends the job in ten minutes.
- **Next move for each result**:
  - Both pass and the longer is under 2 minutes 30 → kept; the protection is
    changed to the two before the merge
  - The cases' job is no shorter than the one job was → its own setting up (it
    builds r64m again, which the tests' job had built) is the wait: what is
    built is kept between runs
  - A job waits for a runner while the other runs → said with the time; that is
    GitHub's queue and not to be cut here
- **Cut-off**: the pull request's own run and one re-run.

**Result (2026-11-27): a pull request waits 2 minutes 17 seconds, where it waited
3 minutes 16. Kept; `main` requires `tests` and `cases`.**

- The pull request's own run, from its start to its last job's end: 2 minutes 17
  seconds. `tests` 1 minute 46; `cases` 1 minute 58, begun 14 seconds after it
  (it waits for `images`, which looks up its cache in 12).
- The cases' job builds r64m again, as said; with that it is still the shorter
  wait by a minute than the one job was.
- `main`'s protection, as GitHub answers it after the change: required checks
  `tests` and `cases`; the rest as it was (pull requests only, administrators
  included, no approving review, no forced push, no deletion).
- From 4 minutes this morning: 3 minutes 16 by four suites at once (R061), 2
  minutes 17 by two jobs. What is left is the runner's start, `rustup`, and two
  builds of r64m; not looked at further.

### What R058 and R060 left owed: the tests' own limit of time (2026-11-27)

Not an experiment. `cargo xtask test` now gives its tests 420 seconds
(`R64M_TEST_SECONDS` says another) and, past them, says every process of its user
with every thread's name, state and call, and ends the lot: R058's two tests would
have shown their children, and what those waited in, after seven minutes and not
stood for twenty-four. `cargo xtask waits` says the same list at any time (the
thing done by a shell script three times today). Tried: on the Mac with a limit of
2 seconds (ended, nothing left running; there is no `/proc` to read there); on
arm64 Linux `waits` beside the unit tests as they ran. A thread's name is cut to
15 letters by the kernel, which is the test's module and not the test; the
harness's own "has been running for over 60 seconds" names the test.

## R063: contention, held to the real machine (2026-11-27)

The owner: 「あとは上から順にやるか」; the third of the three things R056 names.
For ADR 0020's point 3. R052, R056 and R057 were each found by a server under load
and not by a case: the cases have threads, but hardly any that wait for one
another.

The card, written before the run.

- **Hypothesis**: r64m is right where threads wait for one another by every way
  the three worlds have: musl's (a C program: a mutex, a condition variable
  taken in turns, a semaphore, a read-write lock, a barrier, 2,000 threads made
  and ended), glibc's (Python: a queue between producers and consumers, a
  condition in turns, a barrier), and Go's own (a mutex, a ring of channels,
  sleeping goroutines; no C library). Each prints counts that are the same
  wherever it is done rightly. If one is wrong it shows as another count, or as
  a case that does not end, which R060's limit turns into a failure that says
  where.
- **What is changed from before**: three cases added, their lines the real
  machine's (hal). Before they are added each is run under r64m by the three
  ways, and then by the JIT forty times, four at once.
- **Next move for each result**:
  - All the same, none hangs → the cases go into the suites, and that is what is
    known: these ways of waiting hold under this load
  - A count differs or a run does not end → that is a fault found: its own card,
    by the call the threads wait in; the case goes in when it passes
  - A case takes more than ten seconds by the JIT → made smaller: the CI is not
    to be slowed by it
- **Cut-off**: forty runs of each by the JIT, 60 seconds a run; no more.

**Result (2026-11-27): the three programs print the real machine's lines by all
three ways; the C and the Python ones forty times of forty under load. Go's forty
say nothing about waiting (my limit again), and one of them found another fault.**

- The real machine (hal), each program twice: the same lines both times.
- r64m, each by the interpreter, the JIT and the JIT checked: the same lines.
  By the JIT: the C program 1 second, Python's under 1, Go's 21 (nearly all of it
  `go run` compiling). So Go's is not a case of its own: it is put into the
  program the Go suite compiled already, which now prints two lines.
- Forty runs by the JIT, four at once, 60 seconds each: C 40 of 40 as the real
  machine, Python 40 of 40.
- Go: 39 of 40 had not ended at 60 seconds and were not hung: four `go run` at
  once, each compiling Go's library on all the cores, do not finish in a minute
  (the processes listed were compilers at work). The same wrong condition as
  R060's first round, by me again; the count says nothing of Go's waiting. Two
  at once with 150 seconds (R060) is the condition that works.
- The fortieth ended at once and wrong: Go's compiler, started for
  `internal/runtime/atomic`, was stopped 991,106 instructions into its life by
  `Write of 0x7e000000b9 that is not allowed`: a fault r64m handed a process that
  had no handler for it yet. One in the several thousand processes those forty
  runs started. Not of this card's making (the existing case runs the same
  compiler). Its own card: R064.
- The three cases are in the suites (`gcc.txt`, `python.txt`, and Go's program in
  `go.txt`).

## R064: Go's compiler stopped by a write that is not allowed, once (2026-11-27)

Found by R063. For ADR 0020's point 3. One process of several thousand: Go's
`compile`, early in its start, a write to 0x7e000000b9, no handler in place yet,
so r64m ended it and said so. `go run` then says the package's name and fails. On
the CI this would be a case with another output, not a hang; whether it has to do
with R060's hang is not known.

The card, written before the run.

- **Hypothesis**: the write is one the guest may make, and r64m refused it: of
  R055's and R057's kind (a page closed for translated code, or a list that is
  behind), by a way those two fixes do not cover. The address is odd for Go
  (its heap is at 0xc000000000; this is in the range r64m gives to `mmap`), which
  fits a page r64m itself is handling. The other possibility: the address is
  wrong because a register or a value was, which is a fault of translation.
- **What is changed from before**: nothing was known but the line. The condition
  is R063's (the Go case, by the JIT), two at once with 150 seconds, and r64m
  says each fault it hands the guest with what both lists say of the address
  (`--faults`, which the processes it starts inherit), until one is caught or
  60 runs have passed.
- **Next move for each result**:
  - The lists allow the write (mapped, writable) → r64m's refusal: the path by
    which a fault reaches `Event::Stop` without R057's question is found and
    closed, and a test made for it
  - The lists say nothing is there, or read only → the address or the mapping is
    wrong: the calls of that process (`--calls`) for where 0x7e00000000 came
    from, and the block that made the write checked against the interpreter
  - Not caught in 60 runs → 1 in several thousand processes is not to be waited
    for: Go's compiler started alone in a loop (thousands of starts in the time
    of sixty builds) is the condition instead
- **Cut-off**: 60 runs, 150 seconds each; then the last move, once.

**Result (2026-11-27): two faults, both r64m's, both fixed. The write was Go's own
through a pointer half written: r64m stored a byte at a time. On the way a hang
was met and explained: a child of `fork` waited for a lock nobody in it held.
With both fixed, 960 builds of the package, eight at once: none stands, none
fails, no fault is said. (Before: 2 of 320 stood, and 1 start in several
thousand was stopped.)**

How it went, with what I got wrong:

- The card's order was changed before the first run: sixty whole runs of the
  case would have met one or two of several thousand processes in 17 minutes;
  the compiler started alone, or the one package built again and again, makes
  thousands in a minute. `compile -V=full` 2,000 times: nothing. `go build -a
  internal/runtime/atomic` 320 times, eight at once: it did not end.
- **The hang.** Two of the 320 stood for good: `go`'s child, made to start a
  tool, had not started it; its one thread waited in `futex` on an address of
  r64m's own, and its parent waited for it (the pipe by which r64m knows that a
  `vfork`'s child has started its program). The child is a copy of a process
  with several threads, and has one: a lock of r64m's that another thread held
  as the copy was made (the list of regions, held for every `mmap`, `munmap` and
  mark of code) is held in the child for ever. **Very likely R060's hang**: the
  CI's Go suite stood the same way; it was not seen there, so it is not shown.
- The fix: the thread that forks holds r64m's own locks while the copy is made
  (the process's, the list of regions, standard error) and lets go in parent and
  child, as the C library does for its own.
- **My first form of the fix hung every time**, at Go's first child: it took
  standard output's lock too, which r64m's first thread holds for as long as it
  runs, so any other thread that forked waited for ever. An hour went by before
  the owner asked why Go took so long; the run had no limit that could end it
  (`timeout` before `docker run` ends the client and not the container: the
  detached container with `docker wait` is the form that works, as R054 had
  already taught). The case I had written forked from the first thread only and
  passed. It now forks from two threads, 1,000 children each, beside three
  threads that map and unmap; the first form does not end it, the second does in
  a second, and the real machine prints the same line.
- **The write.** With the hang gone and `--faults` on, the 320 builds ended in 65
  seconds and one said it: `Write of 0x7e000000b9`, nothing mapped there, and
  Go's own report: `runtime.preemptone`, from `sysmon`, its register holding
  0x7e00000000 where a pointer to a goroutine should be: the high half of a
  pointer and a low half of zeros. Another thread was setting that pointer to
  nothing at that moment.
- r64m's interpreter, in a memory more than one thread has, made every store by
  a routine that copies **a byte at a time** (so that a store the host refuses
  can be stepped out of). A store of eight bytes was eight stores, and a reader
  between them saw half a pointer. On x86 it is one store. The JIT's stores were
  always one; this is the interpreter's, which every block falls back to.
- The fix: the routine stores eight bytes at a time, then four, two, one. A
  test of two threads (`a_store_seen_by_another_thread_is_whole`) fails with the
  old routine at once ("eight bytes read as 0x33") and passes with the new.
- After both: three rounds of 320 builds, eight at once, each ended in under 170
  seconds with no build failed and no fault said.
- Not looked at: what else reads memory in pieces. The interpreter's loads go by
  the compiler's copy, which for two, four and eight bytes is one load (not
  checked in the built code). An instruction that reads, changes and writes
  without LOCK is two acts on x86 as well.
- Tests with both fixes: 123 unit tests; every suite by the JIT; the three
  suites with new cases (GCC's, Python's, Go's) by all three ways.

## R065: threads and fork by the book, with musl and with glibc (2026-11-27)

The owner: 「forkパターンもそうだけどpthread周りもちゃんとテストするか」, and to
whether glibc too, which needs an image fetched: 「glibcももちろんやろう」. For ADR
0020's point 3. R064's hang was met by a case that forked from a second thread,
which no case had done; what else no case does is not known until it is done.

The card, written before the run.

- **Hypothesis**: r64m does what Linux does for the whole of what a C program
  can ask of threads and of `fork`, by either C library. Where it does not, a
  program built for the purpose says so in a line that differs from the real
  machine's. The parts: how a thread ends (its value, detached, cancelled at a
  point and at any moment, cleanup handlers, the first thread ending before the
  others); what is a thread's own (`__thread`, keys and their destructors,
  `pthread_once`, a stack of a size asked for); waiting with a limit of time
  (a condition, a mutex, a semaphore) and the kinds of mutex (recursive,
  checked, tried); signals and threads (one sent to a thread, masks, `sigwait`,
  a stack overrun caught on another stack); `fork` (from a second thread, a
  child that makes threads, `pthread_atfork`, a lock held across it,
  `posix_spawn` and `vfork` beside threads, a grandchild, SIGCHLD, a child
  stopped and continued, a child ended by a signal).
- **What is changed from before**: five programs in one new file of cases
  (`tests/container/threads.txt`), run in two images: Alpine's with musl (the one
  there is) and a new one, Debian 12 with GCC and glibc
  (`tools/images/glibc`; Debian's packages fetched, by the owner's word). The
  lines are the real machine's (hal), which must be the same for both libraries
  or the program is at fault and is changed.
- **Next move for each result**:
  - A line differs under r64m → a fault found: put right here if it is small (a
    call's answer), its own card if it is not (a mechanism missing); the case
    goes in when it passes, and what does not pass is listed as not done
  - hal's two libraries print different lines for a program → the program
    depends on the library and is changed until they agree
  - All the same → the cases go in, by three ways, in both images
- **Cut-off**: the five programs; each fault put right or listed. Robust mutexes
  (the kernel walks a list the library registers, `set_robust_list`, which r64m
  does not have) are tried apart and expected not to work: listed, not fixed
  here.

**Result (2026-11-27): six things r64m did not do as Linux does, five put right;
the five programs print the real machine's lines with musl and with glibc, by
all three ways. Robust mutexes do not work and are listed.**

Found before the first program was written, by making the image:

- **`apt-get install` failed under r64m.** The Debian image is built on the Mac by
  whatever runs x86-64 there, which is r64m. dpkg could not set the owner of the
  symbolic links it unpacked: `lchown` was not known. Added, with `prctl` (a
  thread's name, the numbers that go to the kernel as they are; a filter of calls
  is refused) and the two `ioctl`s of a pseudo-terminal's first side (`ptsname`,
  `unlockpt`: apt makes one for dpkg). With them `apt-get install gcc libc6-dev`
  ends well under r64m, and the GCC it installs compiles and its program runs:
  the image the glibc cases run in was made by r64m.
- **SIGPIPE was ignored.** `yes | head -1`: on the real machine `yes` is ended by
  the signal (141); under r64m it was told EPIPE, said "Broken pipe" and ended
  with 1. A Rust program's runtime sets that signal to be ignored before `main`;
  r64m now notes what it was given before the runtime starts (a function the C
  library calls first) and gives the guest that.

Found by the programs (the real machine printed the same lines with both
libraries at the first try; r64m differed in two of five):

- **A cancelled thread ended the process.** `pthread_cancel` is a signal of the
  C library's own (33 with musl, 32 with glibc). r64m is built with musl, whose
  `sigaction` refuses those three numbers, so the host's signal got no handler
  and its default ended everything. r64m now asks the kernel itself for handlers
  and masks, and uses none of the three.
- **`sigwait` gave nothing.** `rt_sigtimedwait` was not known. Added: the host's
  kernel holds the signal back as the guest blocked it, and the same call there
  takes it.
- **Robust mutexes** (a lock whose holder ended is given to the next with
  EOWNERDEAD): the real machine, both libraries, as said; r64m does not end. The
  kernel does that for a thread that ends, by a list the library registers
  (`set_robust_list`), and r64m has neither. Listed, not done here.

What passed as it was: a thread's own variables, keys and their destructors,
`pthread_once`, stacks of a size asked for; every wait with a limit of time and
every kind of lock; signals to particular threads, masks, a stack overrun caught
on another stack; and all of `fork`'s: from a second thread, `pthread_atfork`,
a lock across it, 150 programs started beside threads that map memory, a
grandchild, SIGCHLD, stop and continue, a child ended by a signal.

- The cases: `tests/container/threads.txt` (five programs), run in Alpine's image
  and in the new Debian one; two in `shell.txt` (the pipe, a link's owner), one in
  `python.txt` (`prctl`).
- Tests: 123 unit tests; the five programs by the interpreter, the JIT and the
  JIT checked in both images (30 runs, all the real machine's lines); every
  suite by the JIT.
- Not done: robust mutexes; `prctl`'s other requests (a filter of calls
  answers EINVAL, where Linux says EFAULT for the null filter the trial gave it);
  `clone3`, `rseq` and `set_robust_list` answer ENOSYS, which both libraries
  take as an older kernel's answer.

## R066: a signal between r64m's look and the kernel's call (2026-11-27)

Found by R065's case of signals, after it was in: on the CI its first form did
not end (a race of the program's own: a signal before `pause`; rewritten with
semaphores, the real machine six of six), and the second form did not end once in
ten runs with glibc on the Mac. The threads then (`cargo xtask waits`): the first
in a sleep, waiting to see the handler's count; the one thread that lets the
signal in, in `futex` (glibc's `sem_wait`). A signal sent to the process, which
only that thread could take, was never handled.

The card, written before the run.

- **Hypothesis**: the window `sys::call` names itself ("between this look and the
  kernel taking the call a signal can still slip in and wait out the call: a few
  instructions wide, and not closed yet"). The signal arrived after r64m had
  looked for one and before the host's `futex` was entered: r64m's handler noted
  it, and the call then waited for a wake nobody would send. On Linux a signal
  that is there before a call is entered is taken first. The program makes the
  window likely: the thread tells the first it is ready and waits at once, and
  the first sends at once.
- **What is changed from before**: the window is closed for the calls that
  wait: they are entered by a routine of r64m's own that looks at the thread's
  word and makes the call in one stretch of code; the handler, finding the
  thread in that stretch before the call, sends it on to the routine's other
  ending, which says "not made"; the guest's call is then begun again after the
  signal's handler, as if the signal had come first. (musl does the same for a
  cancellation.)
- **Next move for each result**:
  - The program ends 40 of 40 in each image by the JIT, and the suites pass →
    kept
  - It still does not end → the thread's state again (`waits`): another way a
    signal is lost, by the call it waits in
  - Something that passed fails → the calls given the routine are too many (one
    that is not safe to begin again): named and taken out
- **Cut-off**: 40 runs in each image, 60 seconds a run.

**Result (2026-11-27): the hypothesis held as far as it can be shown: with the
window closed the program ends 40 of 40 in each image, where it did not end 1 of
10 with glibc. Kept.**

- The calls that wait (twenty-one of the guest's, each one call of the host's)
  are entered through `call_unless_noted`: the thread's word looked at and the
  call made in one stretch of nine instructions' reach; r64m's handler of
  signals, finding the thread in that stretch with the call not entered, sends
  it to the ending that answers "not made"; `sys::call` then puts the guest
  back on its SYSCALL instruction, the signal's handler is entered, and the call
  is made after it.
- The program by the JIT, 60 seconds a run: 40 of 40 as the real machine in
  Alpine's image, 40 of 40 in Debian's. Before: 9 of 10 with glibc (the one that
  stood had its only signal-taking thread in `futex` and the signal unhandled),
  10 of 10 with musl.
- The one that stood was not looked at closely enough to say the signal arrived
  in exactly those instructions; what is shown is that closing them ends it.
- 123 unit tests; every suite by the JIT.
- Not covered: calls that wait and are more than one call of the host's (a
  write that r64m makes in pieces, `recvmmsg`), which are not begun again
  safely and are left as they were: a signal in their window is still handled
  only when the call comes back.

### The JIT with nothing said (2026-11-27)

The owner: 「デフォルトJITにしてオプションなしでJITにするか」. Not an experiment.
r64m runs a program by its JIT unless told otherwise; `--interpret` asks for the
interpreter alone; a trace (`--trace`, `--trace-xmm`) is the interpreter's, as it
was; `--jit` is still taken and changes nothing; `--jit-check` as before. Started
by the kernel (under Docker) it was the JIT already. The tests name the
interpreter where they mean it. A program that starts another hands its options
on, so `--interpret` holds for a whole tree of processes, and without it they are
all by the JIT.

## R067: the goal beside QEMU again (2026-11-27)

The owner: 「あとは上から順にやるか」; the last of that list. For ADR 0020's point
2, whose fourth measure was under its aim when last raced (R046: a kept
connection 1.84 times QEMU) and has not been raced since R054 made r64m's side of
it 13.5% shorter. Since then also: R055 to R066, none of them for speed, some of
them in every call that waits (R066) and every `fork` (R064).

The card, written before the run.

- **Hypothesis**: a kept connection is now at twice QEMU or past it (1.84 by
  1/0.865 is 2.13, if nothing since has cost it), and the other three measures
  are where they were (the start 2.98, a new connection 2.34), R066's routine
  for the calls that wait costing nothing that shows.
- **What is changed from before** (R046): r64m as it is on `main` today (the JIT
  with nothing said); the same script, images, client and counts (three launches
  of the two side by side; the start timed; 300 requests to warm; then 300 on
  new connections and 1,000 on one kept open; the least of the three for each).
  r64m is now the kernel's runner of x86-64, so its entry is taken out for the
  minutes of the race (QEMU's side is started by the kernel) and put back.
- **Next move for each result**:
  - A kept connection at 2 or past, the others where they were → ADR 0020's
    point 2 holds in all four; said in the ADR and the README
  - Under 2 → said as it is; where a kept request's time goes now is looked at
    by the profile (R054's way) in a card of its own, not here
  - A measure that was past 2 is under it → something since R046 cost it: the
    builds between are raced for that measure alone (R054's, R064's, R066's)
- **Cut-off**: three launches; nothing else running on the Mac meanwhile.

**Result (2026-11-27): a kept connection 1.92 times QEMU: still under 2. The
start 3.05, a new connection 2.42. My 2.13 was wrong.**

Three launches, the two side by side; milliseconds by the client's clock:

| launch | | the start | a new connection | a kept connection |
|---|---|---|---|---|
| 1 | QEMU | 4,473 | 18.09 | 4.59 |
| | r64m | 1,667 | 7.48 | 1.95 |
| 2 | QEMU | 3,419 | 18.21 | 3.68 |
| | r64m | 1,103 | 8.56 | 1.92 |
| 3 | QEMU | 3,245 | 19.27 | 5.16 |
| | r64m | 1,065 | 7.71 | 2.67 |

| the least of three | QEMU | r64m | QEMU / r64m | R046 |
|---|---|---|---|---|
| the start | 3,245 ms | 1,065 ms | 3.05 | 2.98 |
| a new connection | 18.09 ms | 7.48 ms | 2.42 | 2.34 |
| a kept connection | 3.68 ms | 1.92 ms | 1.92 | 1.84 |

- By the card's rule (the least of three for each side) a kept connection is at
  1.92. Launch by launch the two side by side stand at 2.35, 1.92 and 1.93: the
  measure moves by a quarter between launches (QEMU's own from 3.68 to 5.16),
  so the second figure after the point is not to be leaned on; that it is not
  clearly past 2 is.
- r64m's kept request is 1.92 ms where R046 had 1.95: the 13.5% R054 measured
  with r64m alone (1.71 to 1.48 ms, another client and no QEMU beside it) does
  not show here. Either the two servers side by side hide it, or something since
  has taken it back (R066's routine is in every call that waits; R064's stores
  are wider, which should help). Not known; not looked into here.
- ADR 0020's point 2 stands as it stood: three of four past twice QEMU, the kept
  connection under it.
- Owed, as the card says, in a card of its own: where a kept request's 1.9 ms
  goes now (the profile, R054's way), and R054's build beside today's for that
  one measure, to say whether anything was lost.

## R068: r64m beside the host's own code (2026-11-27)

The owner, of what r64m is for (「Armだから重いエミュレータ入れないとダメだ、諦めよう、
作り直そう、サーバ借りようってパターンを弾きたい」), and of how far it is from the
machine it runs on: 「それは測ったほうがいいかもな 最適化の指標になるし」. Every
measure so far is against QEMU, which says who is ahead; none says how much is
left to gain.

The card, written before the run.

- **Hypothesis**: on the primitives r64m takes between two and five times as
  long as the same C compiled for arm64 and run as it is, on the same core; the
  ones that are memory and branches nearer two, the ones that are arithmetic of
  floats and calls through tables further off. (A guess with nothing under it:
  that is why it is measured.)
- **What is changed from before**: a second column. The 23 primitives' sources
  (g64m's `tests/prim64`), with their one header given the arm64 calls for
  writing and ending, compiled for arm64 by the same compiler version that made
  the x86-64 ones (GCC 15.2 both) with the same flags, each with its work done
  many times over; timed in the VM beside r64m running the x86-64 ones, each less
  a program that does nothing. Two are left out: the one made of x86's string
  instructions written by hand, and long double's (80 bits on x86, 128 in
  software on arm64: not the same work).
- **Next move for each result**:
  - The ratios are had → written as a table, per primitive and for the sum: the
    measure to optimise by from now on, beside QEMU's
  - A primitive at ten times or more → named as the first place to look
  - r64m at 1.5 times or under on the sum → the primitives say little more; a
    whole build (NetHack natively, which needs packages fetched for arm64: the
    owner's word) is the measure that would
- **Cut-off**: one round of timing, three runs of each, the least taken; nothing
  else running.

**Result (2026-11-27): on the primitives r64m takes 1.52 times as long as the same
C compiled for arm64 and run as it is. My two to five was wrong, and by a lot.
Two primitives stand out: the conversions of doubles (4.8) and the packed
arithmetic (3.2).**

Both sides: the same sources, GCC 15.2 (Ubuntu's on hal for x86-64, Alpine's for
arm64), the same flags, each program's work done 300 times; in the Mac's Linux VM,
one run timed three times and the least taken, to a hundredth of a second; a
program that does nothing takes under a hundredth by either.

| primitive | native arm64 | r64m | r64m / native |
|---|---|---|---|
| p_dbl_conv | 810 ms | 3900 ms | 4.81 |
| p_simd | 210 ms | 670 ms | 3.19 |
| p_dbl_cmp | 930 ms | 2190 ms | 2.35 |
| p_bytes | 600 ms | 1200 ms | 2.00 |
| p_bitfield | 810 ms | 1550 ms | 1.91 |
| p_index | 560 ms | 980 ms | 1.75 |
| p_funcptr | 2790 ms | 4490 ms | 1.61 |
| p_loop | 330 ms | 510 ms | 1.55 |
| p_recursion | 1080 ms | 1650 ms | 1.53 |
| p_switch | 2920 ms | 4420 ms | 1.51 |
| p_ternary | 740 ms | 1110 ms | 1.50 |
| p_pixel | 480 ms | 690 ms | 1.44 |
| p_float | 2650 ms | 3760 ms | 1.42 |
| p_bignum | 1250 ms | 1690 ms | 1.35 |
| p_dbl_arith | 3040 ms | 4050 ms | 1.33 |
| p_pointer | 1260 ms | 1640 ms | 1.30 |
| p_muldiv | 1030 ms | 1310 ms | 1.27 |
| p_if | 1520 ms | 1840 ms | 1.21 |
| p_sha | 360 ms | 390 ms | 1.08 |
| p_rotate | 4240 ms | 4570 ms | 1.08 |
| p_shift | 1090 ms | 1070 ms | 0.98 |
| **the sum of 21** | 28700 ms | 43680 ms | **1.52** |

- What the number is and is not: the x86-64 program is what GCC makes for
  x86-64 and the arm64 one what it makes for arm64, which are not the same
  quality of code everywhere (one primitive, the shifts, is faster under r64m
  than native: 0.98). It is the ratio a user meets, not r64m's cost alone.
- 19 of the 21 native programs print in their first round what the x86-64 ones
  print; the two of floats do not (the processors differ in what a conversion
  out of range gives, and arm64's compiler may join a multiplication to an
  addition), and do the same amount of work.
- Left out as the card said: the x86 string instructions written by hand, and
  long double.
- Where to look first, by this measure: `p_dbl_conv` (4.81), `p_simd` (3.19),
  `p_dbl_cmp` (2.35), `p_bytes` (2.00), `p_bitfield` (1.91).
- By the card: at 1.5 on the sum the primitives have little more to say. They
  are tight loops in which nearly every instruction is in a block. What a user
  waits for is a build or a server, where the calls, the making of blocks and
  the starting of many processes count: NetHack built natively beside its
  x86-64 build under r64m is that measure, and needs `make` and `ncurses-dev`
  fetched for the arm64 image: the owner's word.
- What I did badly: a first round sized at sixty runs of each program would
  have taken three quarters of an hour (the owner: 「チョロで終わると思ったら時間
  掛かってんな」), and it used x86-64 programs already at hand whose count of
  rounds I had not checked (it was not the 30 I took it for): its numbers were
  thrown away. One run of each, both built anew with the same count, took four
  minutes.

## R069: a whole build beside the host's own (2026-11-27)

The owner, after R068: 「実際の仕事でも測っていいよ」 (the packages for an arm64
image may be fetched). R068's primitives put r64m at 1.52 times native, in loops
where nearly every instruction is in a block; what a user waits for is not that.

The card, written before the run.

- **Hypothesis**: NetHack 3.6.7 built by `make` with one job takes r64m three to
  six times as long as the same build done natively on arm64: the compiler is
  many short-lived processes (each one's blocks made anew), with system calls
  and files between, none of which the primitives have. (R068's guess of mine
  was wrong by a factor of two the other way; this one has as little under it.)
- **What is changed from before** (R030, R031): the second side is not QEMU but
  the host itself: the same Alpine 3.22.8 with the same packages by version
  (GCC 14.2.0, the Dockerfile of `tools/images/gcc` built for arm64), the same
  source prepared the same way, `make all` with one job; three builds of each,
  one after the other, taking turns, nothing else running. The x86-64 side is
  the image of R030, run by r64m as Docker runs it today.
- **Next move for each result**:
  - The ratio is had → written beside R068's as the second measure to optimise
    by; with `--profile` of one compilation to say where r64m's share goes, if
    the ratio is past 3
  - Under 2 → r64m is near enough the host on real work that speed is no longer
    the first thing; compatibility and long running are
  - The native build fails where the x86-64 one does not → the image differs:
    put right, not measured around
- **Cut-off**: three builds of each.

**Result (2026-11-27): the whole build takes r64m 3.85 times as long as the host's
own: 200 seconds where the native build takes 52. One large compilation alone: 3.0
times. The guess of three to six held.**

NetHack 3.6.7, `make all` with one job, Alpine 3.22.8 and GCC 14.2.0 on both
sides, three builds of each taking turns (seconds; the clock in the image counts
whole seconds):

| | 1 | 2 | 3 | the least |
|---|---|---|---|---|
| native arm64 | 54 | 52 | 52 | 52 |
| x86-64 under r64m | 202 | 200 | 202 | 200 |

Each build made 284 files; between two builds of one side three differ, the
three with the date in them.

Beside it, in the built tree (the least of three, hundredths of a second read
from the kernel's clock):

| | native | r64m | r64m / native |
|---|---|---|---|
| `apply.c` compiled alone (125 KB of source) | 1.06 s | 3.21 s | 3.0 |
| `monst.c` compiled alone (tables, little code) | 0.06 s | 0.25 s | 4.2 |
| 200 starts of `/bin/true` | 0.03 s | 0.27 s | 9 |
| 20 starts of `gcc --version` | under 0.01 s | 0.09 s | |

- So there are two measures now, and they say different things: tight loops
  1.52 times native (R068), a compiler at work 3.0, a whole build 3.85.
- Where the difference between 1.5 and 3 is, is not shown here. The compiler is
  a large program that goes through a great deal of its own code a few times
  each (blocks made and little used, calls through tables, the interpreter for
  what is not translated), where a primitive is a few blocks run a billion
  times. That is my reading, not a finding: `--profile` and `--jit-stats` of
  one compilation are the next look, in a card of its own.
- Where the difference between 3.0 and 3.85 is, the table says in part: a
  process that does nothing costs r64m 1.3 ms to start where it costs the host
  0.15, and a small compilation is 4.2 times and not 3.0. The build starts some
  six hundred processes, which is under a second of the 150 that r64m is
  behind; the short compilations and the linking are the rest.
- A first try at timing one compilation used a file that had not been
  preprocessed rightly (2,000 lines, a hundredth of a second) and is thrown
  away; the table's is `make` of one object in the built tree.
- For what r64m is for: a build that takes a minute natively takes under four
  under r64m, and ten under QEMU (R031: 590 s).

## R070: where a compilation's time goes (2026-11-28)

The owner: 「早速やるか」. R068 and R069 left two numbers that do not agree: tight
loops 1.52 times native, GCC compiling one file 3.0 times. The difference is
r64m's to explain before anything is changed for it.

The card, written before the run.

- **Hypothesis**: one of three, and the three ask for different work. (a) Making
  blocks: a compiler goes through a great deal of code a few times each, and the
  translating is a large part of the 3.2 seconds. (b) The interpreter: some
  instruction GCC uses much is not translated, and each one leaves a block and
  comes back. (c) Leaving generated code for r64m's own: jumps and calls
  through registers looked up, blocks not joined, the calls that manage memory.
- **What is changed from before**: nothing in r64m. One compilation (`apply.c`,
  as `make` runs it in the built tree) is run three times more with r64m
  saying of itself: `--jit-stats` (blocks made, time making them, time in
  generated code, instructions in blocks and out), `--profile` (where the
  host's time is, by r64m's own functions), `--jit-why` (what the interpreter
  ran, by instruction).
- **Next move for each result**:
  - Making blocks is a quarter of the time or more → the translator's own speed,
    or keeping translations (per process they are made anew), in a card
  - An instruction or a few are most of what the interpreter runs, and the
    interpreter is a tenth of the time or more → they are translated, in a card
  - Most of the time is in generated code and in finding blocks → the lookups
    and the joining, by the profile's names
  - None of the three stands out → the generated code itself is slower per
    instruction on this program than on the primitives: one hot function of
    GCC's read beside what was made of it
- **Cut-off**: the three runs; no change to r64m in this card.

**Result (2026-11-28): none of the three. Seven tenths of the time is in generated
code, spread evenly over the compiler, and that code is five and a half of the
host's instructions for each of the guest's. The three guesses together are the
other three tenths.**

One compilation: GCC 14.2's `cc1 -O2` on `apply.c` preprocessed (10,831 lines), 3.1
seconds under r64m, 1.06 natively (R069).

r64m's own account (`--jit-stats`, `--jit-memory`):

- 7,071 million instructions of the guest's, 7,049 million of them (99.7%) in
  blocks. 117,432 blocks made, entered 4.9 million times (1,440 instructions for
  each time generated code is entered: the joining works).
- 0.36 s making blocks, 2.27 s in generated code.
- 1.34 million of the guest's instructions translated into 29.4 MB of code: 7.35
  million of the host's instructions, **5.5 for each of the guest's**, and 11.4
  of the guest's to a block.

The profile (3,009 samples, a millisecond each):

| where | samples | share |
|---|---|---|
| generated code | 2,114 | 70% |
| making blocks (reading instructions, translating, writing, joining) | 245 | 8% |
| the guest's memory (`mmap`, `munmap`, `mprotect` and the list) | 210 | 7% |
| the interpreter | 165 | 5.5% |
| finding blocks and entering them (`Jit::run`, `enter`) | 151 | 5% |
| the rest | 124 | 4% |

- In generated code the samples fall in 1,736 blocks: the ten with most have 5%
  of them, the hundred with most 17%, a thousand 65%. There is no hot place: the
  compiler's time is its whole self, and r64m's code for it is evenly about
  twice the host's own (2.27 s of 3.1 against 1.06 for everything natively).
- So the card's fourth line: the generated code itself. The primitives are a
  few blocks whose instructions translate one for one or nearly; a compiler is
  calls, returns, compares and branches, loads through pointers, bytes. At 5.5
  for one the question is which of the guest's instructions cost the most of
  the host's, weighed by how often they run; r64m cannot say that yet (the
  profile knows the guest's address of a sample, not what instruction it is).
- What the interpreter ran (22 million, 0.3%), by `--jit-why`: mostly the first
  four passes over an address before its block is made (conditional jumps and
  moves lead the list). Not translated at all and met often: MOVS of a byte
  alone and with REP (358,000), LOCK OR to memory (158,000), MOVLPS (164,000),
  SHUFPD (161,000), a shift by one of a 64-bit register by the short form
  (57,000), REP STOSQ (39,000). Small beside the rest; each is a block left.
- The guest's memory at 7%: GCC asks for and gives back memory constantly, and
  each call changes r64m's list and the host's mapping.
- Met on the way: a program under r64m cannot start a program that is the host's
  own (an arm64 one). I first wrote this down as a gap. The owner: 「穴ではなく仕様
  では？ 実行できてしまったら一つのことうまくやるが別のこともできちゃわない？」.
  It is as it should be: r64m behaves as x86-64, and an x86-64 machine does not
  run an arm64 program either (ADR 0018). Asked of the real machine: `execve`
  comes back with ENOEXEC, and a shell then takes the file for a script and
  fails. What differs under r64m is only the manner: `execve` does not come
  back; r64m is started again for the file, says "not an x86-64 ELF" and ends
  with 2. A program that looks at what `execve` answered sees the difference
  (it never gets its answer); a shell ends with 2 either way. To be put right
  as its own small change: ENOEXEC from `execve`, while there is still a program
  to tell. (The profile was taken by making r64m the container's first
  program.)

What follows, each a card of its own, in the order I would take them:

1. The profile says what instruction a sample is in (its bytes), and the
   translator says how many of the host's instructions each kind of the guest's
   becomes: the table to optimise by.
2. By that table, the commonest costly kinds (my guess, to be held to the
   table: flags kept for a compare that a jump follows at once, calls and
   returns, operands in memory with an index).
3. Blocks kept between processes (a build starts the same compiler 143 times
   and translates it anew each time: 8% here, more for small files).

## R071: what each kind of instruction costs (2026-11-28)

The owner: 「マージして進める」. R070 left an average: 5.5 of the host's
instructions for each of the guest's, and 70% of a compilation's time in
generated code with no hot place. An average cannot be optimised.

The card, written before the run.

- **Hypothesis**: a few kinds of instruction are most of the cost: by time, the
  conditional jumps and what sets the flags before them, calls and returns, and
  loads and stores with an index; and what stands at a block's beginning and end
  for every block (the look for a signal, the count, the way out) is a tenth or
  more by itself. A guess: the table is to say.
- **What is changed from before**: a tool, no change to what is generated. With
  `--profile` r64m now also says, for the time in generated code, which of the
  guest's instructions each sample was in, by kind (prefixes and opcode, as
  `--jit-why` names them; a block's own beginning apart), and for each kind how
  often it was translated and into how many of the host's instructions on
  average.
- **Next move for each result**:
  - A few kinds hold half the samples or more → those, by what they are made
    into (their code read), in the order of samples times what could be saved
  - The samples are as flat by kind as they are by block → the saving is in what
    every instruction pays (how registers and flags are kept), not in kinds: the
    conventions are looked at
  - A block's beginning is a tenth or more → that first: it is paid 4.9 million
    times in this compilation whatever the block holds
- **Cut-off**: the tool, one run on the compilation of R070, the table.

**Result (2026-11-28): the cost is in going from one block to another, not in
the instructions. Of the time in generated code, 37 in a hundred is at blocks'
beginnings and another 24 in calls, returns and jumps; an ordinary instruction is
one to two of the host's. The 5.5 of R070 is mostly code that is not run: nine
instructions kept beside every way out of every block.**

The compilation of R070 again, with the new table (`--profile`): 2352 samples in
generated code. The kinds with most:

| kind | samples | share | translated | the host's instructions for each |
|---|---|---|---|---|
| a block's own beginning | 877 | 37.3% | 117,431 | 1.6 |
| RET (`c3`) | 228 | 9.7% | 15,497 | 35.8 |
| CALL (`e8`) | 205 | 8.7% | 59,733 | 44.4 |
| MOV r64, r/m64 (`48 8b`) | 126 | 5.4% | 146,667 | 2.1 |
| MOV r/m64, r64 (`48 89`) | 75 | 3.2% | 133,697 | 1.4 |
| MOV r32, r/m32 (`8b`) | 53 | 2.3% | 42,453 | 2.3 |
| CALL through a register or memory (`ff fa`) | 49 | 2.1% | 3,377 | 53.6 |
| CMP r/m8, imm8 (`80 ff`) | 41 | 1.7% | 21,974 | 7.0 |
| MOVZX from a byte (`0f b6`) | 32 | 1.4% | 18,271 | 2.1 |
| JMP short (`eb`) | 30 | 1.3% | 9,046 | 30.7 |
| JMP (`e9`) | 27 | 1.1% | 21,766 | 28.9 |
| JMP through a register or memory (`ff fc`) | 27 | 1.1% | 1,429 | 48.4 |
| MOVZX from a word (`0f b7`) | 27 | 1.1% | 17,002 | 1.7 |
| JE short (`74`) | 25 | 1.1% | 43,908 | 1.8 |
| JNE short (`75`) | 24 | 1.0% | 29,805 | 4.2 |

(A block's last instruction is counted to where the next block begins, so what
stands after it is in its number: that is why RET, CALL and the jumps show 30 to
50.)

Read in the code of the blocks with most samples:

- An ordinary instruction is what it should be: a move 1.4 to 2.1, a test 1.1, a
  conditional jump 1.3 to 1.8.
- A RET is ten instructions on its way: the return address loaded and the stack
  pointer moved, the count, the address made into a place in the table of
  16,384, two loads from it, a compare, and the host's own `ret` to the block
  (R046). A jump through memory is twelve, ending in `br`.
- **Every way out of a block has nine instructions standing after the block**:
  the count added, a branch (to the next block once the two are joined, which
  they nearly all are), and seven more that hand the address to r64m when they
  are not joined, never run again after the joining. A block with eight ways out
  has 63 instructions after its last one. A conditional jump taken goes by two
  of them (the count and the branch) on its way to the next block.
- So of the 7.35 million instructions made, the larger part is these: the code
  that runs is about a third of the 29 MB, with the rest between its pieces.

What the samples at blocks' beginnings are, I do not know and say so: 37% of
the time on what is one or two instructions (the look for a signal, where a
block has one). A sample lands on the instruction the processor was waiting to
run: after a branch it had guessed wrongly, or for code it had to fetch. Both
are costs of arriving at a block; which of the two it is, this machine's VM
gives no counter to say. My reading: with the code that runs spread over three
times the memory it needs, fetching it is a large part. That is a guess until
something is changed and timed.

By the card's lines, two hold: a block's beginning is far more than a tenth, and
a few kinds hold more than half. What follows, each its own card:

1. The seven instructions of a way out that are not run after joining go out
   of the way: one place for them all, or a short form (a call to a shared
   piece with the address after it), so that the code that runs lies together.
   Timed on this compilation and on the whole build; this is the test of the
   guess above.
2. A conditional jump taken to a joined block goes straight there, the count
   kept some other way than by two instructions on the way.
3. Then calls and returns, by what is left of them in the table.

- The tool stays: `--profile` prints the table (`r64m: cost:` lines).
- Unit tests 123; the profile is not run by any case (it prints addresses).

## R072: the ways out that are not run, set aside (2026-11-28)

The owner: 「マージして進める」. R071 found every way out of a block written as a
branch and seven instructions after it that hand the address to r64m; once the
branch is joined to the next block (as nearly all are) the seven are never run
again, and they lie between the pieces of code that are. Of a compilation's time
in generated code 37 in a hundred was at blocks' beginnings, and what it is was
not known: branches guessed wrongly, or code fetched from far apart.

The card, written before the run.

- **Hypothesis**: it is code fetched from far apart. With the seven set aside
  (written after the block's last instruction, past its ways to the
  interpreter, the branch going there until it is joined) the code that runs
  lies together: about a third of the memory it took. Then a compilation is
  faster by a tenth or more, and the samples at blocks' beginnings fall by more
  than that.
- **What is changed from before**: `leave_counted` writes the branch alone and
  notes it; the block's end writes, for each, the seven, with `adr` pointing back
  at the branch, as before. A join is undone by pointing the branch back at its
  own seven (it pointed at the next word), which r64m learns from the branch
  before the join. Nothing else of what is generated changes.
- **Next move for each result**:
  - The compilation is faster by a tenth or more, and the whole build by a good
    part of that → kept; the beginnings were fetching
  - Faster by under a twentieth → it was not fetching: what is left is the
    branches (the look for a signal, the count, a block's entry by the table),
    and the next card is those
  - Slower → the seven far from their branch cost when a block is new (each is
    run once): taken back, said
- **Cut-off**: the unit tests, the 62 programs four ways, the container suites
  three ways; then the compilation of R070 and the build of R069, three each,
  beside the build before.

**Result (2026-11-28): the hypothesis was wrong. With the code that runs lying
together, a compilation is 3 in a hundred faster by the median and the whole
build not faster at all (203 s for 204); the samples at blocks' beginnings did
not fall. It was not fetching. Kept, for doing no harm and some good.**

- One compilation (R070's), the two builds in turn, eight rounds, hundredths of
  a second: before 3.07 at least, 3.14 the median; after 3.02 and 3.04.
- Its profile: samples at blocks' beginnings 670 before, 706 after (of 2,173 and
  2,040 in generated code); time in generated code by r64m's own clock 2.42 s
  and 2.25 s. The code made is the same in size (29.4 MB): its words are in
  another order, nothing fewer.
- The whole build, three of each in turn: 204, 205, 205 s before; 203, 203, 204
  after.
- Tests: 124 unit tests on Linux, the 62 programs four ways (248 runs as the
  interpreter), every container suite by the JIT.
- A first round of the builds measured nothing: they were run under `chroot`
  into the x86-64 image's files from an arm64 shell (so that its clock times
  them), and without `/proc` there r64m could not start another program
  (`/proc/self/exe`): every build ended at its first `cp`. `/proc` mounted, the
  second round is the table above.

By the card, the next is the branches: what a block's beginning holds and what
its arrival costs (the look for a signal, the count, entering by the table and
by `ret`). The 37 in a hundred is still not explained; the profile cannot tell a
branch guessed wrongly from anything else on this VM, so the next card changes
one of them and times it.

## R073: what a block's arrival costs (2026-11-28)

The owner: 「マージして進める」. R072 showed the 37 in a hundred at blocks'
beginnings is not code fetched from far apart. Before changing a branch to see,
r64m was made to say why generated code is left for r64m's own (`--jit-stats`
now says it), since every leaving is an arrival by `enter` at a block's
beginning after a branch through a register, which the host cannot foresee, and
r64m's own work between.

What it says of the compilation of R070: of 4.9 million times generated code was
entered, it was left

- at a way out not yet joined to its block: 2.73 million;
- by an address looked up in the table (a return, a jump or call through a
  register) and not found there: 2.02 million, **1.97 million of them because
  its place held another address**, which this one then put out;
- for the interpreter: 0.14 million; by a fault: none.

The table has 2^14 places (`TABLE_BITS`), chosen by an address's low bits, and
the compiler has 117,000 blocks: returns and calls through registers put each
other out of it two million times.

The card, written before the run.

- **Hypothesis**: the table is too small, and its misses are a large part of the
  time at blocks' beginnings and in r64m's own keeping. With 2^16 places the
  misses that put another out fall to a quarter or less, and the compilation is
  faster by a tenth or more; 2^18 adds less again.
- **What is changed from before**: `TABLE_BITS`, 14 then 16 then 18; nothing else.
  The table is 16 bytes a place, one for each thread: 256 KB now, 1 MB at 2^16,
  4 MB at 2^18 (R051 counted a thread's memory: this is to be said with the
  result).
- **Next move for each result**:
  - Faster by a tenth or more at 2^16 → kept at the size where the gain stops,
    with the memory said; then the ways out that are left unjoined (2.73
    million) in a card of their own
  - The misses fall and the time does not → arriving by `enter` is not what
    costs: the unjoined ways out next, and the beginnings are still not known
  - The misses do not fall → the places are chosen badly (low bits of addresses
    that are aligned alike): another way of choosing, in place of more places
- **Cut-off**: the three sizes, the compilation eight rounds each in turn, the
  whole build three times at the size chosen.

**Result (2026-11-28): the hypothesis held in its direction and fell short in its
size: with 2^16 places the misses that put another out fall to a third and the
compilation is 6 in a hundred faster, not a tenth; the whole build 3 in a
hundred (205 s to 199 s). Kept at 2^16.**

| places | misses | of which another put out | the compilation (median of eight) | a thread's table |
|---|---|---|---|---|
| 2^14 | 2.02 million | 1.97 million | 3.25 s | 256 KB |
| 2^16 | 0.70 million | 0.60 million | 3.06 s | 1 MB |
| 2^18 | 0.29 million | 0.15 million | 3.03 s | 4 MB |

- About 125 ns a miss (1.73 million fewer from 2^14 to 2^18 for 0.22 s). What is
  left at 2^18 is about 1 in a hundred of the compilation, and 0.13 million of it
  is the first arrival at each address, which no size takes away.
- The whole build, three of each in turn: 205, 206, 209 s with 2^14; 199, 198,
  202 s with 2^16.
- Why 2^16 and not 2^18: most of the gain for a quarter of the memory, which
  every thread pays (a JVM has twenty and more).
- The ways out left unjoined (2.73 million, the larger part) are untouched; their
  own card.
- Tests: 123 unit tests, 116 primitives, 39 traces; every container suite by the
  JIT.

The owner, on the result: 「でもVM１台に256とか可変で割り当てられるから、俺らも可変に
したほうがいい気がするけどどうなんだろう？」: a table that grows with the thread's
work, in place of one size for all. Noted for its own card.

## R074: a table of blocks that grows with its thread (2026-11-28)

The owner, on R073's fixed size: 「でもVM１台に256とか可変で割り当てられるから、俺らも
可変にしたほうがいい気がするけどどうなんだろう？」, and then 「固定で入れてから可変に」.
A thread's table is one size for every thread: a compiler with 117,000 blocks
gains from 4 MB of it, and a server's twenty threads that mostly wait pay 1 MB
each for nothing.

The card, written before the run.

- **Hypothesis**: a table that begins at 2^12 places (64 KB) and is made four
  times larger whenever as many addresses have been put out of it as it has
  places, to 2^18 at most, does on a compilation at least as well as R073's
  fixed 2^16 (it reaches 2^18 there, which was 1 in a hundred better), and holds
  a waiting thread at 64 KB.
- **What is changed from before**: the size is the thread's own. The one
  instruction of every lookup that says the size (`ubfx`, the low bits taken)
  is noted where it is written; when the table grows its entries are put in
  their new places and each of those instructions is written again with the
  new size, as a jump is written again when it is joined. A block made after is
  written with the size of the moment.
- **Next move for each result**:
  - A compilation no slower than with 2^16 fixed, a waiting thread's table 64 KB
    → kept
  - Slower → the growing itself costs (a hundred thousand instructions written
    again, each with the processor's caches told): grown earlier, or by more at
    once
  - A case fails → a lookup not noted, still asking with the old size: found by
    the case and noted
- **Cut-off**: the unit tests with a case of growing, the 62 programs four ways,
  the suites by the JIT; the compilation eight rounds beside 2^16 fixed; the
  sizes the threads of the goal's server reach.

**Result (2026-11-28): the hypothesis held. A compilation is 2 in a hundred faster
than with R073's 2^16 for every thread (the table reached 2^18), and a process of
forty threads that wait holds 34 MB where it held 72.**

- The compilation of R070, eight rounds in turn, hundredths of a second: fixed
  2^16 least 2.99, median 3.08; growing least 2.94, median 3.01. It grew three
  times, to 2^18 (4.2 MB with the list of its lookups); misses that put another
  out 0.22 million (2^16 fixed: 0.60).
- Python with forty threads waiting (`VmRSS`, the same both times of two):
  fixed 2^16 72.3 MB; growing 34.0 MB; and for comparison R072's fixed 2^14,
  40.8 MB. A waiting thread's table stays at 64 KB.
- The lookups are noted where they are written (`Built::lookups`), and written
  again when the table grows; a new block is written with the size of the
  moment. A unit test (`the_table_grows_and_what_was_made_before_finds_its_way`):
  sixteen return addresses that share a place grow the table twice; with the
  lookups not written again it fails (91,281 returns not found, where 30,000 is
  the bound).
- Tests: 124 unit tests on macOS and 125 on Linux, 116 primitives, 39 traces, the
  62 programs four ways, every container suite by the JIT.
- What it does not do: a table never shrinks while its blocks stand (it is made
  small again only when generated code is emptied).

The owner, along the way (「r64mは独自性を発揮したいけど、いいとこ取りをしたいよなぁ」):
a miss still leaves generated code for r64m's own (about 125 ns); QEMU, FEX-Emu
and box64 each have a second level that does not. Which to borrow, by this
ledger's numbers, is the next card of this kind; the ways out not yet joined
(2.73 million in the compilation) are the larger part still.

### R075, before its card: where the ways out not yet joined go (2026-11-28)

Counted on the compilation of R070 (`--jit-stats`, `--jit-why`), before any card:
of 2.73 million leavings at a way out not yet joined, 2.48 million arrived where no
block can begin, 0.18 million at a block (joined then), 0.14 million at an address
not made yet. Where no block can begin is nearly always a conditional jump whose
condition the host's flags do not say, with the flags in a record: JE after a
shift's record of 32 bits (0.62 million), JO after a multiplication's of 64 bits
(0.37 million), JNE after a shift's of 64 (0.14 million), and the jumps after a
subtraction's. Each is a way out, the interpreter for one instruction, and a way
in. The card follows; the owner asked meanwhile for a picture of how an
instruction goes, which comes first.

## R075: conditional jumps after a shift and a multiplication, in the block (2026-11-28)

The owner: 「マージして戻る」. What the count before this card found: of 2.73
million leavings of generated code at a way out not yet joined, 2.48 million
arrived at a conditional jump that no block can begin with, because the host's
flags do not say what it asks. The two largest: JE and JNE after a shift (0.76
million: SHR of 32 bits, then 64), JO after an unsigned multiplication of 64
bits (0.37 million). A shift is translated with arm64's shifts, which set no
flags, and a multiplication with `umulh` and `mul`; both leave a record and say
the host's flags are nothing. So the block ends before the jump, the jump is the
interpreter's at every pass, and the way back into generated code is by Rust.

The card, written before the run.

- **Hypothesis**: the host's flags can say these two at the cost of one
  instruction. After a shift by a constant that is not zero, of 32 or 64 bits,
  x86's ZF and SF are the result's: an `ands` of the result with itself gives the
  host's Z and N, which answer JE, JNE, JS and JNS. After MUL (unsigned) CF and
  OF are both "the upper half is not zero": a `subs` of the D register less
  nothing gives the host's Z, which answers JO, JNO, JB and JAE. With those the
  jumps are in their blocks; the leavings at a conditional jump fall by 1.1
  million or more, and the compilation is faster by a tenth (2.48 million of
  them cost, by R073's 125 ns, about 0.3 s of 3).
- **What is changed from before**: two kinds of host flags (`ZeroSign`,
  `HighHalf`) the translator knows, the instruction after the shift and the
  MUL, and what a jump asks of each. Where a record was written for a jump that
  is now answered by the host, it is not written (the scan of what is dead sees
  the host answers).
- **Next move for each result**:
  - The leavings fall as said and the compilation by a twentieth or more →
    kept; what is left of 2.48 million is counted again for the next kinds
    (the jumps after a subtraction that begin blocks)
  - They fall and the time does not → the interpreter's single step was cheap
    and the arrivals were not: said, and kept for the leavings
  - A case fails → the condition is wrong for some width or count: found by
    the case, put right
- **Cut-off**: unit tests of each jump after each instruction against the
  interpreter; the 62 programs four ways; the suites by the JIT; the
  compilation eight rounds; the build three times.

**Result (2026-11-28): the leavings fell as said and the time hardly did: a
compilation 2 in a hundred faster, the build 1 to 2 (190 s to 187). Kept, for the
leavings. The hypothesis of a tenth was wrong: the interpreter's single step and
the way back in were cheap.**

| | before (R074) | after |
|---|---|---|
| generated code entered (the compilation) | 4.90 million | 2.08 million |
| leavings that arrived where no block begins | 2.48 million | 1.33 million |
| the compilation, median of eight | 2.90 s | 2.83 s |
| the whole build, three of each in turn | 190, 190, 191 s | 186, 188, 188 s |

- 2.8 million fewer times in and out for 0.07 s: about 25 ns each, a fifth of what
  a miss in the table cost (R073: there r64m looks the address up in its map and
  fills the table; here the interpreter runs one jump).
- What is left where no block begins: MOVSB (`a4`) after a subtraction's record
  and after a logic's (0.27 million), JNE, JA and JL after a subtraction's record
  of 32 bits (0.24 million), SHUFPD (0.13 million), LOCK OR (0.08), MOVLPS (0.06),
  a shift by one of 64 bits with SAR's short form (0.06), CMOVE after a BSF's
  record (0.05). Each is small; together 1.3 million at 25 ns is under a twentieth
  of a second. Not worth a card each by this ledger.
- So the 37 in a hundred at blocks' beginnings is still not explained, and it is
  not the arrivals by r64m. What is left to look at is in generated code itself:
  arrivals by joined branches and by the table, which the processor cannot be
  asked about on this VM. The next look needs a machine with its counters (hal is
  x86-64; an arm64 Linux machine with `perf` is needed).
- Tests: 125 unit tests on macOS and 126 on Linux, among them a test of every jump
  on zero, sign, overflow and carry after each shift and MUL against the
  interpreter; 116 primitives, 39 traces; the 62 programs four ways; every suite
  by the JIT; GCC's, glibc's and Python's suites three ways.

## R076: a measure of what runs, by kind of instruction (2026-11-28)

The owner: 「各々の命令の統計をとってどこが削減できるか調べてみる？」, and 「物差しを
作ろう」. R071's table says where the samples fall and how many of the host's
instructions each kind is made into; not how often each ran. Without it the cost
of a kind cannot be weighed (a kind made into thirty instructions and run once is
nothing), and the length of what runs between blocks is not known.

The card, written before the run.

- **Hypothesis**: the measure can be had exactly enough to trust, without the
  processor's counters: a counter at every block's beginning and at every way out
  of a block before its last instruction (a jump taken past which the block went
  on, a way to the interpreter) gives how often each instruction of each block
  ran. Its total is checked against what r64m counts already (the instructions
  run in blocks): it must be the same to within a hundredth.
- **What is changed from before**: `--jit-count`, for measuring only: with it the
  translator writes the counters (five or six instructions each); at the end r64m
  says, by kind of instruction, how often it ran, into how many of the host's
  instructions it is made, their product, and its share; and how many of the
  guest's instructions run between two arrivals at a block's beginning. Without
  it nothing that is generated changes.
- **Next move for each result**:
  - The total is the count's to a hundredth → the table is the measure, taken on
    the compilation, and written down as the order of what to cut
  - It is not → a way out not counted (a fault, a repeat): found and counted,
    before the table is believed
  - What runs between arrivals is short (under ten instructions) → what every
    arrival costs is the next card, whatever the kinds say
- **Cut-off**: the tool, its check on the compilation, the table.

**Result (2026-11-28): the measure holds (its total is r64m's own count less 0.12
in a hundred), and it says three things R071 could not.**

GCC's `cc1 -O2` on `apply.c` with `--jit-count`:

- 7,042 million of the guest's instructions counted by the counters, against 7,050
  million by r64m's own count (the difference is what ran in blocks left by a way
  not counted: a fault, a repeat; it is 0.12 in a hundred).
- **2.49 of the host's instructions for each of the guest's that ran.** R070's 5.5
  was of what was made, not of what ran: most of what is made is ways out that are
  not run.
- **Blocks were begun 842 million times: 8.4 instructions each.** Under the card's
  ten: what every arrival at a block costs is the next thing to look at, whatever
  the kinds say.

By kind (share of the host's instructions that ran; how often; how many each):

| kind | share | ran (millions) | host each |
|---|---|---|---|
| RET (`c3`) | 10.8% | 135 | 14.0 |
| MOV r64, r/m64 (`48 8b`) | 6.7% | 558 | 2.1 |
| CALL (`e8`) | 6.4% | 125 | 9.0 |
| MOV r/m64, r64 (`48 89`) | 4.4% | 606 | 1.3 |
| CMP r/m8, imm8 (`80 ff`) | 3.4% | 97 | 6.1 |
| MOV r32, r/m32 (`8b`) | 2.6% | 220 | 2.1 |
| PUSH rbp, POP rbp (`55`, `5d`) | 5.2% | 300 | 3.0 |
| MOV r/m32, r32 (`89`) | 2.5% | 354 | 1.2 |
| CMP rax, imm8 (`48 83 f8`) | 2.2% | 130 | 3.0 |
| JE short (`74`) | 2.0% | 280 | 1.3 |
| CMP AL, imm8 (`3c`) | 1.8% | 66 | 4.9 |
| TEST r/m8, imm8 (`f6 f8`) | 1.6% | 44 | 6.4 |
| JMP through memory or a register (`ff fc`) | 1.5% | 16 | 16.9 |
| CALL through memory or a register (`ff fa`) | 1.2% | 10 | 21.8 |
| SUB rax, FS:[...] (the stack protector's check, `64 48 2b`) | 1.2% | 27 | 8.0 |

(The full list is what `--jit-count` prints. The count for a block's last
instruction runs to the end of the block's own code, so a RET's 14 holds the four
instructions of its way to r64m on a miss, which are not run when the table has
the address: what runs is ten.)

Grouped: calls and returns 20 in a hundred of what the host ran (285 million of
them, 9 to 22 instructions each); PUSH and POP 13 (755 million, 3 each, where POP
of a register other than RSP needs 2); comparisons and tests of a byte or a word
6 (5 to 6 each, where a register of 32 or 64 bits needs 1 to 3); the conditional
jumps 6.5 at 1.1 to 1.4, which is what they should be.

The order to cut, by this measure:

1. What every arrival at a block costs (842 million arrivals): the count added and
   the branch at every way out, the look at a block that a cycle goes through.
   Longer blocks (going on past a jump to a block already made) would arrive less
   often; or the count kept another way.
2. Calls and returns: a RET of ten instructions and a CALL of nine.
3. PUSH and POP: a POP of a register in two.
4. Comparisons of a byte and a word.

Each its own card; the measure stays (`--jit-count`, for measuring only: without it
nothing generated changes).

## R077: what an arrival at a block costs, by making more of them (2026-11-28)

The owner: 「マージして進める」. R076 found blocks begun every 8.4 instructions (842
million times in one compilation), and R071 a third of the samples in generated
code at blocks' beginnings, which nothing so far has explained. Before changing how
blocks are made to be longer, what an arrival costs is to be known: the processor's
counters cannot be had on this VM, so the condition is made instead.

The card, written before the run.

- **Hypothesis**: an arrival costs about a nanosecond (two or three instructions and
  a branch taken, which the processor's front end pays for), so 842 million of them
  are a quarter of the compilation and longer blocks would gain much. The other
  possibility: it costs a few tenths of a nanosecond, and the samples at blocks'
  beginnings are where the processor waited for something else (a load, a branch
  guessed wrongly before) and are not the arrival's.
- **What is changed from before**: nothing to keep. Builds with the most
  instructions in a block (`MOST`, 64) at 2 and at 4, which make many more
  arrivals and nothing else different; for each, the arrivals by `--jit-count` and
  the compilation's time (eight rounds, in turn with the build as it is). The
  slope, more time over more arrivals, is what one costs.
- **Next move for each result**:
  - A nanosecond or more → fewer arrivals is the next card: blocks that go on
    through a jump to a constant (72 million JMP of 32 bits ended blocks) and into
    a call's callee
  - A few tenths or less → arrivals are not where the time is: the samples at
    beginnings are someone else's wait; the next card is calls and returns
    (R076: a fifth of what the host runs), by their instructions
- **Cut-off**: the two builds, eight rounds each beside the build as it is.

**Result (2026-11-28): an arrival at a block costs about a nanosecond. The 842
million in a compilation are about 0.8 s of its 2.7: the third of the samples at
blocks' beginnings is the arrivals, and fewer of them is the next card.**

- The card's way of making more arrivals, a smaller `MOST`, made something else
  too: a block cut where the flags are still to be read leaves its conditional
  jump to begin the next block, which no block can do; with `MOST` at 4 the
  compilation left generated code 247 million times (2 million as it is) and took
  18 s. That measured R075's kind of leaving, not an arrival.
- So the condition was made otherwise, for this measurement only and not kept: a
  build that cuts a block after N instructions only where the flags are dead
  (`R64M_SPLIT`), so that nothing but the arrivals changes (the times generated
  code was left: 2.08, 2.33, 2.68 million; the output the same).

| cut every | blocks begun | the compilation (median of six) |
|---|---|---|
| (as it is) | 842 million (8.4 each) | 2.74 s |
| 4 | 1,742 million (4.0 each) | 3.51 s |
| 2 | 2,857 million (2.5 each) | 4.93 s |

- The slope: 0.77 s for 900 million more, 0.86 ns each; 2.19 s for 2,015 million
  more, 1.08 ns each. About three of the processor's cycles for an arrival.
- The arrivals made here are the simplest kind (a branch to the block that
  follows in the guest, joined); those of returns and jumps through registers go
  by the table and cost more. So 0.8 s for the 842 million is the least of it.
- What an arrival is made of: the count added at the way out, a branch taken (to
  another place in the code), and for a block a cycle passes through the look.
  Of those the branch taken is the processor's front end starting again
  somewhere else; which part is the most, this cannot say.

What follows, as its own card: fewer arrivals. Blocks that go on through a jump to
a constant (JMP: 72 million arrivals of 842 by R076's count) and through a call
into its callee (CALL: 125 million), where the guest's code allows it.

## R078: blocks that go on through a jump to a constant (2026-11-28)

The owner: 「マージして進める」. R077: an arrival at a block costs about a nanosecond,
and a compilation has 842 million. A JMP to a constant ends a block today: the way
out is joined to the block at the target, so it is a branch and an arrival. R076
counted 72 million JMP of 32 bits run in the compilation (and some of 8).

The card, written before the run.

- **Hypothesis**: a block that goes on at the target of a JMP, as if the JMP were
  not there, arrives at blocks 70 million times less and is faster by about 0.07
  s of 2.7 (a nanosecond each), without anything else changing. Where the target
  is far, or is the block's own beginning or one of its instructions, the block
  ends at the JMP as now.
- **What is changed from before**: a JMP whose target is within the pages a block
  may rest on (8 KB before its start to 16 KB after, as for what is read to find
  flags dead) and not already in the block is translated as nothing, and the
  block goes on there. A block's instructions are then not in one run of
  addresses: what it rests on (for stores that forget it) is from the lowest to
  the highest of them, and an instruction's address is kept as a distance that
  may be before the block's start.
- **Next move for each result**:
  - Arrivals fall by about as many JMP as ran, and the compilation by a
    nanosecond each or more → kept; CALL into its callee is the next card
  - They fall and the time does not → following a jump costs elsewhere (blocks
    made longer and more of them made twice): said, and taken back if slower
  - A case fails → an address taken for one in a single run: found, put right
- **Cut-off**: unit tests (a block through a JMP forward and back, against the
  interpreter; a store into the code past the JMP forgets the block), the 62
  programs four ways, the suites by the JIT and three ways for GCC's, glibc's and
  Python's; the compilation eight rounds, the build three times.

**Result (2026-11-28): arrivals fell as said and the compilation was slower:
taken back. What following a jump costs is making more blocks: a guest
instruction is translated for about 270 ns, and following jumps translated a
quarter more of them.**

| | before | going on through JMP |
|---|---|---|
| blocks begun | 842 million (8.4 each) | 741 million (9.5 each) |
| the host's instructions run | 17.53 billion | 17.33 billion |
| guest instructions translated | 1.34 million | 1.66 million |
| code made | 37.9 MB | 43.3 MB |
| the compilation, median of eight | 2.88 s | 2.96 s |

- 101 million fewer arrivals (about 0.1 s by R077) and 1 in a hundred fewer
  instructions run; and 0.32 million more instructions translated, the same code
  made again where a jump's target is also a block of its own. Making blocks took
  0.36 s for 1.34 million instructions in R070 (about 270 ns each): 0.32 million
  more is about 0.09 s. The two nearly cancel; the rest of the 0.08 s lost may be
  the larger code (14 in a hundred more) in the processor's caches. Not
  separated.
- The tests the change needed (a block across a jump forward and back, and two
  older tests that used `jmp +0` to cut a block) passed; with the change taken
  back they are not kept.

What this says beyond itself: making blocks is not a small cost. A compiler is a
short-lived process; the build of NetHack starts `cc1` 143 times, and each makes
its blocks anew. If each spends a few tenths of a second making them, that is a
quarter of the build. How much it is, summed over a whole build, is the next
thing to count; keeping translations between processes is what it would be for.
