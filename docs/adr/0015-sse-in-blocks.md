# 0015: SSE in blocks: the host's vector registers, and the interpreter for what is not a number

**Status**: proposed (2026-11-25). Three points are the owner's: A, B and C below.

## Context

Blocks hold the integer instructions of the primitives (R018: sixteen primitives past
QEMU). Six primitives are floating point and one is packed integers; a block ends at
their first SSE instruction, and they run at the interpreter's speed. The goal's
programs (0011) are compiled for x86-64, where every `double` is SSE2 and every
`memcpy` and `strlen` of the C library is too.

R019 looked at the host first. arm64's arithmetic gives x86's bits for every result
that is a number, and differs in four places: the sign of a result that is not a
number, which of two such operands comes out, FMIN and FMAX, and a conversion that does
not fit. The interpreter does all four x86's way (`src/cpu/sse.rs`), held to the real
CPU's traces.

The interpreter keeps of MXCSR what a program sets; it does not raise the six sticky
flags, and its arithmetic rounds to nearest whatever MXCSR says (only the conversion
to an integer reads the rounding). Blocks are held to the interpreter, so this
decision changes neither; both are named under "Not yet".

## Decision

1. **A: the guest's sixteen XMM registers live in the host's v0 to v15**, in every
   block, as the sixteen integer registers live in x0 to x15 (0014). Going in loads
   them from the state and coming out stores them: eight `ldp` of two 128-bit
   registers going in and eight `stp` coming out. v16 to v31 are scratch. The host's rule that d8 to d15 are kept
   across a call is met in `enter`, once.
   - The other way is to leave them in the state and load and store around each
     instruction. A `double` loop is then two loads and a store for each `addsd`, the
     cost g64m's ledger counted for integers before its registers were pinned.
   - The price of this way is paid by every entry, with or without SSE in the block:
     sixteen host instructions that touch 256 bytes, and four each way for d8 to d15.
     Entries are rare where blocks are joined (R012); the race says whether it shows.

2. **B: a block computes with the host's instruction and leaves to the interpreter
   when the result is not a number.** `addsd xmm1, xmm2` is

   ```
   fadd d16, d1, d2        ; into scratch: nothing of the guest's is changed yet
   fcmp d16, d16           ; unordered only if the result is not a number
   b.vs bail               ; out of line: this instruction is the interpreter's
   mov  v1.d[0], v16.d[0]  ; the low element; the upper half stays, as on x86
   ```

   A result is not a number only if an operand was not, or the operation was one of
   R019's row 1; in both the interpreter's `far64` gives x86's bits. So the block is
   right in every case and never works out which of the four cases it is in. The way
   out is the one division already has (`LEFT_FOR_THE_INTERPRETER`): the registers are
   as they were before the instruction.
   - The other ways: x86's rules inline (a dozen host instructions an operation, for a
     case a program meets only by mistake), or the host's results as they are (QEMU
     does not; a program that prints a NaN's sign, as `printf("%f", 0.0/0.0)` does with
     `-nan`, would differ from the real CPU, and 0001 holds r64m to the real CPU).
   - What it costs: two host instructions an operation, the branch never taken. And
     the flags of the host are spent by the `fcmp`, so a guest comparison whose answer
     a later jump reads must not have arithmetic between; x86's arithmetic leaves
     RFLAGS alone, so the translator notes that the host's flags are gone and the jump
     reads the record (as it does today after a `mov` that needed them).

3. **The smaller and the larger are a comparison and a choice**: `fcmp a, b;
   fcsel r, a, b, mi` (`gt`), which R019 showed to be x86's rule in all eight rows.
   No way out.

4. **UCOMISD and COMISD set the host's flags by `fcmp` and write a record of the two
   operands** (a new kind; `settle` works ZF, PF and CF out as the interpreter does).
   A jump after it reads the host's flags where one condition answers:

   | x86 | true when | arm64 |
   |---|---|---|
   | `ja` | greater | GT |
   | `jae` | greater, equal | GE |
   | `jb` | less, unordered | LT |
   | `jbe` | less, equal, unordered | LE |
   | `jp` / `jnp` | unordered / not | VS / VC |
   | `je` | equal, unordered | EQ or VS: two branches |
   | `jne` | less, greater | NE and VC: two branches |

   This is a fourth `HostFlags`, beside add, subtract and logic.

5. **A float to an integer, cut off (CVTTSD2SI, CVTTSS2SI)**, is `fcvtzs` and a test
   that the result is neither the largest nor the smallest integer nor zero from a
   value that was not; where it may be, the instruction is the interpreter's. CVTSD2SI,
   which rounds as MXCSR says, is the interpreter's until it is counted.

6. **What needs nothing special is translated straight**: the moves (MOVSD, MOVSS,
   MOVAPS, MOVUPS, MOVDQA, MOVDQU, MOVD, MOVQ, to and from memory and the integer
   registers), AND, ANDN, OR, XOR on the whole register, an integer to a float
   (CVTSI2SD: `scvtf`, exact on both), and between the two sizes (CVTSD2SS, CVTSS2SD:
   `fcvt` with the same test as 2, since a NaN's quieting is a result that is not a
   number).

7. **`--jit-check` compares the sixteen XMM registers and MXCSR** as it compares the
   integer ones, and every instruction of 2 to 6 has a test that runs it in a block
   beside the interpreter on R019's operands, on both hosts. A test asserts the
   instruction was in a block (the lesson of the 8-bit shifts).

8. **C: the order.** First the scalar instructions of 2 to 6, which are what the six
   floating-point primitives and compiled C use; raced. Second the packed integer
   instructions p_simd and the C library's string functions use (PADD, PSUB, PCMPEQ,
   PMOVMSKB, the shifts and shuffles), by a count of what the programs execute, a card
   of its own. **x87 is not put in blocks**: it is 80 bits by soft arithmetic in the
   interpreter, the host has nothing like it, and x86-64 code uses it for `long double`
   only (p_ldbl is the one primitive).

## Not yet

- **The sticky flags of MXCSR** (inexact and the rest). The interpreter does not raise
  them. A program that reads them (`fetestexcept`) is wrong in both alike; when one is
  met, the interpreter is put right first and blocks after, perhaps from the host's
  FPSR, which keeps the same six.
- **Rounding other than to nearest** for arithmetic: the same; the host's FPCR has the
  same four modes.
- **Exceptions that are not masked**: a program that unmasks one gets no signal.
- **Packed floating point** (ADDPD and the rest): by the count, with the packed
  integers.
- **Loading only the XMM registers a block uses**: if 1's price shows in the race.

## Consequences

- `enter` grows by twelve instructions each way and the state's `xmm` must stay where
  `enter` expects it (`#[repr(C)]`, an offset asserted).
- The fault handler and the way out by fault store v0 to v15 as they store x0 to x15.
- The encoder gains the floating-point and vector forms, each tested against the
  assembler's bits as the 152 are.
- A block with floating point has one more way out per arithmetic instruction; the
  bail stubs are out of line after the block's end, where the taken ways of jumps are.
