# 0003: 64-bit only: no 16-bit, no 32-bit, no BIOS

**Status**: accepted (2026-11-24, the owner)

## Context

g64m's repository holds two machines: the 64-bit one, and g86m, the 16- and 32-bit one it
grew from, which boots FreeDOS and ELKS through BIOS services done in host code. In
64-bit mode there are no segments to speak of (FS and GS have a base and nothing else),
an address is used as it is, and Linux can be entered at its 64-bit entry with no
firmware. 16-bit code brings a base and a limit to every access, its own way of forming
addresses, far calls, real mode's interrupts, A20, and a BIOS with the devices it
expects. 32-bit programs for a 64-bit kernel bring a second set of system calls. Few
container images are still made for i386.

## Decision

r64m emulates 64-bit mode only. No real mode, no 16-bit or 32-bit protected mode, no
32-bit programs, no BIOS.

## Consequences

- The fast path of a memory access has no segment in it.
- One mode to decode, to translate and to take expected output for.
- FreeDOS, ELKS and booting from a disk's boot sector are not r64m's; g86m still does
  them, in g64m's repository.
- A disk image cannot be booted as it is: a kernel is given (0005).
