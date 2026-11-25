//! SSE and SSE2: sixteen XMM registers and MXCSR.
//!
//! - An XMM register is two 64-bit words, low then high (`xmm[2i]` and `xmm[2i + 1]`).
//! - There are no MMX registers: a compiler for x86-64 does not use them. The forms of
//!   these opcodes that name one are not written.
//! - Floats are the host's. MXCSR's rounding mode is obeyed where a float becomes an
//!   integer, and not in arithmetic, which rounds to nearest. No exception is raised
//!   and MXCSR's six status bits stay as the program wrote them.
//! - Nothing checks that the memory operand of an aligned move is aligned.
//! - What the host's arithmetic does differently from x86 where a number is missing is
//!   done here by hand: see `far32`.
//!
//! All of it is as g64m has it, which is as the real CPU's traces have it.

use super::{sx, Run, Stop, ARITH, CF, PF, R, RDI, ZF};

const LOW32: u64 = 0xffff_ffff;

/// Is the second opcode byte one of this file's?
pub(super) fn is(op: u8) -> bool {
    matches!(op, 0x10..=0x17 | 0x28..=0x2f | 0x50..=0x7f | 0xc2..=0xc6 | 0xd0..=0xff | 0xae)
}

// ---- Packed integers, lane by lane ----------------------------------------------------

fn sxl(x: u64, w: u32) -> i64 {
    sx(x, w) as i64
}

/// How wide the lanes of an instruction are; 0 for one that is not lane by lane.
fn lane_w(op: u8) -> u32 {
    match op {
        0xfc | 0xf8 | 0xdc | 0xd8 | 0xec | 0xe8 | 0x74 | 0x64 | 0xda | 0xde | 0xe0 => 8,
        0xfd | 0xf9 | 0xdd | 0xd9 | 0xed | 0xe9 | 0x75 | 0x65 | 0xd5 | 0xe5 | 0xe4 | 0xea
        | 0xee | 0xe3 => 16,
        0xfe | 0xfa | 0x76 | 0x66 => 32,
        _ => 0,
    }
}

/// One lane: `x` and `y` are the lanes' bits, `w` their width (8, 16 or 32). The bits
/// above the lane in what comes back are not to be used.
fn lane(op: u8, x: u64, y: u64, w: u32) -> u64 {
    let m = (1u64 << w) - 1;
    let (a, b) = (sxl(x, w), sxl(y, w));
    let top = 1i64 << (w - 1);
    let s = match op {
        0xfc..=0xfe => return x + y,                              // PADD
        0xf8..=0xfa => return x + (m + 1 - y),                    // PSUB
        0xdc | 0xdd => return (x + y).min(m),                     // PADDUS
        0xd8 | 0xd9 => return x.saturating_sub(y),                // PSUBUS
        0xec | 0xed => a + b,                                     // PADDS
        0xe8 | 0xe9 => a - b,                                     // PSUBS
        0x74..=0x76 => return if x == y { m } else { 0 },         // PCMPEQ
        0x64..=0x66 => return if a > b { m } else { 0 },          // PCMPGT
        0xd5 => return (a * b) as u64,                            // PMULLW
        0xe5 => return ((a * b) >> 16) as u64,                    // PMULHW
        0xe4 => return x * y >> 16,                               // PMULHUW
        0xda => return x.min(y),                                  // PMINUB
        0xde => return x.max(y),                                  // PMAXUB
        0xea => return a.min(b) as u64,                           // PMINSW
        0xee => return a.max(b) as u64,                           // PMAXSW
        _ => return (x + y + 1) >> 1,                             // PAVG
    };
    s.clamp(-top, top - 1) as u64
}

/// A packed integer instruction on one 64-bit word; `None` for an opcode that is none.
fn pop64(op: u8, a: u64, b: u64) -> Option<u64> {
    Some(match op {
        0xdb => a & b,
        0xdf => !a & b,
        0xeb => a | b,
        0xef => a ^ b,
        0xd4 => a.wrapping_add(b),                                // PADDQ
        0xfb => a.wrapping_sub(b),                                // PSUBQ
        0xf4 => (a & LOW32) * (b & LOW32),                        // PMULUDQ
        0xf5 => {
            // PMADDWD: the products of the words, added in pairs.
            let mut r = 0;
            for i in 0..2 {
                let p0 = sxl(a >> (i * 32), 16) * sxl(b >> (i * 32), 16);
                let p1 = sxl(a >> (i * 32 + 16), 16) * sxl(b >> (i * 32 + 16), 16);
                r |= ((p0 + p1) as u64 & LOW32) << (i * 32);
            }
            r
        }
        0xf6 => {
            // PSADBW: the sum of the bytes' differences.
            (0..8).map(|i| (a >> (i * 8) & 0xff).abs_diff(b >> (i * 8) & 0xff)).sum()
        }
        _ => {
            let w = lane_w(op);
            if w == 0 {
                return None;
            }
            let m = (1u64 << w) - 1;
            let mut r = 0;
            for at in (0..64).step_by(w as usize) {
                r |= (lane(op, a >> at & m, b >> at & m, w) & m) << at;
            }
            r
        }
    })
}

/// A shift of every lane of a word by `n`: kind 0 right, 1 right keeping the sign, 2
/// left.
fn pshift(kind: u8, w: u32, a: u64, n: u64) -> u64 {
    if w == 64 {
        return match (n > 63, kind) {
            (true, _) => 0,
            (false, 2) => a << n,
            (false, _) => a >> n,
        };
    }
    let m = (1u64 << w) - 1;
    let mut r = 0;
    for at in (0..64).step_by(w as usize) {
        let x = a >> at & m;
        let v = if kind == 1 {
            (sxl(x, w) >> n.min(w as u64 - 1)) as u64
        } else if n >= w as u64 {
            0
        } else if kind == 2 {
            x << n
        } else {
            x >> n
        };
        r |= (v & m) << at;
    }
    r
}

/// The lanes of `x` and `y`, `e` bits each, taken in turn: x0 y0 x1 y1 ... The first 64
/// bits and the rest.
fn weave(x: u64, y: u64, e: u32) -> (u64, u64) {
    let m = if e == 32 { LOW32 } else { (1u64 << e) - 1 };
    let (mut lo, mut hi) = (0, 0);
    for i in 0..64 / e {
        let (a, b) = (x >> (i * e) & m, y >> (i * e) & m);
        let at = i * 2 * e;
        if at < 64 {
            lo |= a << at | b << (at + e);
        } else {
            hi |= a << (at - 64) | b << (at - 64 + e);
        }
    }
    (lo, hi)
}

/// The lanes of `x`, `w` bits each, each squeezed into half as many bits: by its sign,
/// or (to an unsigned one) from zero up.
fn pack(x: u64, w: u32, unsigned: bool) -> u64 {
    let h = w / 2;
    let top = 1i64 << (h - 1);
    let mut r = 0;
    for i in 0..64 / w {
        let v = sxl(x >> (i * w), w);
        let v = if unsigned { v.clamp(0, top * 2 - 1) } else { v.clamp(-top, top - 1) };
        r |= (v as u64 & ((1 << h) - 1)) << (i * h);
    }
    r
}

/// The two-operand packed integer instructions on the words of the destination (`a`)
/// and of the source (`b`); `None` for an opcode that is none.
fn pint(op: u8, a: [u64; 2], b: [u64; 2]) -> Option<[u64; 2]> {
    // Shifts by the count in the source's low word.
    let by_count = match op {
        0xd1 => Some((0, 16)),
        0xd2 => Some((0, 32)),
        0xd3 => Some((0, 64)),
        0xe1 => Some((1, 16)),
        0xe2 => Some((1, 32)),
        0xf1 => Some((2, 16)),
        0xf2 => Some((2, 32)),
        0xf3 => Some((2, 64)),
        _ => None,
    };
    if let Some((kind, w)) = by_count {
        return Some([pshift(kind, w, a[0], b[0]), pshift(kind, w, a[1], b[0])]);
    }
    let lane_of = |op: u8, first: u8| 8 << (op - first);
    Some(match op {
        0x60..=0x62 => weave(a[0], b[0], lane_of(op, 0x60)).into(),
        0x68..=0x6a => weave(a[1], b[1], lane_of(op, 0x68)).into(),
        0x6c => [a[0], b[0]],                                     // PUNPCKLQDQ
        0x6d => [a[1], b[1]],                                     // PUNPCKHQDQ
        0x63 | 0x67 | 0x6b => {
            let pw = if op == 0x6b { 32 } else { 16 };
            let un = op == 0x67;
            [pack(a[0], pw, un) | pack(a[1], pw, un) << 32, pack(b[0], pw, un) | pack(b[1], pw, un) << 32]
        }
        _ => [pop64(op, a[0], b[0])?, pop64(op, a[1], b[1])?],
    })
}

/// The lanes of 32 bits picked by two bits of `imm` each: the low two from `x`, the
/// high two from `y` (SHUFPS; PSHUFD with `x` and `y` the same).
fn pick4(x: [u64; 2], y: [u64; 2], imm: u64) -> [u64; 2] {
    let (mut lo, mut hi) = (0, 0);
    for i in 0..2 {
        let k = (imm >> (i * 2) & 3) as usize;
        lo |= (x[k / 2] >> ((k & 1) * 32) & LOW32) << (i * 32);
        let j = (imm >> (i * 2 + 4) & 3) as usize;
        hi |= (y[j / 2] >> ((j & 1) * 32) & LOW32) << (i * 32);
    }
    [lo, hi]
}

/// A shuffle of four lanes of 16 bits inside a word, by two bits of `imm` to a lane.
fn shuf4(x: u64, imm: u64) -> u64 {
    (0..4).map(|i| (x >> ((imm >> (i * 2) & 3) * 16) & 0xffff) << (i * 16)).fold(0, |r, v| r | v)
}

// ---- Floats -----------------------------------------------------------------------------

fn fs(q: u64) -> f32 {
    f32::from_bits(q as u32)
}

fn fd(q: u64) -> f64 {
    f64::from_bits(q)
}

fn nan32(x: u64) -> bool {
    x & 0x7f80_0000 == 0x7f80_0000 && x & 0x007f_ffff != 0
}

fn nan64(x: u64) -> bool {
    x & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000 && x & 0x000f_ffff_ffff_ffff != 0
}

/// Add, subtract, multiply, divide, the square root, and the two reciprocals, which
/// are exact here where the chip's are approximate.
macro_rules! fop {
    ($op:expr, $a:expr, $b:expr) => {
        match $op {
            0x58 => $a + $b,
            0x59 => $a * $b,
            0x5c => $a - $b,
            0x5e => $a / $b,
            0x51 => $b.sqrt(),
            0x52 => 1.0 / $b.sqrt(),
            _ => 1.0 / $b,
        }
    };
}

/// The arithmetic of one opcode on the bits of two singles (the low halves of `a` and
/// `b`), with what the chip does where a number is missing:
/// - the smaller and the larger give the second operand when either is not a number,
///   and when the two are equal (so for zeros of both signs);
/// - an operand that is not a number comes out, the first before the second, made
///   quiet;
/// - a result that is not a number, of operands that were, is the one "real
///   indefinite": quiet, with the sign set.
fn far32(op: u8, a: u64, b: u64) -> u64 {
    let (a, b) = (a & LOW32, b & LOW32);
    if op == 0x5d || op == 0x5f {
        if nan32(a) || nan32(b) {
            return b;
        }
        let (x, y) = (fs(a), fs(b));
        return if (op == 0x5d && x < y) || (op == 0x5f && x > y) { a } else { b };
    }
    let unary = matches!(op, 0x51..=0x53);
    if !unary && nan32(a) {
        return a | 0x0040_0000;
    }
    if nan32(b) {
        return b | 0x0040_0000;
    }
    let (x, y) = (fs(a), fs(b));
    let r: f32 = fop!(op, x, y);
    if r.is_nan() { 0xffc0_0000 } else { r.to_bits() as u64 }
}

/// The same for two doubles.
fn far64(op: u8, a: u64, b: u64) -> u64 {
    if op == 0x5d || op == 0x5f {
        if nan64(a) || nan64(b) {
            return b;
        }
        let (x, y) = (fd(a), fd(b));
        return if (op == 0x5d && x < y) || (op == 0x5f && x > y) { a } else { b };
    }
    if op != 0x51 && nan64(a) {
        return a | 0x0008_0000_0000_0000;
    }
    if nan64(b) {
        return b | 0x0008_0000_0000_0000;
    }
    let (x, y) = (fd(a), fd(b));
    let r: f64 = fop!(op, x, y);
    if r.is_nan() { 0xfff8_0000_0000_0000 } else { r.to_bits() }
}

/// A comparison by predicate 0 to 7: equal, less, less or equal, unordered, and their
/// opposites.
fn fcmp(pred: u64, a: f64, b: f64) -> bool {
    let un = a.is_nan() || b.is_nan();
    match pred & 7 {
        0 => !un && a == b,
        1 => !un && a < b,
        2 => !un && a <= b,
        3 => un,
        4 => un || a != b,
        5 => un || a >= b,
        6 => un || a > b,
        _ => !un,
    }
}

/// A double's bits as a single's. What is not a number keeps its sign and the top of
/// what it carries, and is made quiet.
fn cvt_ds(q: u64) -> u64 {
    if nan64(q) {
        return (q >> 63) << 31 | 0x7fc0_0000 | q >> 29 & 0x003f_ffff;
    }
    (fd(q) as f32).to_bits() as u64
}

/// A single's bits as a double's, in the same way.
fn cvt_sd(q: u64) -> u64 {
    let q = q & LOW32;
    if nan32(q) {
        return (q >> 31) << 63 | 0x7ff8_0000_0000_0000 | (q & 0x003f_ffff) << 29;
    }
    (fs(q) as f64).to_bits()
}

/// A 32-bit integer in the low half of `v` as a single's bits, and as a double's.
fn i32_single(v: u64) -> u64 {
    (v as i32 as f32).to_bits() as u64
}

fn i32_double(v: u64) -> u64 {
    (v as i32 as f64).to_bits()
}

impl Run<'_> {
    /// Word `k` (0 low, 1 high) of the XMM register or the 128 bits of memory that r/m
    /// names.
    fn xq(&self, k: usize) -> R<u64> {
        if self.md == 3 {
            return Ok(self.c.xmm[self.rm * 2 + k]);
        }
        self.rd(self.ea.wrapping_add(8 * k as u64), 64)
    }

    fn xboth(&self) -> R<[u64; 2]> {
        Ok([self.xq(0)?, self.xq(1)?])
    }

    /// The low 32 bits of the XMM register, or 32 bits of memory, that r/m names.
    fn xs(&self) -> R<u64> {
        if self.md == 3 {
            return Ok(self.c.xmm[self.rm * 2] & LOW32);
        }
        self.rd(self.ea, 32)
    }

    /// A float as a whole number: cut off, or rounded as MXCSR says (to nearest, down,
    /// up, or toward zero).
    fn round_rc(&self, v: f64, cut: bool) -> f64 {
        if cut {
            return v.trunc();
        }
        match self.c.mxcsr >> 13 & 3 {
            0 => v.round_ties_even(),
            1 => v.floor(),
            2 => v.ceil(),
            _ => v.trunc(),
        }
    }

    /// As a 32-bit integer; out of range, or not a number, the "integer indefinite".
    fn to_i32(&self, v: f64, cut: bool) -> u64 {
        let x = self.round_rc(v, cut);
        if x.is_nan() || !(-2147483648.0..2147483648.0).contains(&x) {
            return 0x8000_0000;
        }
        x as i64 as u64 & LOW32
    }

    fn to_i64(&self, v: f64, cut: bool) -> u64 {
        let x = self.round_rc(v, cut);
        if x.is_nan() || !(-9223372036854775808.0..9223372036854775808.0).contains(&x) {
            return 0x8000_0000_0000_0000;
        }
        x as i64 as u64
    }

    /// The flags of a comparison of two singles or two doubles: ZF, PF and CF, the rest
    /// of the six clear.
    fn comis(&mut self, x: f64, y: f64) {
        self.c.rflags &= !ARITH;
        if x.is_nan() || y.is_nan() {
            self.c.rflags |= ZF | PF | CF;
        } else if x < y {
            self.c.rflags |= CF;
        } else if x == y {
            self.c.rflags |= ZF;
        }
    }

    /// The four forms of one arithmetic opcode: packed single, packed double, scalar
    /// single, scalar double.
    fn farith(&mut self, op: u8, p: u8, r: usize) -> R<()> {
        let (a0, a1) = (self.c.xmm[r], self.c.xmm[r + 1]);
        match p {
            0 => {
                let [b0, b1] = self.xboth()?;
                self.c.xmm[r] = far32(op, a0, b0) | far32(op, a0 >> 32, b0 >> 32) << 32;
                self.c.xmm[r + 1] = far32(op, a1, b1) | far32(op, a1 >> 32, b1 >> 32) << 32;
            }
            1 => {
                let [b0, b1] = self.xboth()?;
                self.c.xmm[r] = far64(op, a0, b0);
                self.c.xmm[r + 1] = far64(op, a1, b1);
            }
            2 => self.c.xmm[r] = a0 & !LOW32 | far32(op, a0, self.xs()?),
            _ => self.c.xmm[r] = far64(op, a0, self.xq(0)?),
        }
        Ok(())
    }

    /// FXSAVE: the x87's and SSE's state in 512 bytes of memory, which is how a kernel
    /// keeps it for a program that is not running. The control and status words, a bit
    /// for each x87 register in use, MXCSR and the bits of it that exist, the eight x87
    /// registers as 80-bit values in sixteen bytes each, and the sixteen XMM registers.
    /// The last x87 instruction's address and operand are not kept, and are written as
    /// zero. The bytes after the XMM registers are left as they were.
    fn fxsave(&mut self) -> R<()> {
        let image = fx_image(self.c);
        Ok(self.m.write_bytes(self.ea, &image)?)
    }

    /// FXRSTOR. An MXCSR with a bit that does not exist is refused.
    fn fxrstor(&mut self) -> R<()> {
        let mut b = [0u8; 416];
        self.m.read_bytes(self.ea, &mut b)?;
        if fx_take(self.c, &b) { Ok(()) } else { Err(Stop::Protection) }
    }

    /// 0F AE: FXSAVE, FXRSTOR, LDMXCSR and STMXCSR by the reg field with a memory
    /// operand; the fences with a register one, and CLFLUSH, which have nothing to do
    /// here.
    fn group15(&mut self) -> R<()> {
        self.modrm(0)?;
        let field = self.reg & 7;
        if self.md == 3 {
            return if field < 5 { Err(self.not_written()) } else { Ok(()) };
        }
        match field {
            0 => self.fxsave()?,
            1 => self.fxrstor()?,
            2 => self.c.mxcsr = self.rd(self.ea, 32)? as u32 & 0xffff,
            3 => self.wr(self.ea, 32, self.c.mxcsr as u64)?,
            7 => {}
            _ => return Err(self.not_written()),
        }
        Ok(())
    }

    /// An instruction behind 0Fh that `is` this file's.
    pub(super) fn sse(&mut self, op: u8) -> R<()> {
        // Which prefix chose the instruction: 0 none, 1 66h, 2 F3h, 3 F2h.
        let p = match (self.rep, self.p66) {
            (0xf3, _) => 2,
            (0xf2, _) => 3,
            (_, true) => 1,
            _ => 0,
        };
        if op == 0xae {
            return self.group15();
        }
        if op == 0x77 {
            return Err(self.not_written());
        }
        let has_imm = matches!(op, 0x70..=0x73 | 0xc2 | 0xc4..=0xc6);
        self.modrm(has_imm as u64)?;
        let (r, b) = (self.reg * 2, self.rm * 2);
        let direct = self.md == 3;
        // A general register's width here.
        let gw = if self.rex & 8 != 0 { 64 } else { 32 };
        // What is refused by `no` is a form with a prefix that gives no instruction
        // here, or one that names an MMX register.
        let no = |refused: bool, run: &Self| if refused { Err(run.not_written()) } else { Ok(()) };
        match op {
            0x10 => {
                // MOVUPS, MOVUPD, MOVSS, MOVSD: to the register. From memory the scalar
                // ones clear the rest of it; from a register they leave it.
                if p < 2 {
                    [self.c.xmm[r], self.c.xmm[r + 1]] = self.xboth()?;
                } else if direct {
                    let keep = if p == 2 { !LOW32 } else { 0 };
                    self.c.xmm[r] = self.c.xmm[r] & keep | self.c.xmm[b] & !keep;
                } else {
                    self.c.xmm[r] = self.rd(self.ea, if p == 2 { 32 } else { 64 })?;
                    self.c.xmm[r + 1] = 0;
                }
            }
            0x11 => {
                // And from it.
                if p < 2 {
                    self.xput(r)?;
                } else if direct {
                    let keep = if p == 2 { !LOW32 } else { 0 };
                    self.c.xmm[b] = self.c.xmm[b] & keep | self.c.xmm[r] & !keep;
                } else {
                    self.wr(self.ea, if p == 2 { 32 } else { 64 }, self.c.xmm[r])?;
                }
            }
            0x12 => {
                // MOVLPS and MOVLPD from memory; between registers MOVHLPS, the
                // source's high word.
                no(p > 1, self)?;
                self.c.xmm[r] = if direct { self.c.xmm[b + 1] } else { self.rd(self.ea, 64)? };
            }
            0x13 => {
                no(direct || p > 1, self)?;
                self.wr(self.ea, 64, self.c.xmm[r])?;
            }
            0x16 => {
                // MOVHPS, MOVHPD, MOVLHPS.
                no(p > 1, self)?;
                self.c.xmm[r + 1] = self.xq(0)?;
            }
            0x17 => {
                no(direct || p > 1, self)?;
                self.wr(self.ea, 64, self.c.xmm[r + 1])?;
            }
            0x14 | 0x15 => {
                // UNPCKLPS, UNPCKLPD and the high ones.
                no(p > 1, self)?;
                let k = (op - 0x14) as usize;
                let (x, y) = (self.c.xmm[r + k], self.xq(k)?);
                [self.c.xmm[r], self.c.xmm[r + 1]] = if p == 1 { [x, y] } else { weave(x, y, 32).into() };
            }
            // MOVAPS, MOVAPD.
            0x28 => [self.c.xmm[r], self.c.xmm[r + 1]] = self.xboth()?,
            0x29 => self.xput(r)?,
            0x2b | 0xe7 => {
                // MOVNTPS, MOVNTPD, MOVNTDQ.
                no(direct || if op == 0x2b { p > 1 } else { p != 1 }, self)?;
                self.xput(r)?;
            }
            0x2a => {
                // An integer in a general register or memory, 32 or 64 bits, to a float.
                no(p < 2, self)?;
                let v = self.rm_get(gw)?;
                let wide = if gw == 64 { v as i64 } else { v as i32 as i64 };
                if p == 2 {
                    self.c.xmm[r] = self.c.xmm[r] & !LOW32 | (wide as f32).to_bits() as u64;
                } else {
                    self.c.xmm[r] = (wide as f64).to_bits();
                }
            }
            0x2c | 0x2d => {
                // A float to an integer in a general register, cut off (2Ch) or rounded.
                no(p < 2, self)?;
                let cut = op == 0x2c;
                let v = if p == 2 { fs(self.xs()?) as f64 } else { fd(self.xq(0)?) };
                if gw == 64 {
                    self.c.reg[self.reg] = self.to_i64(v, cut);
                } else {
                    self.sr(self.reg, 32, self.to_i32(v, cut));
                }
            }
            0x2e | 0x2f => {
                // UCOMISS, UCOMISD and COMISS, COMISD.
                no(p > 1, self)?;
                if p == 0 {
                    self.comis(fs(self.c.xmm[r]) as f64, fs(self.xs()?) as f64);
                } else {
                    self.comis(fd(self.c.xmm[r]), fd(self.xq(0)?));
                }
            }
            0x50 => {
                // MOVMSKPS, MOVMSKPD: the signs.
                no(!direct || p > 1, self)?;
                let (lo, hi) = (self.c.xmm[b], self.c.xmm[b + 1]);
                let signs = if p == 1 {
                    lo >> 63 | hi >> 63 << 1
                } else {
                    lo >> 31 & 1 | lo >> 63 << 1 | (hi >> 31 & 1) << 2 | hi >> 63 << 3
                };
                self.sr(self.reg, 32, signs);
            }
            0x51 | 0x58 | 0x59 | 0x5c..=0x5f => self.farith(op, p, r)?,
            0x52 | 0x53 => {
                no(p == 1 || p == 3, self)?;
                self.farith(op, p, r)?;
            }
            0x54..=0x57 => {
                // ANDPS, ANDNPS, ORPS, XORPS and the PD ones: the same bits.
                no(p > 1, self)?;
                let s = self.xboth()?;
                for k in 0..2 {
                    let x = &mut self.c.xmm[r + k];
                    *x = match op {
                        0x54 => *x & s[k],
                        0x55 => !*x & s[k],
                        0x56 => *x | s[k],
                        _ => *x ^ s[k],
                    };
                }
            }
            0x5a => {
                // Between single and double.
                match p {
                    0 => {
                        let s = self.xq(0)?;
                        self.c.xmm[r] = cvt_sd(s);
                        self.c.xmm[r + 1] = cvt_sd(s >> 32);
                    }
                    1 => {
                        let [s0, s1] = self.xboth()?;
                        self.c.xmm[r] = cvt_ds(s0) | cvt_ds(s1) << 32;
                        self.c.xmm[r + 1] = 0;
                    }
                    2 => self.c.xmm[r] = cvt_sd(self.xs()?),
                    _ => self.c.xmm[r] = self.c.xmm[r] & !LOW32 | cvt_ds(self.xq(0)?),
                }
            }
            0x5b => {
                // Four integers to singles, and back, rounded or cut off.
                no(p == 3, self)?;
                let s = self.xboth()?;
                for k in 0..2 {
                    let (lo, hi) = (s[k] & LOW32, s[k] >> 32);
                    self.c.xmm[r + k] = if p == 0 {
                        i32_single(lo) | i32_single(hi) << 32
                    } else {
                        self.to_i32(fs(lo) as f64, p == 2) | self.to_i32(fs(hi) as f64, p == 2) << 32
                    };
                }
            }
            0x6e => {
                // MOVD and MOVQ from a general register or memory.
                no(p != 1, self)?;
                self.c.xmm[r] = self.rm_get(gw)?;
                self.c.xmm[r + 1] = 0;
            }
            0x7e => {
                // MOVD and MOVQ to one; with F3h, MOVQ to an XMM register.
                if p == 2 {
                    self.c.xmm[r] = self.xq(0)?;
                    self.c.xmm[r + 1] = 0;
                } else {
                    no(p != 1, self)?;
                    self.rm_set(gw, self.c.xmm[r])?;
                }
            }
            0x6f => {
                // MOVDQA, MOVDQU.
                no(p == 0 || p == 3, self)?;
                [self.c.xmm[r], self.c.xmm[r + 1]] = self.xboth()?;
            }
            0x7f => {
                no(p == 0 || p == 3, self)?;
                self.xput(r)?;
            }
            0xd6 => {
                // MOVQ from an XMM register.
                no(p != 1, self)?;
                if direct {
                    self.c.xmm[b] = self.c.xmm[r];
                    self.c.xmm[b + 1] = 0;
                } else {
                    self.wr(self.ea, 64, self.c.xmm[r])?;
                }
            }
            0xc3 => {
                // MOVNTI.
                no(direct || p != 0, self)?;
                self.wr(self.ea, gw, self.c.reg[self.reg])?;
            }
            0xe6 => {
                // Two integers to doubles, and two doubles to integers.
                no(p == 0, self)?;
                if p == 2 {
                    let s = self.xq(0)?;
                    self.c.xmm[r] = i32_double(s);
                    self.c.xmm[r + 1] = i32_double(s >> 32);
                } else {
                    let [s0, s1] = self.xboth()?;
                    self.c.xmm[r] = self.to_i32(fd(s0), p == 1) | self.to_i32(fd(s1), p == 1) << 32;
                    self.c.xmm[r + 1] = 0;
                }
            }
            0xc2 => {
                // CMPPS and the others: all ones where the predicate holds.
                let x = [self.c.xmm[r], self.c.xmm[r + 1]];
                let y = match p {
                    2 => [self.xs()?, 0],
                    3 => [self.xq(0)?, 0],
                    _ => self.xboth()?,
                };
                let pred = self.fb()?;
                let single = |a: u64, b: u64| if fcmp(pred, fs(a) as f64, fs(b) as f64) { LOW32 } else { 0 };
                let double = |a: u64, b: u64| if fcmp(pred, fd(a), fd(b)) { !0 } else { 0 };
                match p {
                    0 => {
                        for k in 0..2 {
                            self.c.xmm[r + k] = single(x[k], y[k]) | single(x[k] >> 32, y[k] >> 32) << 32;
                        }
                    }
                    1 => {
                        self.c.xmm[r] = double(x[0], y[0]);
                        self.c.xmm[r + 1] = double(x[1], y[1]);
                    }
                    2 => self.c.xmm[r] = x[0] & !LOW32 | single(x[0], y[0]),
                    _ => self.c.xmm[r] = double(x[0], y[0]),
                }
            }
            0xc6 => {
                // SHUFPS: two lanes from the destination, two from the source. SHUFPD:
                // one each.
                no(p > 1, self)?;
                let x = [self.c.xmm[r], self.c.xmm[r + 1]];
                let y = self.xboth()?;
                let imm = self.fb()?;
                [self.c.xmm[r], self.c.xmm[r + 1]] = if p == 1 {
                    [x[(imm & 1) as usize], y[(imm >> 1 & 1) as usize]]
                } else {
                    pick4(x, y, imm)
                };
            }
            0xc4 => {
                // PINSRW: a word from a general register or memory into a lane.
                no(p != 1, self)?;
                let v = self.rm_get(16)?;
                let imm = self.fb()?;
                let at = (imm & 3) * 16;
                let x = &mut self.c.xmm[r + (imm >> 2 & 1) as usize];
                *x = *x & !(0xffff << at) | v << at;
            }
            0xc5 => {
                // PEXTRW: a lane into a general register.
                no(!direct || p != 1, self)?;
                let imm = self.fb()?;
                let x = self.c.xmm[b + (imm >> 2 & 1) as usize];
                self.sr(self.reg, 32, x >> ((imm & 3) * 16) & 0xffff);
            }
            0xd7 => {
                // PMOVMSKB: the bytes' top bits.
                no(!direct || p != 1, self)?;
                let (lo, hi) = (self.c.xmm[b], self.c.xmm[b + 1]);
                let mut bits = 0;
                for i in 0..8 {
                    bits |= (lo >> (i * 8 + 7) & 1) << i | (hi >> (i * 8 + 7) & 1) << (i + 8);
                }
                self.sr(self.reg, 32, bits);
            }
            0xf7 => {
                // MASKMOVDQU: the bytes of the register whose byte in the mask has its
                // top bit set go to memory at RDI.
                no(!direct || p != 1, self)?;
                let am = if self.asz32 { LOW32 } else { !0 };
                let at = (self.c.reg[RDI] & am).wrapping_add(self.seg_base());
                for i in 0..16u64 {
                    let (k, sh) = ((i / 8) as usize, i % 8 * 8);
                    if self.c.xmm[b + k] >> (sh + 7) & 1 != 0 {
                        self.wr(at.wrapping_add(i), 8, self.c.xmm[r + k] >> sh & 0xff)?;
                    }
                }
            }
            0x70 => {
                // PSHUFD, PSHUFHW, PSHUFLW.
                no(p == 0, self)?;
                let s = self.xboth()?;
                let imm = self.fb()?;
                [self.c.xmm[r], self.c.xmm[r + 1]] = match p {
                    1 => pick4(s, s, imm),
                    2 => [s[0], shuf4(s[1], imm)],
                    _ => [shuf4(s[0], imm), s[1]],
                };
            }
            0x71..=0x73 => {
                // The shifts by a constant, of the register r/m names: /2 right, /4
                // right keeping the sign, /6 left; and of the whole register by bytes,
                // /3 right and /7 left.
                no(!direct || p != 1, self)?;
                let n = self.fb()?;
                let w = 16 << (op - 0x71);
                let field = self.reg & 7;
                if op == 0x73 && (field == 3 || field == 7) {
                    let whole = (self.c.xmm[b + 1] as u128) << 64 | self.c.xmm[b] as u128;
                    let whole = match (n > 15, field) {
                        (true, _) => 0,
                        (false, 3) => whole >> (n * 8),
                        (false, _) => whole << (n * 8),
                    };
                    self.c.xmm[b] = whole as u64;
                    self.c.xmm[b + 1] = (whole >> 64) as u64;
                    return Ok(());
                }
                let kind = match field {
                    2 => 0,
                    4 if op != 0x73 => 1,
                    6 => 2,
                    _ => return Err(Stop::Undefined),
                };
                self.c.xmm[b] = pshift(kind, w, self.c.xmm[b], n);
                self.c.xmm[b + 1] = pshift(kind, w, self.c.xmm[b + 1], n);
            }
            _ => {
                // Everything else is a packed integer instruction of two operands, or
                // nothing.
                no(p != 1, self)?;
                let s = self.xboth()?;
                let Some(v) = pint(op, [self.c.xmm[r], self.c.xmm[r + 1]], s) else {
                    return Err(self.not_written());
                };
                [self.c.xmm[r], self.c.xmm[r + 1]] = v;
            }
        }
        Ok(())
    }

    /// The whole register at `r` to the XMM register or the 128 bits of memory that
    /// r/m names.
    fn xput(&mut self, r: usize) -> R<()> {
        let v = [self.c.xmm[r], self.c.xmm[r + 1]];
        if self.md == 3 {
            self.c.xmm[self.rm * 2] = v[0];
            self.c.xmm[self.rm * 2 + 1] = v[1];
            return Ok(());
        }
        self.wr(self.ea, 64, v[0])?;
        self.wr(self.ea.wrapping_add(8), 64, v[1])
    }
}

/// The first 416 bytes of what FXSAVE writes, from the CPU's state.
pub(super) fn fx_image(c: &super::Cpu) -> [u8; 416] {
    let (cw, sw, used, st) = c.x87.fx_get();
    let mut b = [0u8; 416];
    let mut put = |at: usize, bytes: usize, v: u64| b[at..at + bytes].copy_from_slice(&v.to_le_bytes()[..bytes]);
    put(0, 2, cw as u64);
    put(2, 2, sw as u64);
    put(4, 2, used as u64);
    put(24, 4, c.mxcsr as u64);
    put(28, 4, 0xffff);
    for (i, (m, se)) in st.into_iter().enumerate() {
        put(32 + i * 16, 8, m);
        put(40 + i * 16, 2, se as u64);
    }
    for (i, v) in c.xmm.into_iter().enumerate() {
        put(160 + i * 8, 8, v);
    }
    b
}

/// The CPU's state from such bytes; false, and nothing taken, if MXCSR has a bit
/// that does not exist.
pub(super) fn fx_take(c: &mut super::Cpu, b: &[u8; 416]) -> bool {
    let get = |at: usize, bytes: usize| {
        let mut v = [0u8; 8];
        v[..bytes].copy_from_slice(&b[at..at + bytes]);
        u64::from_le_bytes(v)
    };
    let mxcsr = get(24, 4);
    if mxcsr & 0xffff_0000 != 0 {
        return false;
    }
    c.mxcsr = mxcsr as u32;
    let st = std::array::from_fn(|i| (get(32 + i * 16, 8), get(40 + i * 16, 2) as u16));
    c.x87.fx_set(get(0, 2) as u16, get(2, 2) as u16, get(4, 2) as u8, st);
    for i in 0..32 {
        c.xmm[i] = get(160 + i * 8, 8);
    }
    true
}
