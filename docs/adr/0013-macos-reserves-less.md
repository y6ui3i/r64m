# 0013: On macOS the reservation is 2^40 bytes

**Status**: accepted (2026-11-24, the owner; changes point 1 of 0008 for macOS only)

## Context

0008's first point: the guest's memory is one reservation, "as large as the host gives
(2^47 bytes on Linux, 2^46 on macOS)". On Linux a reservation costs nothing that can be
timed. On macOS it costs time in proportion to its size (R004): 2^46 bytes take 65 ms to
reserve and 104 ms to give back, so `r64m` took 0.17 s to run a program that does
nothing, and each unit test of the memory a quarter of a second.

## Decision

On macOS r64m asks for 2^40 bytes (a thousand gigabytes), which cost about 2 ms to
reserve and give back. On Linux nothing changes: 2^47 bytes, all an x86 program can
address.

## Consequences

- On macOS a program's stack is under 2^40, not under 2^46, and a program that asks for
  memory at a fixed address above 2^40 is refused there. Neither is true of Linux,
  which is the host r64m is made for (0008, point B).
- 0008's point A (an address outside the reservation is not caught) is wider on macOS:
  every address from 2^40 up, not only those no x86 program can validly form. macOS is
  where r64m is developed; this is said in the README with the rest.
