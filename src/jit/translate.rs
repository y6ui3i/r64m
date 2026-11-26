//! From a run of the guest's instructions to arm64 code that does the same (ADR 0014,
//! points 1, 3, 5 and 6).
//!
//! The integer instructions the primitives are mostly made of: moves, arithmetic and
//! logic on operands of 8, 16, 32 and 64 bits, shifts and rotations and bit scans on 32
//! and 64, the stack, and jumps. An instruction that is not in the list, or has a
//! prefix that is not read here, is not translated: the block ends before it and the
//! interpreter runs it.
//!
//! What generated code may rely on, and must keep:
//! - x0 to x15 are the guest's registers at every instruction's start, and at every
//!   load or store inside one: a fault there leaves the block with them as they are.
//!   So an instruction changes no guest register before its last load or store.
//! - the record of the flags (x21 to x23) is what the last instruction that set them
//!   left: written when that instruction is done, not before.
//! - x16, x17, x25, x26, x29 and x30 are scratch within one instruction (the way in
//!   keeps the host's own x29 and x30; generated code calls nothing).
//! - x27 counts the instructions of blocks run to their ends: each way out of a block
//!   adds the block's to it, so that a block may go on to the next without Rust
//!   between (R009).

use super::a64::{self, Cond, Fp, FpSize, Lane, Logic, Mem as Op, Shift, Size, V3, VLogic, VShift};
use super::{BASE, CPU, HOST, KEPT_D8, LEFT_AT_END, LEFT_FOR_THE_INTERPRETER, RAN, REASON, TABLE, TABLE_BITS, XMM_LOADED};
use crate::cpu::{Cpu, REC_ADC, REC_ADD, REC_ADD_BY_RESULT, REC_BSF, REC_BSR, REC_CARRY_GIVEN, REC_FLOAT, REC_IMUL, REC_KEEP, REC_LOGIC, REC_MUL, REC_SBB, REC_SHIFT, REC_SUB, REC_SUB_BY_RESULT};
use crate::mem::Mem;
use std::mem::offset_of;

/// The most instructions in one block.
const MOST: usize = 64;

// Scratch, and the record of the flags.
const T0: u8 = 16;
const T1: u8 = 17;
const T2: u8 = 25;
const T3: u8 = 26;
const T4: u8 = 29;
const T5: u8 = 30;
const RCX: u8 = 1;
const REC_KIND: u8 = 21;
const REC_A: u8 = 22;
const REC_B: u8 = 23;
const RSP: u8 = 4;
// Scratch among the vector registers (ADR 0015).
const V0: u8 = 16;
const V1: u8 = 17;

/// A block as translated: its code, and for the instruction at each place in it where
/// its code begins (in instructions of the host's) and its address in the guest.
pub struct Built {
    pub words: Vec<u32>,
    pub starts: Vec<(u32, u64)>,
    /// The first address after the block's last instruction.
    pub end: u64,
    /// What of the guest's code the block rests on besides its own instructions: the
    /// lowest and one past the highest address that was read to decide that a record
    /// of the flags need not be written (R013). A store there forgets the block as a
    /// store to its own instructions does.
    pub seen: Option<(u64, u64)>,
}

/// What a block that uses the XMM registers begins with: if none has loaded them since
/// the way in, they are loaded (the lower halves of the host's v8 to v15, which are
/// its caller's, kept first), and the way out is told to store them (ADR 0015, point
/// 1, as built in R020).
fn load_xmm() -> Vec<u32> {
    const SP: u8 = 31;
    let mut w = vec![a64::mem_imm(Size::X, Op::Load, T0, SP, XMM_LOADED), 0];
    for i in 0..4 {
        w.push(a64::stp_d(8 + 2 * i, 9 + 2 * i, SP, KEPT_D8 + 16 * i as u32));
    }
    w.push(a64::arith_imm(true, false, false, T0, CPU, offset_of!(Cpu, xmm) as u32, false));
    for i in 0..8 {
        w.push(a64::ldp_q(2 * i, 2 * i + 1, T0, 32 * i as u32));
    }
    w.push(a64::movz(true, T0, 1, 0));
    w.push(a64::mem_imm(Size::X, Op::Store, T0, SP, XMM_LOADED));
    // Over all of it, if they are loaded.
    w[1] = a64::cbz(true, true, T0, w.len() as i32 - 1);
    w
}

/// An operand that is a register or memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rm {
    Reg(u8),
    /// The address is `base + index << scale + disp`; with neither register, `disp`
    /// alone (an address relative to RIP is made that when the instruction is read).
    Mem { base: Option<u8>, index: Option<(u8, u32)>, disp: i64 },
}

/// One instruction as read: enough of it to translate, or it is not read at all.
#[derive(Debug, PartialEq, Eq)]
struct Ins {
    /// The opcode; one behind 0Fh has 100h added.
    op: u16,
    len: usize,
    /// The operand's size in bits: 8, 16, 32 or 64.
    size: u32,
    /// Whether there was a REX prefix (it changes which byte registers 4 to 7 are).
    rex: bool,
    /// ModRM's register field, or the register in the opcode's low bits.
    reg: u8,
    rm: Rm,
    /// The constant, with its sign; for a jump, where it goes.
    imm: i64,
    /// The segment a memory operand is relative to: 0 none, 1 FS, 2 GS. (The others
    /// are nothing in 64-bit code.)
    seg: u8,
    /// For an SSE instruction, the prefix that says which it is: 0 none, 1 66h,
    /// 2 F3h, 3 F2h (the interpreter's numbers). Else 0.
    pre: u8,
}

/// Is this opcode an SSE instruction that is translated (ADR 0015)?
fn is_sse(op: u16) -> bool {
    matches!(
        op,
        0x110 | 0x111 | 0x116 | 0x128 | 0x129 | 0x12a | 0x12c | 0x12e | 0x12f | 0x151 | 0x154..=0x15a | 0x15c..=0x15f
            | 0x16e | 0x16f | 0x17e | 0x17f | 0x1d6 | 0x1db | 0x1df | 0x1eb | 0x1ef
    ) || is_packed(op)
}

/// The packed instructions of whole numbers (R021), and SHUFPS with them: PUNPCKL and
/// PUNPCKH of each size, PCMPGT and PCMPEQ, PSHUFD, the shifts by a constant, PADD
/// and PSUB of each size, PMULUDQ.
fn is_packed(op: u16) -> bool {
    matches!(op, 0x160..=0x162 | 0x164..=0x166 | 0x168..=0x16a | 0x16c | 0x16d | 0x170..=0x176 | 0x1c6 | 0x1d4 | 0x1f4 | 0x1f8..=0x1fe)
}

/// Is it one with this prefix?
fn sse_with(op: u16, pre: u8) -> bool {
    match op {
        // The moves of 128 bits and of a low element; the scalar ones by F3h and F2h.
        0x110 | 0x111 => pre < 4,
        // MOVAPS and MOVAPD, UCOMIS and COMIS, and the logic of the whole register;
        // MOVHPS and MOVHPD from memory, MOVLHPS between registers.
        0x116 | 0x128 | 0x129 | 0x12e | 0x12f | 0x154..=0x157 => pre < 2,
        // MOVDQA and MOVDQU; MOVD and MOVQ out of a register (66h), MOVQ into one
        // (F3h).
        0x16f | 0x17f | 0x17e => pre == 1 || pre == 2,
        // MOVD and MOVQ into a register, MOVQ out of one, PAND, PANDN, POR, PXOR.
        0x16e | 0x1d6 | 0x1db | 0x1df | 0x1eb | 0x1ef => pre == 1,
        // The scalar arithmetic and the conversions.
        0x12a | 0x12c | 0x151 | 0x158..=0x15a | 0x15c..=0x15f => pre == 2 || pre == 3,
        // SHUFPS has no prefix; the packed integers have 66h (without it they are the
        // MMX registers', and PSHUFD is another shuffle with F3h and with F2h).
        0x1c6 => pre == 0,
        op if is_packed(op) => pre == 1,
        _ => false,
    }
}

/// Does this instruction's translation spend the host's flags without setting x86's:
/// the arithmetic and the conversions, which test their result with them (ADR 0015,
/// point 2), and the smaller and the larger, which are a comparison?
fn sse_spends_flags(op: u16) -> bool {
    matches!(op, 0x12c | 0x151 | 0x158..=0x15a | 0x15c..=0x15f)
}

/// Reads the instruction at `rip` from its bytes, if it is one of those translated.
///
/// An opcode for 8-bit operands is given back as its neighbour for larger ones with
/// `size` 8 (00h as 01h, 88h as 89h, F6h as F7h and so on), so that what follows has
/// one opcode for each instruction.
fn read(bytes: &[u8], rip: u64) -> Option<Ins> {
    let mut at = 0;
    let mut next = |n: usize| -> Option<u64> {
        let b = bytes.get(at..at + n)?;
        at += n;
        let mut v = [0u8; 8];
        v[..n].copy_from_slice(b);
        Some(u64::from_le_bytes(v))
    };
    // The prefixes that are read: 66h (16 bits), F3h (kept for the few opcodes it is
    // allowed before), F2h (before an SSE opcode only), and the segments that do
    // nothing in 64-bit code. Then REX.
    let (mut p66, mut f3, mut f2) = (false, false, false);
    // 67h (addresses of 32 bits) is read where there is no address for it to change:
    // hand-written code pads with it (R043).
    let mut p67 = false;
    let mut seg = 0;
    let mut b = next(1)? as u8;
    loop {
        match b {
            0x66 => p66 = true,
            0xf3 => f3 = true,
            0xf2 => f2 = true,
            0x26 | 0x2e | 0x36 | 0x3e => {}
            0x67 => p67 = true,
            0x64 => seg = 1,
            0x65 => seg = 2,
            _ => break,
        }
        b = next(1)? as u8;
    }
    let mut rex = 0;
    if b & 0xf0 == 0x40 {
        rex = b;
        b = next(1)? as u8;
    }
    let mut op = if b == 0x0f { 0x100 | next(1)? as u16 } else { b as u16 };
    // The opcodes that are their neighbour with 8-bit operands.
    let byte = matches!(op, 0x00..=0x3f | 0x80 | 0x84 | 0x88 | 0x8a | 0xa8 | 0xc0 | 0xc6 | 0xd0 | 0xf6) && op & 1 == 0;
    if byte {
        op |= 1;
    }
    // An SSE opcode's prefix says which instruction it is, not how wide: `size` is
    // that of its general register or integer in memory, if it has one.
    let sse = is_sse(op);
    let pre = match (p66, f3, f2) {
        (false, false, false) => 0,
        (true, false, false) => 1,
        (false, true, false) => 2,
        (false, false, true) => 3,
        _ => 4,
    };
    // SETcc writes a byte.
    let size = match (byte || matches!(op, 0x190..=0x19f), rex & 8 != 0, p66 && !sse) {
        (true, _, _) => 8,
        (_, true, _) => 64,
        (_, _, true) => 16,
        _ => 32,
    };
    let mut ins = Ins { op, len: 0, size, rex: rex != 0, reg: 0, rm: Rm::Reg(0), imm: 0, seg, pre: if sse { pre } else { 0 } };

    // What follows the opcode: a ModRM byte, and a constant of how many bytes (for
    // the opcodes whose constant is as wide as the operand, 4 at most).
    let whole = match size {
        8 => 1,
        16 => 2,
        _ => 4,
    };
    let (modrm, mut imm) = match op {
        // The shuffles and the shifts by a constant have a byte after the operand.
        0x170..=0x173 | 0x1c6 => (true, 1),
        _ if sse => (true, 0),
        0x01 | 0x03 | 0x09 | 0x0b | 0x21 | 0x23 | 0x29 | 0x2b | 0x31 | 0x33 | 0x39 | 0x3b => (true, 0),
        0x11 | 0x13 | 0x19 | 0x1b => (true, 0),
        0x1c8..=0x1cf => (false, 0),
        0x63 | 0x85 | 0x89 | 0x8b | 0x8d | 0xd1 | 0xd3 | 0xf7 | 0xff => (true, 0),
        0x11e | 0x11f | 0x1b6 | 0x1b7 | 0x1bc | 0x1bd | 0x1be | 0x1bf => (true, 0),
        // BT, BTS, BTR, BTC by a register; by a constant (a group).
        0x1a3 | 0x1ab | 0x1b3 | 0x1bb => (true, 0),
        0x1ba => (true, 1),
        0x140..=0x14f | 0x190..=0x19f | 0x1af => (true, 0),
        0x81 | 0xc7 | 0x69 => (true, whole),
        0x83 | 0xc1 | 0x6b => (true, 1),
        // The arithmetic of the A register and a constant, and TEST of them.
        0x05 | 0x0d | 0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d | 0xa9 => (false, whole),
        0x50..=0x5f | 0x90 | 0x98 | 0x99 | 0xc3 | 0xc9 => (false, 0),
        // PREFETCH and its neighbours: hints.
        0x118 => (true, 0),
        0x68 | 0xe8 | 0xe9 | 0x180..=0x18f => (false, 4),
        0x6a | 0x70..=0x7f | 0xeb => (false, 1),
        0xb8..=0xbf => (false, if size == 64 { 8 } else { 4 }),
        _ => return None,
    };
    let mut relative = false;
    if modrm {
        let m = next(1)? as u8;
        let (md, rm) = (m >> 6, m & 7);
        ins.reg = m >> 3 & 7 | (rex & 4) << 1;
        if md == 3 {
            ins.rm = Rm::Reg(rm | (rex & 1) << 3);
        } else {
            let (mut base, mut index, mut disp) = (None, None, 0i64);
            if rm == 4 {
                let s = next(1)? as u8;
                let i = s >> 3 & 7 | (rex & 2) << 2;
                if i != 4 {
                    index = Some((i, (s >> 6) as u32));
                }
                if s & 7 == 5 && md == 0 {
                    disp = next(4)? as u32 as i32 as i64;
                } else {
                    base = Some(s & 7 | (rex & 1) << 3);
                }
            } else if rm == 5 && md == 0 {
                disp = next(4)? as u32 as i32 as i64;
                relative = true;
            } else {
                base = Some(rm | (rex & 1) << 3);
            }
            match md {
                1 => disp += next(1)? as u8 as i8 as i64,
                2 => disp += next(4)? as u32 as i32 as i64,
                _ => {}
            }
            if p67 {
                return None;
            }
            ins.rm = Rm::Mem { base, index, disp };
        }
        // F7h has a constant only as TEST.
        if op == 0xf7 && ins.reg & 7 == 0 {
            imm = whole;
        }
    } else {
        ins.reg = (op as u8 & 7) | (rex & 1) << 3;
    }
    ins.imm = match imm {
        0 => 0,
        1 => next(1)? as u8 as i8 as i64,
        2 => next(2)? as u16 as i16 as i64,
        4 => next(4)? as u32 as i32 as i64,
        _ => next(8)? as i64,
    };
    ins.len = at;
    let end = rip.wrapping_add(at as u64);
    if relative {
        if let Rm::Mem { disp, .. } = &mut ins.rm {
            *disp = disp.wrapping_add(end as i64);
        }
    }
    // A jump's constant is how far from the instruction's end.
    if matches!(op, 0xe8 | 0xe9 | 0xeb | 0x70..=0x7f | 0x180..=0x18f) {
        ins.imm = end.wrapping_add(ins.imm as u64) as i64;
    }
    if sse {
        // Of the shifts by a constant: of a register, to the right without and with
        // the sign and to the left (fields 2, 4 and 6); of 64 bits not with the sign.
        let shift_ok = match op {
            0x171 | 0x172 => matches!(ins.rm, Rm::Reg(_)) && matches!(ins.reg & 7, 2 | 4 | 6),
            // (Fields 3 and 7 of 73h move the whole register by bytes.)
            0x173 => matches!(ins.rm, Rm::Reg(_)) && matches!(ins.reg & 7, 2 | 3 | 6 | 7),
            // (With 66h and a register it is no instruction.)
            0x116 => pre == 0 || matches!(ins.rm, Rm::Mem { .. }),
            _ => true,
        };
        return (sse_with(op, pre) && shift_ok).then_some(ins);
    }
    if f2 {
        return None;
    }
    // The A register and a constant: read as the same instruction in its general form
    // (81h with the kind in the ModRM byte's field; F7h with field 0 for TEST).
    if matches!(op, 0x05 | 0x0d | 0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d | 0xa9) {
        ins.reg = if op == 0xa9 { 0 } else { (op >> 3) as u8 };
        ins.op = if op == 0xa9 { 0xf7 } else { 0x81 };
        ins.rm = Rm::Reg(0);
    }
    let op = ins.op;
    let field = ins.reg & 7;
    let register = matches!(ins.rm, Rm::Reg(_));
    // An operand of 8 or 16 bits is translated for the arithmetic and the moves only.
    let small_ok = matches!(op, 0x01..=0x3b | 0x81 | 0x83 | 0x85 | 0x89 | 0x8b | 0xc1 | 0xc7 | 0xd1 | 0xf7 | 0x11e | 0x11f | 0x190..=0x19f | 0x90);
    let small = size < 32;
    // ADC and SBB are translated at 32 and 64 bits.
    let with_carry = matches!(op, 0x11 | 0x13 | 0x19 | 0x1b) || matches!(op, 0x81 | 0x83) && matches!(field, 2 | 3);
    // F3h is REP RET before a return, part of ENDBR64 before the hint 0F 1Eh, and
    // nothing before BSF and BSR on a CPU without TZCNT and LZCNT.
    let f3_ok = matches!(op, 0xc3 | 0x11e | 0x1bc | 0x1bd);
    // What is in the list above by its opcode but is not translated in this form.
    let refused = (small && !small_ok)
        || (small && with_carry)
        || (f3 && !f3_ok)
        || match op {
            // XCHG with R8.
            0x90 => rex & 1 != 0,
            // The one field of C7h that is MOV.
            0xc7 => field != 0,
            // LEA of a register is not an instruction.
            0x8d => register,
            // The bit tests of a register only (of memory the bit may lie outside
            // the operand); of the group, the four that are bit tests.
            0x1a3 | 0x1ab | 0x1b3 | 0x1bb => !register,
            0x1ba => !register || field < 4,
            // Of FFh: INC and DEC at 32 and 64 bits, CALL and JMP through the operand,
            // PUSH.
            0xff => !(matches!(field, 2 | 4) || field < 2 && !small || field == 6 && !small),
            // A hint is of memory.
            0x118 => register,
            // Of F7h: TEST with a constant and NOT; NEG of a register, and MUL, IMUL,
            // DIV and IDIV, at 32 and 64 bits.
            0xf7 => !(matches!(field, 0 | 2) || field == 3 && register && !small || field >= 4 && !small),
            // The shifts and rotations of a register, not through the carry; of 8 and
            // 16 bits the shifts by a constant only.
            0xc1 | 0xd1 | 0xd3 => !register || matches!(field, 2 | 3) || small && (op == 0xd3 || field < 2),
            _ => false,
        };
    if refused { None } else { Some(ins) }
}

/// What an instruction does to the flags, as far as a rotation before it needs to
/// know: a rotation changes two and leaves four, which the record cannot say, so it is
/// translated only where a later instruction of the block writes all six before
/// anything can read them.
enum Flags {
    /// Writes all six, whatever they were.
    All,
    /// ADC and SBB: reads the carry, then writes all six.
    Carry,
    /// Leaves them, or may leave them.
    Kept,
    /// Reads them, or leaves the block.
    Read,
}

fn flags_of(ins: &Ins) -> Flags {
    let field = ins.reg & 7;
    match ins.op {
        0x11 | 0x13 | 0x19 | 0x1b => Flags::Carry,
        0x81 | 0x83 if matches!(field, 2 | 3) => Flags::Carry,
        // INC and DEC keep the carry and write the rest: to what was before them
        // they are as ADC is, a reader of the carry and then a writer of all (their
        // record says the carry it was given).
        0xff if field < 2 => Flags::Carry,
        0xff if field == 6 => Flags::Kept,
        // UCOMIS and COMIS.
        0x12e | 0x12f => Flags::All,
        0x01..=0x3b | 0x81 | 0x83 | 0x85 | 0x1bc | 0x1bd | 0x69 | 0x6b | 0x1af => Flags::All,
        // TEST, NEG, MUL and IMUL write them; NOT, DIV and IDIV leave them.
        0xf7 if matches!(field, 0 | 3 | 4 | 5) => Flags::All,
        // A shift by a constant that is not zero; by CL it may be by nothing.
        0xc1 if field >= 4 && ins.imm as u32 & (if ins.size == 64 { 63 } else { 31 }) != 0 => Flags::All,
        0xd1 if field >= 4 => Flags::All,
        0x70..=0x7f | 0x180..=0x18f | 0xc3 | 0xe8 | 0xe9 | 0xeb | 0xff => Flags::Read,
        // CMOVcc and SETcc read them and go on.
        0x140..=0x14f | 0x190..=0x19f => Flags::Read,
        _ => Flags::Kept,
    }
}

/// What of the host's flags an instruction that writes all of x86's leaves behind for
/// a conditional jump to use.
fn host_flags_of(ins: &Ins) -> Option<HostFlags> {
    let field = ins.reg & 7;
    match ins.op {
        0x01 | 0x03 | 0x11 | 0x13 => Some(HostFlags::Add),
        0x19 | 0x1b | 0x29 | 0x2b | 0x39 | 0x3b => Some(HostFlags::Sub),
        0x09 | 0x0b | 0x21 | 0x23 | 0x31 | 0x33 | 0x85 => Some(HostFlags::Logic),
        0x81 | 0x83 => Some(match field {
            0 | 2 => HostFlags::Add,
            3 | 5 | 7 => HostFlags::Sub,
            _ => HostFlags::Logic,
        }),
        0xf7 if field == 0 => Some(HostFlags::Logic),
        0xf7 if field == 3 => Some(HostFlags::Sub),
        0xff if field < 2 => Some(HostFlags::IncDec),
        0x12e | 0x12f => Some(HostFlags::Float),
        _ => None,
    }
}

/// A record of the flags that is not written by its instruction but, if at all, by the
/// jump that follows it at once, on the ways where the flags are not dead (R014).
#[derive(Clone, Copy)]
struct Later {
    kind: u64,
    bits: u32,
    /// The register that holds the first operand, or the result.
    a: u8,
    b: Second,
}

#[derive(Clone, Copy)]
enum Second {
    None,
    Reg(u8),
    Imm(u64),
    /// TEST: the record is the AND of `a` and this register, or this constant.
    AndReg(u8),
    AndImm(u64),
}

/// What an instruction's record needs, if all of it is still in the guest's registers
/// when the instruction is done: an arithmetic or logic instruction of 32 or 64 bits
/// on a register with a register or a constant. Not where the two operands are one
/// register and the result has taken its place.
fn later_of(ins: &Ins) -> Option<Later> {
    if ins.size < 32 {
        return None;
    }
    let field = ins.reg & 7;
    // (the kind among the opcodes 00h to 3Fh, the register written or compared, the
    // other operand)
    let (kind, a, b) = match (ins.op, ins.rm) {
        (0x01 | 0x09 | 0x21 | 0x29 | 0x31 | 0x39, Rm::Reg(d)) => ((ins.op >> 3) as u8, d, Second::Reg(ins.reg)),
        (0x03 | 0x0b | 0x23 | 0x2b | 0x33 | 0x3b, Rm::Reg(s)) => ((ins.op >> 3) as u8, ins.reg, Second::Reg(s)),
        (0x85, Rm::Reg(d)) => (8, d, Second::Reg(ins.reg)),
        (0x81 | 0x83, Rm::Reg(d)) => (field, d, Second::Imm(ins.imm as u64)),
        (0xf7, Rm::Reg(d)) if field == 0 => (8, d, Second::Imm(ins.imm as u64)),
        _ => return None,
    };
    let same = matches!(b, Second::Reg(r) if r == a);
    let (kind, b) = match (kind, b) {
        (0, _) if !same => (REC_ADD_BY_RESULT, b),
        // (A register less itself is nothing less nothing by its flags too, so this
        // one would come out right; it is refused with the addition for plainness.)
        (5, _) if !same => (REC_SUB_BY_RESULT, b),
        (7, _) => (REC_SUB, b),
        (1 | 4 | 6, _) => (REC_LOGIC, Second::None),
        (8, Second::Reg(r)) => (REC_LOGIC, Second::AndReg(r)),
        (8, Second::Imm(v)) => (REC_LOGIC, Second::AndImm(v)),
        _ => return None,
    };
    Some(Later { kind, bits: ins.size, a, b })
}

/// Does a block go on past a conditional jump that the host's flags answer? Where
/// blocks do at all, the jump is not to the block's own start (that is a loop, whose
/// jump goes there itself and ends the block), and the block has room for an
/// instruction more.
fn goes_on(blocks_do: bool, block: u64, target: u64, room: usize) -> bool {
    blocks_do && target != block && room > 0
}

/// The reading on from an instruction that sets the flags, to find whether anything
/// can read them before they are written whole again.
struct Scan<'a> {
    m: &'a Mem,
    /// How many instructions more may be read before giving up.
    budget: usize,
    /// The lowest and one past the highest address read.
    span: (u64, u64),
    /// A conditional jump of the block was passed that the host's flags answer: the
    /// flags are dead as a record, but the host's are asked for.
    asked: bool,
    /// Where the block the question is asked for begins, and whether its blocks go
    /// on past a conditional jump (R018): the translator's own rule for which jumps
    /// end a block, which this reading must have the same.
    block: u64,
    goes_on: bool,
    /// The flags must be written whole inside the block itself: where each block is
    /// compared with the interpreter at its end (`--jit-check`), flags that are dead
    /// only by what a later block does are still compared. (Found by busybox's MD5,
    /// R026: a rotation was the 64th instruction of a block, and the addition that
    /// writes its flags over the first of the next.)
    within: bool,
}

impl Scan<'_> {
    /// Are the flags dead from `at` on: written whole on every way before anything
    /// reads them? `host` is what the host's flags can say for a conditional jump, and
    /// `room` how many instructions more the block the question is asked for may hold:
    /// a conditional jump inside that block which the host's flags answer does not
    /// read the record, and both its ways are followed. Anything not known, a return,
    /// a jump through an operand, or the budget run out: not dead.
    fn dead(&mut self, mut at: u64, mut host: Option<HostFlags>, mut room: usize, depth: u32) -> bool {
        loop {
            if self.budget == 0 {
                return false;
            }
            self.budget -= 1;
            let (code, have) = self.m.code_at(at);
            let Some(ins) = read(unsafe { std::slice::from_raw_parts(code, have) }, at) else {
                return false;
            };
            let next = at.wrapping_add(ins.len as u64);
            self.span = (self.span.0.min(at), self.span.1.max(next));
            let inside = room > 0;
            if self.within && !inside {
                return false;
            }
            room = room.saturating_sub(1);
            match flags_of(&ins) {
                Flags::All => return true,
                // In the block, with the host's flags saying what the carry is, it
                // reads no record; and after it the flags are its own.
                Flags::Carry => {
                    if !(inside && host.and_then(|h| condition(2, h)).is_some()) {
                        return false;
                    }
                    self.asked = true;
                    return true;
                }
                Flags::Kept => {
                    // A shift by CL or a rotation leaves the host's flags no longer
                    // what a jump would ask (and a rotation may end the block); so
                    // does a division, which tests its operands with them.
                    if matches!(ins.op, 0xc1 | 0xd1 | 0xd3 | 0xf7 | 0x1a3 | 0x1ab | 0x1b3 | 0x1bb | 0x1ba) || sse_spends_flags(ins.op) {
                        host = None;
                    }
                    at = next;
                }
                Flags::Read => match ins.op {
                    // CMOVcc and SETcc: in the block and answered by the host's flags
                    // they read no record, and the flags are as they were after them.
                    0x140..=0x14f | 0x190..=0x19f => {
                        if !(inside && host.and_then(|h| condition(ins.op as u8 & 15, h)).is_some()) {
                            return false;
                        }
                        self.asked = true;
                        at = next;
                    }
                    0x70..=0x7f | 0x180..=0x18f => {
                        // In the block and answered by the host's flags, it reads no
                        // record. Past it the host's flags are someone else's.
                        let answered = inside && host.and_then(|h| condition(ins.op as u8 & 15, h)).is_some();
                        if !answered || depth == 0 {
                            return false;
                        }
                        self.asked = true;
                        // The way taken is another block's from here.
                        if !self.dead(ins.imm as u64, None, 0, depth - 1) {
                            return false;
                        }
                        // The way not taken is this block's still, with the host's
                        // flags as they are, if the block goes on past the jump.
                        if goes_on(self.goes_on, self.block, ins.imm as u64, room) {
                            at = next;
                        } else {
                            return self.dead(next, None, 0, depth - 1);
                        }
                    }
                    // A jump or a call to a constant: on from there, in another block.
                    0xe8 | 0xe9 | 0xeb => {
                        at = ins.imm as u64;
                        host = None;
                        room = 0;
                    }
                    _ => return false,
                },
            }
        }
    }
}

/// The way taken of a conditional jump that its block goes on past: written after the
/// block's end.
struct Taken {
    /// Where the jump is in the block's code, to be given its distance.
    jump: usize,
    cond: Cond,
    /// Where it goes in the guest, and how many of the block's instructions were run
    /// when it is taken.
    target: u64,
    n: usize,
    /// The record that was left for the jump, if this way needs it.
    later: Option<Later>,
}

/// What kind of instruction last set the host's flags, so that a conditional jump
/// after it can use them as they are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HostFlags {
    /// Of an ADDS: the host's carry is x86's.
    Add,
    /// Of a SUBS: the host's carry is x86's turned over.
    Sub,
    /// Of an ANDS: carry and overflow are clear, as x86's are.
    Logic,
    /// Of an ADDS or SUBS of one for INC or DEC: zero, sign and overflow are x86's;
    /// the carry is not (x86's is the one from before, and is in the record).
    IncDec,
    /// Of a bit test: the host's carry is clear where x86's is set, as after a SUBS,
    /// and nothing else of x86's flags is in the host's (a bit test leaves them).
    Bit,
    /// Of a comparison of two floats, with "either is not a number" made 0111 (N Z C
    /// V): then each of x86's conditions on ZF, PF and CF is one of the host's
    /// (ADR 0015, point 4; the table there has two branches for equality, which the
    /// 0111 saves).
    Float,
}

/// How one of x86's sixteen conditions is asked of the host's flags.
enum Jump {
    If(Cond),
    Always,
    Never,
}

fn condition(cc: u8, flags: HostFlags) -> Option<Jump> {
    use Cond::*;
    use HostFlags::*;
    // By pairs: the odd one of each is the even one's opposite.
    let even = match (cc >> 1, flags) {
        (1, Bit) => Jump::If(Lo),
        (_, Bit) => return None,
        // After a comparison of floats OF and SF are clear, CF is "less or not
        // numbers", ZF "equal or not numbers", PF "not numbers".
        (0 | 4 | 6, Float) => Jump::Never,
        (1, Float) => Jump::If(Lt),
        (2 | 7, Float) => Jump::If(Eq),
        (3, Float) => Jump::If(Le),
        (5, Float) => Jump::If(Vs),
        // After INC and DEC the host's carry is not x86's, which they keep.
        (1 | 3, IncDec) => return None,
        (0, _) => Jump::If(Vs),                     // overflow
        (1, Sub) => Jump::If(Lo),                   // below: carry
        (1, Add) => Jump::If(Hs),
        (1, Logic) => Jump::Never,
        (2, _) => Jump::If(Eq),                     // equal
        (3, Sub) => Jump::If(Ls),                   // below or equal: carry or zero
        (3, Add) => return None,
        (3, Logic) => Jump::If(Eq),
        (4, _) => Jump::If(Mi),                     // sign
        (5, _) => return None,                      // parity: the host has none
        (6, _) => Jump::If(Lt),                     // less: sign is not overflow
        _ => Jump::If(Le),                          // less or equal
    };
    Some(match (even, cc & 1 != 0) {
        (j, false) => j,
        (Jump::If(c), true) => Jump::If(c.not()),
        (Jump::Always, true) => Jump::Never,
        (Jump::Never, true) => Jump::Always,
    })
}

struct Emit<'a> {
    w: Vec<u32>,
    /// Loads and stores of the guest's memory are ordered as x86 orders them: the
    /// process has more than one thread.
    ordered: bool,
    /// The segment the memory operand of the instruction being translated is
    /// relative to (`Ins::seg`); nothing for LEA, which only works an address out.
    seg: u8,
    flags: Option<HostFlags>,
    m: &'a Mem,
    /// Where the block begins: what is read to prove flags dead is to lie near it.
    rip: u64,
    /// Every record is to be written (`--jit-check` compares the flags).
    exact: bool,
    /// The instruction being translated need not write its record: its flags are dead.
    skip: bool,
    /// What was read to decide that, for all the instructions so far.
    seen: Option<(u64, u64)>,
    /// The record that the jump now to be translated writes on the ways that need it.
    later: Option<Later>,
    /// The instruction being translated must leave the host's flags as x86's would
    /// be: its record is written (something may read the flags), or a jump of the
    /// block will ask the host's.
    host: bool,
    /// The ways out to the interpreter that are written after the block's end: where
    /// the conditional jump to each is, the instruction it leaves to the interpreter,
    /// and how many of the block's instructions came before that one.
    bails: Vec<(usize, u64, usize)>,
    /// The instruction being translated: where it is, and how many of the block
    /// came before it (for a way out to the interpreter from inside it).
    now: (u64, usize),
}

impl Emit<'_> {
    /// Are the flags dead from `at` on (`Scan::dead`)? What is read to say so becomes
    /// part of what the block rests on, and must lie within a few pages of it.
    fn dead(&mut self, at: u64, host: Option<HostFlags>, room: usize) -> bool {
        let mut scan = Scan { m: self.m, budget: 48, span: (at, at), asked: false, block: self.rip, goes_on: !self.exact, within: self.exact };
        if !scan.dead(at, host, room, 2) {
            return false;
        }
        self.host = scan.asked;
        let (lo, hi) = scan.span;
        if lo < self.rip.saturating_sub(0x2000) || hi > self.rip.saturating_add(0x4000) {
            return false;
        }
        self.seen = Some(match self.seen {
            Some((a, b)) => (a.min(lo), b.max(hi)),
            None => (lo, hi),
        });
        true
    }

    fn put(&mut self, word: u32) {
        self.w.push(word);
    }

    /// The jump to the way out. Its address is in x24 as the way in left it, but for
    /// blocks with ordered accesses, which use x24 for an address: those load it from
    /// the `Cpu`. (T1 is free wherever a block leaves. Loading it always cost a
    /// compilation with one thread 1 to 2 in a hundred, R034.)
    fn way_out(&mut self) {
        if self.ordered {
            self.put(a64::mem_imm(Size::X, Op::Load, T1, CPU, offset_of!(Cpu, way_out) as u32));
            self.put(a64::br(T1));
        } else {
            self.put(a64::br(HOST));
        }
    }

    /// A load from the guest's memory at the address in `at`, or a store to it: plain,
    /// or, where memory is shared between threads, ordered as x86 orders it (ADR
    /// 0019, point D): the host's address worked out, then the load that sees what
    /// was released or the store that releases. Either way one instruction touches
    /// the memory, and a fault is at it.
    fn mem(&mut self, size: Size, op: Op, rt: u8, at: u8) {
        if !self.ordered {
            return self.put(a64::mem_reg(size, op, rt, BASE, at));
        }
        self.put(a64::arith(true, false, false, HOST, BASE, at, 0));
        match op {
            Op::Store => self.put(a64::stlr(size, rt, HOST)),
            Op::Load => self.put(a64::ldapr(size, rt, HOST)),
            Op::LoadSigned => {
                self.put(a64::ldapr(size, rt, HOST));
                self.put(a64::sxt(true, rt, rt, 8 << size as u32));
            }
        }
    }

    /// The same for a vector register. Ordered, it goes by way of T4 (and T5 for the
    /// upper half of all 128 bits): the host's ordered accesses are of general
    /// registers. A register loaded is not changed before its last load.
    fn fmem(&mut self, size: FpSize, load: bool, rt: u8, at: u8) {
        if !self.ordered {
            return self.put(a64::fp_mem(size, load, rt, BASE, at));
        }
        let part = if size == FpSize::S { Size::W } else { Size::X };
        let wide = size != FpSize::S;
        if !load {
            self.put(a64::fmov_from_fp(wide, T4, rt));
            if size == FpSize::Q {
                self.put(a64::high_to(T5, rt));
            }
        }
        self.put(a64::arith(true, false, false, HOST, BASE, at, 0));
        self.put(if load { a64::ldapr(part, T4, HOST) } else { a64::stlr(part, T4, HOST) });
        if size == FpSize::Q {
            self.put(a64::arith_imm(true, false, false, HOST, HOST, 8, false));
            self.put(if load { a64::ldapr(Size::X, T5, HOST) } else { a64::stlr(Size::X, T5, HOST) });
        }
        if load {
            self.put(a64::fmov_to_fp(wide, rt, T4));
            if size == FpSize::Q {
                self.put(a64::ins_high_from(rt, T5));
            }
        }
    }

    fn constant(&mut self, rd: u8, value: u64) {
        let mut out = [0; 4];
        let n = a64::mov_imm(rd, value, &mut out);
        self.w.extend(&out[..n]);
    }

    /// The register that holds a memory operand's address in the guest: one of the
    /// guest's own if the address is just that, else `T1`, made here.
    fn address(&mut self, base: Option<u8>, index: Option<(u8, u32)>, disp: i64) -> u8 {
        let at = self.address_in_segment(base, index, disp);
        if self.seg == 0 {
            return at;
        }
        // Relative to FS or GS: the segment's base is added (the thread's own data
        // and the stack protector's word are found so).
        let base = if self.seg == 1 { offset_of!(Cpu, fs_base) } else { offset_of!(Cpu, gs_base) };
        self.put(a64::mem_imm(Size::X, Op::Load, T5, CPU, base as u32));
        self.put(a64::arith(true, false, false, T1, at, T5, 0));
        T1
    }

    fn address_in_segment(&mut self, base: Option<u8>, index: Option<(u8, u32)>, disp: i64) -> u8 {
        let mut at = base;
        if let Some((i, scale)) = index {
            match at {
                Some(b) => self.put(a64::arith(true, false, false, T1, b, i, scale)),
                None if scale == 0 => {}
                None => self.put(a64::lsl(true, T1, i, scale)),
            }
            at = Some(if at.is_none() && scale == 0 { i } else { T1 });
        }
        let Some(at) = at else {
            self.constant(T1, disp as u64);
            return T1;
        };
        match disp {
            0 => return at,
            1..4096 => self.put(a64::arith_imm(true, false, false, T1, at, disp as u32, false)),
            -4095..0 => self.put(a64::arith_imm(true, true, false, T1, at, -disp as u32, false)),
            _ => {
                // (In T5: T0 may hold an operand brought down from AH or its like.)
                self.constant(T5, disp as u64);
                self.put(a64::arith(true, false, false, T1, at, T5, 0));
            }
        }
        T1
    }

    /// The record of the flags: the kind and the operands' width, the first operand
    /// (or the result), and the second if there is one.
    fn record(&mut self, kind: u64, bits: u32, a: u8, b: Option<u8>) {
        if self.skip {
            return;
        }
        self.put(a64::mov(true, REC_A, a));
        if let Some(b) = b {
            self.put(a64::mov(true, REC_B, b));
        }
        self.put(a64::movz(true, REC_KIND, (kind << 8 | bits as u64) as u16, 0));
    }

    /// Writes a record that was left for the jump to write.
    fn write_later(&mut self, l: Later) {
        match l.b {
            Second::None => self.put(a64::mov(true, REC_A, l.a)),
            Second::Reg(b) => {
                self.put(a64::mov(true, REC_A, l.a));
                self.put(a64::mov(true, REC_B, b));
            }
            Second::Imm(v) => {
                self.put(a64::mov(true, REC_A, l.a));
                self.constant(REC_B, v);
            }
            Second::AndReg(b) => self.put(a64::logic(true, Logic::And, REC_A, l.a, b)),
            Second::AndImm(v) => {
                self.constant(T3, v);
                self.put(a64::logic(true, Logic::And, REC_A, l.a, T3));
            }
        }
        self.put(a64::movz(true, REC_KIND, (l.kind << 8 | l.bits as u64) as u16, 0));
    }

    /// Must the record that was left for a conditional jump be written on its way to
    /// `rip`? Not if the flags are dead from there.
    fn way_needs(&mut self, rip: u64, later: Option<Later>) -> Option<Later> {
        later.filter(|_| !self.dead(rip, None, 0))
    }

    /// A way out of a conditional jump to `rip`, the block's instructions counted
    /// already: with the record that was left for the jump, if this way needs it.
    fn leave_way(&mut self, rip: u64, later: Option<Later>) {
        if let Some(l) = self.way_needs(rip, later) {
            self.write_later(l);
        }
        self.leave_counted(rip);
    }

    /// The record of an ADC or an SBB: its operands, and the carry that came in (in
    /// T4, as a number) above the kind.
    fn record_carry(&mut self, sub: bool, bits: u32, a: u8, b: u8) {
        if self.skip {
            return;
        }
        self.record(if sub { REC_SBB } else { REC_ADC }, bits, a, Some(b));
        self.put(a64::bfi(REC_KIND, T4, 16, 1));
    }

    /// The carry into T4 as a number, for an instruction that reads it where the
    /// host's flags do not say it. If the record says its carry was given (by INC, DEC
    /// or a bit test: the top of a loop whose carry came round through a DEC and a
    /// jump), that; if there is no record, RFLAGS has it; if the record is of an
    /// addition of 64 bits (a long run of code cut in two between an ADD and its
    /// ADC), the two added again; with any other record the instruction is the
    /// interpreter's. Nothing of the instruction is done yet when this is asked. The
    /// host's flags are spent.
    fn given_carry(&mut self) {
        let given = REC_CARRY_GIVEN.trailing_zeros();
        self.put(a64::ubfx(T4, REC_KIND, given, 1));
        let to_given = self.w.len();
        self.put(a64::nop());
        let to_none = self.w.len();
        self.put(a64::nop());
        self.put(a64::arith_imm(true, true, true, a64::ZR, REC_KIND, (REC_ADD << 8 | 64) as u32, false));
        let j = self.w.len();
        self.put(a64::b_cond(Cond::Ne, 0));
        let (at, n) = self.now;
        self.bail(&[j], at, n);
        self.put(a64::arith(true, false, true, a64::ZR, REC_A, REC_B, 0));
        self.put(a64::cset(false, T4, Cond::Hs));
        let over_one = self.w.len();
        self.put(a64::nop());
        self.w[to_none] = a64::cbz(true, false, REC_KIND, (self.w.len() - to_none) as i32);
        self.put(a64::mem_imm(Size::X, Op::Load, T4, CPU, offset_of!(Cpu, rflags) as u32));
        self.put(a64::ubfx(T4, T4, 0, 1));
        let over_two = self.w.len();
        self.put(a64::nop());
        self.w[to_given] = a64::cbz(true, true, T4, (self.w.len() - to_given) as i32);
        self.put(a64::ubfx(T4, REC_KIND, given + 1, 1));
        for over in [over_one, over_two] {
            self.w[over] = a64::b((self.w.len() - over) as i32);
        }
    }

    /// The carry into T4 as a number: as the host's flags say it, or as the record
    /// does.
    fn carry_in(&mut self) {
        match self.flags.and_then(|f| condition(2, f)) {
            Some(Jump::If(cond)) => self.put(a64::cset(false, T4, cond)),
            Some(Jump::Always) => self.put(a64::movz(false, T4, 1, 0)),
            Some(Jump::Never) => self.put(a64::movz(false, T4, 0, 0)),
            None => self.given_carry(),
        }
    }

    /// INC or DEC of `dst`: an addition or a subtraction of one whose record says
    /// that the carry is not its own but the one given (R043).
    fn inc_dec(&mut self, sub: bool, bits: u32, dst: Rm) {
        let wide = bits == 64;
        let (a, to, at) = match dst {
            Rm::Reg(d) => (d, d, None),
            Rm::Mem { base, index, disp } => {
                let at = self.address(base, index, disp);
                self.mem(size(bits), Op::Load, T2, at);
                (T2, T0, Some(at))
            }
        };
        if self.skip {
            // No record is wanted: the flags are dead after it, or a jump the
            // host's flags answer reads them and they are dead after that.
            self.put(a64::arith_imm(wide, sub, true, to, a, 1, false));
            if let Some(at) = at {
                self.mem(size(bits), Op::Store, to, at);
            }
            self.flags = Some(HostFlags::IncDec);
            return;
        }
        self.carry_in();
        self.put(a64::movz(true, T3, 1, 0));
        let kind = if sub { REC_SUB } else { REC_ADD };
        if at.is_none() {
            self.record(kind, bits, a, Some(T3));
        }
        self.put(a64::arith_imm(wide, sub, true, to, a, 1, false));
        if let Some(at) = at {
            self.mem(size(bits), Op::Store, to, at);
            self.record(kind, bits, T2, Some(T3));
        }
        let given = REC_CARRY_GIVEN.trailing_zeros();
        self.put(a64::bfi(REC_KIND, T3, given, 1));
        self.put(a64::bfi(REC_KIND, T4, given + 1, 1));
        self.flags = Some(HostFlags::IncDec);
    }

    /// A way out to the interpreter at `at`, the instruction now being translated,
    /// for the cases of it that are not done here (a division that would fault). `n`
    /// is how many instructions of the block came before it. Gives back where the
    /// jumps to it are to be written from: each is a place holding the condition's
    /// instruction with a distance of zero.
    fn bail(&mut self, jumps: &[usize], at: u64, n: usize) {
        // Over the way out, for the case that is done here.
        let over = self.w.len();
        self.put(a64::nop());
        let stub = self.w.len();
        for &j in jumps {
            // The distance goes into bits 5 to 23 of a CBZ, CBNZ or B.cond.
            self.w[j] |= ((stub - j) as u32 & 0x7_ffff) << 5;
        }
        self.bail_out(at, n);
        self.w[over] = a64::b((self.w.len() - over) as i32);
    }

    /// The way out itself. Not one that can be joined: the block that begins at `at`
    /// may be this one, and the instruction is the interpreter's this time whatever
    /// begins there.
    fn bail_out(&mut self, at: u64, n: usize) {
        self.put(a64::arith_imm(true, false, false, RAN, RAN, n as u32, false));
        self.put(a64::mem_imm(Size::X, Op::Store, a64::ZR, CPU, offset_of!(Cpu, left_from) as u32));
        self.constant(T0, at);
        self.put(a64::mem_imm(Size::X, Op::Store, T0, CPU, offset_of!(Cpu, rip) as u32));
        self.put(a64::movz(true, REASON, LEFT_FOR_THE_INTERPRETER as u16, 0));
        self.way_out();
    }

    /// A jump, if what the host's instruction made is not a number (the host's
    /// overflow flag after a comparison says so), to a way out written after the
    /// block's end: the instruction at `at` is the interpreter's then, which has
    /// x86's rules for it (ADR 0015, point 2).
    fn bail_if_unordered(&mut self, at: u64, n: usize) {
        self.bails.push((self.w.len(), at, n));
        self.put(a64::b_cond(Cond::Vs, 0));
    }

    /// The vector register that holds an SSE instruction's source operand: the
    /// guest's own, or memory of `size` read into `V1`.
    fn fsrc(&mut self, rm: Rm, size: FpSize) -> u8 {
        match rm {
            Rm::Reg(s) => s,
            Rm::Mem { base, index, disp } => {
                let a = self.address(base, index, disp);
                self.fmem(size, true, V1, a);
                V1
            }
        }
    }

    /// One SSE instruction (ADR 0015). `at` is where it is and `n` how many of the
    /// block's instructions came before it. The guest's XMM registers are the host's
    /// v0 to v15; none is changed before the instruction's load, and none at all on
    /// the way that leaves it to the interpreter.
    fn sse(&mut self, ins: &Ins, at: u64, n: usize) {
        let (reg, pre, wide) = (ins.reg, ins.pre, ins.size == 64);
        // The element of a scalar instruction: F2h is a double, F3h a single.
        let double = pre == 3;
        let low = if double { FpSize::D } else { FpSize::S };
        match ins.op {
            // Into a register: all of it, or the low element (from a register the
            // rest of the one written is kept; from memory it is zero).
            0x110 | 0x128 | 0x16f => match (ins.rm, ins.op == 0x110 && pre >= 2) {
                (Rm::Reg(s), false) => self.put(a64::vlogic(VLogic::Orr, reg, s, s)),
                (Rm::Reg(s), true) => self.put(a64::ins_low(double, reg, s)),
                (Rm::Mem { base, index, disp }, scalar) => {
                    let a = self.address(base, index, disp);
                    self.fmem(if scalar { low } else { FpSize::Q }, true, reg, a);
                }
            },
            // And out of one.
            0x111 | 0x129 | 0x17f => match (ins.rm, ins.op == 0x111 && pre >= 2) {
                (Rm::Reg(d), false) => self.put(a64::vlogic(VLogic::Orr, d, reg, reg)),
                (Rm::Reg(d), true) => self.put(a64::ins_low(double, d, reg)),
                (Rm::Mem { base, index, disp }, scalar) => {
                    let a = self.address(base, index, disp);
                    self.fmem(if scalar { low } else { FpSize::Q }, false, reg, a);
                }
            },
            // Into the upper half, the lower kept: from memory, or another's lower.
            0x116 => {
                let b = self.fsrc(ins.rm, FpSize::D);
                self.put(a64::ins_high(reg, b));
            }
            // MOVD and MOVQ from a general register or memory: the rest is zero.
            0x16e => match ins.rm {
                Rm::Reg(s) => self.put(a64::fmov_to_fp(wide, reg, s)),
                Rm::Mem { base, index, disp } => {
                    let a = self.address(base, index, disp);
                    self.fmem(if wide { FpSize::D } else { FpSize::S }, true, reg, a);
                }
            },
            // MOVD and MOVQ to a general register or memory (66h).
            0x17e if pre == 1 => match ins.rm {
                Rm::Reg(d) => self.put(a64::fmov_from_fp(wide, d, reg)),
                Rm::Mem { base, index, disp } => {
                    let a = self.address(base, index, disp);
                    self.fmem(if wide { FpSize::D } else { FpSize::S }, false, reg, a);
                }
            },
            // MOVQ between XMM registers or from memory (F3h): the upper half zero.
            0x17e => match ins.rm {
                Rm::Reg(s) => self.put(a64::fmov(true, reg, s)),
                Rm::Mem { base, index, disp } => {
                    let a = self.address(base, index, disp);
                    self.fmem(FpSize::D, true, reg, a);
                }
            },
            // MOVQ to an XMM register (its upper half zero) or to memory.
            0x1d6 => match ins.rm {
                Rm::Reg(d) => self.put(a64::fmov(true, d, reg)),
                Rm::Mem { base, index, disp } => {
                    let a = self.address(base, index, disp);
                    self.fmem(FpSize::D, false, reg, a);
                }
            },
            // AND, ANDN (not the register, and the operand), OR, XOR of all 128 bits.
            0x154..=0x157 | 0x1db | 0x1df | 0x1eb | 0x1ef => {
                let b = self.fsrc(ins.rm, FpSize::Q);
                self.put(match ins.op {
                    0x154 | 0x1db => a64::vlogic(VLogic::And, reg, reg, b),
                    0x155 | 0x1df => a64::vlogic(VLogic::Bic, reg, b, reg),
                    0x156 | 0x1eb => a64::vlogic(VLogic::Orr, reg, reg, b),
                    _ => a64::vlogic(VLogic::Eor, reg, reg, b),
                });
            }
            // The arithmetic of the low element: the host's, where what it makes is
            // a number.
            0x151 | 0x158 | 0x159 | 0x15c | 0x15e => {
                let b = self.fsrc(ins.rm, low);
                self.put(match ins.op {
                    0x151 => a64::fsqrt(double, V0, b),
                    0x158 => a64::fp_arith(double, Fp::Add, V0, reg, b),
                    0x159 => a64::fp_arith(double, Fp::Mul, V0, reg, b),
                    0x15c => a64::fp_arith(double, Fp::Sub, V0, reg, b),
                    _ => a64::fp_arith(double, Fp::Div, V0, reg, b),
                });
                self.put(a64::fcmp(double, V0, V0));
                self.bail_if_unordered(at, n);
                self.put(a64::ins_low(double, reg, V0));
            }
            // The smaller and the larger: a comparison and a choice, which is x86's
            // rule in every case (R019).
            0x15d | 0x15f => {
                let b = self.fsrc(ins.rm, low);
                self.put(a64::fcmp(double, reg, b));
                self.put(a64::fcsel(double, V0, reg, b, if ins.op == 0x15d { Cond::Mi } else { Cond::Gt }));
                self.put(a64::ins_low(double, reg, V0));
            }
            // UCOMIS and COMIS (66h: doubles). The host's flags are the comparison's
            // if both are numbers, else 0111: three instructions and no jump.
            0x12e | 0x12f => {
                let double = pre == 1;
                let b = self.fsrc(ins.rm, if double { FpSize::D } else { FpSize::S });
                self.put(a64::fcmp(double, reg, reg));
                self.put(a64::fccmp(double, b, b, 0b0111, Cond::Vc));
                self.put(a64::fccmp(double, reg, b, 0b0111, Cond::Vc));
                if !self.skip {
                    self.put(a64::fmov_from_fp(double, REC_A, reg));
                    self.put(a64::fmov_from_fp(double, REC_B, b));
                    let bits = if double { 64 } else { 32 };
                    self.put(a64::movz(true, REC_KIND, (REC_FLOAT << 8 | bits) as u16, 0));
                }
                self.flags = Some(HostFlags::Float);
            }
            // An integer of a general register or memory to a float: exact on both.
            0x12a => {
                let src = self.operand(ins, ins.rm);
                self.put(a64::scvtf(wide, double, V0, src));
                self.put(a64::ins_low(double, reg, V0));
            }
            // A float to an integer, cut off. The host gives the largest or the
            // smallest integer for what does not fit and zero for what is not a number,
            // where x86 gives the smallest for all three; so that is chosen here, with
            // no way out.
            0x12c if wide => {
                let b = self.fsrc(ins.rm, low);
                self.put(a64::fcmp(double, b, b));
                self.put(a64::fcvtzs(true, double, T0, b));
                // Overflow set: not a number, or the largest (and one overflows),
                // which no float is: it is what the host gives for one too large.
                self.put(a64::ccmn_imm(true, T0, 1, 0b0001, Cond::Vc));
                self.put(a64::movz(true, T2, 0x8000, 48));
                self.put(a64::csel(true, reg, T2, T0, Cond::Vs));
            }
            // To 32 bits, by way of 64 (the largest of 32 bits is a number a double
            // can be): it must be its own lower half's extension, and a number.
            0x12c => {
                let b = self.fsrc(ins.rm, low);
                self.put(a64::fcvtzs(true, double, T0, b));
                self.put(a64::sxt(true, T2, T0, 32));
                self.put(a64::arith(true, true, true, a64::ZR, T2, T0, 0));
                // Equal still, if it fits and compares equal to itself.
                self.put(a64::fccmp(double, b, b, 0b0000, Cond::Eq));
                self.put(a64::movz(false, T2, 0x8000, 16));
                self.put(a64::csel(false, reg, T0, T2, Cond::Eq));
            }
            // A single to a double (F3h) and a double to a single: the same test as
            // the arithmetic's, since x86 makes what is not a number quiet its own way.
            0x15a => {
                let to_double = pre == 2;
                let b = self.fsrc(ins.rm, if to_double { FpSize::S } else { FpSize::D });
                self.put(a64::fcvt(to_double, V0, b));
                self.put(a64::fcmp(to_double, V0, V0));
                self.bail_if_unordered(at, n);
                self.put(a64::ins_low(to_double, reg, V0));
            }
            // Element by element, and the layings side by side: one instruction of
            // the host's each. PUNPCKL is ZIP1 and PUNPCKH is ZIP2.
            0x160..=0x162 | 0x164..=0x166 | 0x168..=0x16a | 0x16c | 0x16d | 0x174..=0x176 | 0x1d4 | 0x1f8..=0x1fe => {
                let b = self.fsrc(ins.rm, FpSize::Q);
                let (op, lane) = match ins.op {
                    0x160..=0x162 => (V3::Zip1, ins.op - 0x160),
                    0x164..=0x166 => (V3::Cmgt, ins.op - 0x164),
                    0x168..=0x16a => (V3::Zip2, ins.op - 0x168),
                    0x16c => (V3::Zip1, 3),
                    0x16d => (V3::Zip2, 3),
                    0x174..=0x176 => (V3::Cmeq, ins.op - 0x174),
                    0x1d4 => (V3::Add, 3),
                    0x1f8..=0x1fb => (V3::Sub, ins.op - 0x1f8),
                    _ => (V3::Add, ins.op - 0x1fc),
                };
                self.put(a64::vec3(op, lane_of(lane), reg, reg, b));
            }
            // PMULUDQ: the 64-bit products of the lower halves of the two 64-bit
            // elements of each.
            0x1f4 => {
                let b = self.fsrc(ins.rm, FpSize::Q);
                self.put(a64::xtn_2d(V0, reg));
                self.put(a64::xtn_2d(V1, b));
                self.put(a64::umull_2d(reg, V0, V1));
            }
            // PSHUFD: each element of the register is the operand's element that two
            // bits of the constant name.
            0x170 => {
                let b = self.fsrc(ins.rm, FpSize::Q);
                let pick = |i: u32| ins.imm as u32 >> (2 * i) & 3;
                if (1..4).all(|i| pick(i) == pick(0)) {
                    self.put(a64::dup_s(reg, b, pick(0)));
                } else {
                    // Into another register if the operand is the one written.
                    let to = if b == reg { V0 } else { reg };
                    for i in 0..4 {
                        self.put(a64::ins_s(to, i, b, pick(i)));
                    }
                    if to != reg {
                        self.put(a64::vlogic(VLogic::Orr, reg, to, to));
                    }
                }
            }
            // SHUFPS: two elements of the register named by the constant, then two
            // of the operand. The odd ones of both and the even ones of both are one
            // instruction of the host's.
            0x1c6 => {
                let b = self.fsrc(ins.rm, FpSize::Q);
                let pick = |i: u32| ins.imm as u32 >> (2 * i) & 3;
                match ins.imm as u8 {
                    0xdd => self.put(a64::vec3(V3::Uzp2, Lane::S, reg, reg, b)),
                    0x88 => self.put(a64::vec3(V3::Uzp1, Lane::S, reg, reg, b)),
                    _ => {
                        for i in 0..4 {
                            self.put(a64::ins_s(V0, i, if i < 2 { reg } else { b }, pick(i)));
                        }
                        self.put(a64::vlogic(VLogic::Orr, reg, V0, V0));
                    }
                }
            }
            // The shifts of every element by a constant. By the element's width or
            // more, x86 gives nothing (or, with the sign, the sign everywhere); the
            // host's instruction has no such count.
            0x171..=0x173 => {
                let Rm::Reg(d) = ins.rm else { unreachable!("a shift of memory was read") };
                let lane = lane_of(ins.op - 0x170);
                let bits = 8u32 << lane as u32;
                let by = ins.imm as u8 as u32;
                match (ins.reg & 7, by) {
                    (_, 0) => {}
                    // The whole register by bytes: to the right (3) what comes in
                    // at the top is nothing, to the left (7) at the bottom.
                    (3 | 7, 16..) => self.put(a64::vlogic(VLogic::Eor, d, d, d)),
                    (3, _) => {
                        self.put(a64::vlogic(VLogic::Eor, V0, V0, V0));
                        self.put(a64::ext(d, d, V0, by));
                    }
                    (7, _) => {
                        self.put(a64::vlogic(VLogic::Eor, V0, V0, V0));
                        self.put(a64::ext(d, V0, d, 16 - by));
                    }
                    (4, _) => self.put(a64::vshift(VShift::Sshr, lane, d, d, by.min(bits - 1))),
                    (_, _) if by >= bits => self.put(a64::vlogic(VLogic::Eor, d, d, d)),
                    (2, _) => self.put(a64::vshift(VShift::Ushr, lane, d, d, by)),
                    _ => self.put(a64::vshift(VShift::Shl, lane, d, d, by)),
                }
            }
            _ => unreachable!("an SSE opcode that is read and not translated"),
        }
        if sse_spends_flags(ins.op) {
            self.flags = None;
        }
    }

    /// MUL (unsigned) and IMUL with one operand: the A register by `b`, the product's
    /// lower half to the A register and its upper half to the D register.
    fn mul_wide(&mut self, signed: bool, wide: bool, b: u8) {
        self.record(if signed { REC_IMUL } else { REC_MUL }, if wide { 64 } else { 32 }, 0, Some(b));
        if wide {
            self.put(a64::mulh(signed, T4, 0, b));
            self.put(a64::mul(true, T5, 0, b));
            self.put(a64::mov(true, 0, T5));
            self.put(a64::mov(true, 2, T4));
        } else {
            self.put(a64::mull(signed, T4, 0, b));
            self.put(a64::mov(false, 0, T4));
            self.put(a64::lsr(true, 2, T4, 32));
        }
        self.flags = None;
    }

    /// DIV (unsigned) and IDIV: the D and A registers together by `b`, the quotient
    /// to the A register and the remainder to the D register. Where x86 would fault
    /// (a divisor of zero, a quotient that does not fit), and where the dividend's
    /// upper half is not just the lower's extension so that the host's division
    /// cannot take it, the instruction is left to the interpreter.
    fn div_wide(&mut self, signed: bool, wide: bool, b: u8, at: u64, n: usize) {
        let mut out = Vec::new();
        let mut jump = |e: &mut Emit, word: u32| {
            out.push(e.w.len());
            e.put(word);
        };
        if wide {
            jump(self, a64::cbz(true, false, b, 0));
            if signed {
                // The upper half must be the lower's sign, and the divisor not -1
                // (the one quotient of 64 bits by 64 that does not fit is by -1).
                self.put(a64::cmp_asr(2, 0, 63));
                jump(self, a64::b_cond(Cond::Ne, 0));
                self.put(a64::arith_imm(true, false, true, a64::ZR, b, 1, false));
                jump(self, a64::b_cond(Cond::Eq, 0));
            } else {
                jump(self, a64::cbz(true, true, 2, 0));
            }
            self.put(a64::div(true, signed, T4, 0, b));
            self.put(a64::msub(true, T5, T4, b, 0));
            self.put(a64::mov(true, 0, T4));
            self.put(a64::mov(true, 2, T5));
        } else {
            // EDX:EAX as one 64-bit number in T4, the divisor as 64 bits in T1.
            self.put(a64::mov(false, T5, 0));
            self.put(a64::orr_lsl(T4, T5, 2, 32));
            self.put(if signed { a64::sxt(true, T1, b, 32) } else { a64::mov(false, T1, b) });
            jump(self, a64::cbz(true, false, T1, 0));
            self.put(a64::div(true, signed, T2, T4, T1));
            // The quotient must fit 32 bits.
            if signed {
                self.put(a64::sxt(true, T3, T2, 32));
                self.put(a64::arith(true, true, true, a64::ZR, T3, T2, 0));
                jump(self, a64::b_cond(Cond::Ne, 0));
            } else {
                self.put(a64::lsr(true, T3, T2, 32));
                jump(self, a64::cbz(true, true, T3, 0));
            }
            self.put(a64::msub(true, T3, T2, T1, T4));
            self.put(a64::mov(false, 0, T2));
            self.put(a64::mov(false, 2, T3));
        }
        self.bail(&out, at, n);
        // The tests above used the host's flags.
        self.flags = None;
    }

    /// Is this a byte register that is the second byte of a register (AH, CH, DH,
    /// BH)? Then it is the register four numbers lower, from bit 8.
    fn high(ins: &Ins, r: u8) -> bool {
        ins.size == 8 && !ins.rex && (4..8).contains(&r)
    }

    /// The register that holds an 8 or 16-bit register's value in its low bits (what
    /// is above them is not to be used): the register itself, or for AH and its like
    /// the byte brought down into `to`.
    fn small(&mut self, ins: &Ins, r: u8, to: u8) -> u8 {
        if Self::high(ins, r) {
            self.put(a64::ubfx(to, r - 4, 8, 8));
            to
        } else {
            r
        }
    }

    /// Writes the low bits of `from` to an 8 or 16-bit register, leaving the rest of
    /// it.
    fn set_small(&mut self, ins: &Ins, r: u8, from: u8) {
        if Self::high(ins, r) {
            self.put(a64::bfi(r - 4, from, 8, 8));
        } else {
            self.put(a64::bfi(r, from, 0, ins.size));
        }
    }

    /// One of ADD, OR, AND, SUB, XOR, CMP or TEST (8) on operands of 8 or 16 bits:
    /// `dst` and the register `src`, whose low bits are the operand. The host's flags
    /// are those of the same operation on the operands moved to the top of a 32-bit
    /// register, where the carry out and the overflow are the small operation's.
    fn alu_small(&mut self, kind: u8, ins: &Ins, dst: Rm, src: u8) {
        let bits = ins.size;
        let up = 32 - bits;
        let small = if bits == 8 { Size::B } else { Size::H };
        let (a, at) = match dst {
            Rm::Reg(d) => (self.small(ins, d, T2), None),
            Rm::Mem { base, index, disp } => {
                let at = self.address(base, index, disp);
                self.mem(small, Op::Load, T2, at);
                (T2, Some(at))
            }
        };
        let keeps = !matches!(kind, 7 | 8);
        let arith = matches!(kind, 0 | 5 | 7);
        if arith {
            let sub = kind != 0;
            self.put(a64::lsl(false, T4, a, up));
            self.put(a64::lsl(false, T5, src, up));
            self.put(a64::arith(false, sub, true, T4, T4, T5, 0));
            if keeps {
                self.put(a64::lsr(false, T4, T4, up));
            }
            self.flags = Some(if sub { HostFlags::Sub } else { HostFlags::Add });
        } else {
            let op = match kind {
                1 => Logic::Orr,
                6 => Logic::Eor,
                _ => Logic::And,
            };
            self.put(a64::logic(false, op, T4, a, src));
            self.put(a64::lsl(false, T5, T4, up));
            self.put(a64::logic(false, Logic::Ands, a64::ZR, T5, T5));
            self.flags = Some(HostFlags::Logic);
        }
        // To memory: the store first, then the record (the old value is still in T2).
        // To a register: the record first, since the register is an operand.
        let kind_of = match kind {
            0 => REC_ADD,
            5 | 7 => REC_SUB,
            _ => REC_LOGIC,
        };
        let (rec_a, rec_b) = if arith { (a, Some(src)) } else { (T4, None) };
        match (dst, at) {
            (_, Some(at)) => {
                if keeps {
                    self.mem(small, Op::Store, T4, at);
                }
                self.record(kind_of, bits, rec_a, rec_b);
            }
            (Rm::Reg(d), None) => {
                self.record(kind_of, bits, rec_a, rec_b);
                if keeps {
                    self.set_small(ins, d, T4);
                }
            }
            _ => unreachable!(),
        }
    }

    /// One of ADD, OR, AND, SUB, XOR, CMP (by its number among the opcodes 00h to
    /// 3Fh) or TEST (8), of `dst` and the register `src`, with its flags.
    fn alu(&mut self, kind: u8, ins: &Ins, dst: Rm, src: u8) {
        if ins.size < 32 {
            return self.alu_small(kind, ins, dst, src);
        }
        let wide = ins.size == 64;
        let bits = ins.size;
        // The value worked on is the register itself, or memory read into T2; the
        // result goes to the register, or to T0 and from there to memory.
        let (a, to, at) = match dst {
            Rm::Reg(d) => (d, d, None),
            Rm::Mem { base, index, disp } => {
                let at = self.address(base, index, disp);
                self.mem(size(bits), Op::Load, T2, at);
                (T2, T0, Some(at))
            }
        };
        let keeps = !matches!(kind, 7 | 8);
        match kind {
            2 | 3 => {
                // ADC and SBB. The carry as the host's flags say it (the caller has
                // seen that they do) goes into T4 as a number, for the record, and
                // from there into the host's carry as ADCS or SBCS wants it: x86's
                // for an addition, its opposite for a subtraction.
                let sub = kind == 3;
                self.carry_in();
                if at.is_none() {
                    self.record_carry(sub, bits, a, src);
                }
                if sub {
                    self.put(a64::arith(true, true, true, a64::ZR, a64::ZR, T4, 0));
                } else {
                    self.put(a64::arith_imm(false, true, true, a64::ZR, T4, 1, false));
                }
                self.put(a64::with_carry(wide, sub, to, a, src));
                self.flags = Some(if sub { HostFlags::Sub } else { HostFlags::Add });
            }
            0 | 5 | 7 => {
                let sub = kind != 0;
                // A register's old value is recorded before it is written over.
                if at.is_none() {
                    self.record(if sub { REC_SUB } else { REC_ADD }, bits, a, Some(src));
                }
                self.put(a64::arith(wide, sub, true, if keeps { to } else { a64::ZR }, a, src, 0));
                self.flags = Some(if sub { HostFlags::Sub } else { HostFlags::Add });
            }
            4 => self.put(a64::logic(wide, Logic::Ands, to, a, src)),
            8 => self.put(a64::logic(wide, Logic::Ands, if self.skip { a64::ZR } else { REC_A }, a, src)),
            _ => {
                self.put(a64::logic(wide, if kind == 1 { Logic::Orr } else { Logic::Eor }, to, a, src));
                // The host has no OR or XOR that sets its flags: a test of the result,
                // where the flags are asked for.
                if self.host {
                    self.put(a64::logic(wide, Logic::Ands, a64::ZR, to, to));
                }
            }
        }
        if let Some(at) = at {
            if keeps {
                self.mem(size(bits), Op::Store, to, at);
            }
            // Memory's old value is still in T2: recorded when the store is done.
            if matches!(kind, 2 | 3) {
                self.record_carry(kind == 3, bits, T2, src);
            }
            if matches!(kind, 0 | 5 | 7) {
                self.record(if kind == 0 { REC_ADD } else { REC_SUB }, bits, T2, Some(src));
            }
        }
        if matches!(kind, 1 | 4 | 6 | 8) {
            if !self.skip {
                if kind != 8 {
                    self.put(a64::mov(true, REC_A, to));
                }
                self.put(a64::movz(true, REC_KIND, (REC_LOGIC << 8 | bits as u64) as u16, 0));
            }
            self.flags = Some(HostFlags::Logic);
        }
    }

    /// A way out to a constant address, after `n` instructions of this block: a jump
    /// that goes at first to the code after it, which leaves for Rust saying which
    /// jump it was; Rust rewrites the jump to go to the block at that address once
    /// there is one, and from then on this is one jump.
    fn leave(&mut self, rip: u64, n: usize) {
        self.put(a64::arith_imm(true, false, false, RAN, RAN, n as u32, false));
        self.leave_counted(rip);
    }

    /// The same, the block's instructions having been counted already.
    fn leave_counted(&mut self, rip: u64) {
        self.put(a64::b(1));
        self.put(a64::adr(T1, -4));
        self.put(a64::mem_imm(Size::X, Op::Store, T1, CPU, offset_of!(Cpu, left_from) as u32));
        self.constant(T0, rip);
        self.out();
    }

    /// A way out with RIP in `T0`, which is not known until it is run: nothing to
    /// join. The address is looked up in the table of blocks (R011): its low bits
    /// choose a place, and if the place holds this address the code beside it is gone
    /// to. Else it is the way out to Rust, which fills the place when it has found the
    /// block.
    fn leave_at_t0(&mut self, n: usize) {
        self.put(a64::arith_imm(true, false, false, RAN, RAN, n as u32, false));
        self.put(a64::ubfx(T1, T0, 0, TABLE_BITS));
        self.put(a64::arith(true, false, false, T1, TABLE, T1, 4));
        self.put(a64::mem_imm(Size::X, Op::Load, T2, T1, 0));
        self.put(a64::mem_imm(Size::X, Op::Load, T3, T1, 8));
        self.put(a64::arith(true, true, true, a64::ZR, T2, T0, 0));
        self.put(a64::b_cond(Cond::Ne, 2));
        self.put(a64::br(T3));
        self.put(a64::mem_imm(Size::X, Op::Store, a64::ZR, CPU, offset_of!(Cpu, left_from) as u32));
        self.out();
    }

    fn out(&mut self) {
        self.put(a64::mem_imm(Size::X, Op::Store, T0, CPU, offset_of!(Cpu, rip) as u32));
        self.put(a64::movz(true, REASON, LEFT_AT_END as u16, 0));
        self.way_out();
    }
}

fn lane_of(n: u16) -> Lane {
    match n {
        0 => Lane::B,
        1 => Lane::H,
        2 => Lane::S,
        _ => Lane::D,
    }
}

fn size(bits: u32) -> Size {
    match bits {
        8 => Size::B,
        16 => Size::H,
        32 => Size::W,
        _ => Size::X,
    }
}

/// Translates the block that begins at `rip`. `None` if its first instruction is not
/// one that is translated. With `exact` every record of the flags is written, whether
/// or not anything can read them.
#[cfg(test)]
pub fn translate(m: &Mem, rip: u64, exact: bool) -> Option<Built> {
    translate_as(m, rip, exact, false)
}

/// The same; with `ordered`, for a process that has more than one thread: every load
/// and store of the guest's memory keeps x86's order between threads.
pub fn translate_as(m: &Mem, rip: u64, exact: bool, ordered: bool) -> Option<Built> {
    let mut e = Emit { w: Vec::new(), ordered, seg: 0, flags: None, m, rip, exact, skip: false, seen: None, later: None, host: true, bails: Vec::new(), now: (0, 0) };
    let mut starts = Vec::new();
    let mut taken: Vec<Taken> = Vec::new();
    let mut sse = false;
    let mut at = rip;
    // A block begins by looking whether a signal waits for the thread (ADR 0019,
    // point 2): if one does, it is left at once for the interpreter, before its
    // first instruction. A loop that jumps to its own start looks each time round.
    // Only a block that a cycle of blocks must pass through keeps the look (R037):
    // one with a jump or a call to an address not after itself, a return, or a jump
    // through an operand. Whatever goes round for ever goes through one of those.
    let mut cycle = false;
    // The look is one instruction: the count's top bit, which the way in sets from
    // the thread's word and the host's handler sets in a thread it interrupts here.
    e.bails.push((0, rip, 0));
    e.put(a64::tbnz_top(RAN, 0));
    loop {
        let (code, have) = m.code_at(at);
        let bytes = unsafe { std::slice::from_raw_parts(code, have) };
        let ins = if starts.len() < MOST { read(bytes, at) } else { None };
        // Not translated, or enough: the block ends before it.
        let Some(ins) = ins else {
            if starts.is_empty() {
                return None;
            }
            e.leave(at, starts.len());
            break;
        };
        let before = e.w.len();
        e.seg = if ins.op == 0x8d { 0 } else { ins.seg };
        cycle |= match ins.op {
            0xc3 | 0xff => true,
            0xe8 | 0xe9 | 0xeb | 0x70..=0x7f | 0x180..=0x18f => ins.imm as u64 <= at,
            _ => false,
        };
        starts.push((e.w.len() as u32, at));
        e.now = (at, starts.len() - 1);
        let next = at.wrapping_add(ins.len as u64);
        let (bits, reg, field) = (ins.size, ins.reg, ins.reg & 7);
        let (wide, small) = (bits == 64, bits < 32);
        let mut ended = false;
        // An instruction that is read but cannot be translated where it stands (the
        // host's flags do not say what a jump asks; a rotation's flags may be read):
        // the block ends before it and it is the interpreter's.
        let mut not_here = false;
        // The record of the flags is not written by an instruction after which they
        // are dead. (A shift by CL may leave them: if they are dead after it, its own
        // record is not needed either.)
        let room = MOST - starts.len();
        let sets = matches!(flags_of(&ins), Flags::All | Flags::Carry) || ins.op == 0xd3 && field >= 4;
        e.host = true;
        e.skip = !e.exact && sets && e.dead(next, host_flags_of(&ins), room);
        if !e.skip {
            e.host = true;
        }
        // Not dead, but read at once by a jump that the host's flags answer, with all
        // that a record needs still in the registers: the jump writes it, where needed.
        let later = e.later.take();
        if !e.exact && !e.skip && room > 0 {
            if let (Some(l), Some(host)) = (later_of(&ins), host_flags_of(&ins)) {
                let (code, have) = m.code_at(next);
                let jump = read(unsafe { std::slice::from_raw_parts(code, have) }, next);
                let answered = jump.is_some_and(|j| {
                    matches!(j.op, 0x70..=0x7f | 0x180..=0x18f) && condition(j.op as u8 & 15, host).is_some()
                });
                if answered {
                    e.skip = true;
                    e.host = true;
                    e.later = Some(l);
                }
            }
        }
        match ins.op {
            0x01 | 0x09 | 0x11 | 0x19 | 0x21 | 0x29 | 0x31 | 0x39 => {
                let src = if small { e.small(&ins, reg, T0) } else { reg };
                e.alu((ins.op >> 3) as u8, &ins, ins.rm, src);
            }
            0x03 | 0x0b | 0x13 | 0x1b | 0x23 | 0x2b | 0x33 | 0x3b => {
                let src = e.operand(&ins, ins.rm);
                e.alu((ins.op >> 3) as u8, &ins, Rm::Reg(reg), src);
            }
            0x85 => {
                let src = if small { e.small(&ins, reg, T0) } else { reg };
                e.alu(8, &ins, ins.rm, src);
            }
            0x81 | 0x83 => match ins.rm {
                // ADD, SUB and CMP of a register and a constant the host's instruction
                // can carry, where no record is written here (which would want the
                // constant in a register).
                Rm::Reg(d) if !small && e.skip && matches!(field, 0 | 5 | 7) && (0..4096).contains(&ins.imm) => {
                    let to = if field == 7 { a64::ZR } else { d };
                    e.put(a64::arith_imm(wide, field != 0, true, to, d, ins.imm as u32, false));
                    e.flags = Some(if field == 0 { HostFlags::Add } else { HostFlags::Sub });
                }
                _ => {
                    // The constant in T3: an address may need T1 and T5, a value T2.
                    e.constant(T3, ins.imm as u64);
                    e.alu(field, &ins, ins.rm, T3);
                }
            },
            0xf7 if field == 0 => {
                e.constant(T3, ins.imm as u64);
                e.alu(8, &ins, ins.rm, T3);
            }
            0xf7 if field == 2 => match ins.rm {
                // NOT: no flags.
                Rm::Reg(d) if small => {
                    let a = e.small(&ins, d, T2);
                    e.put(a64::mvn(false, T4, a));
                    e.set_small(&ins, d, T4);
                }
                Rm::Reg(d) => e.put(a64::mvn(wide, d, d)),
                Rm::Mem { base, index, disp } => {
                    let a = e.address(base, index, disp);
                    e.mem(size(bits), Op::Load, T2, a);
                    e.put(a64::mvn(wide, T4, T2));
                    e.mem(size(bits), Op::Store, T4, a);
                }
            },
            0xf7 if field >= 4 => {
                // MUL, IMUL, DIV, IDIV with one operand.
                let b = e.operand(&ins, ins.rm);
                match field {
                    4 | 5 => e.mul_wide(field == 5, wide, b),
                    _ => e.div_wide(field == 7, wide, b, at, starts.len() - 1),
                }
            }
            0xf7 => {
                // NEG of a register: a subtraction from nothing.
                let Rm::Reg(d) = ins.rm else { unreachable!() };
                e.record(REC_SUB, bits, a64::ZR, Some(d));
                e.put(a64::arith(wide, true, true, d, a64::ZR, d, 0));
                e.flags = Some(HostFlags::Sub);
            }
            0xc1 | 0xd1 if small => {
                // SHL, SHR, SAR of an 8 or 16-bit register by a constant. The count
                // is taken by its low five bits whatever the width: one as large as
                // the register or larger shifts everything out.
                let Rm::Reg(d) = ins.rm else { unreachable!() };
                let n = if ins.op == 0xc1 { ins.imm as u32 & 31 } else { 1 };
                if n != 0 {
                    let kind = if field == 6 { 4 } else { field } as u64;
                    let a = e.small(&ins, d, T2);
                    if !e.skip {
                        e.constant(T1, n as u64);
                    }
                    e.record(REC_SHIFT + kind, bits, a, Some(T1));
                    match kind {
                        4 => e.put(a64::lsl(false, T4, a, n)),
                        5 => {
                            e.put(a64::uxt(T4, a, bits));
                            e.put(a64::lsr(false, T4, T4, n));
                        }
                        _ => {
                            e.put(a64::sxt(false, T4, a, bits));
                            e.put(a64::asr(false, T4, T4, n));
                        }
                    }
                    e.set_small(&ins, d, T4);
                }
                e.flags = None;
            }
            0x1c8..=0x1cf => e.put(a64::rev(wide, reg, reg)),
            0x1a3 | 0x1ab | 0x1b3 | 0x1bb | 0x1ba => {
                // BT, BTS, BTR, BTC of a register: the carry is the bit, and the
                // other flags are left. The host's flags are made to say the carry,
                // for a jump on it; and unless the flags are written over before
                // anything reads the record, the record is told that its carry was
                // given since (R031: before, the instruction was the interpreter's
                // wherever that could not be shown, and `cc1` is full of them).
                let Rm::Reg(d) = ins.rm else { unreachable!() };
                let by_constant = ins.op == 0x1ba;
                let what = if by_constant { field & 3 } else { (ins.op >> 3 & 3) as u8 };
                let dead = !e.exact && e.dead(next, Some(HostFlags::Bit), room);
                // The bit, alone, in T0; and for the three that change it, a one at
                // its place in T1.
                if by_constant {
                    let n = ins.imm as u32 & (bits - 1);
                    e.put(a64::ubfx(T0, d, n, 1));
                    if what != 0 {
                        e.constant(T1, 1 << n);
                    }
                } else {
                    // (A shift by a register takes its count by the width.)
                    e.put(a64::shift_reg(wide, Shift::Lsr, T0, d, reg));
                    e.put(a64::ubfx(T0, T0, 0, 1));
                    if what != 0 {
                        e.put(a64::movz(true, T1, 1, 0));
                        e.put(a64::shift_reg(wide, Shift::Lsl, T1, T1, reg));
                    }
                }
                if !dead {
                    // No record is "the flags as RFLAGS has them"; then, above the
                    // kind, "the carry was given" and the bit.
                    e.put(a64::arith_imm(true, true, true, a64::ZR, REC_KIND, 0, false));
                    e.put(a64::movz(true, T2, (REC_KEEP << 8) as u16, 0));
                    e.put(a64::csel(true, REC_KIND, T2, REC_KIND, Cond::Eq));
                    e.put(a64::movz(true, T2, 1, 0));
                    e.put(a64::bfi(REC_KIND, T2, REC_CARRY_GIVEN.trailing_zeros(), 1));
                    e.put(a64::bfi(REC_KIND, T0, REC_CARRY_GIVEN.trailing_zeros() + 1, 1));
                }
                // Nothing less the bit: a borrow, the host's carry clear, where the
                // bit is set.
                e.put(a64::arith(true, true, true, a64::ZR, a64::ZR, T0, 0));
                match what {
                    0 => {}
                    // Set; cleared (set, then turned over); turned over.
                    1 => e.put(a64::logic(wide, Logic::Orr, d, d, T1)),
                    2 => {
                        e.put(a64::logic(wide, Logic::Orr, d, d, T1));
                        e.put(a64::logic(wide, Logic::Eor, d, d, T1));
                    }
                    _ => e.put(a64::logic(wide, Logic::Eor, d, d, T1)),
                }
                e.flags = Some(HostFlags::Bit);
            }
            0xc1 | 0xd1 | 0xd3 => {
                let Rm::Reg(d) = ins.rm else { unreachable!() };
                let by_cl = ins.op == 0xd3;
                let n = if ins.op == 0xc1 { ins.imm as u32 & (bits - 1) } else { 1 };
                if field < 2 {
                    // ROL and ROR: translated only where their flags are not read.
                    if !e.dead(next, None, room) {
                        not_here = true;
                    } else if by_cl && field == 0 {
                        // To the left by CL is to the right by its negative.
                        e.put(a64::arith(false, true, false, T1, a64::ZR, RCX, 0));
                        e.put(a64::shift_reg(wide, Shift::Ror, d, d, T1));
                    } else if by_cl {
                        e.put(a64::shift_reg(wide, Shift::Ror, d, d, RCX));
                    } else if n == 0 {
                        if !wide {
                            e.put(a64::mov(false, d, d));
                        }
                    } else {
                        e.put(a64::ror(wide, d, d, if field == 0 { bits - n } else { n }));
                    }
                } else {
                    // SHL (4, and 6 is the same), SHR, SAR: the flags are a record of
                    // what was shifted and by how much, unless that is by nothing.
                    let kind = if field == 6 { 4 } else { field } as u64;
                    let op = match kind {
                        4 => Shift::Lsl,
                        5 => Shift::Lsr,
                        _ => Shift::Asr,
                    };
                    if by_cl {
                        if !e.skip {
                            e.put(a64::ubfx(T1, RCX, 0, if wide { 6 } else { 5 }));
                            // Over the three instructions of the record.
                            e.put(a64::cbz(true, false, T1, 4));
                            e.record(REC_SHIFT + kind, bits, d, Some(T1));
                        }
                        e.put(a64::shift_reg(wide, op, d, d, RCX));
                    } else if n == 0 {
                        if !wide {
                            e.put(a64::mov(false, d, d));
                        }
                    } else {
                        if !e.skip {
                            e.constant(T1, n as u64);
                        }
                        e.record(REC_SHIFT + kind, bits, d, Some(T1));
                        e.put(match op {
                            Shift::Lsl => a64::lsl(wide, d, d, n),
                            Shift::Lsr => a64::lsr(wide, d, d, n),
                            _ => a64::asr(wide, d, d, n),
                        });
                    }
                    // What a jump after it asks is not in the host's flags.
                    e.flags = None;
                }
            }
            0x140..=0x14f | 0x190..=0x19f => {
                // CMOVcc and SETcc, where the host's flags answer the condition.
                match e.flags.and_then(|f| condition(ins.op as u8 & 15, f)) {
                    None => not_here = true,
                    Some(jump) if ins.op < 0x190 => {
                        // The operand is read whatever the condition (and may fault);
                        // a 32-bit register loses its upper half either way.
                        let src = e.operand(&ins, ins.rm);
                        match jump {
                            Jump::If(cond) => e.put(a64::csel(wide, reg, src, reg, cond)),
                            Jump::Always => e.put(a64::mov(wide, reg, src)),
                            Jump::Never if !wide => e.put(a64::mov(false, reg, reg)),
                            Jump::Never => {}
                        }
                    }
                    Some(jump) => {
                        match jump {
                            Jump::If(cond) => e.put(a64::cset(false, T4, cond)),
                            Jump::Always => e.put(a64::movz(false, T4, 1, 0)),
                            Jump::Never => e.put(a64::movz(false, T4, 0, 0)),
                        }
                        match ins.rm {
                            Rm::Reg(d) => e.set_small(&ins, d, T4),
                            Rm::Mem { base, index, disp } => {
                                let a = e.address(base, index, disp);
                                e.mem(Size::B, Op::Store, T4, a);
                            }
                        }
                    }
                }
            }
            0x1af | 0x69 | 0x6b => {
                // IMUL of two operands, or of an operand and a constant, into a
                // register: the lower half of the product. Its flags are a record of
                // the two.
                let b = e.operand(&ins, ins.rm);
                let a = if ins.op == 0x1af {
                    reg
                } else {
                    e.constant(T3, ins.imm as u64);
                    T3
                };
                e.record(REC_IMUL, bits, a, Some(b));
                e.put(a64::mul(wide, reg, a, b));
                e.flags = None;
            }
            0x1bc | 0x1bd => {
                // BSF and BSR: of a zero the register is left as it was; the flags are
                // a record of the operand.
                let src = e.operand(&ins, ins.rm);
                e.record(if ins.op == 0x1bc { REC_BSF } else { REC_BSR }, bits, src, None);
                if ins.op == 0x1bc {
                    e.put(a64::cbz(wide, false, src, 3));
                    e.put(a64::rbit(wide, T1, src));
                    e.put(a64::clz(wide, reg, T1));
                } else {
                    e.put(a64::cbz(wide, false, src, 4));
                    e.put(a64::clz(wide, T1, src));
                    e.put(a64::movz(true, T2, bits as u16 - 1, 0));
                    e.put(a64::arith(wide, true, false, reg, T2, T1, 0));
                }
                e.flags = None;
            }
            0x89 => {
                let src = if small { e.small(&ins, reg, T0) } else { reg };
                match ins.rm {
                    Rm::Reg(d) if small => e.set_small(&ins, d, src),
                    Rm::Reg(d) => e.put(a64::mov(wide, d, src)),
                    Rm::Mem { base, index, disp } => {
                        let a = e.address(base, index, disp);
                        e.mem(size(bits), Op::Store, src, a);
                    }
                }
            }
            0x8b if small => {
                let src = e.operand(&ins, ins.rm);
                e.set_small(&ins, reg, src);
            }
            0x8b => match ins.rm {
                Rm::Reg(s) => e.put(a64::mov(wide, reg, s)),
                Rm::Mem { base, index, disp } => {
                    let a = e.address(base, index, disp);
                    e.mem(size(bits), Op::Load, reg, a);
                }
            },
            0x8d => {
                let Rm::Mem { base, index, disp } = ins.rm else { unreachable!() };
                let a = e.address(base, index, disp);
                e.put(a64::mov(wide, reg, a));
            }
            0xc7 => match ins.rm {
                Rm::Reg(d) if small => {
                    e.constant(T3, ins.imm as u64);
                    e.set_small(&ins, d, T3);
                }
                Rm::Reg(d) => e.constant(d, if wide { ins.imm as u64 } else { ins.imm as u32 as u64 }),
                Rm::Mem { base, index, disp } => {
                    e.constant(T3, ins.imm as u64);
                    let a = e.address(base, index, disp);
                    e.mem(size(bits), Op::Store, T3, a);
                }
            },
            0xb8..=0xbf => e.constant(reg, if wide { ins.imm as u64 } else { ins.imm as u32 as u64 }),
            0x63 if wide => match ins.rm {
                Rm::Reg(s) => e.put(a64::sxt(true, reg, s, 32)),
                Rm::Mem { base, index, disp } => {
                    let a = e.address(base, index, disp);
                    e.mem(Size::W, Op::LoadSigned, reg, a);
                }
            },
            0x63 => {
                let src = e.operand(&ins, ins.rm);
                e.put(a64::mov(false, reg, src));
            }
            0x1b6 | 0x1b7 | 0x1be | 0x1bf => {
                let (from, small) = if ins.op & 1 == 0 { (8, Size::B) } else { (16, Size::H) };
                let signed = ins.op >= 0x1be;
                match ins.rm {
                    // From AH and its like: the second byte of the register four
                    // numbers lower. (There is no REX, so the result is 32 bits.)
                    Rm::Reg(s) if from == 8 && !ins.rex && (4..8).contains(&s) => {
                        e.put(if signed { a64::sbfx(false, reg, s - 4, 8, 8) } else { a64::ubfx(reg, s - 4, 8, 8) });
                    }
                    Rm::Reg(s) if signed => e.put(a64::sxt(wide, reg, s, from)),
                    Rm::Reg(s) => e.put(a64::uxt(reg, s, from)),
                    Rm::Mem { base, index, disp } => {
                        let a = e.address(base, index, disp);
                        e.mem(small, if signed { Op::LoadSigned } else { Op::Load }, reg, a);
                        // The sign went to all 64 bits; a 32-bit result has none above.
                        if signed && !wide {
                            e.put(a64::mov(false, reg, reg));
                        }
                    }
                }
            }
            // CWDE and CDQE: the A register's lower half, with its sign, to all of it.
            0x98 => e.put(a64::sxt(wide, 0, 0, bits / 2)),
            // CDQ and CQO: the A register's sign to all of the D register.
            0x99 => e.put(a64::asr(wide, 2, 0, bits - 1)),
            0x50..=0x57 | 0x68 | 0x6a => {
                let src = if ins.op < 0x58 {
                    reg
                } else {
                    e.constant(T0, ins.imm as u64);
                    T0
                };
                e.push(src);
            }
            0x58..=0x5f => {
                e.mem(Size::X, Op::Load, T0, RSP);
                e.put(a64::arith_imm(true, false, false, RSP, RSP, 8, false));
                e.put(a64::mov(true, reg, T0));
            }
            0x90 | 0x11e | 0x11f | 0x118 => {}
            0xc9 => {
                // LEAVE: RSP is RBP, then RBP is popped. What is popped is read
                // first: nothing is changed if it cannot be.
                e.mem(Size::X, Op::Load, T0, 5);
                e.put(a64::arith_imm(true, false, false, RSP, 5, 8, false));
                e.put(a64::mov(true, 5, T0));
            }
            0xff if field < 2 => e.inc_dec(field == 1, bits, ins.rm),
            0xff if field == 6 => match ins.rm {
                Rm::Reg(s) => e.push(s),
                Rm::Mem { base, index, disp } => {
                    let a = e.address(base, index, disp);
                    e.mem(Size::X, Op::Load, T2, a);
                    e.push(T2);
                }
            },
            0xe9 | 0xeb => {
                e.leave(ins.imm as u64, starts.len());
                ended = true;
            }
            0xe8 => {
                e.constant(T0, next);
                e.push(T0);
                e.leave(ins.imm as u64, starts.len());
                ended = true;
            }
            0xff => {
                // Where to: read before anything is pushed (the operand may be found
                // through RSP), and kept in T2 while T0 and T1 push.
                match ins.rm {
                    Rm::Reg(s) => e.put(a64::mov(true, T2, s)),
                    Rm::Mem { base, index, disp } => {
                        let a = e.address(base, index, disp);
                        e.mem(Size::X, Op::Load, T2, a);
                    }
                }
                if field == 2 {
                    e.constant(T0, next);
                    e.push(T0);
                }
                e.put(a64::mov(true, T0, T2));
                e.leave_at_t0(starts.len());
                ended = true;
            }
            0xc3 => {
                e.mem(Size::X, Op::Load, T0, RSP);
                e.put(a64::arith_imm(true, false, false, RSP, RSP, 8, false));
                e.leave_at_t0(starts.len());
                ended = true;
            }
            0x70..=0x7f | 0x180..=0x18f => {
                let n = starts.len();
                let target = ins.imm as u64;
                // The block's instructions are counted once, before the jump, for both
                // its ways (an addition that leaves the host's flags alone).
                let count = a64::arith_imm(true, false, false, RAN, RAN, n as u32, false);
                match e.flags.and_then(|f| condition(ins.op as u8 & 15, f)) {
                    // The host's flags do not say. (Never where a record was left for
                    // this jump: it was left because they do.)
                    None => {
                        assert!(later.is_none(), "a record left for a jump that reads it, at {at:#x}");
                        not_here = true;
                    }
                    Some(Jump::Always) => {
                        e.put(count);
                        e.leave_way(target, later);
                    }
                    // The block goes on past the jump (R018): the way taken is written
                    // after the block's end, and the way not taken is the next
                    // instruction, with the host's flags as they are. The record that
                    // was left for the jump is written here if anything on from here
                    // can read it.
                    Some(Jump::If(cond)) if goes_on(!e.exact, rip, target, MOST - n) => {
                        let needed = e.way_needs(target, later);
                        taken.push(Taken { jump: e.w.len(), cond, target, n, later: needed });
                        e.put(a64::nop());
                        if let Some(l) = later {
                            if !e.dead(next, e.flags, MOST - n) {
                                e.write_later(l);
                            }
                        }
                        at = next;
                        continue;
                    }
                    Some(Jump::Never) if goes_on(!e.exact, rip, target, MOST - n) => {
                        // Never taken: on, as if it were not there.
                        if let Some(l) = later {
                            if !e.dead(next, e.flags, MOST - n) {
                                e.write_later(l);
                            }
                        }
                        at = next;
                        continue;
                    }
                    Some(Jump::Never) => {
                        e.put(count);
                        e.leave_way(next, later);
                    }
                    // Back to this block's own start, with no record to write on the
                    // way: the jump goes there itself, and a loop of one block is one
                    // jump taken a round (R015). Not where each pass through a block
                    // is to be checked.
                    Some(Jump::If(cond)) if target == rip && !e.exact && e.way_needs(target, later).is_none() => {
                        e.put(count);
                        e.put(a64::b_cond(cond, -(e.w.len() as i32)));
                        e.leave_way(next, later);
                    }
                    Some(Jump::If(cond)) => {
                        // Over the way out for "not taken" to the one for "taken": the
                        // jump is written when it is known how far that is.
                        e.put(count);
                        let jump_at = e.w.len();
                        e.put(a64::nop());
                        e.leave_way(next, later);
                        e.w[jump_at] = a64::b_cond(cond, (e.w.len() - jump_at) as i32);
                        e.leave_way(target, later);
                    }
                }
                ended = true;
            }
            op if is_sse(op) => {
                sse = true;
                e.sse(&ins, at, starts.len() - 1);
            }
            _ => unreachable!("read gave an instruction that is not translated"),
        }
        // What was asked about the flags before this instruction was written was asked
        // with what `host_flags_of` says it leaves in the host's flags: that must be
        // what it did leave, or a jump after it was reasoned about wrongly.
        if matches!(flags_of(&ins), Flags::All | Flags::Carry) && !not_here {
            assert!(e.flags == host_flags_of(&ins), "the host's flags after {:#x} at {at:#x}", ins.op);
        }
        if not_here {
            e.w.truncate(before);
            starts.pop();
            if starts.is_empty() {
                return None;
            }
            e.leave(at, starts.len());
            break;
        }
        at = next;
        if ended {
            break;
        }
    }
    // The ways taken of the jumps the block went on past.
    for way in taken {
        let here = e.w.len();
        e.w[way.jump] = a64::b_cond(way.cond, (here - way.jump) as i32);
        if let Some(l) = way.later {
            e.write_later(l);
        }
        e.leave(way.target, way.n);
    }
    // And the ways out to the interpreter. The first is the look's, which a block
    // that no cycle passes through has no use for: such a block is not made longer
    // by it (blocks are small, and seven instructions more at each one's end showed
    // as some hundredths of the time of code that is all blocks, R037).
    let skip = if cycle { 0 } else { 1 };
    for (jump, at, n) in std::mem::take(&mut e.bails).into_iter().skip(skip) {
        // The distance goes into bits 5 to 23 of the B.cond (5 to 18 of the look's
        // test of a bit, which is never further than that reaches).
        debug_assert!(e.w.len() - jump < 1 << 13);
        e.w[jump] |= ((e.w.len() - jump) as u32 & 0x7_ffff) << 5;
        e.bail_out(at, n);
    }
    // The look is taken out of a block no cycle passes through: the instruction at
    // its start, with everything after it one place earlier. (What jumps within the
    // block jumps by a distance, which is the same.)
    let mut words = e.w;
    if !cycle {
        words.remove(0);
        for start in &mut starts {
            start.0 -= 1;
        }
    }
    // A block that uses the XMM registers loads them first. What was written for it
    // comes after: its jumps to itself are to where it was to begin, past the loading
    // (a loop of one block has loaded them).
    if sse {
        let mut first = load_xmm();
        for start in &mut starts {
            start.0 += first.len() as u32;
        }
        first.append(&mut words);
        words = first;
    }
    Some(Built { words, starts, end: at, seen: e.seen })
}

impl Emit<'_> {
    /// The register that holds a source operand in its low bits: the guest's own (or
    /// for AH and its like the byte brought down into `T0`), or memory read into `T0`.
    fn operand(&mut self, ins: &Ins, rm: Rm) -> u8 {
        match rm {
            Rm::Reg(s) => self.small(ins, s, T0),
            Rm::Mem { base, index, disp } => {
                let a = self.address(base, index, disp);
                self.mem(size(ins.size), Op::Load, T0, a);
                T0
            }
        }
    }

    /// PUSH of a register's 64 bits: stored first, so that a fault finds RSP as it was.
    fn push(&mut self, src: u8) {
        self.put(a64::arith_imm(true, true, false, T1, RSP, 8, false));
        self.mem(Size::X, Op::Store, src, T1);
        self.put(a64::mov(true, RSP, T1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_op(base: Option<u8>, index: Option<(u8, u32)>, disp: i64) -> Rm {
        Rm::Mem { base, index, disp }
    }

    #[test]
    fn instructions_are_read_as_what_they_are() {
        let at = 0x40_1000;
        let ins = |bytes: &[u8]| read(bytes, at).unwrap_or_else(|| panic!("not read: {bytes:02x?}"));
        // add eax, ebx; add rax, r11; mov r9d, [rsi]
        let i = ins(&[0x01, 0xd8]);
        assert_eq!((i.op, i.len, i.size, i.reg, i.rm), (0x01, 2, 32, 3, Rm::Reg(0)));
        let i = ins(&[0x4c, 0x01, 0xd8]);
        assert_eq!((i.len, i.size, i.reg, i.rm), (3, 64, 11, Rm::Reg(0)));
        let i = ins(&[0x44, 0x8b, 0x0e]);
        assert_eq!((i.reg, i.rm), (9, mem_op(Some(6), None, 0)));
        // mov rax, [rbx + rcx*8 + 0x10]; mov eax, [r12]; mov eax, [r13 + 0]
        let i = ins(&[0x48, 0x8b, 0x44, 0xcb, 0x10]);
        assert_eq!((i.len, i.rm), (5, mem_op(Some(3), Some((1, 3)), 0x10)));
        assert_eq!(ins(&[0x41, 0x8b, 0x04, 0x24]).rm, mem_op(Some(12), None, 0));
        assert_eq!(ins(&[0x41, 0x8b, 0x45, 0x00]).rm, mem_op(Some(13), None, 0));
        // mov eax, [r9*4 - 0x80000000]: an index and no base; mov eax, [0x1234]
        assert_eq!(ins(&[0x42, 0x8b, 0x04, 0x8d, 0, 0, 0, 0x80]).rm, mem_op(None, Some((9, 2)), -0x8000_0000));
        assert_eq!(ins(&[0x8b, 0x04, 0x25, 0x34, 0x12, 0, 0]).rm, mem_op(None, None, 0x1234));
        // mov eax, [rip - 8]: from the instruction's end; and with a constant after it
        assert_eq!(ins(&[0x8b, 0x05, 0xf8, 0xff, 0xff, 0xff]).rm, mem_op(None, None, at as i64 + 6 - 8));
        let i = ins(&[0x81, 0x05, 0x10, 0, 0, 0, 0x78, 0x56, 0x34, 0x12]);
        assert_eq!((i.len, i.rm, i.imm), (10, mem_op(None, None, at as i64 + 10 + 0x10), 0x1234_5678));
        // The constants: with their sign from 8 and from 32 bits, whole at 64.
        assert_eq!(ins(&[0x83, 0xe8, 0xff]).imm, -1);
        assert_eq!(ins(&[0x48, 0x81, 0xc4, 0, 0, 0, 0x80]).imm, -0x8000_0000);
        assert_eq!(ins(&[0xb8, 0xff, 0xff, 0xff, 0xff]).imm, -1);
        let i = ins(&[0x49, 0xb8, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!((i.len, i.reg, i.imm), (10, 8, 0x0807_0605_0403_0201));
        // The jumps: where they go.
        assert_eq!(ins(&[0xeb, 0xfe]).imm, at as i64);
        assert_eq!(ins(&[0x74, 0x10]).imm, at as i64 + 0x12);
        assert_eq!(ins(&[0x0f, 0x85, 0, 0x01, 0, 0]).imm, at as i64 + 6 + 0x100);
        assert_eq!(ins(&[0xe8, 0xfb, 0xff, 0xff, 0xff]).imm, at as i64);
        // CALL and JMP through a register and through memory.
        let i = ins(&[0xff, 0xd0]);
        assert_eq!((i.op, i.reg & 7, i.rm), (0xff, 2, Rm::Reg(0)));
        let i = ins(&[0x41, 0xff, 0x24, 0xc3]);
        assert_eq!((i.reg & 7, i.rm), (4, mem_op(Some(11), Some((0, 3)), 0)));
        // Operands of 8 and 16 bits: a byte's opcode is read as its neighbour's.
        let i = ins(&[0x00, 0xd8]);
        assert_eq!((i.op, i.size, i.len), (0x01, 8, 2));
        let i = ins(&[0x66, 0x01, 0xd8]);
        assert_eq!((i.op, i.size, i.len), (0x01, 16, 3));
        let i = ins(&[0x66, 0x81, 0xe1, 0xff, 0x03]);
        assert_eq!((i.op, i.size, i.len, i.imm), (0x81, 16, 5, 0x3ff));
        let i = ins(&[0x80, 0xcc, 0x81]);
        assert_eq!((i.op, i.size, i.reg & 7, i.rm, i.imm), (0x81, 8, 1, Rm::Reg(4), -0x7f));
        let i = ins(&[0xc6, 0x05, 0x10, 0, 0, 0, 0x0a]);
        assert_eq!((i.op, i.size, i.len, i.rm, i.imm), (0xc7, 8, 7, mem_op(None, None, at as i64 + 7 + 0x10), 10));
        let i = ins(&[0x66, 0xc7, 0x06, 0x34, 0x12]);
        assert_eq!((i.op, i.size, i.len, i.imm), (0xc7, 16, 5, 0x1234));
        assert_eq!(ins(&[0x66, 0x83, 0xc0, 0xff]).imm, -1);
        // Shifts of 8 and 16 bits by a constant and by one.
        let i = ins(&[0xc0, 0xea, 0x03]);
        assert_eq!((i.op, i.size, i.reg & 7, i.rm, i.imm), (0xc1, 8, 5, Rm::Reg(2), 3));
        assert_eq!((ins(&[0xd0, 0xe4]).op, ins(&[0xd0, 0xe4]).size), (0xd1, 8));
        assert_eq!(ins(&[0x66, 0x41, 0xc1, 0xf9, 0x07]).size, 16);
        // F7h: TEST has a constant as wide as the operand; NOT has none.
        let i = ins(&[0xf6, 0xc2, 0x80]);
        assert_eq!((i.op, i.size, i.len, i.imm), (0xf7, 8, 3, -0x80));
        assert_eq!(ins(&[0x48, 0xf7, 0xc2, 1, 0, 0, 0]).len, 7);
        assert_eq!(ins(&[0x48, 0xf7, 0xd0]).len, 3);
        // The shifts; the prefixes that are read.
        let i = ins(&[0x48, 0xc1, 0xe0, 0x11]);
        assert_eq!((i.op, i.size, i.reg & 7, i.imm), (0xc1, 64, 4, 0x11));
        assert_eq!(ins(&[0x49, 0xd3, 0xcb]).rm, Rm::Reg(11));
        let i = ins(&[0xf3, 0x4c, 0x0f, 0xbc, 0xc8]);
        assert_eq!((i.op, i.size, i.len, i.reg, i.rm), (0x1bc, 64, 5, 9, Rm::Reg(0)));
        assert_eq!(ins(&[0xf3, 0xc3]).op, 0xc3);
        assert_eq!(ins(&[0xf3, 0x0f, 0x1e, 0xfa]).len, 4);
        let i = ins(&[0x66, 0x2e, 0x0f, 0x1f, 0x84, 0x00, 0, 0, 0, 0]);
        assert_eq!((i.op, i.len), (0x11f, 10));
        assert_eq!((ins(&[0x48, 0x98]).size, ins(&[0x99]).size), (64, 32));
        // The registers in an opcode's low bits.
        assert_eq!(ins(&[0x41, 0x57]).reg, 15);
        assert_eq!(ins(&[0x5d]).reg, 5);
    }

    #[test]
    fn what_is_not_translated_is_not_read() {
        let no = |bytes: &[u8]| assert_eq!(read(bytes, 0x40_1000), None, "{bytes:02x?}");
        no(&[0xf3, 0x90]); // PAUSE: F3h where it is not read
        no(&[0xf2, 0x0f, 0xbc, 0xc3]); // F2h
        no(&[0x67, 0x8b, 0x00]); // a 32-bit address
        no(&[0xf0, 0x01, 0x06]); // LOCK
        no(&[0x66, 0x50]); // a 16-bit push
        no(&[0x66, 0xd3, 0xe0]); // a 16-bit shift by CL
        no(&[0xc0, 0xc0, 0x03]); // an 8-bit rotation
        no(&[0xd2, 0xe0]); // an 8-bit shift by CL
        no(&[0xc0, 0x26, 0x03]); // an 8-bit shift of memory
        no(&[0x66, 0x11, 0xd8]); // a 16-bit ADC
        no(&[0x10, 0xd8]); // an 8-bit one
        no(&[0x66, 0xf7, 0xe3]); // a 16-bit MUL
        no(&[0xf6, 0xf3]); // an 8-bit DIV
        no(&[0x66, 0x0f, 0xc8]); // a 16-bit BSWAP
        no(&[0xc1, 0x26, 0x03]); // a shift of memory
        no(&[0xd1, 0xd0]); // RCL
        no(&[0xf7, 0x1e]); // NEG of memory
        no(&[0xf6, 0xdb]); // NEG of a byte
        no(&[0x66, 0x0f, 0xbc, 0xc3]); // a 16-bit BSF
        no(&[0x66, 0x98]); // CBW
        no(&[0x66, 0x0f, 0xa3, 0xd8]); // a 16-bit BT
        no(&[0x0f, 0xa3, 0x18]); // BT of memory
        no(&[0x8d, 0xc0]); // LEA of a register
        no(&[0xc7, 0xf8, 0, 0, 0, 0]); // C7h's other field
        no(&[0x41, 0x90]); // XCHG R8, RAX
        no(&[0x0f, 0x05]); // SYSCALL
        no(&[0x66, 0xff, 0xc0]); // INC of 16 bits
        no(&[0x67, 0xff, 0x00]); // INC through a 32-bit address
        // Cut short anywhere, it is not read.
        let whole = [0x48u8, 0x81, 0x84, 0xcb, 0x10, 0x20, 0x30, 0x40, 1, 2, 3, 4];
        assert!(read(&whole, 0).is_some());
        for n in 0..whole.len() {
            no(&whole[..n]);
        }
        // With REX, register 4 as a byte is SPL: read.
        assert!(read(&[0x40, 0x0f, 0xb6, 0xc4], 0).is_some());
    }
}
