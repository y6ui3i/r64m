# 0014: The JIT: blocks for arm64, the simple form first

**Status**: proposed (2026-11-24; the three points at the end are the owner's)

## Context

The interpreter runs 34.8 million instructions a second (R004). QEMU runs 1,267
million on the same primitives, g64m with its JIT 2,562 million. Twice QEMU is 73
times the interpreter.

What g64m's JIT arrived at is in its `docs/jit64.md` and its ledger (E120 to E186), and
so is what cost it time for reasons that were Goose's and not the design's: globals
reached by their offset, a guest's memory that had to be an array and be tested at
every use (0008 removes this), and above all the price of leaving generated code for
the interpreter and coming back, 400 to 600 host instructions.

This decides the JIT's shape for a program without a kernel. It is the simple form
(0007): each thing g64m added after its first block is named under "Not yet" and comes
back only when counted.

## Decision

1. **A block is a straight run of guest instructions**, from an address that was
   jumped to, to the first instruction that leaves the line (a jump, a call, a return,
   a system call) or that is not translated. It is translated to arm64 code that does
   the same to the same state.

2. **A block is made when its first address has been arrived at often** (a count kept
   by address, g64m's 16 to begin with). Until then, and for every instruction no
   translator is written for, the interpreter runs. A program's start, run once, is not
   worth translating: making an instruction's translation costs far more than
   interpreting it once (how much more is not measured here yet; the count of 16 is
   g64m's and is to be measured again).

3. **The guest's sixteen registers live in sixteen host registers**, the same ones in
   every block, for as long as generated code runs. One more holds the base of the
   guest's memory and one the address of the CPU's state. Going in loads the sixteen
   from the state and coming out stores them; a block reads and writes none of them
   in memory. So an x86 instruction on registers is one arm64 instruction on
   registers, and a load is `ldr x, [base, address]` (0008).

4. **Going in and coming out are one small routine in assembly inside Rust**
   (`global_asm!`), called as a function: in with the state's address and the block's,
   out with the reason (the next guest address is in the state). No C (0012).

5. **RIP is a constant in a block**, written to the state only where the block leaves.

6. **The flags are a record, as in g64m**: the last instruction that set them leaves
   what kind it was, its operands and its result in three host registers, and the
   flags are worked out from the record only when something reads them. A conditional
   jump straight after a compare is the compare's own arm64 flags and no record read.

7. **A fault in generated code is found by the host** (0008, points 2 to 4). The
   handler is a signal handler on its own stack. From the host's program counter it
   finds the block and, in the block's table, the guest instruction; the sixteen
   registers are in the context the kernel saved, since a block keeps them current at
   every instruction that touches memory. It writes them to the state with that
   instruction's RIP, and changes the saved program counter to the way out. The
   interpreter then meets the same instruction, and what it finds is the answer: a
   fault of the guest's (0010), or a store to a page with translated code, whose
   blocks are forgotten and whose protection is given back.

8. **A page with translated code is not writable to the host** (0008, point 3), and
   the interpreter's own stores ask first whether the page holds translations.

9. **Generated code is in one piece of memory that can be written and run**: on
   Linux as it is; on macOS a `MAP_JIT` mapping with the thread's write switch turned
   (`pthread_jit_write_protect_np`). After writing, the instruction cache is told: on
   macOS by `sys_icache_invalidate`, on Linux by the cache instructions themselves in
   inline assembly. When the piece is full everything is forgotten and it begins
   again.

10. **The arm64 instructions are written by r64m's own encoder**: one small function
    for each form used, each with a test of its bits against what an assembler gives.
    No crate (0009).

11. **Every block is checked against the interpreter from the first day**
    (`--jit-check`, g64m's E120): the state is kept, the block run, the state put back
    and the same instructions interpreted, and the two ends compared, memory written
    included. The 39 cases and the 23 primitives are run that way on both hosts before
    any block is believed.

## Not yet (each is g64m's, each waits to be counted here)

- Blocks joined to one another (a table looked in from generated code, a jump patched
  to the next block). At first every block comes out to Rust, which finds the next.
  Leaving and coming back is here some forty loads and stores, not 400 instructions:
  what joining gains is to be measured, not assumed.
- A block that goes on past a conditional jump; the cells; the settle routine; the
  address reuse; string instructions in blocks; SSE and the x87 in blocks (the first
  translators are for the integer instructions the primitives spend their time in).
- The same address as the host's (0008, point 5).

## The three points that are the owner's

**A. Translate what is hot, or everything.** QEMU, Rosetta and FEX-Emu translate every
instruction the first time it is met and have no interpreter in the way. g64m
translates what has been arrived at 16 times and interprets the rest.
*Proposed: what is hot (point 2). r64m has an interpreter that is right on every case,
which is also what checks the blocks (point 11); a translator for every instruction
before the first program runs is the larger first step.*

**B. The flags as a record, or in the host's own flags.** arm64's N, Z and V are x86's
SF, ZF and OF after an addition, and its carry is x86's turned over after a
subtraction; Rosetta and FEX-Emu keep x86's flags there, and Rosetta is faster than
g64m on seven primitives. But PF and AF have no place there, and every instruction
between a compare and its jump must then leave the host's flags alone.
*Proposed: the record (point 6), which is known to reach twice QEMU, with the compare
and jump pair using the host's flags directly. The host's flags throughout is the
first thing to try afterwards if the primitives ask for it.*

**C. The encoder: r64m's own, or a crate** (`dynasm-rs` is the usual one: arm64
assembly written in Rust source, encoded when r64m is built).
*Proposed: r64m's own (point 10). It is some forty forms; a crate would be the third
from outside and brings a macro's tool chain into the program itself, where 0009 kept
it to the tests.*

## Consequences

- r64m gets its first code that is not portable Rust: the way in and out, the cache
  instructions, the signal handler's reading of the saved registers. Each is written
  twice where the hosts differ (the context's layout, how memory becomes runnable),
  and both are tested from the first block (R003 is why that is cheap).
- Signals are needed now, not later: the handler of point 7 is the first.
- The order of work: the encoder; the piece of memory and the way in and out, with a
  block made by hand; the translator for register-to-register instructions with
  `--jit-check`; memory operands with the handler; then the count of what the
  primitives spend their time in decides what is translated next.
