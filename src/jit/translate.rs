//! From a run of the guest's instructions to arm64 code that does the same (ADR 0014,
//! points 1, 3, 5 and 6).
//!
//! The first translators: the integer instructions on 32 and 64-bit operands that the
//! primitives are mostly made of. An instruction with any prefix but REX, with an 8 or
//! 16-bit operand, or simply not in the list, is not translated: the block ends before
//! it and the interpreter runs it.
//!
//! What generated code may rely on, and must keep:
//! - x0 to x15 are the guest's registers at every instruction's start, and at every
//!   load or store inside one: a fault there leaves the block with them as they are.
//!   So an instruction changes no guest register before its last load or store.
//! - the record of the flags (x21 to x23) is what the last instruction that set them
//!   left: written when that instruction is done, not before.
//! - x16, x17, x25 and x26 are scratch within one instruction.
//! - x27 counts the instructions of blocks run to their ends: each way out of a block
//!   adds the block's to it, so that a block may go on to the next without Rust
//!   between (R009).

use super::a64::{self, Cond, Logic, Mem as Op, Size};
use super::{BASE, CPU, LEFT_AT_END, OUT, RAN, REASON, TABLE, TABLE_BITS};
use crate::cpu::{Cpu, REC_ADD, REC_LOGIC, REC_SUB};
use crate::mem::Mem;
use std::mem::offset_of;

/// The most instructions in one block.
const MOST: usize = 64;

// Scratch, and the record of the flags.
const T0: u8 = 16;
const T1: u8 = 17;
const T2: u8 = 25;
const T3: u8 = 26;
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
    /// REX.W: the operand is 64 bits, not 32.
    wide: bool,
    /// Whether there was a REX prefix (it changes which byte registers 4 to 7 are).
    rex: bool,
    /// ModRM's register field, or the register in the opcode's low bits.
    reg: u8,
    rm: Rm,
    /// The constant, with its sign; for a jump, where it goes.
    imm: i64,
}

/// Reads the instruction at `rip` from its bytes, if it is one of those translated.
fn read(bytes: &[u8], rip: u64) -> Option<Ins> {
    let mut at = 0;
    let mut next = |n: usize| -> Option<u64> {
        let b = bytes.get(at..at + n)?;
        at += n;
        let mut v = [0u8; 8];
        v[..n].copy_from_slice(b);
        Some(u64::from_le_bytes(v))
    };
    let mut b = next(1)? as u8;
    let mut rex = 0;
    if b & 0xf0 == 0x40 {
        rex = b;
        b = next(1)? as u8;
    }
    let op = if b == 0x0f { 0x100 | next(1)? as u16 } else { b as u16 };
    let wide = rex & 8 != 0;
    let mut ins = Ins { op, len: 0, wide, rex: rex != 0, reg: 0, rm: Rm::Reg(0), imm: 0 };

    // What follows the opcode: a ModRM byte, and a constant of how many bytes.
    let (modrm, imm) = match op {
        0x01 | 0x03 | 0x09 | 0x0b | 0x21 | 0x23 | 0x29 | 0x2b | 0x31 | 0x33 | 0x39 | 0x3b => (true, 0),
        0x63 | 0x85 | 0x89 | 0x8b | 0x8d | 0xff | 0x11f | 0x1b6 | 0x1b7 | 0x1be | 0x1bf => (true, 0),
        0x81 | 0xc7 => (true, 4),
        0x83 => (true, 1),
        0x50..=0x5f | 0x90 | 0xc3 => (false, 0),
        0x68 | 0xe8 | 0xe9 | 0x180..=0x18f => (false, 4),
        0x6a | 0x70..=0x7f | 0xeb => (false, 1),
        0xb8..=0xbf => (false, if wide { 8 } else { 4 }),
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
    } else {
        ins.reg = (op as u8 & 7) | (rex & 1) << 3;
    }
    ins.imm = match imm {
        0 => 0,
        1 => next(1)? as u8 as i8 as i64,
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
    // What is in the list above by its opcode but is not translated in this form.
    let refused = match op {
        // XCHG with R8; and the one field of C7h that is MOV.
        0x90 => rex & 1 != 0,
        0xc7 | 0x11f => ins.reg & 7 != 0 && op == 0xc7,
        // ADC and SBB read the carry.
        0x81 | 0x83 => matches!(ins.reg & 7, 2 | 3),
        // Of FFh only CALL and JMP through the operand.
        0xff => !matches!(ins.reg & 7, 2 | 4),
        // LEA of a register is not an instruction.
        0x8d => matches!(ins.rm, Rm::Reg(_)),
        // Without REX the byte registers 4 to 7 are AH, CH, DH and BH.
        0x1b6 | 0x1be => rex == 0 && matches!(ins.rm, Rm::Reg(4..=7)),
        // A 32-bit push or pop does not exist; 66h is a prefix and is not read.
        _ => false,
    };
    if refused { None } else { Some(ins) }
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

struct Emit {
    w: Vec<u32>,
    flags: Option<HostFlags>,
}

impl Emit {
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
                self.constant(T0, disp as u64);
                self.put(a64::arith(true, false, false, T1, at, T0, 0));
            }
        }
        T1
    }

    /// The record of the flags for an addition or a subtraction, or for a result.
    fn record(&mut self, kind: u64, wide: bool, a: u8, b: Option<u8>) {
        self.put(a64::mov(true, REC_A, a));
        if let Some(b) = b {
            self.put(a64::mov(true, REC_B, b));
        }
        self.put(a64::movz(true, REC_KIND, (kind << 8 | if wide { 64 } else { 32 }) as u16, 0));
    }

    /// One of ADD, OR, AND, SUB, XOR, CMP (by its number among the opcodes 00h to
    /// 3Fh) or TEST (8), of `dst` and the register `src`, with its flags.
    fn alu(&mut self, kind: u8, wide: bool, dst: Rm, src: u8) {
        // The value worked on is the register itself, or memory read into T2; the
        // result goes to the register, or to T0 and from there to memory.
        let (a, to, at) = match dst {
            Rm::Reg(d) => (d, d, None),
            Rm::Mem { base, index, disp } => {
                let at = self.address(base, index, disp);
                self.put(a64::mem_reg(size(wide), Op::Load, T2, BASE, at));
                (T2, T0, Some(at))
            }
        };
        let keeps = !matches!(kind, 7 | 8);
        match kind {
            0 | 5 | 7 => {
                let sub = kind != 0;
                // A register's old value is recorded before it is written over.
                if at.is_none() {
                    self.record(if sub { REC_SUB } else { REC_ADD }, wide, a, Some(src));
                }
                self.put(a64::arith(wide, sub, true, if keeps { to } else { a64::ZR }, a, src, 0));
                self.flags = Some(if sub { HostFlags::Sub } else { HostFlags::Add });
            }
            4 => self.put(a64::logic(wide, Logic::Ands, to, a, src)),
            8 => self.put(a64::logic(wide, Logic::Ands, REC_A, a, src)),
            _ => {
                self.put(a64::logic(wide, if kind == 1 { Logic::Orr } else { Logic::Eor }, to, a, src));
                self.put(a64::logic(wide, Logic::Ands, a64::ZR, to, to));
            }
        }
        if let Some(at) = at {
            if keeps {
                self.put(a64::mem_reg(size(wide), Op::Store, to, BASE, at));
            }
            // Memory's old value is still in T2: recorded when the store is done.
            if matches!(kind, 0 | 5 | 7) {
                self.record(if kind == 0 { REC_ADD } else { REC_SUB }, wide, T2, Some(src));
            }
        }
        if matches!(kind, 1 | 4 | 6 | 8) {
            if kind != 8 {
                self.put(a64::mov(true, REC_A, to));
            }
            self.put(a64::movz(true, REC_KIND, (REC_LOGIC << 8 | if wide { 64 } else { 32 }) as u16, 0));
            self.flags = Some(HostFlags::Logic);
        }
    }

    /// A way out to a constant address, after `n` instructions of this block: a jump
    /// that goes at first to the code after it, which leaves for Rust saying which
    /// jump it was; Rust rewrites the jump to go to the block at that address once
    /// there is one, and from then on this is one jump.
    fn leave(&mut self, rip: u64, n: usize) {
        self.put(a64::arith_imm(true, false, false, RAN, RAN, n as u32, false));
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

fn size(wide: bool) -> Size {
    if wide { Size::X } else { Size::W }
}

/// Translates the block that begins at `rip`. `None` if its first instruction is not
/// one that is translated.
pub fn translate(m: &Mem, rip: u64) -> Option<Built> {
    let mut e = Emit { w: Vec::new(), flags: None };
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
        let (wide, reg) = (ins.wide, ins.reg);
        let mut ended = false;
        match ins.op {
            0x01 | 0x09 | 0x21 | 0x29 | 0x31 | 0x39 => e.alu((ins.op >> 3) as u8, wide, ins.rm, reg),
            0x03 | 0x0b | 0x23 | 0x2b | 0x33 | 0x3b => {
                let src = e.operand(ins.rm, wide);
                e.alu((ins.op >> 3) as u8, wide, Rm::Reg(reg), src);
            }
            0x85 => e.alu(8, wide, ins.rm, reg),
            0x81 | 0x83 => {
                // The constant in T3: an address may need T0 and T1, a value T2.
                e.constant(T3, ins.imm as u64);
                e.alu(reg & 7, wide, ins.rm, T3);
            }
            0x89 => match ins.rm {
                Rm::Reg(d) => e.put(a64::mov(wide, d, reg)),
                Rm::Mem { base, index, disp } => {
                    let a = e.address(base, index, disp);
                    e.put(a64::mem_reg(size(wide), Op::Store, reg, BASE, a));
                }
            },
            0x8b => match ins.rm {
                Rm::Reg(s) => e.put(a64::mov(wide, reg, s)),
                Rm::Mem { base, index, disp } => {
                    let a = e.address(base, index, disp);
                    e.put(a64::mem_reg(size(wide), Op::Load, reg, BASE, a));
                }
            },
            0x8d => {
                let Rm::Mem { base, index, disp } = ins.rm else { unreachable!() };
                let a = e.address(base, index, disp);
                e.put(a64::mov(wide, reg, a));
            }
            0xc7 => match ins.rm {
                Rm::Reg(d) => e.constant(d, if wide { ins.imm as u64 } else { ins.imm as u32 as u64 }),
                Rm::Mem { base, index, disp } => {
                    e.constant(T3, ins.imm as u64);
                    let a = e.address(base, index, disp);
                    e.put(a64::mem_reg(size(wide), Op::Store, T3, BASE, a));
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
                let src = e.operand(ins.rm, false);
                e.put(a64::mov(false, reg, src));
            }
            0x1b6 | 0x1b7 | 0x1be | 0x1bf => {
                let (bits, small) = if ins.op & 1 == 0 { (8, Size::B) } else { (16, Size::H) };
                let signed = ins.op >= 0x1be;
                match ins.rm {
                    Rm::Reg(s) if signed => e.put(a64::sxt(wide, reg, s, bits)),
                    Rm::Reg(s) => e.put(a64::uxt(reg, s, bits)),
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
            0x90 | 0x11f => {}
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
                if reg & 7 == 2 {
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
                let jump = e.flags.and_then(|f| condition(ins.op as u8 & 15, f));
                let Some(jump) = jump else {
                    // The host's flags do not say: the interpreter's instruction.
                    e.w.truncate(before);
                    starts.pop();
                    if starts.is_empty() {
                        return None;
                    }
                    e.leave(at, starts.len());
                    break;
                };
                let n = starts.len();
                match jump {
                    Jump::Always => e.leave(ins.imm as u64, n),
                    Jump::Never => e.leave(next, n),
                    Jump::If(cond) => {
                        // Over the way out for "not taken" to the one for "taken": the
                        // jump is written when it is known how far that is.
                        let jump_at = e.w.len();
                        e.put(a64::nop());
                        e.leave(next, n);
                        e.w[jump_at] = a64::b_cond(cond, (e.w.len() - jump_at) as i32);
                        e.leave(ins.imm as u64, n);
                    }
                }
                ended = true;
            }
            _ => unreachable!("read gave an instruction that is not translated"),
        }
        at = next;
        if ended {
            break;
        }
    }
    Some(Built { words: e.w, starts, end: at })
}

impl Emit {
    /// The register that holds a source operand: the guest's own, or memory read into
    /// `T0`.
    fn operand(&mut self, rm: Rm, wide: bool) -> u8 {
        match rm {
            Rm::Reg(s) => s,
            Rm::Mem { base, index, disp } => {
                let a = self.address(base, index, disp);
                self.put(a64::mem_reg(size(wide), Op::Load, T0, BASE, a));
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
        assert_eq!((i.op, i.len, i.wide, i.reg, i.rm), (0x01, 2, false, 3, Rm::Reg(0)));
        let i = ins(&[0x4c, 0x01, 0xd8]);
        assert_eq!((i.len, i.wide, i.reg, i.rm), (3, true, 11, Rm::Reg(0)));
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
        // The registers in an opcode's low bits.
        assert_eq!(ins(&[0x41, 0x57]).reg, 15);
        assert_eq!(ins(&[0x5d]).reg, 5);
    }

    #[test]
    fn what_is_not_translated_is_not_read() {
        let no = |bytes: &[u8]| assert_eq!(read(bytes, 0x40_1000), None, "{bytes:02x?}");
        no(&[0x66, 0x01, 0xd8]); // a 16-bit operand
        no(&[0x00, 0xd8]); // an 8-bit one
        no(&[0xf3, 0x90]); // a prefix
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
