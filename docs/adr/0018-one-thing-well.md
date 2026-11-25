# 0018: r64m does one thing: on Arm, it behaves as x86-64

**Status**: accepted (2026-11-25, the owner: 「一つのことをうまくやろう」, said of r64m; and
then, to say what the thing is: 「単純に Arm で x86_64 に振る舞うという行為をうまくやるだけ、
そこから Fork なり何なりでマシンになればいい」)

## Context

g64m is a whole machine: it boots a kernel, and so it has a disk, interrupts, paging,
a console, and its own answer to everything a kernel asks of hardware. r64m was begun
as its rebuild, and 0002 set g64m's whole list as the bar for `main`: programs without
a kernel, Linux booted, NetHack built in the guest.

Since then the owner chose the container's way (0017). There an x86-64 program is a
process of the host's kernel, and r64m ran busybox's shell, GCC and NetHack's whole
build by handing the program's calls on: files, processes and time are the kernel's,
the image is Docker's, linking is the guest's own loader's. r64m was not changed at
all for GCC. And the one thing that made the build take fifty minutes was r64m doing
work that was no part of running instructions (R029).

## Decision

1. **r64m's one thing is to behave as x86-64 on Arm**: to do to a program's state
   what an x86-64 CPU would do, right, by the real CPU (the traces, and what the
   real machine prints), and fast, by QEMU. The interpreter, the JIT, the arithmetic
   of SSE and of the x87 are that thing. Today it is done for a Linux program, which
   is the form the goal needs (0011).

2. **What it has besides is what that thing cannot be done without**, because
   nothing else can do it for a program of another machine's: loading the program
   (0005), the program's memory (0008), and the handing on of its system calls with
   the few differences between the two kernels made good (0017). Tools for seeing
   that it is right and fast (`--trace`, `--jit-check`, `--jit-stats`, `--jit-why`)
   serve the one thing and stay.

3. **Before anything is added, the question is whether the host's kernel, Docker, or
   the guest's own loader does it already.** If so it is handed on, not built. What
   would make r64m a second kernel is not built: files, descriptors or processes of
   its own, a scheduler, devices, a network.

4. **r64m is not a machine, and a machine may come of it.** It does not boot a
   kernel and has no disk. If a machine is wanted, it is made from r64m by a fork or
   the like: another program around the same behaving-as-x86-64, with the devices
   and the guest's own paging that a kernel needs. It is not grown inside r64m, and
   it is not owed here.

5. **0002 is changed by this.** `main` still waits for a version that works, and the
   bar is now the container's goal (0011): the request through nginx to FastAPI over
   TLS, in an x86-64 container on an arm64 host. Not g64m's list. Everything else of
   0002 stands: until then the work is on `port`, with no pull request and no merge.

## Not yet

- Reading a `#!` line and starting r64m again at `execve` are the kernel's work,
  done by r64m only because it is not registered with the kernel (`binfmt_misc`).
  They go when it is, which is a setting of the whole Docker VM and the owner's word
  (0017).

## Consequences

- Nothing of g64m's machine is ported into r64m: no CPU modes below 64-bit user
  mode, no paging of the guest's own, no devices. The README's "What r64m is not"
  says so.
- For a fork to be possible, what behaves as x86-64 (`src/cpu`, `src/jit`) does not
  reach into what is Linux's process (`src/elf.rs`, `src/sys.rs`): it does not
  today, and that is kept. What a machine would need changed is the memory: here a
  program's memory is the host's (0008), which a guest kernel's paging is not. That
  is the fork's to solve.
- A slow path is first looked at for work that is not r64m's (R029 was).
- The comparison with g64m is of the JIT on the primitives only; a build in the
  guest (g64m) and a build in a container (r64m) are different things (0017).
