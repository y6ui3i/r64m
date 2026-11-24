# 0005: A program and a kernel are loaded by host code

**Status**: accepted (2026-11-24, the owner)

## Context

Something must put the first state in place: the guest's memory filled and the CPU at its
first instruction. A real PC does it with firmware run by the CPU itself. With no 16-bit
mode (0003) there is nothing for firmware to run in.

## Decision

Loading is host code, apart from the machine's devices (`src/load/`):

- a program: its ELF segments placed, its stack given the arguments, the environment and
  the auxiliary vector; if it names a dynamic linker, that is loaded too and entered
  first;
- a kernel: Linux's boot protocol followed (the kernel and the initramfs in memory, the
  boot parameters and the map of memory filled in), and the CPU started in 64-bit mode
  at the kernel's entry, as Firecracker does.

## Consequences

- No BIOS, real or done in host code.
- Programs that are linked dynamically are r64m's to run; g64m runs only static ones.
