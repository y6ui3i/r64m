# r64m

An x86-64 emulator with a JIT for arm64 hosts, in Rust. It runs the programs of x86-64
container images as processes on an arm64 Linux kernel: under Docker it takes the place
QEMU's user mode has, with nothing in the `docker` command that names it
([Running x86-64 images with Docker](#running-x86-64-images-with-docker)). It is a
rebuild of [g64m](https://github.com/y6ui3i/g64m), which is written in Goose: not a
port of its code; what g64m found out is used, and the design begins again
(`docs/adr/`).

**Where it stands (2026-11-27).** Official images run unchanged by `docker run` alone:
Alpine's busybox and GCC, Debian's GCC with glibc (`apt-get install` runs), Python 3.13, redis, nginx, memcached, Node.js, a JVM
(Temurin 21), Go, PostgreSQL, and nginx over TLS in front of FastAPI. About a hundred
cases from those eleven images are held to what a real x86-64 machine printed for the same
commands, by the interpreter, by the JIT, and by the JIT with every block checked
against the interpreter ([docs/ci.md](docs/ci.md)).

Beside QEMU 9.2.2's user mode in the same Docker VM, on a MacBook Air (Apple M1), by
the clock of whoever waits:

| work | r64m against QEMU |
|---|---|
| NetHack 3.6.7 built by `make` with GCC, one job (590 s under QEMU, 203 s) | 2.9 times as fast |
| nginx over TLS in front of FastAPI: the container's start to its first answer | 3.0 |
| the same: a request on a new connection | 2.4 |
| the same: a request on a connection kept open | 1.9 |

The aim is twice QEMU; the kept connection is under it (raced again on 2026-11-27,
R067: 1.92, in a measure that moves by a quarter from one launch to the next). Rosetta, where there is one, is faster
than both: it builds NetHack in 168 s. r64m is for where there is none.

**How it is made, in short.** An interpreter for the integer instructions, SSE and SSE2
and the x87 (`src/cpu.rs`, `src/cpu/sse.rs`, `src/cpu/x87.rs`), and a JIT
([0014](docs/adr/0014-the-jit.md), `src/jit/`) that translates blocks of the integer
instructions, the scalar SSE instructions and the packed instructions of whole numbers
([0015](docs/adr/0015-sse-in-blocks.md)), joins blocks to one another, and can check
each against the interpreter (`--jit-check`). The JIT is what runs a program unless
the interpreter alone is asked for (`--interpret`). The x87 is interpreted, by soft
arithmetic of 80 bits: the one place r64m is behind QEMU. A program's memory is the
host's, at an offset ([0008](docs/adr/0008-a-programs-memory-is-the-hosts.md)); its
system calls are the host kernel's, translated
([0017](docs/adr/0017-the-containers-way.md)); its threads are host threads, with
memory ordered between them as x86 orders it
([0019](docs/adr/0019-threads-and-signals.md)): by the processor itself where it can
(an Apple processor, when the kernel lets a thread ask), by ordered loads and stores
elsewhere, which costs a program with threads half again and more. There is no
machine: no kernel is booted, no device is emulated.

Every result is held to the real CPU before it is timed, and every experiment has a
card in [docs/experiments.md](docs/experiments.md) before it runs, with its result
after, whether it worked or not.

## The principle, the aim, the goal

**The first principle: r64m does one thing well.** On Arm, it behaves as x86-64
([0018](docs/adr/0018-one-thing-well.md)). **The aim: twice QEMU's speed**, on the
primitives and on real work in a container, with every result held to the real CPU
first. **The goal** is where both are shown:

In an x86-64 container on an arm64 host, nginx takes a request over TLS and passes it to
a Python process serving FastAPI, and `Hello, world` comes back
([0011](docs/adr/0011-the-goal.md)).

## What r64m is not

- **Not a machine.** r64m does one thing ([0018](docs/adr/0018-one-thing-well.md)): on
  Arm, it behaves as x86-64. It boots no kernel and has no disk; a program's files,
  processes and network are the host kernel's. A machine may be made from it by a
  fork; it is not grown here.
- **Not a boundary of security.** A program's memory is the host's
  ([0008](docs/adr/0008-a-programs-memory-is-the-hosts.md)): the guest and r64m are one
  process with one set of rights, and an address that no x86 program can form validly
  (bit 47 or above set) is not caught and may reach r64m's own memory. QEMU's user mode
  and FEX-Emu are the same. What a container may not do is for the host's kernel to
  enforce.
- **Not exact on a host with 16 KB pages**, where generated code is concerned. The host
  r64m is made for is arm64 Linux with 4 KB pages. macOS on Apple silicon is where it
  is developed; there the host protects 16 KB at a time, so a closed page beside an
  open one can be read by generated code, and a guard page does not guard.
  There the guest also has 2^40 bytes of address space and not 2^47
  ([0013](docs/adr/0013-macos-reserves-less.md)).
- **64-bit only**: no 16-bit or 32-bit code, no BIOS.

## Why this exists

g64m asked how far a well-worn technique (an emulator with a JIT) can get when it is
written in a language made by AI and built by AI, with QEMU's own emulation (TCG) as the
yardstick. On 2026-11-24 it runs NetHack's build in a Linux guest at twice QEMU's speed
on a MacBook Air (Apple M1), and boots that guest at twice QEMU's speed.

r64m is to do the same things in Rust and then the goal above, and both are to be tried on a Raspberry Pi
(arm64 Linux, where there is no Rosetta and QEMU's user mode is the only rival).

**Why Rust**: the work is a JIT and the memory of a guest, where speed and low-level
control matter, and Rust is a well-worn language that the tools building this know
exactly. (g64m's own record of what cost it time is the other reason: globals reached by
their offset in one block, arrays that could not be mapped, a C file for every call the
language could not make.)

## What carries over from g64m

Not its code. What does not depend on the language:

- the tests and what the real CPU answered for each (g64m's `tests/`, with expected
  output taken under KVM and by ptrace on an x86 host);
- the design the experiments arrived at (blocks, the table of blocks, the cells, the
  flags' record), which is in g64m's `docs/jit64.md`;
- the ledger of what was tried and what did not work (`docs/experiments.md` there), so
  that it is not tried again here.

g64m is also the oracle for speed and for behaviour where the real CPU cannot be asked:
r64m is right when it prints what g64m prints, and g64m is right when it prints what the
real CPU does.

## What comes first

1. Done (R001): g64m built and its tests passed on arm64 Linux as it is. Two hosts need
   no second design.
2. Done (R002, ADR 0008, `src/mem.rs`): a program's memory is a reservation of the
   host's address space, protected by the host.
3. Done: a static program loaded and run by the interpreter on that memory, its trace
   the real CPU's, for the integer instructions, SSE and the x87.
4. Done (R004 to R007): the interpreter timed, and made faster than g64m's on every
   primitive without giving up the simple form.
5. The JIT, in ADR 0014's order. Done: the encoder; runnable memory and the way in and
   out; the translator for integer instructions with `--jit-check`; a fault in a
   block; a store to translated code, and to data beside it; blocks joined; returns
   and jumps through an operand found in a table. The record of the flags is written only where
   something can read it (R013 to R015); the integer instructions the primitives use
   (R016, R017); a block goes on past a conditional jump (R018); the scalar SSE
   instructions (R020) and the packed integer ones (R021); stores to data beside
   code on 16 KB pages go straight to memory (R023).
6. The container's way ([0017](docs/adr/0017-the-containers-way.md)): x86-64
   programs of a container image as processes on the host's own Linux kernel. Done
   (R025, R026): a program that is dynamically linked and its interpreter loaded;
   memory by `brk` and `mmap`; files, time and identity handed to the host's kernel;
   `fork`, `execve` (r64m started again) and `wait4`; busybox's shell with pipes,
   loops and scripts, and GCC compiling and linking (R027: what it makes is byte for
   byte the real machine's), held to what the real machine prints
   (`tests/container.rs`). NetHack 3.6.7 is built there by `make` under r64m, every
   file as the real machine makes it, 2.2 times as fast as user-mode QEMU 9.2.2
   (R030: 266 s for 590 s, three builds each; Rosetta 168 s), and 2.9 times since
   R031 (203 s). `--profile` shows where r64m's own time goes. Python 3.13 runs,
   and its threads (R033): a guest thread is a host thread
   ([0019](docs/adr/0019-threads-and-signals.md)). Memory is ordered
   between threads as x86 orders it (R034), by the processor itself where it can (R035). Sockets and `epoll`:
   Python's HTTP server, `asyncio`, and redis-server answer as on the real machine
   (R036). Signals are delivered to the guest's handlers, its own faults among
   them (R037; the cases a shell cannot make are a Rust program built for x86-64 Linux,
   `guest/`, R038); blocks that a cycle passes through look for one as they begin, in
   one instruction (R039), which costs the tightest loops 1 to 4%. TLS 1.3 between a
   Python server and client, with keys `openssl` made, completes (R040). **The goal's request completes** (R041): nginx takes
   it over TLS and passes it to FastAPI, `Hello, world` comes back, to a client in the
   container and to `curl` outside it. Beside QEMU (R042 to R046): the start 3.0 times
   as fast, a request on a new connection 2.3, on a kept one 1.8 by the client's
   clock (R046: a return is foreseen by the host's own stack of returns). Both are as the real machine has them.
   What done means is [0020](docs/adr/0020-what-done-means.md). Six other images
   run as the real machine has them (R047, R048): nginx, memcached, Node, a JVM,
   Go, PostgreSQL. Registered with the host's Docker in QEMU's place, the goal's
   request completes in the ordinary images unchanged by `docker run` alone (R049,
   R050). A thread that ends gives back what it held (R051: until then a server
   whose threads come and go grew without end), and a new thread knows its own id
   before it runs (R052: Node's workers stopped one run in ten). A file's pages
   are mapped, not copied (R053: an idle Node holds 64 MB where it held 260; the
   real machine 49). A kept request is 13.5% shorter (R054: compare-and-swap,
   swap and short repeated moves in blocks); the race with QEMU for it is owed.
   The JVM's hang at its end under load was a `futex` wake r64m refused (R056:
   fixed), and its SIGSEGV one run in a hundred a store refused while another
   thread had the page closed (R057: fixed): 96 of 96 runs of eight at once
   end well.
   Every call is held to the real machine with the registers it does not use
   filled (R059: two more calls looked at what the kernel ignores, and the last
   address of all brought r64m itself down).
   A child of `fork` no longer waits for a lock that a thread of its parent
   held, and a store the interpreter makes is whole to other threads (R064: Go's
   `go build` stood, or its compiler was stopped, a few times in a thousand).
   Threads and `fork` are held to the real machine by the book, with musl and
   with glibc (R065: cancelling a thread, `sigwait`, SIGPIPE and `apt-get
   install` were not as Linux has them; robust mutexes still are not).
   The tests are run by a CI on every push
   ([docs/ci.md](docs/ci.md)): `cargo xtask ci` is the same by hand.

One case is owed: the x87's square root when what is left over equals the root (the
rounding there is not touched by any of g64m's cases; found by breaking it on purpose).
It needs a trace from the real CPU, and so the oracle in Rust (ADR 0012).

## Rules of this repository

Everything kept here is written in English: source comments, documentation, the ledger,
commit messages, branch names, pull requests. An experiment gets a card in
`docs/experiments.md` before it runs, and its result in the same place, kept or not.

## Building and testing

```sh
cargo build --release
cargo test
target/release/r64m program               # run an x86-64 program, by the JIT
target/release/r64m --interpret program   # by the interpreter alone
target/release/r64m --jit-check program   # by the JIT, every block checked against the interpreter
target/release/r64m --trace program       # the registers after every instruction (the interpreter)
```

With nothing said r64m runs a program by its JIT (since 2026-11-27; `--jit`, which
asked for it until then, is still taken). Programs that need files of their own, a
dynamic linker among them, are run where those files are: in the image's container,
as the next section has it.

Everything here is Rust; nothing is added in another language (0012). The toolchain's
version is fixed in `rust-toolchain.toml`, and the versions of the two crates from
outside in `Cargo.lock` ([0009](docs/adr/0009-two-crates.md)).

For arm64 Linux, from the Mac, a static executable (the target is fetched once with
`rustup target add aarch64-unknown-linux-musl`):

```sh
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    cargo build --release --target aarch64-unknown-linux-musl
```

## Running x86-64 images with Docker

On an arm64 machine Docker runs an x86-64 image by whatever the kernel of its
containers has registered for x86-64 programs (`binfmt_misc`): QEMU, as a rule.
r64m is put in that place by one command, and taken out by another. Needed: Docker
(tried with OrbStack on a Mac; the kernel is the VM's), Rust with the target
`aarch64-unknown-linux-musl`, and the right to start a container `--privileged`,
which is how the kernel's list is written.

```sh
cargo xtask install      # builds r64m for arm64 Linux and registers it
cargo xtask status       # what the kernel says of r64m's entry, and the other entries
docker run --rm --platform linux/amd64 alpine cat /proc/self/cmdline | tr '\0' ' '
                         # under r64m: "/r64m-binfmt /bin/cat cat ..."; under QEMU: "cat ..."
```

From then on every x86-64 container of that kernel runs under r64m, with nothing
in the `docker` command that names it: `docker run --platform linux/amd64 IMAGE`.

To put things back as they were:

```sh
cargo xtask uninstall    # removes r64m's entry; what was registered before runs again
```

What to know:

- QEMU's entry (or Rosetta's) is not touched. The kernel asks the entry made last
  first, so r64m's is asked while it is there, and the other answers again when it
  is gone. Containers already running go on as they were started.
- The entry does not outlive the kernel: after the VM (OrbStack, Docker Desktop) or
  the machine is started again, x86-64 runs as before the install, and
  `cargo xtask install` is run again.
- The kernel holds r64m's file as it was when registered. After a new build,
  `cargo xtask install` again.
- `install` makes a small image, `r64m-install`, of two files and nothing else
  (r64m and the tool that writes the entry); `docker rmi r64m-install` removes it,
  after `uninstall`.
- What is different from QEMU's entry: a program whose file is set-user-ID is not
  given that user, and `ps` in a container shows r64m's own command line before
  the program's.

## Tests

`cargo test` holds r64m's traces to the real CPU's. The cases and the traces are g64m's
and are read from its checkout, beside this one or where `R64M_G64M` says; `nasm` builds
the cases. Without the checkout those tests are shown as ignored.
