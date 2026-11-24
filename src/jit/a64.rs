//! The arm64 instructions the JIT writes, each a function that gives its 32 bits
//! (ADR 0014, point 10). Only the forms used; each is held by a test to what the
//! system's assembler gives for the same instruction.
//!
//! A register is its number, 0 to 30; 31 is the zero register or the stack pointer, as
//! the instruction has it. `sf` chooses the 64-bit form (X registers) over the 32-bit
//! one (W registers), which clears the upper half of what it writes.

pub const ZR: u8 = 31;

/// The conditions, by their numbers in an instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cond {
    Eq = 0,
    Ne = 1,
    Hs = 2,
    Lo = 3,
    Mi = 4,
    Pl = 5,
    Vs = 6,
    Vc = 7,
    Hi = 8,
    Ls = 9,
    Ge = 10,
    Lt = 11,
    Gt = 12,
    Le = 13,
}

impl Cond {
    /// The condition that holds when this one does not.
    pub fn not(self) -> Cond {
        // A condition and its opposite differ in the lowest bit.
        unsafe { std::mem::transmute(self as u8 ^ 1) }
    }
}

fn r(reg: u8) -> u32 {
    debug_assert!(reg < 32);
    reg as u32
}

fn sf(wide: bool) -> u32 {
    (wide as u32) << 31
}

/// MOVZ, MOVN and MOVK: sixteen bits put at `shift` (0, 16, 32 or 48), the rest of
/// the register zero, ones, or kept.
pub fn movz(wide: bool, rd: u8, imm: u16, shift: u32) -> u32 {
    sf(wide) | 0x5280_0000 | (shift / 16) << 21 | (imm as u32) << 5 | r(rd)
}

pub fn movn(wide: bool, rd: u8, imm: u16, shift: u32) -> u32 {
    sf(wide) | 0x1280_0000 | (shift / 16) << 21 | (imm as u32) << 5 | r(rd)
}

pub fn movk(wide: bool, rd: u8, imm: u16, shift: u32) -> u32 {
    sf(wide) | 0x7280_0000 | (shift / 16) << 21 | (imm as u32) << 5 | r(rd)
}

/// A register to a register.
pub fn mov(wide: bool, rd: u8, rm: u8) -> u32 {
    logic(wide, Logic::Orr, rd, ZR, rm)
}

/// ADD or SUB of two registers, the second shifted left by `lsl`; with `flags`, ADDS
/// or SUBS.
pub fn arith(wide: bool, sub: bool, flags: bool, rd: u8, rn: u8, rm: u8, lsl: u32) -> u32 {
    sf(wide) | (sub as u32) << 30 | (flags as u32) << 29 | 0x0b00_0000 | r(rm) << 16 | lsl << 10 | r(rn) << 5 | r(rd)
}

/// ADD or SUB of a register and a constant of twelve bits, or of twelve bits moved up
/// by twelve (`high`). Register 31 is here the stack pointer, except as the result of
/// one that sets the flags.
pub fn arith_imm(wide: bool, sub: bool, flags: bool, rd: u8, rn: u8, imm: u32, high: bool) -> u32 {
    debug_assert!(imm < 4096);
    sf(wide) | (sub as u32) << 30 | (flags as u32) << 29 | 0x1100_0000 | (high as u32) << 22 | imm << 10 | r(rn) << 5 | r(rd)
}

#[derive(Clone, Copy)]
pub enum Logic {
    And = 0,
    Orr = 1,
    Eor = 2,
    /// AND that sets the flags: N and Z from the result, C and V cleared.
    Ands = 3,
}

pub fn logic(wide: bool, op: Logic, rd: u8, rn: u8, rm: u8) -> u32 {
    sf(wide) | (op as u32) << 29 | 0x0a00_0000 | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// Every bit of a register turned over (ORN with the zero register).
pub fn mvn(wide: bool, rd: u8, rm: u8) -> u32 {
    logic(wide, Logic::Orr, rd, ZR, rm) | 1 << 21
}

/// How much memory a load or a store is of: its `size` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    B = 0,
    H = 1,
    W = 2,
    X = 3,
}

/// What a load does with the bits above what it read, or that it is a store.
#[derive(Clone, Copy)]
pub enum Mem {
    Store = 0,
    /// The rest of the register zero.
    Load = 1,
    /// The rest of the 64-bit register the sign.
    LoadSigned = 2,
}

/// A load or a store at the sum of two registers.
pub fn mem_reg(size: Size, op: Mem, rt: u8, rn: u8, rm: u8) -> u32 {
    (size as u32) << 30 | 0x3820_6800 | (op as u32) << 22 | r(rm) << 16 | r(rn) << 5 | r(rt)
}

/// A load or a store at a register and a constant, which is a multiple of the size and
/// less than 4096 of them.
pub fn mem_imm(size: Size, op: Mem, rt: u8, rn: u8, offset: u32) -> u32 {
    let scaled = offset >> size as u32;
    debug_assert!(scaled << size as u32 == offset && scaled < 4096);
    (size as u32) << 30 | 0x3900_0000 | (op as u32) << 22 | scaled << 10 | r(rn) << 5 | r(rt)
}

fn bitfield(wide: bool, signed: bool, rd: u8, rn: u8, immr: u32, imms: u32) -> u32 {
    sf(wide) | if signed { 0x1300_0000 } else { 0x5300_0000 } | (wide as u32) << 22 | immr << 16 | imms << 10 | r(rn) << 5 | r(rd)
}

/// Shifts by a constant that is less than the register's width (and for LSL more than
/// nothing is not asked: a shift of zero is a move).
pub fn lsl(wide: bool, rd: u8, rn: u8, by: u32) -> u32 {
    let bits = if wide { 64 } else { 32 };
    bitfield(wide, false, rd, rn, (bits - by) % bits, bits - 1 - by)
}

pub fn lsr(wide: bool, rd: u8, rn: u8, by: u32) -> u32 {
    bitfield(wide, false, rd, rn, by, if wide { 63 } else { 31 })
}

pub fn asr(wide: bool, rd: u8, rn: u8, by: u32) -> u32 {
    bitfield(wide, true, rd, rn, by, if wide { 63 } else { 31 })
}

/// The low `bits` (8, 16 or 32) of a register with their sign, to all of a register.
pub fn sxt(wide: bool, rd: u8, rn: u8, bits: u32) -> u32 {
    bitfield(wide, true, rd, rn, 0, bits - 1)
}

/// The low `bits` (8 or 16) of a register, the rest zero.
pub fn uxt(rd: u8, rn: u8, bits: u32) -> u32 {
    bitfield(false, false, rd, rn, 0, bits - 1)
}

pub fn mul(wide: bool, rd: u8, rn: u8, rm: u8) -> u32 {
    sf(wide) | 0x1b00_7c00 | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// A jump by `words` instructions from this one, forward or back.
pub fn b(words: i32) -> u32 {
    debug_assert!((-(1 << 25)..1 << 25).contains(&words));
    0x1400_0000 | (words as u32 & 0x03ff_ffff)
}

pub fn b_cond(cond: Cond, words: i32) -> u32 {
    debug_assert!((-(1 << 18)..1 << 18).contains(&words));
    0x5400_0000 | (words as u32 & 0x7_ffff) << 5 | cond as u32
}

/// A jump if a register is zero, or (`nonzero`) if it is not.
pub fn cbz(wide: bool, nonzero: bool, rt: u8, words: i32) -> u32 {
    debug_assert!((-(1 << 18)..1 << 18).contains(&words));
    sf(wide) | 0x3400_0000 | (nonzero as u32) << 24 | (words as u32 & 0x7_ffff) << 5 | r(rt)
}

pub fn br(rn: u8) -> u32 {
    0xd61f_0000 | r(rn) << 5
}

pub fn ret() -> u32 {
    0xd65f_03c0
}

/// `rn` if the condition holds, else `rm`.
pub fn csel(wide: bool, rd: u8, rn: u8, rm: u8, cond: Cond) -> u32 {
    sf(wide) | 0x1a80_0000 | r(rm) << 16 | (cond as u32) << 12 | r(rn) << 5 | r(rd)
}

/// One if the condition holds, else zero.
pub fn cset(wide: bool, rd: u8, cond: Cond) -> u32 {
    sf(wide) | 0x1a9f_07e0 | (cond.not() as u32) << 12 | r(rd)
}

pub fn nop() -> u32 {
    0xd503_201f
}

/// A trap: what is put where nothing should ever run.
pub fn brk() -> u32 {
    0xd420_0000
}

/// The instructions that put any 64-bit number in a register: one to four of them,
/// written into `out`; how many is given back.
pub fn mov_imm(rd: u8, value: u64, out: &mut [u32; 4]) -> usize {
    let parts: [u16; 4] = std::array::from_fn(|i| (value >> (16 * i)) as u16);
    let mut n = 0;
    for (i, part) in parts.into_iter().enumerate() {
        if part != 0 {
            let shift = 16 * i as u32;
            out[n] = if n == 0 { movz(true, rd, part, shift) } else { movk(true, rd, part, shift) };
            n += 1;
        }
    }
    if n == 0 {
        out[0] = movz(true, rd, 0, 0);
        n = 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each line: what this file gives, and what `clang -c` gave for the instruction
    /// named (Apple clang, 2026-11-24; `otool -t` for the bits).
    #[test]
    fn the_bits_are_the_assemblers() {
        let cases: &[(u32, u32, &str)] = &[
            (movz(true, 5, 0x1234, 0), 0xd2824685, "movz x5, #0x1234"),
            (movz(true, 5, 0x1234, 16), 0xd2a24685, "movz x5, #0x1234, lsl #16"),
            (movz(true, 17, 0xffff, 48), 0xd2fffff1, "movz x17, #0xffff, lsl #48"),
            (movk(true, 5, 0xabcd, 32), 0xf2d579a5, "movk x5, #0xabcd, lsl #32"),
            (movn(true, 3, 0, 0), 0x92800003, "movn x3, #0"),
            (movz(false, 9, 7, 0), 0x528000e9, "movz w9, #7"),
            (mov(true, 3, 14), 0xaa0e03e3, "mov x3, x14"),
            (mov(false, 3, 14), 0x2a0e03e3, "mov w3, w14"),
            (arith(true, false, false, 1, 2, 3, 0), 0x8b030041, "add x1, x2, x3"),
            (arith(false, false, false, 1, 2, 3, 0), 0x0b030041, "add w1, w2, w3"),
            (arith(true, false, false, 16, 4, 9, 3), 0x8b090c90, "add x16, x4, x9, lsl #3"),
            (arith(true, true, false, 1, 2, 3, 0), 0xcb030041, "sub x1, x2, x3"),
            (arith(false, true, false, 15, 0, 7, 0), 0x4b07000f, "sub w15, w0, w7"),
            (arith(true, false, true, 1, 2, 3, 0), 0xab030041, "adds x1, x2, x3"),
            (arith(false, false, true, 1, 2, 3, 0), 0x2b030041, "adds w1, w2, w3"),
            (arith(true, true, true, 1, 2, 3, 0), 0xeb030041, "subs x1, x2, x3"),
            (arith(false, true, true, 1, 2, 3, 0), 0x6b030041, "subs w1, w2, w3"),
            (arith(true, true, true, ZR, 2, 3, 0), 0xeb03005f, "cmp x2, x3"),
            (arith(false, true, true, ZR, 2, 3, 0), 0x6b03005f, "cmp w2, w3"),
            (arith_imm(true, false, false, 1, 2, 0xfff, false), 0x913ffc41, "add x1, x2, #0xfff"),
            (arith_imm(true, false, false, 1, 2, 5, true), 0x91401441, "add x1, x2, #0x5, lsl #12"),
            (arith_imm(true, true, false, 1, 2, 1, false), 0xd1000441, "sub x1, x2, #1"),
            (arith_imm(false, true, false, 1, 2, 1, false), 0x51000441, "sub w1, w2, #1"),
            (arith_imm(true, false, true, 1, 2, 8, false), 0xb1002041, "adds x1, x2, #8"),
            (arith_imm(false, false, true, 1, 2, 8, false), 0x31002041, "adds w1, w2, #8"),
            (arith_imm(true, true, true, 1, 2, 8, false), 0xf1002041, "subs x1, x2, #8"),
            (arith_imm(false, true, true, 1, 2, 8, false), 0x71002041, "subs w1, w2, #8"),
            (arith_imm(true, true, true, ZR, 2, 8, false), 0xf100205f, "cmp x2, #8"),
            (arith_imm(false, true, true, ZR, 2, 0x123, false), 0x71048c5f, "cmp w2, #0x123"),
            (logic(true, Logic::And, 1, 2, 3), 0x8a030041, "and x1, x2, x3"),
            (logic(false, Logic::And, 1, 2, 3), 0x0a030041, "and w1, w2, w3"),
            (logic(true, Logic::Orr, 1, 2, 3), 0xaa030041, "orr x1, x2, x3"),
            (logic(false, Logic::Orr, 1, 2, 3), 0x2a030041, "orr w1, w2, w3"),
            (logic(true, Logic::Eor, 1, 2, 3), 0xca030041, "eor x1, x2, x3"),
            (logic(false, Logic::Eor, 1, 2, 3), 0x4a030041, "eor w1, w2, w3"),
            (logic(true, Logic::Ands, 1, 2, 3), 0xea030041, "ands x1, x2, x3"),
            (logic(false, Logic::Ands, 1, 2, 3), 0x6a030041, "ands w1, w2, w3"),
            (logic(true, Logic::Ands, ZR, 2, 3), 0xea03005f, "tst x2, x3"),
            (logic(false, Logic::Ands, ZR, 2, 3), 0x6a03005f, "tst w2, w3"),
            (mvn(true, 1, 3), 0xaa2303e1, "mvn x1, x3"),
            (mvn(false, 1, 3), 0x2a2303e1, "mvn w1, w3"),
            (arith(true, true, false, 1, ZR, 3, 0), 0xcb0303e1, "neg x1, x3"),
            (mem_reg(Size::X, Mem::Load, 1, 20, 17), 0xf8716a81, "ldr x1, [x20, x17]"),
            (mem_reg(Size::W, Mem::Load, 1, 20, 17), 0xb8716a81, "ldr w1, [x20, x17]"),
            (mem_reg(Size::H, Mem::Load, 1, 20, 17), 0x78716a81, "ldrh w1, [x20, x17]"),
            (mem_reg(Size::B, Mem::Load, 1, 20, 17), 0x38716a81, "ldrb w1, [x20, x17]"),
            (mem_reg(Size::X, Mem::Store, 1, 20, 17), 0xf8316a81, "str x1, [x20, x17]"),
            (mem_reg(Size::W, Mem::Store, 1, 20, 17), 0xb8316a81, "str w1, [x20, x17]"),
            (mem_reg(Size::H, Mem::Store, 1, 20, 17), 0x78316a81, "strh w1, [x20, x17]"),
            (mem_reg(Size::B, Mem::Store, 1, 20, 17), 0x38316a81, "strb w1, [x20, x17]"),
            (mem_reg(Size::W, Mem::LoadSigned, 1, 20, 17), 0xb8b16a81, "ldrsw x1, [x20, x17]"),
            (mem_reg(Size::H, Mem::LoadSigned, 1, 20, 17), 0x78b16a81, "ldrsh x1, [x20, x17]"),
            (mem_reg(Size::B, Mem::LoadSigned, 1, 20, 17), 0x38b16a81, "ldrsb x1, [x20, x17]"),
            (mem_imm(Size::X, Mem::Load, 1, 19, 0x88), 0xf9404661, "ldr x1, [x19, #0x88]"),
            (mem_imm(Size::X, Mem::Store, 1, 19, 0x88), 0xf9004661, "str x1, [x19, #0x88]"),
            (mem_imm(Size::W, Mem::Load, 1, 19, 0x84), 0xb9408661, "ldr w1, [x19, #0x84]"),
            (mem_imm(Size::W, Mem::Store, 1, 19, 0x84), 0xb9008661, "str w1, [x19, #0x84]"),
            (lsl(true, 1, 2, 5), 0xd37be841, "lsl x1, x2, #5"),
            (lsl(false, 1, 2, 5), 0x531b6841, "lsl w1, w2, #5"),
            (lsr(true, 1, 2, 5), 0xd345fc41, "lsr x1, x2, #5"),
            (lsr(false, 1, 2, 5), 0x53057c41, "lsr w1, w2, #5"),
            (asr(true, 1, 2, 5), 0x9345fc41, "asr x1, x2, #5"),
            (asr(false, 1, 2, 5), 0x13057c41, "asr w1, w2, #5"),
            (sxt(true, 1, 2, 32), 0x93407c41, "sxtw x1, w2"),
            (sxt(true, 1, 2, 8), 0x93401c41, "sxtb x1, w2"),
            (sxt(true, 1, 2, 16), 0x93403c41, "sxth x1, w2"),
            (sxt(false, 1, 2, 8), 0x13001c41, "sxtb w1, w2"),
            (sxt(false, 1, 2, 16), 0x13003c41, "sxth w1, w2"),
            (uxt(1, 2, 8), 0x53001c41, "uxtb w1, w2"),
            (uxt(1, 2, 16), 0x53003c41, "uxth w1, w2"),
            (mul(true, 1, 2, 3), 0x9b037c41, "mul x1, x2, x3"),
            (mul(false, 1, 2, 3), 0x1b037c41, "mul w1, w2, w3"),
            (b(0x40), 0x14000040, "b .+0x100"),
            (b(-0x10), 0x17fffff0, "b .-0x40"),
            (b_cond(Cond::Eq, 8), 0x54000100, "b.eq .+0x20"),
            (b_cond(Cond::Ne, -8), 0x54ffff01, "b.ne .-0x20"),
            (b_cond(Cond::Lo, 2), 0x54000043, "b.lo .+8"),
            (b_cond(Cond::Gt, 0x400), 0x5400800c, "b.gt .+0x1000"),
            (cbz(true, false, 3, 4), 0xb4000083, "cbz x3, .+0x10"),
            (cbz(false, true, 3, -4), 0x35ffff83, "cbnz w3, .-0x10"),
            (br(16), 0xd61f0200, "br x16"),
            (ret(), 0xd65f03c0, "ret"),
            (csel(true, 1, 2, 3, Cond::Eq), 0x9a830041, "csel x1, x2, x3, eq"),
            (csel(false, 1, 2, 3, Cond::Lt), 0x1a83b041, "csel w1, w2, w3, lt"),
            (cset(false, 1, Cond::Hi), 0x1a9f97e1, "cset w1, hi"),
            (cset(true, 1, Cond::Vs), 0x9a9f77e1, "cset x1, vs"),
            (nop(), 0xd503201f, "nop"),
            (brk(), 0xd4200000, "brk #0"),
        ];
        for (ours, theirs, name) in cases {
            assert_eq!(ours, theirs, "{name}: {ours:08x}, the assembler {theirs:08x}");
        }
        assert_eq!(cases.len(), 88);
    }

    #[test]
    fn a_condition_and_its_opposite() {
        use Cond::*;
        for (a, b) in [(Eq, Ne), (Hs, Lo), (Mi, Pl), (Vs, Vc), (Hi, Ls), (Ge, Lt), (Gt, Le)] {
            assert_eq!(a.not(), b);
            assert_eq!(b.not(), a);
        }
    }

    #[test]
    fn any_number_in_at_most_four_instructions() {
        let mut out = [0; 4];
        assert_eq!(mov_imm(5, 0, &mut out), 1);
        assert_eq!(out[0], movz(true, 5, 0, 0));
        assert_eq!(mov_imm(5, 0x1234, &mut out), 1);
        assert_eq!(out[0], 0xd2824685);
        // A part that is zero in the middle is passed over.
        assert_eq!(mov_imm(5, 0xabcd_0000_1234, &mut out), 2);
        assert_eq!(out[..2], [0xd2824685, 0xf2d579a5]);
        assert_eq!(mov_imm(7, 0x1111_2222_3333_4444, &mut out), 4);
        assert_eq!(out, [movz(true, 7, 0x4444, 0), movk(true, 7, 0x3333, 16), movk(true, 7, 0x2222, 32), movk(true, 7, 0x1111, 48)]);
        // The first that is not zero is the MOVZ, wherever it is.
        assert_eq!(mov_imm(5, 0x1234 << 16, &mut out), 1);
        assert_eq!(out[0], 0xd2a24685);
    }
}
