# 0004: One executable

**Status**: accepted (2026-11-24, the owner)

## Context

g64m's repository builds two executables that share devices and little else; a change to
the shared part is checked on one and forgotten on the other.

## Decision

r64m is one executable. What it is given says how it runs: a program (an ELF file) runs
without a kernel, its system calls answered by r64m; a kernel boots a machine.

## Consequences

- Run for a program, it must not pay for the machine: started for every process of a
  container, what it sets up before the first instruction is multiplied by them.
