//! The x87 floating-point unit.
//!
//! A register holds a value in one of two forms: an f64, or its exact 80-bit original
//! (64 bits of mantissa, where an f64 has 53).
//!
//! - Add, subtract, multiply and divide work on the 80-bit form, in integers, and round
//!   as the control word says: to 64, 53 or 24 bits of mantissa, in any of the four
//!   directions. So does comparison, and so does conversion to and from integers. C
//!   libraries depend on this: musl's printf decides how to round a digit by asking
//!   whether 2^64 plus a small number is 2^64.
//! - A value that is only moved keeps all its bits.
//! - Everything else is computed as an f64: the functions of the host's library, and
//!   arithmetic whose operands or result are not ordinary numbers (zeros, infinities,
//!   NaNs, denormals, overflow). There the rounding direction is made from the sign of
//!   the exact residual.
//!
//! No floating-point exception is ever raised, and the exception bits of the status
//! word stay zero. An escape opcode that is not written stops the program and says so.
//!
//! All of it is as g64m has it (its E098), which is as the real CPU's traces have it.
//!
//! The unit touches no memory and no register of the CPU itself: the bytes of a memory
//! operand are handed to it in `Io` and taken from there afterwards, with the flags and
//! what FNSTSW puts in AX. So nothing here can fault.

use super::{ARITH, CF, PF, ZF};

// The condition codes, at their places in the status word.
const C0: u16 = 0x0100;
const C1: u16 = 0x0200;
const C2: u16 = 0x0400;
const C3: u16 = 0x4000;
const CONDS: u16 = C0 | C1 | C2 | C3;

const TOP_BIT: u64 = 1 << 63;
const FRAC_MASK: u64 = 0x000f_ffff_ffff_ffff;
const INDEFINITE: u64 = 0xfff8_0000_0000_0000;

/// What a register holds.
#[derive(Clone, Copy)]
enum Value {
    F(f64),
    /// The mantissa (bit 63 is the explicit integer bit) and the sign with the 15-bit
    /// exponent.
    X(u64, u16),
}

pub struct X87 {
    /// The physical registers; ST(i) is `reg[(top + i) & 7]`.
    reg: [Value; 8],
    /// One bit for each physical register: set when it is empty.
    empty: u8,
    top: u8,
    /// C0 to C3.
    cond: u16,
    cw: u16,
}

/// What passes between the CPU and the unit for one instruction.
pub struct Io {
    /// The memory operand's bytes: read before the instruction if it reads them, to be
    /// written after it if it stores.
    pub buf: [u8; 112],
    /// The environment is in its 32-bit layout (28 bytes, not 14).
    pub env32: bool,
    pub rflags: u64,
    /// What FNSTSW AX gives, when the instruction was that.
    pub ax: Option<u16>,
}

impl Io {
    fn rd(&self, k: usize, bytes: usize) -> u64 {
        let mut v = [0u8; 8];
        v[..bytes].copy_from_slice(&self.buf[k..k + bytes]);
        u64::from_le_bytes(v)
    }

    fn wr(&mut self, k: usize, bytes: usize, v: u64) {
        self.buf[k..k + bytes].copy_from_slice(&v.to_le_bytes()[..bytes]);
    }
}

/// An escape opcode that is not written.
pub struct NotWritten;
type R<T> = Result<T, NotWritten>;

/// The bytes of the memory operand of the instruction in `row` (the opcode's low three
/// bits) with ModRM's field `r`, and whether they are stored; `None` for one that is
/// not written.
pub fn shape(row: u8, r: u8, env32: bool) -> Option<(usize, bool)> {
    if row & 1 == 0 {
        return Some((if row == 4 { 8 } else if row == 6 { 2 } else { 4 }, false));
    }
    let env = if env32 { 28 } else { 14 };
    Some(match row << 3 | r {
        0x08 | 0x18 => (4, false),
        0x0a | 0x0b | 0x19 | 0x1a | 0x1b => (4, true),
        0x0c => (env, false),
        0x0d | 0x38 => (2, false),
        0x0e => (env, true),
        0x0f | 0x2f | 0x39 | 0x3a | 0x3b => (2, true),
        0x1d => (10, false),
        0x1f => (10, true),
        0x28 | 0x3d => (8, false),
        0x29 | 0x2a | 0x2b | 0x3f => (8, true),
        0x2c => (env + 80, false),
        0x2e => (env + 80, true),
        _ => return None,
    })
}

// ---- The 80-bit format ----------------------------------------------------------------

fn f64_to_f80(v: f64) -> (u64, u16) {
    let bits = v.to_bits();
    let sign: u16 = if bits >> 63 != 0 { 0x8000 } else { 0 };
    let exp = (bits >> 52 & 0x7ff) as i64;
    let frac = bits & FRAC_MASK;
    if exp == 0 {
        if frac == 0 {
            return (0, sign);
        }
        // An f64 denormal is an ordinary number in 80 bits, where the exponent has more
        // room.
        let lz = frac.leading_zeros() as i64;
        return (frac << lz, sign | (16383 + 63 - 1074 - lz) as u16);
    }
    if exp == 0x7ff {
        // A NaN that is loaded is made quiet.
        if frac != 0 {
            return (TOP_BIT | 0x4000_0000_0000_0000 | frac << 11, sign | 0x7fff);
        }
        return (TOP_BIT, sign | 0x7fff);
    }
    (TOP_BIT | frac << 11, sign | (exp - 1023 + 16383) as u16)
}

/// Rounds to nearest, ties to even, in integers: the 11 bits an f64 has no room for
/// decide.
fn f80_to_f64(mant: u64, se: u16) -> f64 {
    let sign = if se & 0x8000 != 0 { TOP_BIT } else { 0 };
    let mut e = (se & 0x7fff) as i64;
    if e == 0x7fff {
        if mant << 1 == 0 {
            return f64::from_bits(sign | 0x7ff0_0000_0000_0000);
        }
        return f64::from_bits(sign | 0x7ff8_0000_0000_0000 | (mant >> 11 & FRAC_MASK));
    }
    if mant == 0 {
        return f64::from_bits(sign);
    }
    if e == 0 {
        // A denormal's exponent field is 0 and means 1.
        e = 1;
    }
    let lz = mant.leading_zeros() as i64;
    let m = mant << lz;
    let biased = e - lz - 16383 + 1023;
    if biased >= 2047 {
        return f64::from_bits(sign | 0x7ff0_0000_0000_0000);
    }
    if biased >= 1 {
        let mut frac = m >> 11;
        let rem = m & 0x7ff;
        if rem > 0x400 || (rem == 0x400 && frac & 1 != 0) {
            frac += 1;
        }
        // A carry out of the mantissa lands in the exponent, which is what it should do.
        return f64::from_bits(sign | (((biased as u64) << 52) + (frac - 0x0010_0000_0000_0000)));
    }
    // Too small for a normal f64: a denormal.
    let shift = 12 - biased;
    if shift > 64 {
        return f64::from_bits(sign);
    }
    let (mut frac, mut rem, mut half) = (0, m, TOP_BIT);
    if shift < 64 {
        frac = m >> shift;
        rem = m & ((1 << shift) - 1);
        half = 1 << (shift - 1);
    }
    if rem > half || (rem == half && frac & 1 != 0) {
        frac += 1;
    }
    f64::from_bits(sign | frac)
}

/// An ordinary number: not zero, a denormal, an infinity or a NaN, and with its integer
/// bit set.
fn normal80(m: u64, se: u16) -> bool {
    let e = se & 0x7fff;
    e != 0 && e != 0x7fff && m >> 63 != 0
}

/// The exponent field, as a number to calculate with.
fn exp80(se: u16) -> i64 {
    (se & 0x7fff) as i64
}

fn zero80(m: u64, se: u16) -> bool {
    m == 0 && se & 0x7fff == 0
}

fn nan80(m: u64, se: u16) -> bool {
    se & 0x7fff == 0x7fff && m << 1 != 0
}

fn i64_to_f80(v: i64) -> (u64, u16) {
    if v == 0 {
        return (0, 0);
    }
    let sign: u16 = if v < 0 { 0x8000 } else { 0 };
    let mag = v.unsigned_abs();
    let lz = mag.leading_zeros();
    (mag << lz, sign | (16383 + 63 - lz) as u16)
}

/// The magnitude of an ordinary 80-bit number rounded to an integer in the given
/// direction (the control word's encoding: nearest, down, up, toward zero). `None` when
/// it needs more than 63 bits.
fn round80(m: u64, se: u16, mode: u16) -> Option<u64> {
    let e = exp80(se) - 16383;
    if e >= 63 {
        return None;
    }
    let neg = se & 0x8000 != 0;
    let shift = 63 - e;
    let (mut n, mut half, sticky) = (0, false, true);
    let sticky = if shift >= 65 {
        sticky
    } else if shift == 64 {
        half = true;
        m << 1 != 0
    } else {
        n = m >> shift;
        half = m >> (shift - 1) & 1 != 0;
        m & ((1 << (shift - 1)) - 1) != 0
    };
    let up = match mode {
        0 => half && (sticky || n & 1 != 0),
        1 => neg && (half || sticky),
        2 => !neg && (half || sticky),
        _ => false,
    };
    Some(n + up as u64)
}

/// -1, 0 or 1 for a below, equal to or above b; 2 when either is a NaN.
fn cmp80(am: u64, ase: u16, bm: u64, bse: u16) -> i64 {
    if nan80(am, ase) || nan80(bm, bse) {
        return 2;
    }
    let (an, bn) = (ase & 0x8000 != 0, bse & 0x8000 != 0);
    if am == 0 && bm == 0 {
        return 0;
    }
    if am == 0 {
        return if bn { 1 } else { -1 };
    }
    if bm == 0 || an != bn {
        return if an { -1 } else { 1 };
    }
    let rel = (ase & 0x7fff, am).cmp(&(bse & 0x7fff, bm)) as i64;
    if an { -rel } else { rel }
}

// ---- What the host's library does not give ------------------------------------------------

/// The next f64 above `v`.
fn next_up(v: f64) -> f64 {
    if v.is_nan() || v == f64::INFINITY {
        return v;
    }
    if v == 0.0 {
        return f64::from_bits(1);
    }
    let b = v.to_bits();
    f64::from_bits(if v > 0.0 { b + 1 } else { b - 1 })
}

fn next_down(v: f64) -> f64 {
    -next_up(-v)
}

/// `x` times two to the `e`, without the power itself overflowing on the way.
fn ldexp(mut x: f64, mut e: i64) -> f64 {
    let (big, small) = (f64::from_bits(0x7fe0_0000_0000_0000), f64::from_bits(0x0010_0000_0000_0000));
    while e > 1023 {
        x *= big;
        e -= 1023;
        if !x.is_finite() || x == 0.0 {
            return x;
        }
    }
    // By steps that leave 53 bits of room, so that a result that is not a denormal is
    // not rounded on the way.
    while e < -1022 {
        x *= small * 9007199254740992.0;
        e += 1022 - 53;
        if x == 0.0 {
            return x;
        }
    }
    x * f64::from_bits(((e + 1023) as u64) << 52)
}

/// The exponent of a finite number that is not zero, as a whole number.
fn logb(a: f64) -> f64 {
    let (m, se) = f64_to_f80(a);
    let _ = m;
    (exp80(se) - 16383) as f64
}

/// The remainder of a by b with the quotient taken to the nearest, a tie to the even
/// one. Only for operands that are not ordinary numbers, where the exact way is not
/// taken.
fn remainder(a: f64, b: f64) -> f64 {
    if b.is_infinite() && a.is_finite() {
        return a;
    }
    let r = a % b;
    if r.is_nan() {
        return r;
    }
    let twice = r.abs() * 2.0;
    let odd = (a / b).trunc() % 2.0 != 0.0;
    if twice > b.abs() || (twice == b.abs() && odd) {
        return r - b.abs().copysign(r);
    }
    r
}

impl X87 {
    pub fn new() -> X87 {
        X87 { reg: [Value::F(0.0); 8], empty: 0xff, top: 0, cond: 0, cw: 0x037f }
    }

    fn reset(&mut self) {
        *self = X87::new();
    }

    fn phys(&self, i: u8) -> usize {
        ((self.top + i) & 7) as usize
    }

    fn is_empty(&self, p: usize) -> bool {
        self.empty >> p & 1 != 0
    }

    /// Physical register `p` as an f64.
    fn now(&self, p: usize) -> f64 {
        match self.reg[p] {
            Value::F(v) => v,
            Value::X(m, se) => f80_to_f64(m, se),
        }
    }

    /// ST(i) as an instruction reads it. An empty register reads as the indefinite NaN,
    /// which is what the chip gives with the stack fault masked; the fault itself is
    /// not recorded.
    fn st(&self, i: u8) -> f64 {
        let p = self.phys(i);
        if self.is_empty(p) { f64::from_bits(INDEFINITE) } else { self.now(p) }
    }

    fn st80(&self, i: u8) -> (u64, u16) {
        let p = self.phys(i);
        if self.is_empty(p) { (0xc000_0000_0000_0000, 0xffff) } else { self.raw80(i) }
    }

    /// What is in ST(i), empty or not: for the instructions that save the whole unit.
    fn raw80(&self, i: u8) -> (u64, u16) {
        match self.reg[self.phys(i)] {
            Value::F(v) => f64_to_f80(v),
            Value::X(m, se) => (m, se),
        }
    }

    fn put(&mut self, i: u8, v: Value) {
        let p = self.phys(i);
        self.reg[p] = v;
        self.empty &= !(1 << p);
    }

    fn set_st(&mut self, i: u8, v: f64) {
        self.put(i, Value::F(v));
    }

    fn set_st80(&mut self, i: u8, mant: u64, se: u16) {
        self.put(i, Value::X(mant, se));
    }

    fn down(&mut self) {
        self.top = (self.top + 7) & 7;
    }

    fn push(&mut self, v: f64) {
        self.down();
        self.set_st(0, v);
    }

    fn push80(&mut self, mant: u64, se: u16) {
        self.down();
        self.set_st80(0, mant, se);
    }

    fn pop(&mut self) {
        self.empty |= 1 << self.top;
        self.top = (self.top + 1) & 7;
    }

    /// Copies ST(from) to ST(to), all its bits.
    fn copy_st(&mut self, to: u8, from: u8) {
        self.put(to, self.reg[self.phys(from)]);
    }

    fn swap_st(&mut self, i: u8) {
        let (a, b) = (self.phys(0), self.phys(i));
        self.reg.swap(a, b);
        self.empty &= !(1 << a | 1 << b);
    }

    fn status(&self) -> u16 {
        self.cond | (self.top as u16) << 11
    }

    /// Two bits for each physical register: 0 valid, 1 zero, 2 special (NaN, infinity),
    /// 3 empty.
    fn tag_word(&self) -> u16 {
        let mut w = 0;
        for p in 0..8 {
            let v = self.now(p);
            let t = if self.is_empty(p) {
                3
            } else if v == 0.0 {
                1
            } else if !v.is_finite() {
                2
            } else {
                0
            };
            w |= t << (p * 2);
        }
        w
    }

    fn load_tags(&mut self, w: u16) {
        self.empty = 0;
        for p in 0..8 {
            if w >> (p * 2) & 3 == 3 {
                self.empty |= 1 << p;
            }
        }
    }

    fn rc(&self) -> u16 {
        self.cw >> 10 & 3
    }

    // ---- storing ---------------------------------------------------------------------

    /// An 80-bit value as the bits of a double (`wide`) or a single, rounded once, as
    /// the control word says, from all 64 bits of mantissa.
    fn store_float(&self, m0: u64, se: u16, wide: bool) -> u64 {
        let p: u64 = if wide { 53 } else { 24 };
        let bias: i64 = if wide { 1023 } else { 127 };
        let emax: i64 = if wide { 2047 } else { 255 };
        let neg = se & 0x8000 != 0;
        let sign = if neg { 1u64 << (if wide { 63 } else { 31 }) } else { 0 };
        // The exponent field all ones.
        let top = (emax as u64) << (p - 1);
        let e = (se & 0x7fff) as i64;
        if e == 0x7fff {
            if m0 << 1 == 0 {
                return sign | top;
            }
            return sign | top | 1 << (p - 2) | (m0 << 1 >> (65 - p));
        }
        if m0 == 0 {
            return sign;
        }
        let lz = m0.leading_zeros() as i64;
        let m = m0 << lz;
        let mut biased = (if e == 0 { 1 } else { e }) - lz - 16383 + bias;
        let dir = self.rc();
        // How far the 64 bits are moved right: to p bits for a normal number, further
        // for one too small to be normal.
        let mut shift = 64 - p;
        if biased < 1 {
            shift += (1 - biased) as u64;
        }
        let (mut frac, mut rem, mut half) = (0, m, TOP_BIT);
        // Every bit is beyond even the half place.
        let below = shift > 64;
        if shift < 64 {
            frac = m >> shift;
            rem = m & ((1 << shift) - 1);
            half = 1 << (shift - 1);
        }
        let up = match dir {
            0 => !below && (rem > half || (rem == half && frac & 1 != 0)),
            1 => neg && rem != 0,
            2 => !neg && rem != 0,
            _ => false,
        };
        frac += up as u64;
        if biased < 1 {
            // A denormal, or the smallest normal by the carry.
            return sign | frac;
        }
        if frac >> p != 0 {
            frac >>= 1;
            biased += 1;
        }
        if biased >= emax {
            // Too large: infinity, or the largest number when the rounding is away from
            // it.
            if dir == 3 || (dir == 1 && !neg) || (dir == 2 && neg) {
                return sign | (top - 1);
            }
            return sign | top;
        }
        sign | (biased as u64) << (p - 1) | frac & ((1 << (p - 1)) - 1)
    }

    /// ST(0) as an integer of the given width, in two's complement, rounded in the given
    /// direction. Out of range, or a NaN, gives the "integer indefinite": the most
    /// negative number.
    fn st_int(&self, bits: u32, mode: u16) -> u64 {
        let lim = 1u64 << (bits - 1);
        let (m, se) = self.st80(0);
        if normal80(m, se) {
            if let Some(mag) = round80(m, se, mode) {
                let neg = se & 0x8000 != 0;
                if mag > lim || (mag == lim && !neg) {
                    return lim;
                }
                return if neg { mag.wrapping_neg() } else { mag };
            }
        }
        let v = self.st(0);
        let v = match mode {
            0 => v.round_ties_even(),
            1 => v.floor(),
            2 => v.ceil(),
            _ => v.trunc(),
        };
        let flim = ldexp(1.0, bits as i64 - 1);
        if v.is_nan() || v < -flim || v >= flim {
            return lim;
        }
        v as i64 as u64
    }

    // ---- arithmetic as f64, for what is not an ordinary number ---------------------------

    /// To an integral value, in the direction the control word asks for.
    fn round_int(&self, v: f64) -> f64 {
        match self.rc() {
            0 => v.round_ties_even(),
            1 => v.floor(),
            2 => v.ceil(),
            _ => v.trunc(),
        }
    }

    /// Fits a result to the control word. `r` is the result rounded to nearest; `err`
    /// has the sign of (true result - r).
    fn finish(&self, r: f64, err: f64) -> f64 {
        let dir = self.rc();
        if r.is_infinite() {
            // An overflow stops at the largest finite number when the rounding goes
            // toward zero.
            if r > 0.0 && (dir == 1 || dir == 3) {
                return f64::MAX;
            }
            if r < 0.0 && (dir == 2 || dir == 3) {
                return -f64::MAX;
            }
            return r;
        }
        let mut v = r;
        if (dir == 1 && err < 0.0) || (dir == 3 && err < 0.0 && r > 0.0) {
            v = next_down(r);
        } else if (dir == 2 && err > 0.0) || (dir == 3 && err > 0.0 && r < 0.0) {
            v = next_up(r);
        }
        if self.cw >> 8 & 3 == 0 && v != 0.0 {
            // Single precision: 24 bits of mantissa, but still the x87's exponent range,
            // so the value cannot go through an f32. The low 29 mantissa bits are
            // rounded away, ties to even.
            let bits = v.to_bits();
            let low = bits & 0x1fff_ffff;
            let mut hi = bits >> 29;
            if low > 0x1000_0000 || (low == 0x1000_0000 && hi & 1 != 0) {
                hi += 1;
            }
            return f64::from_bits(hi << 29);
        }
        v
    }

    fn f_add(&self, a: f64, b: f64) -> f64 {
        let r = a + b;
        if r.is_nan() || !a.is_finite() || !b.is_finite() {
            return r;
        }
        let bb = r - a;
        self.finish(r, (a - (r - bb)) + (b - bb))
    }

    fn f_mul(&self, a: f64, b: f64) -> f64 {
        let r = a * b;
        if r.is_nan() || !a.is_finite() || !b.is_finite() {
            return r;
        }
        if r == 0.0 && a != 0.0 && b != 0.0 {
            // The product fell below the smallest f64 and so did its residual; the sign
            // is still known.
            return self.finish(r, if (a < 0.0) == (b < 0.0) { 1.0 } else { -1.0 });
        }
        self.finish(r, a.mul_add(b, -r))
    }

    fn f_div(&self, a: f64, b: f64) -> f64 {
        let r = a / b;
        if r.is_nan() || b == 0.0 || !a.is_finite() || !b.is_finite() {
            return r;
        }
        let res = (-r).mul_add(b, a);
        self.finish(r, if b > 0.0 { res } else { -res })
    }

    fn f_sqrt(&self, a: f64) -> f64 {
        if a < 0.0 {
            return f64::from_bits(INDEFINITE);
        }
        let r = a.sqrt();
        if !r.is_finite() {
            return r;
        }
        self.finish(r, (-r).mul_add(r, a))
    }

    /// The six arithmetic operations of the D8 row, by the reg field: `a` is ST(0), `b`
    /// the other operand.
    fn arith(&self, r: u8, a: f64, b: f64) -> f64 {
        let v = match r {
            0 => self.f_add(a, b),
            1 => self.f_mul(a, b),
            4 => self.f_add(a, -b),
            5 => self.f_add(b, -a),
            6 => self.f_div(a, b),
            _ => self.f_div(b, a),
        };
        // A result that is not a number, of operands that were (infinity less infinity,
        // zero by zero), is the one "indefinite": quiet, with the sign set.
        if v.is_nan() && !a.is_nan() && !b.is_nan() {
            return f64::from_bits(INDEFINITE);
        }
        v
    }

    // ---- arithmetic on the 80-bit format -------------------------------------------------

    /// Rounds an exact result to the precision and direction of the control word and
    /// packs it. `hi` is the mantissa with its top bit set, `lo` what lies below it: its
    /// top bit is the half, and any other bit set means "something more". `None` when
    /// the exponent has left the range of ordinary numbers; the caller then computes
    /// the operation as an f64.
    fn round_pack(&self, sign: u16, mut e: i64, mut hi: u64, lo: u64) -> Option<(u64, u16)> {
        let pc = self.cw >> 8 & 3;
        // Mantissa bits given up.
        let cut = match pc {
            3 => 0,
            0 => 40,
            _ => 11,
        };
        let (half, sticky);
        let mut unit = 1u64;
        if cut == 0 {
            half = lo >> 63 != 0;
            sticky = lo << 1 != 0;
        } else {
            unit <<= cut;
            let dropped = hi & (unit - 1);
            half = dropped >> (cut - 1) != 0;
            sticky = dropped & ((unit >> 1) - 1) != 0 || lo != 0;
            hi -= dropped;
        }
        let up = match self.rc() {
            0 => half && (sticky || hi & unit != 0),
            1 => sign != 0 && (half || sticky),
            2 => sign == 0 && (half || sticky),
            _ => false,
        };
        if up {
            hi = hi.wrapping_add(unit);
            if hi == 0 {
                // The mantissa carried out.
                hi = TOP_BIT;
                e += 1;
            }
        }
        if e <= 0 || e >= 0x7fff {
            return None;
        }
        Some((hi, sign | e as u16))
    }

    /// a + b. Subtraction is addition with b's sign turned.
    fn soft_add(&self, mut am: u64, mut ase: u16, mut bm: u64, mut bse: u16) -> Option<(u64, u16)> {
        // Adding zero must not cost the other operand its low bits.
        if zero80(bm, bse) && normal80(am, ase) {
            return self.round_pack(ase & 0x8000, exp80(ase), am, 0);
        }
        if zero80(am, ase) && normal80(bm, bse) {
            return self.round_pack(bse & 0x8000, exp80(bse), bm, 0);
        }
        if !normal80(am, ase) || !normal80(bm, bse) {
            return None;
        }
        if (ase & 0x7fff, am) < (bse & 0x7fff, bm) {
            (am, ase, bm, bse) = (bm, bse, am, ase);
        }
        let mut e = exp80(ase);
        let d = e - exp80(bse);
        // b, moved down to a's exponent, as 128 bits. Bits that fall off the end leave
        // a mark.
        let b = (bm as u128) << 64;
        let b = if d >= 128 {
            1
        } else {
            b >> d | (b << (127 - d) << 1 != 0) as u128
        };
        let a = (am as u128) << 64;
        let (mut hi, mut lo);
        if ase & 0x8000 == bse & 0x8000 {
            let (sum, carried) = a.overflowing_add(b);
            (hi, lo) = ((sum >> 64) as u64, sum as u64);
            if carried {
                // One place down.
                lo = (lo >> 1) | (lo & 1) | (hi << 63);
                hi = (hi >> 1) | TOP_BIT;
                e += 1;
            }
        } else {
            let diff = a - b;
            if diff == 0 {
                // Exactly zero: positive, except when rounding down.
                return Some((0, if self.rc() == 1 { 0x8000 } else { 0 }));
            }
            let lz = diff.leading_zeros();
            let diff = diff << lz;
            (hi, lo) = ((diff >> 64) as u64, diff as u64);
            e -= lz as i64;
        }
        self.round_pack(ase & 0x8000, e, hi, lo)
    }

    fn soft_mul(&self, am: u64, ase: u16, bm: u64, bse: u16) -> Option<(u64, u16)> {
        if !normal80(am, ase) || !normal80(bm, bse) {
            return None;
        }
        let mut e = exp80(ase) + exp80(bse) - 16383 + 1;
        let mut p = am as u128 * bm as u128;
        if p >> 127 == 0 {
            p <<= 1;
            e -= 1;
        }
        self.round_pack((ase ^ bse) & 0x8000, e, (p >> 64) as u64, p as u64)
    }

    /// a / b: 64 quotient bits, one more for the half, and whether anything is left.
    fn soft_div(&self, am: u64, ase: u16, bm: u64, bse: u16) -> Option<(u64, u16)> {
        if !normal80(am, ase) || !normal80(bm, bse) {
            return None;
        }
        let mut e = exp80(ase) - exp80(bse) + 16383 - 1;
        // The dividend with 64 or 63 zeros under it, so that the quotient has its top
        // bit set.
        let mut n = (am as u128) << 64;
        if am >= bm {
            n >>= 1;
            e += 1;
        }
        let q = (n / bm as u128) as u64;
        let rem = n % bm as u128 * 2;
        let mut lo = 0;
        let mut rem = rem;
        if rem >= bm as u128 {
            rem -= bm as u128;
            lo = TOP_BIT;
        }
        if rem != 0 {
            lo |= 1;
        }
        self.round_pack((ase ^ bse) & 0x8000, e, q, lo)
    }

    /// The six arithmetic operations on 80-bit operands: `a` is ST(0), `b` the other.
    /// Ordinary numbers are computed exactly; anything else goes through f64.
    fn arith80(&self, r: u8, am: u64, ase: u16, bm: u64, bse: u16) -> (u64, u16) {
        let exact = match r {
            0 => self.soft_add(am, ase, bm, bse),
            1 => self.soft_mul(am, ase, bm, bse),
            4 => self.soft_add(am, ase, bm, bse ^ 0x8000),
            5 => self.soft_add(bm, bse, am, ase ^ 0x8000),
            6 => self.soft_div(am, ase, bm, bse),
            _ => self.soft_div(bm, bse, am, ase),
        };
        exact.unwrap_or_else(|| f64_to_f80(self.arith(r, f80_to_f64(am, ase), f80_to_f64(bm, bse))))
    }

    /// The square root of an ordinary positive 80-bit number, in integers: all 64 bits
    /// of the mantissa, with what is left over deciding the rounding.
    fn sqrt80(&self, m: u64, se: u16) -> Option<(u64, u16)> {
        let e = exp80(se) - 16383;
        // The root's 64 bits r are such that r * r is m * 2^63 for an even exponent and
        // m * 2^64 for an odd one: n is that number.
        let odd = e & 1 != 0;
        let mut n = if odd { (m as u128) << 64 } else { (m as u128) << 63 };
        let mut root: u128 = 0;
        // The remainder, which needs 66 bits.
        let mut rem: u128 = 0;
        for _ in 0..64 {
            // Two more bits of n come down.
            rem = rem << 2 | n >> 126;
            n <<= 2;
            // The trial: four times the root so far, and one.
            let trial = root << 2 | 1;
            root <<= 1;
            if rem >= trial {
                rem -= trial;
                root |= 1;
            }
        }
        // More than half a unit is left when the remainder exceeds the root.
        let mut lo = (rem != 0) as u64;
        if rem > root {
            lo |= TOP_BIT;
        }
        let half_e = if odd { (e - 1) / 2 } else { e / 2 };
        self.round_pack(0, 16383 + half_e, root as u64, lo)
    }

    /// The constants the unit holds, to 64 bits: `down` is the mantissa cut off, and
    /// `more` says the part cut off is over a half. So to nearest it is one more when
    /// `more`; upward one more always; downward and toward zero as cut.
    fn push_const(&mut self, down: u64, se: u16, more: bool) {
        let dir = self.rc();
        let m = if dir == 2 || (dir == 0 && more) { down + 1 } else { down };
        self.push80(m, se);
    }

    // ---- comparison --------------------------------------------------------------------

    fn cmp_st(&self, i: u8) -> i64 {
        let ((am, ase), (bm, bse)) = (self.st80(0), self.st80(i));
        cmp80(am, ase, bm, bse)
    }

    fn set_cond(&mut self, rel: i64) {
        // C1 is cleared too: the real CPU.
        self.cond &= !CONDS;
        if rel == 2 {
            self.cond |= C0 | C2 | C3;
        } else if rel < 0 {
            self.cond |= C0;
        } else if rel == 0 {
            self.cond |= C3;
        }
    }

    // ---- the environment -----------------------------------------------------------------

    /// Writes the environment and returns its size: 28 bytes with a 32-bit operand
    /// size, 14 with a 16-bit one. The instruction and operand pointers are written as
    /// zero.
    fn store_env(&self, io: &mut Io) -> usize {
        let words = [self.cw, self.status(), self.tag_word()];
        if io.env32 {
            // The upper half of each of the three words' places is written as ones.
            for (i, w) in words.into_iter().enumerate() {
                io.wr(i * 4, 4, 0xffff_0000 | w as u64);
            }
            io.wr(12, 8, 0);
            io.wr(20, 8, 0);
            return 28;
        }
        for (i, w) in words.into_iter().enumerate() {
            io.wr(i * 2, 2, w as u64);
        }
        io.wr(6, 8, 0);
        14
    }

    fn load_env(&mut self, io: &Io) -> usize {
        let step = if io.env32 { 4 } else { 2 };
        self.cw = io.rd(0, 2) as u16;
        let sw = io.rd(step, 2) as u16;
        self.top = (sw >> 11 & 7) as u8;
        self.cond = sw & CONDS;
        self.load_tags(io.rd(step * 2, 2) as u16);
        step * 7
    }

    // ---- FXSAVE's and FXRSTOR's part (the CPU reads and writes the bytes) ------------------

    /// The control word, the status word, a bit for each register in use, and the eight
    /// registers from ST(0) as 80-bit values.
    pub fn fx_get(&self) -> (u16, u16, u8, [(u64, u16); 8]) {
        (self.cw, self.status(), !self.empty, std::array::from_fn(|i| self.raw80(i as u8)))
    }

    pub fn fx_set(&mut self, cw: u16, sw: u16, used: u8, st: [(u64, u16); 8]) {
        self.cw = cw;
        self.top = (sw >> 11 & 7) as u8;
        self.cond = sw & CONDS;
        for (i, (m, se)) in st.into_iter().enumerate() {
            self.set_st80(i as u8, m, se);
        }
        self.empty = !used;
    }

    // ---- execution -----------------------------------------------------------------------

    /// Arithmetic or comparison of ST(0) with b, the result going to ST(dest).
    fn arith_to(&mut self, r: u8, bm: u64, bse: u16, dest: u8) {
        let (am, ase) = self.st80(0);
        if r == 2 || r == 3 {
            self.set_cond(cmp80(am, ase, bm, bse));
            if r == 3 {
                self.pop();
            }
            return;
        }
        let (m, se) = self.arith80(r, am, ase, bm, bse);
        self.set_st80(dest, m, se);
    }

    fn mem(&mut self, row: u8, r: u8, io: &mut Io) -> R<()> {
        if row & 1 == 0 {
            // D8, DA, DC, DE: arithmetic with a 32-bit real, 32-bit integer, 64-bit
            // real, 16-bit integer.
            let b = match row {
                0 => f32::from_bits(io.rd(0, 4) as u32) as f64,
                2 => io.rd(0, 4) as i32 as f64,
                4 => f64::from_bits(io.rd(0, 8)),
                _ => io.rd(0, 2) as i16 as f64,
            };
            let (bm, bse) = f64_to_f80(b);
            self.arith_to(r, bm, bse, 0);
            return Ok(());
        }
        let (mant, se) = self.st80(0);
        let rc = self.rc();
        match row << 3 | r {
            // D9
            0x08 => self.push(f32::from_bits(io.rd(0, 4) as u32) as f64),
            0x0a | 0x0b => io.wr(0, 4, self.store_float(mant, se, false)),
            0x0c => {
                self.load_env(io);
            }
            0x0d => self.cw = io.rd(0, 2) as u16,
            0x0e => {
                // FNSTENV then masks every exception.
                self.store_env(io);
                self.cw |= 0x3f;
            }
            0x0f => io.wr(0, 2, self.cw as u64),
            // DB
            0x18 => self.push(io.rd(0, 4) as i32 as f64),
            0x19 => io.wr(0, 4, self.st_int(32, 3)),
            0x1a | 0x1b => io.wr(0, 4, self.st_int(32, rc)),
            0x1d => self.push80(io.rd(0, 8), io.rd(8, 2) as u16),
            0x1f => {
                io.wr(0, 8, mant);
                io.wr(8, 2, se as u64);
            }
            // DD
            0x28 => self.push(f64::from_bits(io.rd(0, 8))),
            0x29 => io.wr(0, 8, self.st_int(64, 3)),
            0x2a | 0x2b => io.wr(0, 8, self.store_float(mant, se, true)),
            0x2c => {
                // FRSTOR.
                let n = self.load_env(io);
                for i in 0..8 {
                    let at = n + i * 10;
                    let p = self.phys(i as u8);
                    self.reg[p] = Value::X(io.rd(at, 8), io.rd(at + 8, 2) as u16);
                }
            }
            0x2e => {
                // FNSAVE, which then initialises the unit.
                let n = self.store_env(io);
                for i in 0..8 {
                    let (m, se) = self.raw80(i as u8);
                    io.wr(n + i * 10, 8, m);
                    io.wr(n + i * 10 + 8, 2, se as u64);
                }
                self.reset();
            }
            0x2f => io.wr(0, 2, self.status() as u64),
            // DF
            0x38 => self.push(io.rd(0, 2) as i16 as f64),
            0x39 => io.wr(0, 2, self.st_int(16, 3)),
            0x3a | 0x3b => io.wr(0, 2, self.st_int(16, rc)),
            0x3d => {
                // FILD of 64 bits: exact, where an f64 is not.
                let (m, se) = i64_to_f80(io.rd(0, 8) as i64);
                self.push80(m, se);
            }
            0x3f => io.wr(0, 8, self.st_int(64, rc)),
            // Among them FBLD and FBSTP, the packed decimal pair.
            _ => return Err(NotWritten),
        }
        // The stores that then pop.
        if matches!(row << 3 | r, 0x0b | 0x19 | 0x1b | 0x1f | 0x29 | 0x2b | 0x39 | 0x3b | 0x3f) {
            self.pop();
        }
        Ok(())
    }

    /// The low three bits of a quotient, where FPREM and FPREM1 report them: C0, C3, C1
    /// from the top.
    fn quotient_bits(&mut self, q: f64) {
        self.cond &= !CONDS;
        if !q.is_finite() || q.abs() >= 9.0e18 {
            return;
        }
        self.quotient(q.abs() as u64);
    }

    fn quotient(&mut self, q: u64) {
        if q & 1 != 0 {
            self.cond |= C1;
        }
        if q & 2 != 0 {
            self.cond |= C3;
        }
        if q & 4 != 0 {
            self.cond |= C0;
        }
    }

    /// FPREM and FPREM1 of ordinary numbers, in integers: the remainder of a by b by
    /// long division of the mantissas, a bit of the quotient at a time. When the
    /// exponents are 64 or more apart the chip does part of it and says so (C2),
    /// leaving a smaller number to go on from: 32 bits of the quotient and what the
    /// difference leaves over 32. Else the remainder is whole, and the quotient's low
    /// three bits go to C0, C3 and C1. FPREM cuts the quotient off; FPREM1 (`nearest`)
    /// takes the nearest, a tie to the even one.
    fn rem80(&mut self, am: u64, ase: u16, bm: u64, bse: u16, nearest: bool) -> (u64, u16) {
        self.cond &= !CONDS;
        let (ea, eb) = (exp80(ase), exp80(bse));
        let d = ea - eb;
        if d < 0 {
            // a is the smaller: it is its own remainder, with a quotient of zero;
            // unless, for FPREM1, it is over half of b (which needs the exponents one
            // apart and a's mantissa the larger): then b is taken once and the sign
            // changes.
            if nearest && d == -1 && am > bm {
                // b less a, at a's exponent.
                let r = bm.wrapping_add(bm).wrapping_sub(am);
                self.cond |= C1;
                let lz = r.leading_zeros() as i64;
                if ea - lz <= 0 {
                    return (0, ase ^ 0x8000);
                }
                return (r << lz, (ase ^ 0x8000) & 0x8000 | (ea - lz) as u16);
            }
            return (am, ase);
        }
        let part = d >= 64;
        let steps = if part { 32 + d % 32 + 1 } else { d + 1 };
        let (mut r, mut carry, mut q) = (am, false, 0u64);
        for it in 0..steps {
            let ge = carry || r >= bm;
            q = q << 1 | ge as u64;
            if ge {
                r = r.wrapping_sub(bm);
            }
            if it != steps - 1 {
                carry = r >> 63 != 0;
                r <<= 1;
            }
        }
        let mut sign = ase & 0x8000;
        // Where r stands now.
        let mut e = if part { ea - (steps - 1) } else { eb };
        if part {
            self.cond |= C2;
        } else {
            if nearest {
                // Over half of b left, or exactly half with an odd quotient: one more of
                // b is taken, and the remainder changes sign.
                let over = r >> 63 != 0 || r << 1 > bm;
                let tie = r >> 63 == 0 && r << 1 == bm;
                if over || (tie && q & 1 != 0) {
                    r = bm - r;
                    sign ^= 0x8000;
                    q += 1;
                }
            }
            self.quotient(q);
        }
        if r == 0 {
            return (0, sign);
        }
        let lz = r.leading_zeros() as i64;
        e -= lz;
        if e <= 0 {
            return (0, sign);
        }
        (r << lz, sign | e as u16)
    }

    fn reg(&mut self, row: u8, r: u8, i: u8, io: &mut Io) -> R<()> {
        let a = self.st(0);
        // For D9 F0 to FF, where the r/m field is part of the opcode.
        let s1 = self.st(1);
        let (am, ase) = self.st80(0);
        let (bm, bse) = self.st80(i);
        let flag = |f: u64| io.rflags & f != 0;
        let cmp_flags = |rel: i64, io: &mut Io| {
            // FCOMI and its relatives: the result goes to ZF, PF and CF, and the rest
            // of the six are cleared.
            io.rflags &= !ARITH;
            if rel == 2 {
                io.rflags |= ZF | PF | CF;
            } else if rel < 0 {
                io.rflags |= CF;
            } else if rel == 0 {
                io.rflags |= ZF;
            }
        };
        match row << 3 | r {
            // D8: ST(0) = ST(0) op ST(i)
            0x00..=0x07 => self.arith_to(r, bm, bse, 0),
            // D9
            0x08 => {
                // FLD ST(i).
                self.down();
                self.copy_st(0, i + 1);
            }
            0x09 | 0x29 => self.swap_st(i),
            // FNOP.
            0x0a if i == 0 => {}
            0x0b | 0x2b => {
                self.copy_st(i, 0);
                self.pop();
            }
            0x0c => match i {
                // FCHS, FABS, FTST.
                0 => self.set_st80(0, am, ase ^ 0x8000),
                1 => self.set_st80(0, am, ase & 0x7fff),
                4 => self.set_cond(cmp80(am, ase, 0, 0)),
                5 => {
                    // FXAM: what kind of number.
                    self.cond &= !CONDS;
                    if a.is_sign_negative() {
                        self.cond |= C1;
                    }
                    self.cond |= if self.is_empty(self.phys(0)) {
                        C0 | C3
                    } else if a.is_nan() {
                        C0
                    } else if a.is_infinite() {
                        C0 | C2
                    } else if a == 0.0 {
                        C3
                    } else if ase & 0x7fff == 0 && am != 0 {
                        // Only an 80-bit original can be a denormal: an f64 denormal is
                        // not one in 80 bits.
                        C2 | C3
                    } else {
                        C2
                    };
                }
                _ => return Err(NotWritten),
            },
            0x0d => match i {
                0 => self.push(1.0),
                1 => self.push_const(0xd49a_784b_cd1b_8afe, 0x4000, false), // log2(10)
                2 => self.push_const(0xb8aa_3b29_5c17_f0bb, 0x3fff, true),  // log2(e)
                3 => self.push_const(0xc90f_daa2_2168_c234, 0x4000, true),  // pi
                4 => self.push_const(0x9a20_9a84_fbcf_f798, 0x3ffd, true),  // log10(2)
                5 => self.push_const(0xb172_17f7_d1cf_79ab, 0x3ffe, true),  // ln(2)
                6 => self.push(0.0),
                _ => return Err(NotWritten),
            },
            0x0e => match i {
                // F2XM1, FYL2X, FPTAN, FPATAN.
                0 => self.set_st(0, (a * std::f64::consts::LN_2).exp_m1()),
                1 => {
                    self.set_st(1, s1 * a.log2());
                    self.pop();
                }
                2 => {
                    self.set_st(0, a.tan());
                    self.push(1.0);
                    self.cond &= !C2;
                }
                3 => {
                    self.set_st(1, s1.atan2(a));
                    self.pop();
                }
                4 => {
                    // FXTRACT.
                    if a == 0.0 || !a.is_finite() {
                        self.set_st(0, if a == 0.0 { f64::NEG_INFINITY } else { a.abs() });
                        self.push(a);
                    } else if normal80(am, ase) {
                        // In integers: the exponent as a number, and the mantissa with
                        // an exponent of zero, all 64 bits of it.
                        let (em, ese) = i64_to_f80(exp80(ase) - 16383);
                        self.set_st80(0, em, ese);
                        self.push80(am, ase & 0x8000 | 0x3fff);
                    } else {
                        let e = logb(a);
                        self.set_st(0, e);
                        self.push(ldexp(a, -(e as i64)));
                    }
                }
                5 => {
                    // FPREM1.
                    let (dm, dse) = self.st80(1);
                    if normal80(am, ase) && normal80(dm, dse) {
                        let (m, se) = self.rem80(am, ase, dm, dse, true);
                        self.set_st80(0, m, se);
                    } else {
                        self.set_st(0, remainder(a, s1));
                        self.quotient_bits((a / s1).round_ties_even());
                    }
                }
                // FDECSTP, FINCSTP.
                6 => self.down(),
                _ => self.top = (self.top + 1) & 7,
            },
            0x0f => match i {
                0 => {
                    // FPREM.
                    let (dm, dse) = self.st80(1);
                    if normal80(am, ase) && normal80(dm, dse) {
                        let (m, se) = self.rem80(am, ase, dm, dse, false);
                        self.set_st80(0, m, se);
                    } else {
                        self.set_st(0, a % s1);
                        self.quotient_bits((a / s1).trunc());
                    }
                }
                1 => {
                    // FYL2XP1.
                    self.set_st(1, s1 * (a.ln_1p() / std::f64::consts::LN_2));
                    self.pop();
                }
                2 => {
                    // FSQRT: in integers for an ordinary positive number.
                    let exact = if normal80(am, ase) && ase & 0x8000 == 0 { self.sqrt80(am, ase) } else { None };
                    match exact {
                        Some((m, se)) => self.set_st80(0, m, se),
                        None => self.set_st(0, self.f_sqrt(a)),
                    }
                }
                3 => {
                    // FSINCOS.
                    self.set_st(0, a.sin());
                    self.push(a.cos());
                    self.cond &= !C2;
                }
                4 => {
                    // FRNDINT. 2^63 or more is already an integer.
                    if normal80(am, ase) {
                        match round80(am, ase, self.rc()) {
                            None => {}
                            Some(0) => self.set_st(0, self.round_int(a)),
                            Some(mag) => {
                                let (m, se) = i64_to_f80(mag as i64);
                                self.set_st80(0, m, se | (ase & 0x8000));
                            }
                        }
                    } else {
                        self.set_st(0, self.round_int(a));
                    }
                }
                5 => {
                    // FSCALE.
                    let e = if s1.is_nan() { 0.0 } else { s1.trunc().clamp(-99999.0, 99999.0) };
                    let ne = exp80(ase) + e as i64;
                    if normal80(am, ase) && ne > 0 && ne < 0x7fff {
                        self.set_st80(0, am, ase & 0x8000 | ne as u16);
                    } else {
                        self.set_st(0, ldexp(a, e as i64));
                    }
                }
                6 => {
                    self.set_st(0, a.sin());
                    self.cond &= !C2;
                }
                _ => {
                    self.set_st(0, a.cos());
                    self.cond &= !C2;
                }
            },
            // DA and DB: FCMOV on CF, ZF, CF or ZF, PF, and on their opposites.
            0x10..=0x13 | 0x18..=0x1b => {
                let holds = match r {
                    0 => flag(CF),
                    1 => flag(ZF),
                    2 => flag(CF | ZF),
                    _ => flag(PF),
                };
                if holds == (row == 2) {
                    self.copy_st(0, i);
                }
            }
            // FUCOMPP, FCOMPP.
            0x15 | 0x33 if i == 1 => {
                self.set_cond(self.cmp_st(1));
                self.pop();
                self.pop();
            }
            0x1c => match i {
                // FNENI, FNDISI, FSETPM do nothing; FNCLEX has nothing to clear.
                0 | 1 | 2 | 4 => {}
                // FNINIT.
                3 => self.reset(),
                _ => return Err(NotWritten),
            },
            // FUCOMI, FCOMI; and with a pop.
            0x1d | 0x1e => cmp_flags(self.cmp_st(i), io),
            0x3d | 0x3e => {
                cmp_flags(self.cmp_st(i), io);
                self.pop();
            }
            // DC: ST(i) = ST(0) op ST(i), the reg field meaning what it means in the D8
            // row.
            0x20..=0x27 => self.arith_to(r, bm, bse, i),
            // DD
            // FFREE.
            0x28 => self.empty |= 1 << self.phys(i),
            0x2a => self.copy_st(i, 0),
            // FUCOM, and with a pop.
            0x2c => self.set_cond(self.cmp_st(i)),
            0x2d | 0x32 => {
                self.set_cond(self.cmp_st(i));
                self.pop();
            }
            // DE: as DC, then pop.
            0x30 | 0x31 | 0x34..=0x37 => {
                self.arith_to(r, bm, bse, i);
                self.pop();
            }
            // DF
            0x38 => {
                // FFREEP.
                self.empty |= 1 << self.phys(i);
                self.pop();
            }
            // FNSTSW AX.
            0x3c if i == 0 => io.ax = Some(self.status()),
            _ => return Err(NotWritten),
        }
        Ok(())
    }

    /// An escape opcode, D8h to DFh, with its ModRM byte's three fields.
    pub fn escape(&mut self, op: u8, md: u8, r: u8, rm: u8, io: &mut Io) -> R<()> {
        if md == 3 { self.reg(op & 7, r, rm, io) } else { self.mem(op & 7, r, io) }
    }
}
