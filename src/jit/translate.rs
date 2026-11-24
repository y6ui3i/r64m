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

use super::a64::{self, Cond, Logic, Mem as Op, Shift, Size};
use super::{BASE, CPU, LEFT_AT_END, OUT, RAN, REASON, TABLE, TABLE_BITS};
use crate::cpu::{Cpu, REC_ADD, REC_ADD_BY_RESULT, REC_BSF, REC_BSR, REC_LOGIC, REC_SHIFT, REC_SUB, REC_SUB_BY_RESULT};
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
    // allowed before), and the segments that do nothing in 64-bit code. Then REX.
    let (mut p66, mut f3) = (false, false);
    let mut b = next(1)? as u8;
    loop {
        match b {
            0x66 => p66 = true,
            0xf3 => f3 = true,
            0x26 | 0x2e | 0x36 | 0x3e => {}
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
    let byte = matches!(op, 0x00..=0x3f | 0x80 | 0x84 | 0x88 | 0x8a | 0xc6 | 0xf6) && op & 1 == 0;
    if byte {
        op |= 1;
    }
    let size = match (byte, rex & 8 != 0, p66) {
        (true, _, _) => 8,
        (_, true, _) => 64,
        (_, _, true) => 16,
        _ => 32,
    };
    let mut ins = Ins { op, len: 0, size, rex: rex != 0, reg: 0, rm: Rm::Reg(0), imm: 0 };

    // What follows the opcode: a ModRM byte, and a constant of how many bytes (for
    // the opcodes whose constant is as wide as the operand, 4 at most).
    let whole = match size {
        8 => 1,
        16 => 2,
        _ => 4,
    };
    let (modrm, mut imm) = match op {
        0x01 | 0x03 | 0x09 | 0x0b | 0x21 | 0x23 | 0x29 | 0x2b | 0x31 | 0x33 | 0x39 | 0x3b => (true, 0),
        0x63 | 0x85 | 0x89 | 0x8b | 0x8d | 0xd1 | 0xd3 | 0xf7 | 0xff => (true, 0),
        0x11e | 0x11f | 0x1b6 | 0x1b7 | 0x1bc | 0x1bd | 0x1be | 0x1bf => (true, 0),
        0x81 | 0xc7 => (true, whole),
        0x83 | 0xc1 => (true, 1),
        0x50..=0x5f | 0x90 | 0x98 | 0x99 | 0xc3 => (false, 0),
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
    let field = ins.reg & 7;
    let register = matches!(ins.rm, Rm::Reg(_));
    // An operand of 8 or 16 bits is translated for the arithmetic and the moves only.
    let small_ok = matches!(op, 0x01..=0x3b | 0x81 | 0x83 | 0x85 | 0x89 | 0x8b | 0xc7 | 0xf7 | 0x11e | 0x11f);
    // F3h is REP RET before a return, part of ENDBR64 before the hint 0F 1Eh, and
    // nothing before BSF and BSR on a CPU without TZCNT and LZCNT.
    let f3_ok = matches!(op, 0xc3 | 0x11e | 0x1bc | 0x1bd);
    // What is in the list above by its opcode but is not translated in this form.
    let refused = (size < 32 && !small_ok)
        || (f3 && !f3_ok)
        || match op {
            // XCHG with R8.
            0x90 => rex & 1 != 0,
            // The one field of C7h that is MOV.
            0xc7 => field != 0,
            // ADC and SBB read the carry.
            0x81 | 0x83 => matches!(field, 2 | 3),
            // LEA of a register is not an instruction.
            0x8d => register,
            // Without REX the byte registers 4 to 7 are AH, CH, DH and BH.
            0x1b6 | 0x1be => rex == 0 && matches!(ins.rm, Rm::Reg(4..=7)),
            // Of FFh only CALL and JMP through the operand.
            0xff => !matches!(field, 2 | 4),
            // Of F7h: TEST with a constant, NOT, and NEG of a 32 or 64-bit register.
            0xf7 => !(matches!(field, 0 | 2) || field == 3 && register && size >= 32),
            // The shifts and rotations of a register, not through the carry.
            0xc1 | 0xd1 | 0xd3 => !register || matches!(field, 2 | 3),
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
    /// Leaves them, or may leave them.
    Kept,
    /// Reads them, or leaves the block.
    Read,
}

fn flags_of(ins: &Ins) -> Flags {
    let field = ins.reg & 7;
    match ins.op {
        0x01..=0x3b | 0x81 | 0x83 | 0x85 | 0x1bc | 0x1bd => Flags::All,
        0xf7 if field != 2 => Flags::All,
        // A shift by a constant that is not zero; by CL it may be by nothing.
        0xc1 if field >= 4 && ins.imm as u32 & (ins.size - 1) != 0 => Flags::All,
        0xd1 if field >= 4 => Flags::All,
        0x70..=0x7f | 0x180..=0x18f | 0xc3 | 0xe8 | 0xe9 | 0xeb | 0xff => Flags::Read,
        _ => Flags::Kept,
    }
}

/// What of the host's flags an instruction that writes all of x86's leaves behind for
/// a conditional jump to use.
fn host_flags_of(ins: &Ins) -> Option<HostFlags> {
    let field = ins.reg & 7;
    match ins.op {
        0x01 | 0x03 => Some(HostFlags::Add),
        0x29 | 0x2b | 0x39 | 0x3b => Some(HostFlags::Sub),
        0x09 | 0x0b | 0x21 | 0x23 | 0x31 | 0x33 | 0x85 => Some(HostFlags::Logic),
        0x81 | 0x83 => Some(match field {
            0 => HostFlags::Add,
            5 | 7 => HostFlags::Sub,
            _ => HostFlags::Logic,
        }),
        0xf7 if field == 0 => Some(HostFlags::Logic),
        0xf7 if field == 3 => Some(HostFlags::Sub),
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
            room = room.saturating_sub(1);
            match flags_of(&ins) {
                Flags::All => return true,
                Flags::Kept => {
                    // A shift by CL or a rotation leaves the host's flags no longer
                    // what a jump would ask (and a rotation may end the block).
                    if matches!(ins.op, 0xc1 | 0xd1 | 0xd3) {
                        host = None;
                    }
                    at = next;
                }
                Flags::Read => match ins.op {
                    0x70..=0x7f | 0x180..=0x18f => {
                        // In the block and answered by the host's flags, it reads no
                        // record. Past it the host's flags are someone else's.
                        let answered = inside && host.and_then(|h| condition(ins.op as u8 & 15, h)).is_some();
                        if !answered || depth == 0 {
                            return false;
                        }
                        self.asked = true;
                        return self.dead(next, None, 0, depth - 1) && self.dead(ins.imm as u64, None, 0, depth - 1);
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
}

impl Emit<'_> {
    /// Are the flags dead from `at` on (`Scan::dead`)? What is read to say so becomes
    /// part of what the block rests on, and must lie within a few pages of it.
    fn dead(&mut self, at: u64, host: Option<HostFlags>, room: usize) -> bool {
        let mut scan = Scan { m: self.m, budget: 48, span: (at, at), asked: false };
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

    fn constant(&mut self, rd: u8, value: u64) {
        let mut out = [0; 4];
        let n = a64::mov_imm(rd, value, &mut out);
        self.w.extend(&out[..n]);
    }

    /// The register that holds a memory operand's address in the guest: one of the
    /// guest's own if the address is just that, else `T1`, made here.
    fn address(&mut self, base: Option<u8>, index: Option<(u8, u32)>, disp: i64) -> u8 {
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
                self.put(a64::mem_reg(small, Op::Load, T2, BASE, at));
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
                    self.put(a64::mem_reg(small, Op::Store, T4, BASE, at));
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
                self.put(a64::mem_reg(size(bits), Op::Load, T2, BASE, at));
                (T2, T0, Some(at))
            }
        };
        let keeps = !matches!(kind, 7 | 8);
        match kind {
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
                self.put(a64::mem_reg(size(bits), Op::Store, to, BASE, at));
            }
            // Memory's old value is still in T2: recorded when the store is done.
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
        self.put(a64::br(OUT));
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
pub fn translate(m: &Mem, rip: u64, exact: bool) -> Option<Built> {
    let mut e = Emit { w: Vec::new(), flags: None, m, rip, exact, skip: false, seen: None, later: None, host: true };
    let mut starts = Vec::new();
    let mut at = rip;
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
        starts.push((e.w.len() as u32, at));
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
        let sets = matches!(flags_of(&ins), Flags::All) || ins.op == 0xd3 && field >= 4;
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
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 | 0x39 => {
                let src = if small { e.small(&ins, reg, T0) } else { reg };
                e.alu((ins.op >> 3) as u8, &ins, ins.rm, src);
            }
            0x03 | 0x0b | 0x23 | 0x2b | 0x33 | 0x3b => {
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
                    e.put(a64::mem_reg(size(bits), Op::Load, T2, BASE, a));
                    e.put(a64::mvn(wide, T4, T2));
                    e.put(a64::mem_reg(size(bits), Op::Store, T4, BASE, a));
                }
            },
            0xf7 => {
                // NEG of a register: a subtraction from nothing.
                let Rm::Reg(d) = ins.rm else { unreachable!() };
                e.record(REC_SUB, bits, a64::ZR, Some(d));
                e.put(a64::arith(wide, true, true, d, a64::ZR, d, 0));
                e.flags = Some(HostFlags::Sub);
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
                        e.put(a64::mem_reg(size(bits), Op::Store, src, BASE, a));
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
                    e.put(a64::mem_reg(size(bits), Op::Load, reg, BASE, a));
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
                    e.put(a64::mem_reg(size(bits), Op::Store, T3, BASE, a));
                }
            },
            0xb8..=0xbf => e.constant(reg, if wide { ins.imm as u64 } else { ins.imm as u32 as u64 }),
            0x63 if wide => match ins.rm {
                Rm::Reg(s) => e.put(a64::sxt(true, reg, s, 32)),
                Rm::Mem { base, index, disp } => {
                    let a = e.address(base, index, disp);
                    e.put(a64::mem_reg(Size::W, Op::LoadSigned, reg, BASE, a));
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
                    Rm::Reg(s) if signed => e.put(a64::sxt(wide, reg, s, from)),
                    Rm::Reg(s) => e.put(a64::uxt(reg, s, from)),
                    Rm::Mem { base, index, disp } => {
                        let a = e.address(base, index, disp);
                        e.put(a64::mem_reg(small, if signed { Op::LoadSigned } else { Op::Load }, reg, BASE, a));
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
                e.put(a64::mem_reg(Size::X, Op::Load, T0, BASE, RSP));
                e.put(a64::arith_imm(true, false, false, RSP, RSP, 8, false));
                e.put(a64::mov(true, reg, T0));
            }
            0x90 | 0x11e | 0x11f => {}
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
                        e.put(a64::mem_reg(Size::X, Op::Load, T2, BASE, a));
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
                e.put(a64::mem_reg(Size::X, Op::Load, T0, BASE, RSP));
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
            _ => unreachable!("read gave an instruction that is not translated"),
        }
        // What was asked about the flags before this instruction was written was asked
        // with what `host_flags_of` says it leaves in the host's flags: that must be
        // what it did leave, or a jump after it was reasoned about wrongly.
        if matches!(flags_of(&ins), Flags::All) && !not_here {
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
    Some(Built { words: e.w, starts, end: at, seen: e.seen })
}

impl Emit<'_> {
    /// The register that holds a source operand in its low bits: the guest's own (or
    /// for AH and its like the byte brought down into `T0`), or memory read into `T0`.
    fn operand(&mut self, ins: &Ins, rm: Rm) -> u8 {
        match rm {
            Rm::Reg(s) => self.small(ins, s, T0),
            Rm::Mem { base, index, disp } => {
                let a = self.address(base, index, disp);
                self.put(a64::mem_reg(size(ins.size), Op::Load, T0, BASE, a));
                T0
            }
        }
    }

    /// PUSH of a register's 64 bits: stored first, so that a fault finds RSP as it was.
    fn push(&mut self, src: u8) {
        self.put(a64::arith_imm(true, true, false, T1, RSP, 8, false));
        self.put(a64::mem_reg(Size::X, Op::Store, src, BASE, T1));
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
        no(&[0x66, 0xc1, 0xe0, 0x03]); // a 16-bit shift
        no(&[0xc0, 0xe0, 0x03]); // an 8-bit one
        no(&[0xc1, 0x26, 0x03]); // a shift of memory
        no(&[0xd1, 0xd0]); // RCL
        no(&[0xf7, 0xe3]); // MUL
        no(&[0xf7, 0x1e]); // NEG of memory
        no(&[0xf6, 0xdb]); // NEG of a byte
        no(&[0x66, 0x0f, 0xbc, 0xc3]); // a 16-bit BSF
        no(&[0x66, 0x98]); // CBW
        no(&[0x64, 0x8b, 0x00]); // a segment
        no(&[0x11, 0xd8]); // ADC
        no(&[0x83, 0xd0, 0x01]); // ADC of a constant
        no(&[0x83, 0xd8, 0x01]); // SBB of a constant
        no(&[0x8d, 0xc0]); // LEA of a register
        no(&[0x0f, 0xb6, 0xc4]); // MOVZX from AH
        no(&[0xc7, 0xf8, 0, 0, 0, 0]); // C7h's other field
        no(&[0x41, 0x90]); // XCHG R8, RAX
        no(&[0x0f, 0x05]); // SYSCALL
        no(&[0xff, 0xc0]); // INC by FFh
        no(&[0xff, 0x30]); // PUSH of memory
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
