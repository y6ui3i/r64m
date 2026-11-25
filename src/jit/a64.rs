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

/// ADCS, or SBCS (`sub`): with the host's carry, and setting its flags.
pub fn with_carry(wide: bool, sub: bool, rd: u8, rn: u8, rm: u8) -> u32 {
    sf(wide) | (sub as u32) << 30 | 0x3a00_0000 | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// A compare of a register with another shifted right by `by`, its sign kept.
pub fn cmp_asr(rn: u8, rm: u8, by: u32) -> u32 {
    arith(true, true, true, ZR, rn, rm, by) | 2 << 22
}

/// OR of a 64-bit register and another moved left by `by`.
pub fn orr_lsl(rd: u8, rn: u8, rm: u8, by: u32) -> u32 {
    logic(true, Logic::Orr, rd, rn, rm) | by << 10
}

/// The upper half of the product of two 64-bit registers.
pub fn mulh(signed: bool, rd: u8, rn: u8, rm: u8) -> u32 {
    (if signed { 0x9b40_7c00 } else { 0x9bc0_7c00 }) | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// The 64-bit product of two 32-bit registers.
pub fn mull(signed: bool, rd: u8, rn: u8, rm: u8) -> u32 {
    (if signed { 0x9b20_7c00 } else { 0x9ba0_7c00 }) | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// A quotient, cut off toward zero; a divisor of zero gives zero.
pub fn div(wide: bool, signed: bool, rd: u8, rn: u8, rm: u8) -> u32 {
    sf(wide) | (if signed { 0x1ac0_0c00 } else { 0x1ac0_0800 }) | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// `ra` less the product of `rn` and `rm`.
pub fn msub(wide: bool, rd: u8, rn: u8, rm: u8, ra: u8) -> u32 {
    sf(wide) | 0x1b00_8000 | r(rm) << 16 | r(ra) << 10 | r(rn) << 5 | r(rd)
}

/// A register's bytes end for end.
pub fn rev(wide: bool, rd: u8, rn: u8) -> u32 {
    (if wide { 0xdac0_0c00 } else { 0x5ac0_0800 }) | r(rn) << 5 | r(rd)
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

/// A load that sees every store released before it, and a store that releases: at a
/// register's address and nowhere else. Between threads these keep the order x86
/// keeps (ADR 0019, point D). The rest of the register a load writes is zero.
pub fn ldapr(size: Size, rt: u8, rn: u8) -> u32 {
    (size as u32) << 30 | 0x38bf_c000 | r(rn) << 5 | r(rt)
}

pub fn stlr(size: Size, rt: u8, rn: u8) -> u32 {
    (size as u32) << 30 | 0x089f_fc00 | r(rn) << 5 | r(rt)
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

/// A shift or a rotation to the right by a register's low bits (five of them for a
/// 32-bit register, six for a 64-bit one): LSLV, LSRV, ASRV, RORV.
#[derive(Clone, Copy)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
    Ror = 3,
}

pub fn shift_reg(wide: bool, op: Shift, rd: u8, rn: u8, rm: u8) -> u32 {
    sf(wide) | 0x1ac0_2000 | (op as u32) << 10 | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// A rotation to the right by a constant less than the register's width.
pub fn ror(wide: bool, rd: u8, rn: u8, by: u32) -> u32 {
    sf(wide) | 0x1380_0000 | (wide as u32) << 22 | r(rn) << 16 | by << 10 | r(rn) << 5 | r(rd)
}

/// How many zero bits a register has above its highest one; and its bits end for end.
pub fn clz(wide: bool, rd: u8, rn: u8) -> u32 {
    sf(wide) | 0x5ac0_1000 | r(rn) << 5 | r(rd)
}

pub fn rbit(wide: bool, rd: u8, rn: u8) -> u32 {
    sf(wide) | 0x5ac0_0000 | r(rn) << 5 | r(rd)
}

/// The low `width` bits of `rn` put into `rd` at bit `lsb`, the rest of `rd` kept.
pub fn bfi(rd: u8, rn: u8, lsb: u32, width: u32) -> u32 {
    0xb340_0000 | (64 - lsb) % 64 << 16 | (width - 1) << 10 | r(rn) << 5 | r(rd)
}

/// `width` bits of a 64-bit register from bit `lsb` up, the rest zero.
pub fn ubfx(rd: u8, rn: u8, lsb: u32, width: u32) -> u32 {
    bitfield(true, false, rd, rn, lsb, lsb + width - 1)
}

/// `width` bits of a register from bit `lsb` up, with their sign.
pub fn sbfx(wide: bool, rd: u8, rn: u8, lsb: u32, width: u32) -> u32 {
    bitfield(wide, true, rd, rn, lsb, lsb + width - 1)
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

/// The address `bytes` from this instruction, into a register.
pub fn adr(rd: u8, bytes: i32) -> u32 {
    debug_assert!((-(1 << 20)..1 << 20).contains(&bytes));
    0x1000_0000 | (bytes as u32 & 3) << 29 | (bytes as u32 >> 2 & 0x7_ffff) << 5 | r(rd)
}

/// A jump by `words` if the top bit of a 64-bit register is set.
pub fn tbnz_top(rt: u8, words: i32) -> u32 {
    debug_assert!((-(1 << 13)..1 << 13).contains(&words));
    0xb7f8_0000 | (words as u32 & 0x3fff) << 5 | r(rt)
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

// ---- Floating point and the vector registers (ADR 0015) -----------------------------------
//
// A vector register is its number, 0 to 31. `double` chooses the 64-bit element (D
// registers) over the 32-bit one (S registers).

fn ty(double: bool) -> u32 {
    (double as u32) << 22
}

/// The four of arithmetic with two operands, by their numbers in an instruction.
#[derive(Clone, Copy)]
pub enum Fp {
    Mul = 0,
    Div = 1,
    Add = 2,
    Sub = 3,
}

pub fn fp_arith(double: bool, op: Fp, rd: u8, rn: u8, rm: u8) -> u32 {
    0x1e20_0800 | ty(double) | r(rm) << 16 | (op as u32) << 12 | r(rn) << 5 | r(rd)
}

pub fn fsqrt(double: bool, rd: u8, rn: u8) -> u32 {
    0x1e21_c000 | ty(double) | r(rn) << 5 | r(rd)
}

/// A comparison that sets the host's flags: less 1000, equal 0110, greater 0010, and
/// 0011 where either is not a number (R019).
pub fn fcmp(double: bool, rn: u8, rm: u8) -> u32 {
    0x1e20_2000 | ty(double) | r(rm) << 16 | r(rn) << 5
}

/// The same comparison if the condition holds; else the flags are `nzcv`.
pub fn fccmp(double: bool, rn: u8, rm: u8, nzcv: u32, cond: Cond) -> u32 {
    0x1e20_0400 | ty(double) | r(rm) << 16 | (cond as u32) << 12 | r(rn) << 5 | nzcv
}

/// `rn` if the condition holds, else `rm`.
pub fn fcsel(double: bool, rd: u8, rn: u8, rm: u8, cond: Cond) -> u32 {
    0x1e20_0c00 | ty(double) | r(rm) << 16 | (cond as u32) << 12 | r(rn) << 5 | r(rd)
}

/// A single to a double (`to_double`), or a double to a single.
pub fn fcvt(to_double: bool, rd: u8, rn: u8) -> u32 {
    (if to_double { 0x1e22_c000 } else { 0x1e62_4000 }) | r(rn) << 5 | r(rd)
}

/// A signed integer in a general register (64 bits if `wide`) to a float.
pub fn scvtf(wide: bool, double: bool, rd: u8, rn: u8) -> u32 {
    sf(wide) | 0x1e22_0000 | ty(double) | r(rn) << 5 | r(rd)
}

/// A float to a signed integer in a general register, cut off toward zero; what does
/// not fit is the largest or the smallest integer, and what is not a number is zero.
pub fn fcvtzs(wide: bool, double: bool, rd: u8, rn: u8) -> u32 {
    sf(wide) | 0x1e38_0000 | ty(double) | r(rn) << 5 | r(rd)
}

/// The bits of a general register to a vector register's low element (64 bits if
/// `wide`, else 32), the rest of it zero; and the other way.
pub fn fmov_to_fp(wide: bool, rd: u8, rn: u8) -> u32 {
    (if wide { 0x9e67_0000 } else { 0x1e27_0000 }) | r(rn) << 5 | r(rd)
}

pub fn fmov_from_fp(wide: bool, rd: u8, rn: u8) -> u32 {
    (if wide { 0x9e66_0000 } else { 0x1e26_0000 }) | r(rn) << 5 | r(rd)
}

/// The low element of a vector register to another's, the rest of that one zero.
pub fn fmov(double: bool, rd: u8, rn: u8) -> u32 {
    0x1e20_4000 | ty(double) | r(rn) << 5 | r(rd)
}

/// The low element of a vector register to another's, the rest of that one kept.
pub fn ins_low(double: bool, rd: u8, rn: u8) -> u32 {
    (if double { 0x6e08_0400 } else { 0x6e04_0400 }) | r(rn) << 5 | r(rd)
}

/// The low 64-bit element of a vector register to another's upper one, the lower
/// half of that one kept.
pub fn ins_high(rd: u8, rn: u8) -> u32 {
    0x6e18_0400 | r(rn) << 5 | r(rd)
}

/// The logic of all 128 bits of two vector registers. `Bic` is `rn` and not `rm`.
#[derive(Clone, Copy)]
pub enum VLogic {
    And = 0x4e20_1c00,
    Bic = 0x4e60_1c00,
    Orr = 0x4ea0_1c00,
    Eor = 0x6e20_1c00,
}

pub fn vlogic(op: VLogic, rd: u8, rn: u8, rm: u8) -> u32 {
    op as u32 | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// The 64-bit element above the lowest of a vector register, from a general register
/// (the lowest kept) and to one.
pub fn ins_high_from(rd: u8, rn: u8) -> u32 {
    0x4e18_1c00 | r(rn) << 5 | r(rd)
}

pub fn high_to(rd: u8, rn: u8) -> u32 {
    0x4e18_3c00 | r(rn) << 5 | r(rd)
}

/// How much of a vector register a load or a store is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FpSize {
    S,
    D,
    Q,
}

/// A load (the rest of the register zero) or a store of a vector register at the sum
/// of two general registers.
pub fn fp_mem(size: FpSize, load: bool, rt: u8, rn: u8, rm: u8) -> u32 {
    let form = match size {
        FpSize::S => 0xbc20_6800,
        FpSize::D => 0xfc20_6800,
        FpSize::Q => 0x3ca0_6800,
    };
    form | (load as u32) << 22 | r(rm) << 16 | r(rn) << 5 | r(rt)
}

/// The size of the elements a vector instruction works on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    B = 0,
    H = 1,
    S = 2,
    D = 3,
}

/// The vector instructions of whole numbers with two operands, element by element:
/// sum, difference, all ones where equal, all ones where the first is the greater
/// (signed); and the ones that lay elements side by side: ZIP1 and ZIP2 take the
/// lower and the upper halves of both in turn, UZP1 and UZP2 the even and the odd
/// elements of the first and then of the second.
#[derive(Clone, Copy)]
pub enum V3 {
    Add = 0x4e20_8400,
    Sub = 0x6e20_8400,
    Cmeq = 0x6e20_8c00,
    Cmgt = 0x4e20_3400,
    Zip1 = 0x4e00_3800,
    Zip2 = 0x4e00_7800,
    Uzp1 = 0x4e00_1800,
    Uzp2 = 0x4e00_5800,
}

pub fn vec3(op: V3, lane: Lane, rd: u8, rn: u8, rm: u8) -> u32 {
    op as u32 | (lane as u32) << 22 | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// The lower halves of two 64-bit elements, as two of 32 bits in the lower half of
/// the register written (the rest zero).
pub fn xtn_2d(rd: u8, rn: u8) -> u32 {
    0x0ea1_2800 | r(rn) << 5 | r(rd)
}

/// The two 64-bit products of the two lower 32-bit elements of each, unsigned.
pub fn umull_2d(rd: u8, rn: u8, rm: u8) -> u32 {
    0x2ea0_c000 | r(rm) << 16 | r(rn) << 5 | r(rd)
}

/// 32-bit element `j` of `rn` to element `i` of `rd`, the rest of it kept; and to all
/// four of `rd`.
pub fn ins_s(rd: u8, i: u32, rn: u8, j: u32) -> u32 {
    0x6e00_0400 | (i << 3 | 4) << 16 | j << 13 | r(rn) << 5 | r(rd)
}

pub fn dup_s(rd: u8, rn: u8, j: u32) -> u32 {
    0x4e00_0400 | (j << 3 | 4) << 16 | r(rn) << 5 | r(rd)
}

/// Every element shifted by a constant: to the right without and with its sign (by
/// 1 to the element's width), and to the left (by less than the width).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VShift {
    Ushr,
    Sshr,
    Shl,
}

pub fn vshift(op: VShift, lane: Lane, rd: u8, rn: u8, by: u32) -> u32 {
    let bits = 8 << lane as u32;
    let (form, field) = match op {
        VShift::Ushr => (0x6f00_0400, 2 * bits - by),
        VShift::Sshr => (0x4f00_0400, 2 * bits - by),
        VShift::Shl => (0x4f00_5400, bits + by),
    };
    form | field << 16 | r(rn) << 5 | r(rd)
}

/// Sixteen bytes from byte `from` of `rn` followed by `rm`: a move of the whole
/// register by bytes, with what comes in from the other.
pub fn ext(rd: u8, rn: u8, rm: u8, from: u32) -> u32 {
    debug_assert!(from < 16);
    0x6e00_0000 | r(rm) << 16 | from << 11 | r(rn) << 5 | r(rd)
}

/// A store of the lower halves of two vector registers, and a load of all of two, at a
/// register (31: the stack pointer) and a constant that is a multiple of their size.
pub fn stp_d(rt: u8, rt2: u8, rn: u8, offset: u32) -> u32 {
    debug_assert!(offset % 8 == 0 && offset < 512);
    0x6d00_0000 | offset / 8 << 15 | r(rt2) << 10 | r(rn) << 5 | r(rt)
}

pub fn ldp_q(rt: u8, rt2: u8, rn: u8, offset: u32) -> u32 {
    debug_assert!(offset % 16 == 0 && offset < 1024);
    0xad40_0000 | offset / 16 << 15 | r(rt2) << 10 | r(rn) << 5 | r(rt)
}

/// A compare of a register with the negative of a small constant if the condition
/// holds; else the flags are `nzcv`.
pub fn ccmn_imm(wide: bool, rn: u8, imm: u32, nzcv: u32, cond: Cond) -> u32 {
    sf(wide) | 0x3a40_0800 | imm << 16 | (cond as u32) << 12 | r(rn) << 5 | nzcv
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
            (shift_reg(true, Shift::Lsl, 1, 2, 3), 0x9ac32041, "lslv x1, x2, x3"),
            (shift_reg(false, Shift::Lsl, 1, 2, 3), 0x1ac32041, "lslv w1, w2, w3"),
            (shift_reg(true, Shift::Lsr, 1, 2, 3), 0x9ac32441, "lsrv x1, x2, x3"),
            (shift_reg(false, Shift::Lsr, 1, 2, 3), 0x1ac32441, "lsrv w1, w2, w3"),
            (shift_reg(true, Shift::Asr, 1, 2, 3), 0x9ac32841, "asrv x1, x2, x3"),
            (shift_reg(false, Shift::Asr, 1, 2, 3), 0x1ac32841, "asrv w1, w2, w3"),
            (shift_reg(true, Shift::Ror, 1, 2, 3), 0x9ac32c41, "rorv x1, x2, x3"),
            (shift_reg(false, Shift::Ror, 1, 2, 3), 0x1ac32c41, "rorv w1, w2, w3"),
            (ror(true, 1, 2, 13), 0x93c23441, "ror x1, x2, #13"),
            (ror(false, 1, 2, 7), 0x13821c41, "ror w1, w2, #7"),
            (clz(true, 1, 2), 0xdac01041, "clz x1, x2"),
            (clz(false, 1, 2), 0x5ac01041, "clz w1, w2"),
            (rbit(true, 1, 2), 0xdac00041, "rbit x1, x2"),
            (rbit(false, 1, 2), 0x5ac00041, "rbit w1, w2"),
            (bfi(1, 29, 0, 8), 0xb3401fa1, "bfi x1, x29, #0, #8"),
            (bfi(1, 29, 8, 8), 0xb3781fa1, "bfi x1, x29, #8, #8"),
            (bfi(1, 29, 0, 16), 0xb3403fa1, "bfi x1, x29, #0, #16"),
            (arith(false, true, true, 1, ZR, 3, 0), 0x6b0303e1, "negs w1, w3"),
            (arith(true, true, true, 1, ZR, 3, 0), 0xeb0303e1, "negs x1, x3"),
            (arith(false, true, false, 17, ZR, 1, 0), 0x4b0103f1, "neg w17, w1"),
            (lsl(false, 29, 1, 24), 0x53081c3d, "lsl w29, w1, #24"),
            (lsr(false, 29, 29, 16), 0x53107fbd, "lsr w29, w29, #16"),
            (arith(false, false, true, 29, 29, 30, 0), 0x2b1e03bd, "adds w29, w29, w30"),
            (ubfx(16, 3, 8, 8), 0xd3483c70, "ubfx x16, x3, #8, #8"),
            (with_carry(true, false, 1, 2, 3), 0xba030041, "adcs x1, x2, x3"),
            (with_carry(false, false, 1, 2, 3), 0x3a030041, "adcs w1, w2, w3"),
            (with_carry(true, true, 1, 2, 3), 0xfa030041, "sbcs x1, x2, x3"),
            (with_carry(false, true, 1, 2, 3), 0x7a030041, "sbcs w1, w2, w3"),
            (mulh(false, 1, 2, 3), 0x9bc37c41, "umulh x1, x2, x3"),
            (mulh(true, 1, 2, 3), 0x9b437c41, "smulh x1, x2, x3"),
            (mull(false, 1, 2, 3), 0x9ba37c41, "umull x1, w2, w3"),
            (mull(true, 1, 2, 3), 0x9b237c41, "smull x1, w2, w3"),
            (div(true, false, 1, 2, 3), 0x9ac30841, "udiv x1, x2, x3"),
            (div(false, false, 1, 2, 3), 0x1ac30841, "udiv w1, w2, w3"),
            (div(true, true, 1, 2, 3), 0x9ac30c41, "sdiv x1, x2, x3"),
            (div(false, true, 1, 2, 3), 0x1ac30c41, "sdiv w1, w2, w3"),
            (msub(true, 1, 2, 3, 4), 0x9b039041, "msub x1, x2, x3, x4"),
            (msub(false, 1, 2, 3, 4), 0x1b039041, "msub w1, w2, w3, w4"),
            (rev(true, 1, 2), 0xdac00c41, "rev x1, x2"),
            (rev(false, 1, 2), 0x5ac00841, "rev w1, w2"),
            (sbfx(true, 1, 2, 8, 8), 0x93483c41, "sbfx x1, x2, #8, #8"),
            (sbfx(false, 1, 2, 8, 8), 0x13083c41, "sbfx w1, w2, #8, #8"),
            (arith_imm(false, true, true, ZR, 17, 1, false), 0x7100063f, "subs wzr, w17, #1"),
            (arith(true, true, true, ZR, ZR, 17, 0), 0xeb1103ff, "cmp xzr, x17"),
            (bfi(21, 17, 16, 1), 0xb3700235, "bfi x21, x17, #16, #1"),
            (cmp_asr(2, 0, 63), 0xeb80fc5f, "cmp x2, x0, asr #63"),
            (orr_lsl(29, 30, 2, 32), 0xaa0283dd, "orr x29, x30, x2, lsl #32"),
            (arith_imm(true, false, true, ZR, 3, 1, false), 0xb100047f, "cmn x3, #1"),
            (arith_imm(false, false, true, ZR, 3, 1, false), 0x3100047f, "cmn w3, #1"),
            (cbz(true, true, 2, 2), 0xb5000042, "cbnz x2, .+8"),
            (lsr(true, 26, 25, 32), 0xd360ff3a, "lsr x26, x25, #32"),
            (ubfx(17, 16, 0, 14), 0xd3403611, "ubfx x17, x16, #0, #14"),
            (ubfx(1, 2, 3, 5), 0xd3431c41, "ubfx x1, x2, #3, #5"),
            (arith(true, false, false, 17, 28, 17, 4), 0x8b111391, "add x17, x28, x17, lsl #4"),
            (mem_imm(Size::X, Mem::Load, 25, 17, 0), 0xf9400239, "ldr x25, [x17]"),
            (mem_imm(Size::X, Mem::Load, 26, 17, 8), 0xf940063a, "ldr x26, [x17, #8]"),
            (arith(true, true, true, ZR, 25, 16, 0), 0xeb10033f, "cmp x25, x16"),
            (mov(true, 28, 3), 0xaa0303fc, "mov x28, x3"),
            (adr(17, 0), 0x10000011, "l: adr x17, l"),
            (adr(17, -4), 0x10fffff1, "adr x17, .-4"),
            (adr(1, 0x1004), 0x10008021, "adr x1, .+0x1004"),
            (mem_imm(Size::X, Mem::Store, ZR, 19, 0xb8), 0xf9005e7f, "str xzr, [x19, #0xb8]"),
            (arith_imm(true, false, false, 27, 27, 64, false), 0x9101037b, "add x27, x27, #64"),
            (movz(true, 27, 0, 0), 0xd280001b, "mov x27, #0"),
            (ret(), 0xd65f03c0, "ret"),
            (csel(true, 1, 2, 3, Cond::Eq), 0x9a830041, "csel x1, x2, x3, eq"),
            (csel(false, 1, 2, 3, Cond::Lt), 0x1a83b041, "csel w1, w2, w3, lt"),
            (cset(false, 1, Cond::Hi), 0x1a9f97e1, "cset w1, hi"),
            (cset(true, 1, Cond::Vs), 0x9a9f77e1, "cset x1, vs"),
            (nop(), 0xd503201f, "nop"),
            (brk(), 0xd4200000, "brk #0"),
            // Floating point and vectors (Apple clang, 2026-11-25).
            (fp_arith(true, Fp::Add, 16, 1, 17), 0x1e712830, "fadd d16, d1, d17"),
            (fp_arith(false, Fp::Add, 16, 1, 17), 0x1e312830, "fadd s16, s1, s17"),
            (fp_arith(true, Fp::Sub, 3, 4, 5), 0x1e653883, "fsub d3, d4, d5"),
            (fp_arith(false, Fp::Mul, 31, 0, 15), 0x1e2f081f, "fmul s31, s0, s15"),
            (fp_arith(true, Fp::Div, 16, 15, 17), 0x1e7119f0, "fdiv d16, d15, d17"),
            (fsqrt(true, 16, 17), 0x1e61c230, "fsqrt d16, d17"),
            (fsqrt(false, 2, 3), 0x1e21c062, "fsqrt s2, s3"),
            (fcmp(true, 16, 16), 0x1e702200, "fcmp d16, d16"),
            (fcmp(false, 1, 17), 0x1e312020, "fcmp s1, s17"),
            (fccmp(true, 2, 2, 7, Cond::Vc), 0x1e627447, "fccmp d2, d2, #7, vc"),
            (fccmp(false, 1, 17, 7, Cond::Vc), 0x1e317427, "fccmp s1, s17, #7, vc"),
            (fcsel(true, 16, 1, 17, Cond::Mi), 0x1e714c30, "fcsel d16, d1, d17, mi"),
            (fcsel(false, 16, 1, 17, Cond::Gt), 0x1e31cc30, "fcsel s16, s1, s17, gt"),
            (fcvt(true, 16, 17), 0x1e22c230, "fcvt d16, s17"),
            (fcvt(false, 16, 3), 0x1e624070, "fcvt s16, d3"),
            (scvtf(false, true, 16, 5), 0x1e6200b0, "scvtf d16, w5"),
            (scvtf(true, true, 16, 5), 0x9e6200b0, "scvtf d16, x5"),
            (scvtf(false, false, 16, 16), 0x1e220210, "scvtf s16, w16"),
            (scvtf(true, false, 16, 16), 0x9e220210, "scvtf s16, x16"),
            (fcvtzs(false, true, 16, 3), 0x1e780070, "fcvtzs w16, d3"),
            (fcvtzs(true, true, 16, 3), 0x9e780070, "fcvtzs x16, d3"),
            (fcvtzs(false, false, 0, 17), 0x1e380220, "fcvtzs w0, s17"),
            (fcvtzs(true, false, 15, 17), 0x9e38022f, "fcvtzs x15, s17"),
            (fmov_to_fp(false, 3, 16), 0x1e270203, "fmov s3, w16"),
            (fmov_to_fp(true, 3, 9), 0x9e670123, "fmov d3, x9"),
            (fmov_from_fp(false, 9, 3), 0x1e260069, "fmov w9, s3"),
            (fmov_from_fp(true, 16, 15), 0x9e6601f0, "fmov x16, d15"),
            (fmov(true, 3, 4), 0x1e604083, "fmov d3, d4"),
            (fmov(false, 3, 4), 0x1e204083, "fmov s3, s4"),
            (ins_low(true, 1, 16), 0x6e080601, "mov v1.d[0], v16.d[0]"),
            (ins_low(false, 15, 16), 0x6e04060f, "mov v15.s[0], v16.s[0]"),
            (vlogic(VLogic::And, 1, 2, 17), 0x4e311c41, "and v1.16b, v2.16b, v17.16b"),
            (vlogic(VLogic::Bic, 1, 17, 1), 0x4e611e21, "bic v1.16b, v17.16b, v1.16b"),
            (vlogic(VLogic::Orr, 1, 2, 3), 0x4ea31c41, "orr v1.16b, v2.16b, v3.16b"),
            (vlogic(VLogic::Eor, 15, 15, 15), 0x6e2f1def, "eor v15.16b, v15.16b, v15.16b"),
            (fp_mem(FpSize::S, true, 3, 20, 17), 0xbc716a83, "ldr s3, [x20, x17]"),
            (fp_mem(FpSize::D, true, 3, 20, 17), 0xfc716a83, "ldr d3, [x20, x17]"),
            (fp_mem(FpSize::Q, true, 17, 20, 4), 0x3ce46a91, "ldr q17, [x20, x4]"),
            (fp_mem(FpSize::S, false, 3, 20, 17), 0xbc316a83, "str s3, [x20, x17]"),
            (fp_mem(FpSize::D, false, 15, 20, 17), 0xfc316a8f, "str d15, [x20, x17]"),
            (fp_mem(FpSize::Q, false, 3, 20, 4), 0x3ca46a83, "str q3, [x20, x4]"),
            (ccmn_imm(false, 16, 1, 1, Cond::Vc), 0x3a417a01, "ccmn w16, #1, #1, vc"),
            (ccmn_imm(true, 16, 1, 1, Cond::Vc), 0xba417a01, "ccmn x16, #1, #1, vc"),
            (vec3(V3::Add, Lane::B, 1, 1, 17), 0x4e318421, "add v1.16b, v1.16b, v17.16b"),
            (vec3(V3::Add, Lane::H, 1, 1, 2), 0x4e628421, "add v1.8h, v1.8h, v2.8h"),
            (vec3(V3::Add, Lane::S, 15, 15, 3), 0x4ea385ef, "add v15.4s, v15.4s, v3.4s"),
            (vec3(V3::Add, Lane::D, 1, 1, 2), 0x4ee28421, "add v1.2d, v1.2d, v2.2d"),
            (vec3(V3::Sub, Lane::S, 1, 1, 2), 0x6ea28421, "sub v1.4s, v1.4s, v2.4s"),
            (vec3(V3::Sub, Lane::D, 1, 1, 17), 0x6ef18421, "sub v1.2d, v1.2d, v17.2d"),
            (vec3(V3::Cmeq, Lane::B, 1, 1, 2), 0x6e228c21, "cmeq v1.16b, v1.16b, v2.16b"),
            (vec3(V3::Cmeq, Lane::S, 1, 1, 2), 0x6ea28c21, "cmeq v1.4s, v1.4s, v2.4s"),
            (vec3(V3::Cmgt, Lane::S, 1, 1, 2), 0x4ea23421, "cmgt v1.4s, v1.4s, v2.4s"),
            (vec3(V3::Cmgt, Lane::H, 1, 1, 17), 0x4e713421, "cmgt v1.8h, v1.8h, v17.8h"),
            (vec3(V3::Zip1, Lane::S, 1, 1, 2), 0x4e823821, "zip1 v1.4s, v1.4s, v2.4s"),
            (vec3(V3::Zip2, Lane::S, 1, 1, 2), 0x4e827821, "zip2 v1.4s, v1.4s, v2.4s"),
            (vec3(V3::Zip1, Lane::D, 1, 1, 2), 0x4ec23821, "zip1 v1.2d, v1.2d, v2.2d"),
            (vec3(V3::Zip2, Lane::B, 1, 1, 2), 0x4e027821, "zip2 v1.16b, v1.16b, v2.16b"),
            (vec3(V3::Uzp1, Lane::S, 1, 1, 2), 0x4e821821, "uzp1 v1.4s, v1.4s, v2.4s"),
            (vec3(V3::Uzp2, Lane::S, 1, 1, 17), 0x4e915821, "uzp2 v1.4s, v1.4s, v17.4s"),
            (xtn_2d(16, 1), 0x0ea12830, "xtn v16.2s, v1.2d"),
            (xtn_2d(17, 17), 0x0ea12a31, "xtn v17.2s, v17.2d"),
            (umull_2d(1, 16, 17), 0x2eb1c201, "umull v1.2d, v16.2s, v17.2s"),
            (ins_s(16, 0, 2, 2), 0x6e044450, "mov v16.s[0], v2.s[2]"),
            (ins_s(16, 3, 17, 1), 0x6e1c2630, "mov v16.s[3], v17.s[1]"),
            (ins_s(1, 2, 2, 0), 0x6e140441, "mov v1.s[2], v2.s[0]"),
            (dup_s(1, 2, 0), 0x4e040441, "dup v1.4s, v2.s[0]"),
            (dup_s(1, 17, 3), 0x4e1c0621, "dup v1.4s, v17.s[3]"),
            (vshift(VShift::Ushr, Lane::S, 1, 1, 7), 0x6f390421, "ushr v1.4s, v1.4s, #7"),
            (vshift(VShift::Ushr, Lane::D, 1, 1, 32), 0x6f600421, "ushr v1.2d, v1.2d, #32"),
            (vshift(VShift::Ushr, Lane::H, 1, 1, 15), 0x6f110421, "ushr v1.8h, v1.8h, #15"),
            (vshift(VShift::Sshr, Lane::S, 1, 1, 31), 0x4f210421, "sshr v1.4s, v1.4s, #31"),
            (vshift(VShift::Sshr, Lane::H, 1, 1, 3), 0x4f1d0421, "sshr v1.8h, v1.8h, #3"),
            (vshift(VShift::Shl, Lane::S, 1, 1, 24), 0x4f385421, "shl v1.4s, v1.4s, #24"),
            (vshift(VShift::Shl, Lane::D, 1, 1, 63), 0x4f7f5421, "shl v1.2d, v1.2d, #63"),
            (vshift(VShift::Shl, Lane::H, 1, 1, 1), 0x4f115421, "shl v1.8h, v1.8h, #1"),
            // (Apple clang, -march=armv8.4-a.)
            (tbnz_top(27, 2), 0xb7f8005b, "tbnz x27, #63, .+8"),
            (tbnz_top(27, 100), 0xb7f80c9b, "tbnz x27, #63, .+400"),
            (ldapr(Size::B, 3, 24), 0x38bfc303, "ldaprb w3, [x24]"),
            (ldapr(Size::H, 3, 24), 0x78bfc303, "ldaprh w3, [x24]"),
            (ldapr(Size::W, 3, 24), 0xb8bfc303, "ldapr w3, [x24]"),
            (ldapr(Size::X, 15, 24), 0xf8bfc30f, "ldapr x15, [x24]"),
            (ldapr(Size::X, 24, 24), 0xf8bfc318, "ldapr x24, [x24]"),
            (stlr(Size::B, 3, 24), 0x089fff03, "stlrb w3, [x24]"),
            (stlr(Size::H, 3, 24), 0x489fff03, "stlrh w3, [x24]"),
            (stlr(Size::W, 3, 24), 0x889fff03, "stlr w3, [x24]"),
            (stlr(Size::X, 15, 24), 0xc89fff0f, "stlr x15, [x24]"),
            (ins_high_from(3, 30), 0x4e181fc3, "mov v3.d[1], x30"),
            (high_to(29, 3), 0x4e183c7d, "mov x29, v3.d[1]"),
            (ins_high(1, 17), 0x6e180621, "mov v1.d[1], v17.d[0]"),
            (ins_high(9, 2), 0x6e180449, "mov v9.d[1], v2.d[0]"),
            (ext(1, 1, 16, 3), 0x6e101821, "ext v1.16b, v1.16b, v16.16b, #3"),
            (ext(9, 16, 9, 12), 0x6e096209, "ext v9.16b, v16.16b, v9.16b, #12"),
            (stp_d(8, 9, 31, 96), 0x6d0627e8, "stp d8, d9, [sp, #96]"),
            (stp_d(14, 15, 31, 144), 0x6d093fee, "stp d14, d15, [sp, #144]"),
            (ldp_q(0, 1, 16, 0), 0xad400600, "ldp q0, q1, [x16]"),
            (ldp_q(14, 15, 16, 224), 0xad473e0e, "ldp q14, q15, [x16, #224]"),
            (mem_imm(Size::X, Mem::Load, 16, 31, 160), 0xf94053f0, "ldr x16, [sp, #160]"),
            (mem_imm(Size::X, Mem::Store, 17, 31, 160), 0xf90053f1, "str x17, [sp, #160]"),
        ];
        for (ours, theirs, name) in cases {
            assert_eq!(ours, theirs, "{name}: {ours:08x}, the assembler {theirs:08x}");
        }
        assert_eq!(cases.len(), 250);
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
