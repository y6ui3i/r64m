# 0016: Only code the guest may write is guarded against writes

**Status**: accepted (2026-11-25, the owner: the two points, A and B, as proposed)

## Context

A block is a translation of the guest's bytes and is wrong once they change. Since
R009 the host pages a block's instructions lie in are marked as holding translated
code and made unwritable; a store to them faults, and the runner finds whether it was
to the code (the blocks are forgotten) or to data that only shares the host's page
(the store is made by the interpreter with the page opened for one instruction, R010).

On a host with 4 KB pages the second case does not exist. On macOS, and on a Raspberry
Pi 5's usual kernel, a host page is 16 KB and holds four of the guest's. A program
linked for x86-64 has its segments 4 KB apart: p_sha's code is at 401000h and its data
at 403000h, in one host page, and every store to that data leaves generated code:
30,721 times a round, which is why p_sha is the one integer primitive behind QEMU
(0.129 s a round for 0.002 s). Nineteen of the 23 primitives have their first data
there too; p_sha is the one whose hot data is. Any real program may be.

R022 looked first: p_sha's code is on a page the guest itself may not write (read and
run), as every compiled program's is. With only code from pages the guest may write
marked, p_sha made no store beside code, ran 0.278 s for 200 rounds (QEMU 0.437 s,
g64m's JIT 0.323 s), and all 62 programs were right.

## Decision

1. **A: a block marks its host pages only if the guest may write one of the guest's
   pages it lies in** (or that it rests on, `Built::seen`). Code on a page the guest
   may only read and run cannot be changed by a store the guest is allowed to make,
   so its translation needs no guard. Code the guest may write (what a JIT inside the
   guest makes, or a program that changes itself) is guarded exactly as now: R009's
   and R010's machinery stays for it, untouched.

2. **When the guest makes a page writable that blocks were made from** (`mprotect`,
   or a new mapping over it), **those blocks are forgotten** before the change is
   made. Made again, they find the page writable and are marked. r64m has no such
   system call yet (`write` and `exit` only): the rule is written into `Mem`'s
   `protect`, `map` and `unmap` callers when the first of them exists, with a test
   that changes code after making it writable.

3. **B: what is given up, on a 16 KB host only.** Generated code does not ask whether
   a store is allowed; the host's protection answers (0008), and the host protects a
   whole 16 KB with the most that any of its four guest pages allows. So a store by
   generated code to a page the guest may not write, in a host page it shares with
   one the guest may write, is made where x86 would fault. 0008 accepted that (its
   point B) for data. This decision adds one thing to it: if the page so written is
   code, a block made from it goes on running what it was translated from.
   - Today the mark happens to turn that store into x86's fault: the host page is
     unwritable, the interpreter is asked, and the interpreter asks the list (0010).
     That is given up.
   - Who can meet it: a program that stores into its own code without making it
     writable, within 12 KB of its data. On x86 that program gets SIGSEGV. A correct
     program does not do it; one that does it on purpose to catch the signal would
     differ here, as it would for read-only data already.
   - On a 4 KB host nothing is given up: the host page is the guest's page, and the
     store faults as on x86.

## What else was weighed

- **The store made by the handler of the fault** (the page mapped a second time,
  writable; the handler decodes the host's store, makes it there, and goes on). Exact,
  and no leaving generated code. But a signal for every store: a few microseconds
  where the store is a nanosecond. p_sha would still be far behind QEMU. Not built,
  not measured.
- **A block that compares the guest's bytes with what it was made from** each time it
  is come to, in place of the mark. No fault, but it is paid by every block of a
  mixed page on every arrival, and a block that changes an instruction after itself
  in the same block still runs the old one to its end.
- **Guest pages laid out so that code and data never share a host page.** Not with
  `base + g` (0008): a program's addresses are its own.

## Consequences

- p_sha, and any program whose data is beside its code, runs in generated code on
  16 KB hosts as on 4 KB ones.
- A block from a page that cannot be written is never forgotten for a store. Its host
  pages are not counted in `holds`; `mem::C` is set for fewer pages, and fewer
  `mprotect` calls are made when blocks are made (p_fill was 2.8% faster in R022).
- 0008's point B gains a fourth line, and the test of "a store to data beside code"
  moves to code on a writable page, where the case still is.
- The interpreter is not changed: it asks the list, and faults where x86 does.
