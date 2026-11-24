# 0010: The interpreter asks the list of regions; generated code does not

**Status**: proposed (2026-11-24; changes point 2 of 0008 for the interpreter only, and
waits for the owner's word)

## Context

0008's second point says that generated code *and the interpreter* use an address
without testing it. For generated code that is the whole gain. For the interpreter it
has a price that was not weighed there: a fault taken in the middle of a Rust function
has to be left by jumping out of the signal handler over that function's frames
(`siglongjmp`), which Rust does not promise to survive, or by a handler that knows the
interpreter's own machine code. And on a host with 16 KB pages the interpreter would be
as coarse as generated code is (0008, point B).

r64m has to keep a list of what the guest mapped in any case: `munmap`, `mprotect` and
`/proc/self/maps` are answered from it.

## Decision

The interpreter asks that list before each access (`Mem::check`): is every byte mapped,
with a protection that allows this access. Generated code does not ask, as 0008 says.

## Consequences

- The interpreter is exact to the guest's 4 KB page on every host, and its faults need
  no signal handler. A program can be run and tested on macOS before the handler
  exists.
- The handler of 0008 is needed when generated code is, not before.
- The interpreter pays a search of the list for each access. It runs what is not yet
  translated, so this is to be counted when there is a program to count on, and a
  one-entry memory of the last region is the first thing to try if it shows.
- On a 16 KB host the interpreter and generated code can differ where 0008's point B
  says the host is coarse. Such cases are tested on Linux.
