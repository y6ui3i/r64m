# 0017: The container's way: a program's system calls are the host kernel's

**Status**: accepted (2026-11-25, the owner: the three points, A, B and C, as proposed)

## Context

The goal (0011) is a request through nginx to Python in an x86-64 container on an
arm64 host, and the owner wants a race with QEMU on a build (NetHack, as g64m had).
The owner chose the container's way for both (2026-11-25): no guest kernel and no
machine. A container is processes on the host's kernel; an x86-64 one differs only in
that something must run its programs' instructions. That is r64m's place, as it is
user-mode QEMU's and Rosetta's.

r64m runs one statically linked program and knows three system calls. R024 counted
what is asked for: eight different calls for a dynamically linked hello in Alpine, 33
for a compile and link by GCC, all of kinds the arm64 kernel has or has a near form
of. And r64m built for arm64 Linux already runs inside the x86-64 container.

## Decision

1. **A: a system call of the guest's is handed to the host's kernel**, with its
   number translated, its pointers made the host's (`base + g`, 0008), and the few
   structures laid out differently on the two (`stat` first) written over. r64m keeps
   no files, no descriptors, no processes of its own: the guest's descriptor 5 is the
   host's 5, its working directory and its root are r64m's own, which in a container
   are the container's.
   - The exceptions are what is r64m's by 0008: memory (`brk`, `mmap`, `munmap`,
     `mprotect`, `mremap` are given from the reservation and written into the list
     of regions; a file is mapped by the host at the place r64m chose; 0016's point 2
     is built here), `arch_prctl`, `exit`, and, when they come, signals and threads.
   - A call r64m does not know is answered ENOSYS and named once on standard error.
     Nothing is guessed.
   - **This is for a Linux host.** On macOS r64m keeps what it has (`write`, `exit`):
     the primitives and the unit tests run there as now; the container's work is
     built and tested in the Linux container.

2. **B: a new process is the host's new process.** `fork`, `vfork` and `clone` without
   a shared memory are the host's `fork`: r64m is copied whole, its blocks with it.
   (`vfork` and musl's `posix_spawn`, which share memory until `execve`, are a `fork`
   whose parent waits: user-mode QEMU does the same.) `execve` is the host's `execve`
   of r64m itself with the new program: r64m reads a `#!` line itself. So r64m needs
   nothing registered with the kernel: `r64m /bin/sh` in a container is enough, and a
   race needs no setting of the Docker VM changed.
   - Registering r64m as the kernel's handler of x86-64 programs (`binfmt_misc`),
     which is how Docker would use it unasked, changes a setting of the whole VM. It
     waits for the owner's word, at the end.

3. **The loader takes a program that is not at a fixed address** (`ET_DYN`) **and its
   interpreter** (`PT_INTERP`), places both, and starts at the interpreter with the
   auxiliary vector saying where the program is. No vDSO is given: the C library
   then makes the system calls itself.

4. **C: the order, each a thing that runs in the x86-64 Alpine container and is held
   to what the real machine prints for the same image** (hal: the oracle, as for the
   traces):
   1. hello, dynamically linked (R024's eight calls);
   2. the shell running programs joined by a pipe (`fork`, `execve`, `wait4`, files);
   3. GCC compiling and linking one file (R024's 33);
   4. **the race: NetHack built by `make` in the container, r64m against user-mode
      QEMU**, with Rosetta beside. Here the cost of a process that lives a tenth of a
      second (translation, starting) is first counted, and what to do about it is a
      decision of its own;
   5. threads, signals delivered, sockets: Python with FastAPI, nginx, TLS (0011).
   Each is a card in the ledger. What a step needs that is not in this decision
   (threads share blocks: how?) gets its own.

5. **The tests are Rust, and are data** (0006, 0012): a list of commands and what the
   real machine printed for each, run by `cargo test` on Linux against an x86-64
   root named in the environment; shown as ignored anywhere else, as the traces are
   without g64m's checkout. The image is not in the repository.

## Not yet

- Threads, and signals delivered to the guest's handlers: until step 5 a handler is
  recorded and never called; `clone` with shared memory is refused.
- What a program can read of the machine it is on: `/proc/cpuinfo` is the host's,
  `/proc/self/exe` is r64m. Put right when a program is met that reads them
  (R024's compile calls `readlink` 89 times: to be looked at in step 3).
- `ptrace`, `seccomp`, `io_uring`, 32-bit programs: refused.

## Consequences

- r64m's speed for a real program is then also the kernel's: a call costs what the
  host's costs and a translation of its arguments.
- `src/sys.rs` grows from three calls to the table; `src/elf.rs` gains `ET_DYN` and
  the interpreter; `Mem` gives memory by `mmap`.
- A process forked has its parent's blocks; one started by `execve` has none. The
  build race measures what that costs before anything is built to share them.
- g64m's race (the whole machine beside QEMU's whole machine) and this one (a
  process beside QEMU's process) are different races; their numbers are not compared.
