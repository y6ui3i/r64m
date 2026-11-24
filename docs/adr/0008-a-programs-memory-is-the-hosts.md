# 0008: A program's memory is the host's, with the host's protection

**Status**: accepted (2026-11-24, the owner: the three points as proposed)

## Context

g64m holds a program's memory as an array of its own and tests every address before it
is used: is it inside the array, and for a store, does the page hold translated code.
In one primitive's loop that is thirteen host instructions of 28 (g64m's E184), and it
is why six primitives stay under twice QEMU. QEMU's user mode and FEX-Emu make no such
test: the guest's memory is the host's, a wild address is a fault the host raises, and a
page that holds translated code is kept unwritable so that a store to it is a fault too.

What the hosts allow was looked at first (R002):

- arm64 Linux: 4 KB pages, 48 bits of address, 2^47 bytes reserved at one stroke (all an
  x86 program can address), memory given at any low address, a fault told with its
  address and gone on from in half a microsecond;
- macOS on Apple silicon: 16 KB pages, 47 bits, 2^46 bytes reserved, nothing given below
  4 GB, a fault in 2.2 microseconds.

This decision is about a program without a kernel. A machine (a kernel booted) has the
guest's own page tables between an address and memory, and keeps g64m's TLB in generated
code; its memory is a mapping of the host's too, and nothing more is decided for it here.

## Decision

1. **The guest's memory is one reservation of the host's address space**, as large as
   the host gives (2^47 bytes on Linux, 2^46 on macOS), with no access at first. The
   guest's address `g` is the host's `base + g`. What the guest maps is made accessible
   inside it; everything else stays a fault.

2. **Generated code and the interpreter use an address without testing it.** A load is
   the address formed and the access (`ldr x, [base, address]`). There is no test for
   being inside, and none for translated code.

3. **A page that holds translated code is not writable to the host.** A store to it
   faults; the handler forgets the page's translations, makes it writable again and lets
   the store go on. (When code from it is translated again, it is made unwritable
   again.)

4. **A fault in the guest's memory is the guest's fault.** The handler finds the guest's
   instruction from where the host was (each block keeps, for its instructions that
   touch memory, which guest instruction each is), and the guest gets what Linux would
   have given it: SIGSEGV, or the end of the program.

5. **Offset first, the same address later if counted.** `base` is a register and the
   address is added to it by the host's addressing mode, at no instruction. Making
   `base` zero (the guest's address is the host's, as FEX-Emu has it) would let an x86
   operand `[reg + constant]` be one host instruction where it is two; it needs r64m
   itself to live outside the guest's half of the address space, which Linux with 48
   bits gives and macOS does not. Not done until the simple form is measured (0007).

## The three points that were the owner's, each accepted as proposed

**A. An address outside the reservation is not caught.** On Linux the reservation is
everything an x86 program can address in user space, so this is only an address that is
not canonical on x86 (bit 47 or above set), which a real CPU answers with a general
protection fault. Here such an address, added to `base`, may land on memory of r64m's
own and be read or written. QEMU's user mode and FEX-Emu have the same hole and say that
they are not a boundary of security: the guest and the emulator are one process with one
set of rights, and what a container may not do is the kernel's to enforce. The other
choice is g64m's: two host instructions for each access to close it.
*Proposed: accept the hole, and say so in the README.*

**B. macOS protects 16 KB at a time.** Three things are then coarser than on a real CPU:
a guest's page with no access next to one with access can be read; a page of data next
to a page of code faults on every store until the code's translations are forgotten; a
guard page under a stack does not guard. r64m would run programs on macOS (the
primitives, its own tests) and be exact only on a host with 4 KB pages.
*Proposed: arm64 Linux with 4 KB pages is the host r64m is made for; macOS is where it
is developed, with these three written down. (A Raspberry Pi 5's usual kernel has 16 KB
pages too; its other kernel has 4 KB.)*

**C. What a fault costs is paid by programs that fault on purpose.** A language's
run-time that finds a null pointer by the fault, or grows a stack by one, pays half a
microsecond each time and not the guest kernel's few hundred nanoseconds.
*Proposed: accept; count them when a real program is run.*

## Consequences

- The thirteen instructions of E184 are gone from a program's loads and stores.
- r64m needs a handler for faults from its first day, and each block needs its table
  of where its memory instructions are.
- A program is no longer limited to 256 MB.
- Signals for the guest (SIGSEGV first) are part of the first version, not a later one.
- Where the guest's memory is 16 KB-coarse, a test's expected output can differ from
  the real CPU's; such tests are run on Linux.
