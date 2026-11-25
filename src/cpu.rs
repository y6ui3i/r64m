//! The interpreter: one x86-64 instruction of a program at a time.
//!
//! The simple form (ADR 0007): every instruction is decoded each time it is run, and
//! every flag is worked out when the instruction is. What is right is what the real CPU
//! did (the traces under g64m's `tests/expected64`); where the manual leaves a flag
//! undefined, what is here is what that CPU does, as g64m found it.
//!
//! The integer instructions are here; SSE and SSE2 are in `sse`, the x87 in `x87`.
//!
//! An instruction that cannot go on gives a `Stop` and leaves RIP on itself. It may
//! have changed registers or memory before it stopped: nothing is delivered to the
//! guest yet, so a stop is the end of the program.

use std::cell::Cell;
use crate::mem::{Access, Fault, Mem};

mod sse;
mod x87;

pub const CF: u64 = 0x0001;
pub const PF: u64 = 0x0004;
pub const AF: u64 = 0x0010;
pub const ZF: u64 = 0x0040;
pub const SF: u64 = 0x0080;
pub const DF: u64 = 0x0400;
pub const OF: u64 = 0x0800;
pub const RF: u64 = 0x1_0000;
const ARITH: u64 = CF | PF | AF | ZF | SF | OF;

pub const RAX: usize = 0;
pub const RCX: usize = 1;
pub const RDX: usize = 2;
pub const RBX: usize = 3;
pub const RSP: usize = 4;
pub const RBP: usize = 5;
pub const RSI: usize = 6;
pub const RDI: usize = 7;

/// The layout is fixed (`repr(C)`): the JIT's way in and out reads and writes the
/// first fields by their offsets.
#[repr(C)]
#[derive(Clone)]
pub struct Cpu {
    /// In the order of their numbers in an instruction: RAX, RCX, RDX, RBX, RSP, RBP,
    /// RSI, RDI, R8 to R15.
    pub reg: [u64; 16],
    pub rip: u64,
    pub rflags: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    /// Instructions run so far. RDTSC reads it, so that a run is the same every time.
    pub steps: u64,
    /// The JIT's record of the flags (ADR 0014, point 6), as generated code left it:
    /// kept in three host registers while it runs.
    pub flag_rec: [u64; 3],
    /// Where in generated code the host faulted, when a block was left that way.
    pub fault_pc: u64,
    /// The jump generated code was left by, if it is one that can be rewritten to go
    /// to its target's block; zero if it was left some other way.
    /// Where generated code goes to leave: an address in `jit::enter`, written by it.
    pub way_out: u64,
    pub left_from: u64,
    /// How many instructions the blocks ran that were run to their ends, since
    /// generated code was last gone into.
    pub ran: u64,
    /// The XMM registers, two words each, low then high.
    pub xmm: [u64; 32],
    pub mxcsr: u32,
    pub x87: x87::X87,
    /// A `rep movs` or `rep stos` may do all its repetitions in one step (R006). Off
    /// when each is to be seen (a trace). The count goes on by one for each either way.
    pub rep_whole: bool,
    /// Not zero when a signal waits for this thread: set by the host's handler, looked
    /// at by every block as it begins and between the interpreter's instructions
    /// (ADR 0019, point 2).
    pub kick: u32,
}

/// Floating point's state as FXSAVE lays it out (its first 416 bytes), and the state
/// taken from such bytes: for a signal's frame.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn fx_image(c: &Cpu) -> [u8; 416] {
    sse::fx_image(c)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn fx_take(c: &mut Cpu, image: &[u8; 416]) -> bool {
    sse::fx_take(c, image)
}

impl Cpu {
    /// As Linux starts a program: every register zero, interrupts on.
    pub fn new(rip: u64, rsp: u64) -> Cpu {
        let mut reg = [0; 16];
        reg[RSP] = rsp;
        Cpu {
            reg,
            rip,
            rflags: 0x202,
            fs_base: 0,
            gs_base: 0,
            steps: 0,
            flag_rec: [0; 3],
            fault_pc: 0,
            way_out: 0,
            left_from: 0,
            ran: 0,
            xmm: [0; 32],
            mxcsr: 0x1f80,
            x87: x87::X87::new(),
            rep_whole: false,
            kick: 0,
        }
    }
}

/// Why an instruction did not go on.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop {
    /// Memory that could not be had: a page fault on a real CPU.
    Fault(Fault),
    /// A division by zero, or one whose quotient does not fit.
    Divide,
    /// Not an instruction (UD2, or a form the CPU refuses).
    Undefined,
    /// What the CPU refuses of an instruction that is one: a general protection fault.
    Protection,
    /// An instruction r64m has not been taught yet, with its bytes.
    NotWritten(String),
    /// Never seen outside `step`: an instruction with LOCK found that another thread
    /// had written its operand between its read and its write, and is run again.
    Again,
}

impl From<Fault> for Stop {
    fn from(f: Fault) -> Stop {
        Stop::Fault(f)
    }
}

/// What an instruction that did go on asks of whoever runs the CPU.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Next,
    /// SYSCALL: RCX and R11 are set and RIP is past it; the call is to be answered.
    Syscall,
}

type R<T> = Result<T, Stop>;

#[inline(always)]
fn mask(w: u32) -> u64 {
    if w == 64 { !0 } else { (1 << w) - 1 }
}

#[inline(always)]
fn msb(w: u32) -> u64 {
    1 << (w - 1)
}

/// The low `w` bits of `v` as a signed number, in 64 bits.
#[inline(always)]
fn sx(v: u64, w: u32) -> u64 {
    let s = 64 - w;
    (((v << s) as i64) >> s) as u64
}

/// The zero, sign and parity bits for a result, as they sit in RFLAGS.
#[inline(always)]
fn szp(r: u64, w: u32) -> u64 {
    let mut f = 0;
    if (r as u8).count_ones() % 2 == 0 {
        f |= PF;
    }
    if r & mask(w) == 0 {
        f |= ZF;
    }
    if r & msb(w) != 0 {
        f |= SF;
    }
    f
}

/// The result and the six flags of one of the eight of the opcodes 00h to 3Fh, by its
/// number there (ADD, OR, ADC, SBB, AND, SUB, XOR, CMP), on operands `w` bits wide.
#[inline(always)]
fn alu_flags(op: u8, a: u64, b: u64, w: u32, carry: bool) -> (u64, u64) {
    let (m, s) = (mask(w), msb(w));
    let (a, b) = (a & m, b & m);
    let mut f = 0;
    let r;
    match op {
        0 | 2 => {
            let c = (op == 2 && carry) as u64;
            r = a.wrapping_add(b).wrapping_add(c) & m;
            // The carry out of the top bit: both operands had it, or one had it and
            // the result has lost it.
            if (a & b | (a | b) & !r) & s != 0 {
                f |= CF;
            }
            if (a ^ r) & (b ^ r) & s != 0 {
                f |= OF;
            }
            f |= (a ^ b ^ r) & AF;
        }
        3 | 5 | 7 => {
            let c = (op == 3 && carry) as u64;
            r = a.wrapping_sub(b).wrapping_sub(c) & m;
            if (!a & b | (!a | b) & r) & s != 0 {
                f |= CF;
            }
            if (a ^ b) & (a ^ r) & s != 0 {
                f |= OF;
            }
            f |= (a ^ b ^ r) & AF;
        }
        1 => r = a | b,
        4 => r = a & b,
        _ => r = a ^ b,
    }
    (r, f | szp(r, w))
}

/// kind: 0 ROL 1 ROR 2 RCL 3 RCR 4 SHL 5 SHR 6 SAL 7 SAR. The count uses its low 6
/// bits for a 64-bit operand and its low 5 for any other.
///
/// The manual defines OF for a count of one only. The CPU these flags are taken
/// from sets it, at every count, to what a count of one would have given for the
/// same operand; so OF here is worked out from the operand and not from the result.
/// The one exception is ROL and ROR of a register by a constant larger than one
/// (`quiet`), which leave OF as it was. AF is cleared by the shifts and left alone
/// by the rotations.
pub fn shift_op(kind: u8, v: u64, count: u64, w: u32, quiet: bool, f: &mut u64) -> u64 {
let set = |f: &mut u64, flag: u64, on: bool| if on { *f |= flag } else { *f &= !flag };
    let (m, s) = (mask(w), msb(w));
    let bits = w as u64;
    let v = v & m;
    let mut n = count & if w == 64 { 0x3f } else { 0x1f };
    if n == 0 {
        return v;
    }
    let top = v & s != 0;
    let second = v & s >> 1 != 0;
    let mut r = v;
    let mut cf = *f & CF != 0;
    match kind {
        0 => {
            r = if w == 64 { v.rotate_left(n as u32) } else { (v << n % bits | v >> (bits - n % bits)) & m };
            cf = r & 1 != 0;
            if n == 1 || !quiet {
                set(f, OF, top != second);
            }
        }
        1 => {
            r = if w == 64 { v.rotate_right(n as u32) } else { (v >> n % bits | v << (bits - n % bits)) & m };
            cf = r & s != 0;
            if n == 1 || !quiet {
                set(f, OF, top != (v & 1 != 0));
            }
        }
        2 | 3 => {
            // Through the carry: one bit at a time, of a count taken modulo the
            // width and one. A whole turn changes nothing, the flags included.
            n %= bits + 1;
            if n == 0 {
                return v;
            }
            set(f, OF, top != if kind == 2 { second } else { cf });
            for _ in 0..n {
                if kind == 2 {
                    let out = r & s != 0;
                    r = (r << 1 | cf as u64) & m;
                    cf = out;
                } else {
                    let out = r & 1 != 0;
                    r = r >> 1 | if cf { s } else { 0 };
                    cf = out;
                }
            }
        }
        5 => {
            if n > bits {
                cf = false;
                r = 0;
            } else {
                cf = v >> (n - 1) & 1 != 0;
                r = if n == bits { 0 } else { v >> n };
            }
            set(f, OF, top);
        }
        7 => {
            if n >= bits {
                cf = top;
                r = if top { m } else { 0 };
            } else {
                cf = v >> (n - 1) & 1 != 0;
                r = v >> n;
                if top {
                    r |= m << (bits - n) & m;
                }
            }
            set(f, OF, false);
        }
        _ => {
            if n > bits {
                cf = false;
                r = 0;
            } else {
                cf = v >> (bits - n) & 1 != 0;
                r = if n == bits { 0 } else { v << n & m };
            }
            set(f, OF, top != second);
        }
    }
    set(f, CF, cf);
    if kind >= 4 {
        *f = *f & !(ZF | SF | PF | AF) | szp(r, w);
    }
    r
}

/// The kinds of the JIT's record of the flags (`Cpu::flag_rec`): the first word is
/// the kind times 256 and the operands' width in bits, or zero when RFLAGS is as it
/// should be; the two others are the operands, or for `REC_LOGIC` the result.
pub const REC_ADD: u64 = 1;
pub const REC_SUB: u64 = 2;
pub const REC_LOGIC: u64 = 3;
/// An addition and a subtraction told by their result and their second operand (the
/// first is had back from the two): what is still there after the instruction (R014).
pub const REC_ADD_BY_RESULT: u64 = 4;
pub const REC_SUB_BY_RESULT: u64 = 5;
/// A shift by a count that is not zero: this and the shift's kind (4 SHL, 5 SHR, 7
/// SAR); the operands are what was shifted and by how much.
pub const REC_SHIFT: u64 = 0x10;
/// IMUL of two operands of which the lower half of the product is kept: the two. And
/// IMUL with one operand, whose flags are the same.
pub const REC_IMUL: u64 = 6;
/// MUL with one operand: the two that were multiplied.
pub const REC_MUL: u64 = 7;
/// ADC and SBB: the two operands, and the carry that came in as bit 16 of the first
/// word (above the kind).
pub const REC_ADC: u64 = 8;
pub const REC_SBB: u64 = 9;
/// BSF and BSR: the operand that was scanned.
pub const REC_BSF: u64 = 0x20;
pub const REC_BSR: u64 = 0x21;
/// The flags as RFLAGS holds them: for a record that only says what was done to them
/// since (`REC_CARRY_GIVEN`) when there was no record before.
pub const REC_KEEP: u64 = 0x30;
/// Above the kind: the carry is not what the record's kind makes it but was given
/// since by an instruction that changes the carry alone (a bit test), and is set if
/// `REC_CARRY_SET` is. The other flags are the record's still.
pub const REC_CARRY_GIVEN: u64 = 1 << 24;
pub const REC_CARRY_SET: u64 = 1 << 25;
/// UCOMIS and COMIS: the two operands' bits, singles (width 32) or doubles.
pub const REC_FLOAT: u64 = 0x0a;

/// Works the flags out from the JIT's record, if generated code left one, so that
/// RFLAGS is as the interpreter would have left it.
pub fn settle(c: &mut Cpu) {
    let [kind, a, b] = c.flag_rec;
    if kind == 0 {
        return;
    }
    let w = (kind & 0xff) as u32;
    let carry = kind >> 16 & 1 != 0;
    let f = match kind >> 8 & 0xff {
        REC_KEEP => c.rflags & ARITH,
        REC_ADD => alu_flags(0, a, b, w, false).1,
        REC_SUB => alu_flags(5, a, b, w, false).1,
        REC_ADD_BY_RESULT => alu_flags(0, a.wrapping_sub(b), b, w, false).1,
        REC_SUB_BY_RESULT => alu_flags(5, a.wrapping_add(b), b, w, false).1,
        // A result and nothing: OR's flags are the result's.
        REC_LOGIC => alu_flags(1, a, 0, w, false).1,
        REC_ADC => alu_flags(2, a, b, w, carry).1,
        REC_SBB => alu_flags(3, a, b, w, carry).1,
        REC_MUL => {
            // CF and OF say that the upper half holds something.
            let full = (a & mask(w)) as u128 * (b & mask(w)) as u128;
            let over = if full >> w != 0 { CF | OF } else { 0 };
            over | szp(full as u64 & mask(w), w) & (SF | PF)
        }
        REC_IMUL => {
            // CF and OF say that the upper half holds something; SF and PF are the
            // lower half's; ZF and AF are cleared.
            let full = (sx(a, w) as i64 as i128) * (sx(b, w) as i64 as i128);
            let r = full as u64 & mask(w);
            let over = if sx(r, w) as i64 as i128 != full { CF | OF } else { 0 };
            over | szp(r, w) & (SF | PF)
        }
        REC_FLOAT => {
            // ZF, PF and CF as the interpreter's `comis` has them.
            let (x, y) = if w == 32 {
                (f32::from_bits(a as u32) as f64, f32::from_bits(b as u32) as f64)
            } else {
                (f64::from_bits(a), f64::from_bits(b))
            };
            if x.is_nan() || y.is_nan() {
                ZF | PF | CF
            } else if x < y {
                CF
            } else if x == y {
                ZF
            } else {
                0
            }
        }
        REC_BSF | REC_BSR => {
            // Of a zero: ZF and PF. Else the parity of the bit's number.
            let v = a & mask(w);
            let n = if kind >> 8 & 0xff == REC_BSF { v.trailing_zeros() } else { 63u32.wrapping_sub(v.leading_zeros()) };
            if v == 0 { ZF | PF } else { szp(n as u64, 8) & PF }
        }
        shift => {
            let mut f = c.rflags;
            shift_op((shift - REC_SHIFT) as u8, a, b, w, false, &mut f);
            f & ARITH
        }
    };
    let f = match kind & (REC_CARRY_GIVEN | REC_CARRY_SET) {
        0 | REC_CARRY_SET => f,
        REC_CARRY_GIVEN => f & !CF,
        _ => f | CF,
    };
    c.rflags = c.rflags & !ARITH | f;
    c.flag_rec[0] = 0;
}

/// How many bytes a constant of an operand `w` bits wide takes: never more than four.
#[inline(always)]
fn imm_len(w: u32) -> u64 {
    match w {
        8 => 1,
        16 => 2,
        _ => 4,
    }
}

/// One instruction while it is being run: the CPU, the memory, and what has been read
/// of the instruction so far.
struct Run<'a> {
    c: &'a mut Cpu,
    m: &'a mut Mem,
    /// The instruction's bytes where they lie in the guest's memory, and how many of
    /// the fifteen at most can be run. Every byte is read before the instruction
    /// writes anything.
    code: *const u8,
    have: usize,
    /// The address of the next byte to read; when all is read, of the next instruction.
    ip: u64,
    rex: u8,
    /// The operand size: 16, 32 or 64.
    osz: u32,
    p66: bool,
    asz32: bool,
    /// F2h or F3h if one was in front, else 0.
    rep: u8,
    /// 0 none, 1 FS, 2 GS.
    seg: u8,
    // The ModRM byte's fields, the register numbers with REX's bits in them.
    md: u8,
    reg: usize,
    rm: usize,
    /// A memory operand's address without the segment's base (what LEA gives), and with.
    off: u64,
    ea: u64,
    /// The instruction is to be atomic (it has the LOCK prefix, or exchanges with
    /// memory): its write is made only if its operand is still what it read
    /// (`held`: where, and what), and the registers and flags as they were are kept
    /// for running it again if not (ADR 0019).
    lock: bool,
    held: Cell<(u64, u64)>,
    kept: Option<([u64; 16], u64)>,
}

/// Runs the instruction at RIP.
pub fn step(c: &mut Cpu, m: &mut Mem) -> R<Step> {
    let rip = c.rip;
    // Fewer than fifteen bytes may be there before memory that cannot be run; the
    // instruction faults only if it needs one of the missing ones.
    let (code, have) = m.code_at(rip);
    loop {
        let mut run = Run {
            c: &mut *c,
                m: &mut *m,
            code,
            have,
            ip: rip,
            rex: 0,
            osz: 32,
            p66: false,
            asz32: false,
            rep: 0,
            seg: 0,
            md: 0,
            reg: 0,
            rm: 0,
            off: 0,
                ea: 0,
            lock: false,
            held: Cell::new((u64::MAX, 0)),
            kept: None,
        };
        match run.exec() {
            // Another thread wrote the operand first: from the beginning again, as
            // it began.
            Err(Stop::Again) => {
                if let Some((reg, rflags)) = run.kept {
                    (c.reg, c.rflags) = (reg, rflags);
                }
            }
            done => {
                let (next, what) = done?;
                c.rip = next;
                c.steps += 1;
                return Ok(what);
            }
        }
    }
}

impl Run<'_> {
    // ---- reading the instruction ---------------------------------------------------

    #[inline(always)]
    fn fb(&mut self) -> R<u64> {
        let i = (self.ip - self.c.rip) as usize;
        if i >= 15 {
            // Longer than an instruction can be.
            return Err(Stop::Undefined);
        }
        if i >= self.have {
            return Err(Stop::Fault(Fault { addr: self.ip, access: Access::Fetch }));
        }
        self.ip += 1;
        // Inside the `have` bytes that `code_at` found.
        Ok(unsafe { *self.code.add(i) } as u64)
    }

    #[inline(always)]
    fn fnum(&mut self, bytes: u32) -> R<u64> {
        let i = (self.ip - self.c.rip) as usize;
        if i + bytes as usize > self.have {
            // Past what is there: byte by byte, to fault at the first that is missing.
            let mut v = 0;
            for k in 0..bytes {
                v |= self.fb()? << (8 * k);
            }
            return Ok(v);
        }
        self.ip += bytes as u64;
        let at = unsafe { self.code.add(i) };
        // Inside the `have` bytes that `code_at` found.
        Ok(unsafe {
            match bytes {
                2 => (at as *const u16).read_unaligned() as u64,
                4 => (at as *const u32).read_unaligned() as u64,
                _ => (at as *const u64).read_unaligned(),
            }
        })
    }

    #[inline(always)]
    fn fs8(&mut self) -> R<u64> {
        Ok(sx(self.fb()?, 8))
    }

    #[inline(always)]
    fn fs32(&mut self) -> R<u64> {
        Ok(sx(self.fnum(4)?, 32))
    }

    /// A constant for an operand `w` bits wide: for 64 bits, 32 with their sign.
    #[inline(always)]
    fn fimm(&mut self, w: u32) -> R<u64> {
        match w {
            8 => self.fb(),
            16 => self.fnum(2),
            32 => self.fnum(4),
            _ => self.fs32(),
        }
    }

    /// The next byte without taking it.
    #[inline(always)]
    fn peek(&mut self) -> R<u64> {
        let b = self.fb()?;
        self.ip -= 1;
        Ok(b)
    }

    fn not_written(&self) -> Stop {
        let n = ((self.ip - self.c.rip) as usize).min(self.have);
        let code = unsafe { std::slice::from_raw_parts(self.code, n) };
        let bytes: Vec<String> = code.iter().map(|b| format!("{b:02X}")).collect();
        Stop::NotWritten(bytes.join(" "))
    }

    // ---- registers and memory -------------------------------------------------------

    /// A register `w` bits wide. Without REX, the byte registers 4 to 7 are AH, CH, DH
    /// and BH.
    #[inline(always)]
    fn gr(&self, i: usize, w: u32) -> u64 {
        if w == 8 && self.rex == 0 && (4..8).contains(&i) {
            return self.c.reg[i - 4] >> 8 & 0xff;
        }
        self.c.reg[i] & mask(w)
    }

    /// A 32-bit result clears the register's upper half; 8 and 16-bit ones leave the rest.
    #[inline(always)]
    fn sr(&mut self, i: usize, w: u32, v: u64) {
        let reg = &mut self.c.reg;
        match w {
            64 => reg[i] = v,
            32 => reg[i] = v & 0xffff_ffff,
            16 => reg[i] = reg[i] & !0xffff | v & 0xffff,
            _ if self.rex == 0 && (4..8).contains(&i) => {
                reg[i - 4] = reg[i - 4] & !0xff00 | (v & 0xff) << 8
            }
            _ => reg[i] = reg[i] & !0xff | v & 0xff,
        }
    }

    #[inline(always)]
    fn rd(&self, a: u64, w: u32) -> R<u64> {
        Ok(match w {
            8 => self.m.read_u8(a)? as u64,
            16 => self.m.read_u16(a)? as u64,
            32 => self.m.read_u32(a)? as u64,
            _ => self.m.read_u64(a)?,
        })
    }

    #[inline(always)]
    fn wr(&mut self, a: u64, w: u32, v: u64) -> R<()> {
        Ok(match w {
            8 => self.m.write_u8(a, v as u8)?,
            16 => self.m.write_u16(a, v as u16)?,
            32 => self.m.write_u32(a, v as u32)?,
            _ => self.m.write_u64(a, v)?,
        })
    }

    #[inline(always)]
    fn seg_base(&self) -> u64 {
        match self.seg {
            1 => self.c.fs_base,
            2 => self.c.gs_base,
            _ => 0,
        }
    }

    /// Reads the ModRM byte and what belongs to it. `after` is how many bytes of the
    /// instruction come after the operand (a constant): an address relative to RIP is
    /// relative to the instruction's end.
    #[inline(always)]
    fn modrm(&mut self, after: u64) -> R<()> {
        let b = self.fb()? as u8;
        let (r, x, base_bit) = (self.rex >> 2 & 1, self.rex >> 1 & 1, self.rex & 1);
        self.md = b >> 6;
        self.reg = (b >> 3 & 7 | r << 3) as usize;
        self.rm = (b & 7) as usize;
        if self.md == 3 {
            self.rm |= (base_bit as usize) << 3;
            return Ok(());
        }
        let mut a: u64;
        let mut relative = false;
        if self.rm == 4 {
            let s = self.fb()? as u8;
            let index = (s >> 3 & 7 | x << 3) as usize;
            a = if index == 4 { 0 } else { self.c.reg[index] << (s >> 6) };
            if s & 7 == 5 && self.md == 0 {
                a = a.wrapping_add(self.fs32()?);
            } else {
                a = a.wrapping_add(self.c.reg[(s & 7 | base_bit << 3) as usize]);
            }
        } else if self.rm == 5 && self.md == 0 {
            a = self.fs32()?;
            relative = true;
        } else {
            a = self.c.reg[self.rm | (base_bit as usize) << 3];
        }
        match self.md {
            1 => a = a.wrapping_add(self.fs8()?),
            2 => a = a.wrapping_add(self.fs32()?),
            _ => {}
        }
        if relative {
            a = a.wrapping_add(self.ip).wrapping_add(after);
        }
        if self.asz32 {
            a &= 0xffff_ffff;
        }
        self.off = a;
        self.ea = a.wrapping_add(self.seg_base());
        Ok(())
    }

    #[inline(always)]
    fn rm_get(&self, w: u32) -> R<u64> {
        if self.md == 3 {
            return Ok(self.gr(self.rm, w));
        }
        let v = self.rd(self.ea, w)?;
        if self.lock {
            self.held.set((self.ea, v));
        }
        Ok(v)
    }

    /// From here the instruction is atomic: what it changes before its write is put
    /// back if the write finds its operand changed.
    fn atomic(&mut self) {
        self.lock = true;
        self.kept = Some((self.c.reg, self.c.rflags));
    }

    #[inline(always)]
    fn rm_set(&mut self, w: u32, v: u64) -> R<()> {
        if self.md == 3 {
            self.sr(self.rm, w, v);
            Ok(())
        } else if self.lock && self.held.get().0 == self.ea {
            // Only if it is still what was read.
            match self.m.swap_if(self.ea, w, self.held.get().1 & mask(w), v & mask(w))? {
                true => Ok(()),
                false => Err(Stop::Again),
            }
        } else {
            self.wr(self.ea, w, v)
        }
    }

    /// The operand size of what uses the stack: 64 bits, or 16 with the prefix 66h.
    #[inline(always)]
    fn ssz(&self) -> u32 {
        if self.p66 { 16 } else { 64 }
    }

    #[inline(always)]
    fn push(&mut self, v: u64, w: u32) -> R<()> {
        let sp = self.c.reg[RSP].wrapping_sub(w as u64 / 8);
        self.wr(sp, w, v)?;
        self.c.reg[RSP] = sp;
        Ok(())
    }

    #[inline(always)]
    fn pop(&mut self, w: u32) -> R<u64> {
        let v = self.rd(self.c.reg[RSP], w)?;
        self.c.reg[RSP] = self.c.reg[RSP].wrapping_add(w as u64 / 8);
        Ok(v)
    }

    // ---- arithmetic and flags -------------------------------------------------------

    #[inline(always)]
    fn flag(&self, f: u64) -> bool {
        self.c.rflags & f != 0
    }

    #[inline(always)]
    fn set_flag(&mut self, f: u64, on: bool) {
        if on {
            self.c.rflags |= f;
        } else {
            self.c.rflags &= !f;
        }
    }

    /// The eight of the opcodes 00h to 3Fh, by their number there: ADD, OR, ADC, SBB,
    /// AND, SUB, XOR, CMP.
    #[inline(always)]
    fn alu(&mut self, op: u8, a: u64, b: u64, w: u32) -> u64 {
        let (r, f) = alu_flags(op, a, b, w, self.flag(CF));
        self.c.rflags = self.c.rflags & !ARITH | f;
        r
    }

    /// INC and DEC leave CF alone.
    #[inline(always)]
    fn incdec(&mut self, dec: bool, a: u64, w: u32) -> u64 {
        let carry = self.c.rflags & CF;
        let r = self.alu(if dec { 5 } else { 0 }, a, 1, w);
        self.c.rflags = self.c.rflags & !CF | carry;
        r
    }

    #[inline(always)]
    fn shift(&mut self, kind: u8, v: u64, count: u64, w: u32, quiet: bool) -> u64 {
        shift_op(kind, v, count, w, quiet, &mut self.c.rflags)
    }

    /// SHLD (`left`) and SHRD: shifts `dst` by `count` bits, filling from `src`. OF as
    /// for the shifts: what a count of one would give. With a 16-bit operand and a
    /// count over 16 the bits that come in after src's are dst's own again.
    fn shift_double(&mut self, left: bool, dst: u64, src: u64, count: u64, w: u32) -> u64 {
        let (m, s) = (mask(w), msb(w));
        let n = count & if w == 64 { 0x3f } else { 0x1f };
        let (mut dst, mut src) = (dst & m, src & m);
        if n == 0 {
            return dst;
        }
        let top = dst & s != 0;
        let other = if left { dst & s >> 1 != 0 } else { src & 1 != 0 };
        self.set_flag(OF, top != other);
        let mut cf = false;
        for _ in 0..n {
            if left {
                cf = dst & s != 0;
                dst = (dst << 1 | (src & s != 0) as u64) & m;
                src = (src << 1 | cf as u64) & m;
            } else {
                cf = dst & 1 != 0;
                dst = dst >> 1 | if src & 1 != 0 { s } else { 0 };
                src = src >> 1 | if cf { s } else { 0 };
            }
        }
        self.set_flag(CF, cf);
        self.c.rflags = self.c.rflags & !(ZF | SF | PF | AF) | szp(dst, w);
        dst
    }

    /// The flags after a multiplication: CF and OF say that the upper half holds
    /// something, SF and PF are the lower half's, ZF and AF are cleared.
    fn mul_flags(&mut self, over: bool, lo: u64, w: u32) {
        let f = if over { CF | OF } else { 0 };
        self.c.rflags = self.c.rflags & !ARITH | f | szp(lo, w) & (SF | PF);
    }

    /// A signed multiplication of two operands `w` bits wide, of which the lower half
    /// is kept.
    fn imul2(&mut self, a: u64, b: u64, w: u32) -> u64 {
        let full = (sx(a, w) as i64 as i128) * (sx(b, w) as i64 as i128);
        let r = full as u64 & mask(w);
        self.mul_flags(sx(r, w) as i64 as i128 != full, r, w);
        r
    }

    /// MUL, IMUL, DIV and IDIV with one operand (F6h and F7h, fields 4 to 7). The
    /// divisions leave the flags as they were.
    fn muldiv(&mut self, kind: usize, src: u64, w: u32) -> R<()> {
        let m = mask(w);
        let a = self.c.reg[RAX] & m;
        // The upper half of what is multiplied into or divided: AH for a byte.
        let d = if w == 8 { self.c.reg[RAX] >> 8 & 0xff } else { self.c.reg[RDX] & m };
        let (lo, hi);
        if kind == 4 {
            let full = a as u128 * src as u128;
            (lo, hi) = (full as u64 & m, (full >> w) as u64 & m);
            self.mul_flags(hi != 0, lo, w);
        } else if kind == 5 {
            let full = (sx(a, w) as i64 as i128) * (sx(src, w) as i64 as i128);
            (lo, hi) = (full as u64 & m, (full >> w) as u64 & m);
            self.mul_flags(sx(lo, w) as i64 as i128 != full, lo, w);
        } else if kind == 6 {
            if src == 0 {
                return Err(Stop::Divide);
            }
            let n = (d as u128) << w | a as u128;
            let q = n / src as u128;
            if q > m as u128 {
                return Err(Stop::Divide);
            }
            (lo, hi) = (q as u64, (n % src as u128) as u64);
        } else {
            let n = ((d as u128) << w | a as u128) as i128;
            // The dividend's sign is its own top bit, of twice the width.
            let n = if w == 64 { n } else { n << (128 - 2 * w) >> (128 - 2 * w) };
            let by = sx(src, w) as i64 as i128;
            let Some(q) = n.checked_div(by) else {
                return Err(Stop::Divide);
            };
            let most = msb(w) as i128;
            if q >= most || q < -most {
                return Err(Stop::Divide);
            }
            (lo, hi) = (q as u64 & m, (n - q * by) as u64 & m);
        }
        if w == 8 {
            self.sr(RAX, 16, hi << 8 | lo & 0xff);
        } else {
            self.sr(RAX, w, lo);
            self.sr(RDX, w, hi);
        }
        Ok(())
    }

    /// The sixteen conditions of Jcc, SETcc and CMOVcc.
    #[inline(always)]
    fn cond(&self, cc: u8) -> bool {
        let r = match cc >> 1 & 7 {
            0 => self.flag(OF),
            1 => self.flag(CF),
            2 => self.flag(ZF),
            3 => self.flag(CF) || self.flag(ZF),
            4 => self.flag(SF),
            5 => self.flag(PF),
            6 => self.flag(SF) != self.flag(OF),
            _ => self.flag(ZF) || self.flag(SF) != self.flag(OF),
        };
        r != (cc & 1 != 0)
    }

    /// BT (kind 0), BTS, BTR, BTC. A register's bit number reaches outside a memory
    /// operand: it is a signed number of bits from the operand's address.
    fn bit_op(&mut self, kind: u8, w: u32, off: u64, from_reg: bool) -> R<()> {
        let mut a = self.ea;
        if self.md != 3 && from_reg {
            let words = (sx(off, w) as i64) >> w.trailing_zeros();
            a = a.wrapping_add((words as u64).wrapping_mul(w as u64 / 8));
        }
        let v = if self.md == 3 { self.gr(self.rm, w) } else { self.rd(a, w)? };
        let bit = 1u64 << (off & (w as u64 - 1));
        self.set_flag(CF, v & bit != 0);
        let r = match kind {
            0 => return Ok(()),
            1 => v | bit,
            2 => v & !bit,
            _ => v ^ bit,
        };
        if self.md == 3 {
            self.sr(self.rm, w, r);
            Ok(())
        } else {
            self.wr(a, w, r)
        }
    }

    /// One repetition of a string instruction (A4h to AFh). With REP, RIP stays on the
    /// instruction until the count runs out, as on a CPU, where something may come in
    /// between two repetitions. Until the last one RF is set, as it is in what such an
    /// interruption saves; and the flags of a repeated CMPS or SCAS are not to be seen
    /// until the last one either. Gives back where to go on.
    fn string_op(&mut self, op: u8) -> R<u64> {
        let w = if op & 1 == 0 { 8 } else { self.osz };
        let am = if self.asz32 { 0xffff_ffff } else { !0 };
        if self.rep != 0 && self.c.reg[RCX] & am == 0 {
            return Ok(self.ip);
        }
        if self.rep != 0 && self.c.rep_whole && !self.flag(DF) && !self.asz32 && op & 0xfc != 0xac {
            // MOVS and STOS going up, all the repetitions at once, if all of it can be
            // done: a fault part of the way is left to the repetitions one at a time.
            let n = self.c.reg[RCX];
            let bytes = w as u64 / 8;
            let len = n.saturating_mul(bytes);
            let (si, di) = (self.c.reg[RSI], self.c.reg[RDI]);
            let done = match op & 0xfe {
                0xa4 => self.m.copy_up(di, si.wrapping_add(self.seg_base()), len),
                0xaa => Some(self.m.fill(di, self.c.reg[RAX], bytes, n)),
                _ => None,
            };
            if let Some(Ok(())) = done {
                if op & 0xfe == 0xa4 {
                    self.c.reg[RSI] = si.wrapping_add(len);
                }
                self.c.reg[RDI] = di.wrapping_add(len);
                self.c.reg[RCX] = 0;
                self.c.steps += n - 1;
                return Ok(self.ip);
            }
        }
        let began = self.c.rflags;
        let step = if self.flag(DF) { (w as u64 / 8).wrapping_neg() } else { w as u64 / 8 };
        let si = self.c.reg[RSI] & am;
        let di = self.c.reg[RDI] & am;
        let from = si.wrapping_add(self.seg_base());
        let (uses_si, uses_di, compares) = match op & 0xfe {
            0xa4 => {
                let v = self.rd(from, w)?;
                self.wr(di, w, v)?;
                (true, true, false)
            }
            0xa6 => {
                let (a, b) = (self.rd(from, w)?, self.rd(di, w)?);
                self.alu(7, a, b, w);
                (true, true, true)
            }
            0xaa => {
                self.wr(di, w, self.c.reg[RAX])?;
                (false, true, false)
            }
            0xac => {
                let v = self.rd(from, w)?;
                self.sr(RAX, w, v);
                (true, false, false)
            }
            _ => {
                let b = self.rd(di, w)?;
                self.alu(7, self.c.reg[RAX], b, w);
                (false, true, true)
            }
        };
        if uses_si {
            self.c.reg[RSI] = si.wrapping_add(step) & am;
        }
        if uses_di {
            self.c.reg[RDI] = di.wrapping_add(step) & am;
        }
        if self.rep == 0 {
            return Ok(self.ip);
        }
        let left = self.c.reg[RCX].wrapping_sub(1) & am;
        self.c.reg[RCX] = left;
        if left == 0 || compares && self.flag(ZF) != (self.rep == 0xf3) {
            return Ok(self.ip);
        }
        self.c.rflags = began | RF;
        Ok(self.c.rip)
    }

    /// What r64m says it is: the CPU the traces were taken on, with only what r64m does.
    fn cpuid(&mut self) {
        let (a, b, c, d): (u64, u64, u64, u64) = match self.c.reg[RAX] as u32 {
            // "GenuineIntel"
            0 => (1, 0x756e_6547, 0x6c65_746e, 0x4965_6e69),
            // FPU TSC CX8 CMOV MMX FXSR SSE SSE2; POPCNT. (MMX is said because every
            // x86-64 CPU has it and glibc will not start without: its registers and
            // instructions are not written yet, and are owed when a program uses
            // them. R032.)
            1 => (0x0003_06a9, 0, 0x0080_0000, 0x0780_8111),
            0x8000_0000 => (0x8000_0001, 0, 0, 0),
            // long mode, SYSCALL; LAHF
            0x8000_0001 => (0, 0, 1, 0x2000_0800),
            _ => (0, 0, 0, 0),
        };
        self.c.reg[RAX] = a;
        self.c.reg[RBX] = b;
        self.c.reg[RCX] = c;
        self.c.reg[RDX] = d;
    }

    /// An x87 instruction (D8h to DFh). The unit touches no memory itself: the bytes
    /// of a memory operand are read for it before, or written for it after. A store
    /// that cannot be made faults before anything has changed.
    fn x87(&mut self, op: u8) -> R<()> {
        self.modrm(0)?;
        // REX names no other register stack: the fields are 0 to 7.
        let (md, r, rm) = (self.md, self.reg as u8 & 7, self.rm as u8 & 7);
        // Sixteen bytes hold every operand but an environment and a whole unit saved,
        // which are the only ones the larger piece is cleared for.
        let env32 = !self.p66;
        let shape = if md == 3 { Some((0, false)) } else { x87::shape(op & 7, r, env32) };
        let Some((n, store)) = shape else {
            return Err(self.not_written());
        };
        let (mut small, mut large);
        let buf: &mut [u8] = if n <= 16 {
            small = [0u8; 16];
            &mut small
        } else {
            large = [0u8; 112];
            &mut large
        };
        let mut io = x87::Io { buf, env32, rflags: self.c.rflags, ax: None };
        let mut stores = 0;
        if md != 3 {
            if store {
                self.m.check(self.ea, n as u64, Access::Write)?;
                stores = n;
            } else {
                // The usual sizes as one or two reads; an environment as bytes.
                match n {
                    2 | 4 | 8 => io.buf[..8].copy_from_slice(&self.rd(self.ea, n as u32 * 8)?.to_le_bytes()),
                    10 => {
                        self.m.check(self.ea, 10, Access::Read)?;
                        io.buf[..8].copy_from_slice(&self.rd(self.ea, 64)?.to_le_bytes());
                        io.buf[8..16].copy_from_slice(&self.rd(self.ea.wrapping_add(8), 16)?.to_le_bytes());
                    }
                    _ => self.m.read_bytes(self.ea, &mut io.buf[..n])?,
                }
            }
        }
        if self.c.x87.escape(op, md, r, rm, &mut io).is_err() {
            return Err(self.not_written());
        }
        self.c.rflags = io.rflags;
        if let Some(ax) = io.ax {
            self.sr(RAX, 16, ax as u64);
        }
        let word = u64::from_le_bytes(io.buf[..8].try_into().unwrap());
        match stores {
            0 => {}
            2 | 4 | 8 => self.wr(self.ea, stores as u32 * 8, word)?,
            10 => {
                self.wr(self.ea, 64, word)?;
                self.wr(self.ea.wrapping_add(8), 16, io.buf[8] as u64 | (io.buf[9] as u64) << 8)?;
            }
            _ => self.m.write_bytes(self.ea, &io.buf[..stores])?,
        }
        Ok(())
    }

    // ---- one instruction ------------------------------------------------------------

    /// Runs it; gives back where to go on and what it asks.
    fn exec(&mut self) -> R<(u64, Step)> {
        let op = loop {
            let b = self.fb()? as u8;
            if b & 0xf0 == 0x40 {
                self.rex = b;
                continue;
            }
            match b {
                0x66 => self.p66 = true,
                0x67 => self.asz32 = true,
                0xf2 | 0xf3 => self.rep = b,
                0xf0 => self.atomic(),
                0x26 | 0x2e | 0x36 | 0x3e => {}
                0x64 => self.seg = 1,
                0x65 => self.seg = 2,
                _ => break b,
            }
            // A REX prefix counts only when it is the last prefix.
            self.rex = 0;
        };
        self.osz = if self.rex & 8 != 0 {
            64
        } else if self.p66 {
            16
        } else {
            32
        };
        let osz = self.osz;
        let w = if op & 1 == 0 { 8 } else { osz };
        // The register in the opcode's low bits.
        let low = (op & 7 | (self.rex & 1) << 3) as usize;
        let mut next = None;
        let mut what = Step::Next;
        self.c.rflags &= !RF;

        match op {
            0x00..=0x3f if op & 7 < 6 => {
                let kind = op >> 3;
                let (a, b);
                match op & 7 {
                    0 | 1 => {
                        self.modrm(0)?;
                        (a, b) = (self.rm_get(w)?, self.gr(self.reg, w));
                    }
                    2 | 3 => {
                        self.modrm(0)?;
                        (a, b) = (self.gr(self.reg, w), self.rm_get(w)?);
                    }
                    _ => (a, b) = (self.gr(RAX, w), self.fimm(w)?),
                }
                let r = self.alu(kind, a, b, w);
                if kind != 7 {
                    match op & 7 {
                        0 | 1 => self.rm_set(w, r)?,
                        2 | 3 => self.sr(self.reg, w, r),
                        _ => self.sr(RAX, w, r),
                    }
                }
            }
            0x50..=0x57 => self.push(self.c.reg[low], self.ssz())?,
            0x58..=0x5f => {
                let v = self.pop(self.ssz())?;
                self.sr(low, self.ssz(), v);
            }
            0x63 => {
                self.modrm(0)?;
                let v = self.rm_get(32)?;
                self.sr(self.reg, osz, if osz == 64 { sx(v, 32) } else { v });
            }
            0x68 => {
                let v = if self.p66 { self.fnum(2)? } else { self.fs32()? };
                self.push(v, self.ssz())?;
            }
            0x6a => {
                let v = self.fs8()?;
                self.push(v, self.ssz())?;
            }
            0x69 | 0x6b => {
                self.modrm(if op == 0x6b { 1 } else { imm_len(osz) })?;
                let b = if op == 0x6b { self.fs8()? } else { self.fimm(osz)? };
                let r = self.imul2(self.rm_get(osz)?, b, osz);
                self.sr(self.reg, osz, r);
            }
            0x70..=0x7f => {
                let d = self.fs8()?;
                if self.cond(op) {
                    next = Some(self.ip.wrapping_add(d));
                }
            }
            0x80 | 0x81 | 0x83 => {
                self.modrm(if op == 0x81 { imm_len(w) } else { 1 })?;
                let b = if op == 0x81 { self.fimm(w)? } else { self.fs8()? };
                let kind = self.reg as u8 & 7;
                let r = self.alu(kind, self.rm_get(w)?, b, w);
                if kind != 7 {
                    self.rm_set(w, r)?;
                }
            }
            0x84 | 0x85 => {
                self.modrm(0)?;
                self.alu(4, self.rm_get(w)?, self.gr(self.reg, w), w);
            }
            0x86 | 0x87 => {
                self.modrm(0)?;
                // An exchange with memory is atomic without the prefix.
                if self.md != 3 {
                    self.atomic();
                }
                let (a, b) = (self.rm_get(w)?, self.gr(self.reg, w));
                self.rm_set(w, b)?;
                self.sr(self.reg, w, a);
            }
            0x88 | 0x89 => {
                self.modrm(0)?;
                self.rm_set(w, self.gr(self.reg, w))?;
            }
            0x8a | 0x8b => {
                self.modrm(0)?;
                let v = self.rm_get(w)?;
                self.sr(self.reg, w, v);
            }
            0x8d => {
                self.modrm(0)?;
                if self.md == 3 {
                    return Err(Stop::Undefined);
                }
                self.sr(self.reg, osz, self.off);
            }
            0x8f => {
                // The operand's address is worked out with RSP already moved.
                let v = self.pop(self.ssz())?;
                self.modrm(0)?;
                if self.reg & 7 != 0 {
                    return Err(Stop::Undefined);
                }
                self.rm_set(self.ssz(), v)?;
            }
            // 90h alone is NOP (and PAUSE with F3h); with REX.B it is XCHG with R8.
            0x90 if self.rex & 1 == 0 => {}
            0x90..=0x97 => {
                let (a, b) = (self.gr(RAX, osz), self.gr(low, osz));
                self.sr(RAX, osz, b);
                self.sr(low, osz, a);
            }
            0x98 => {
                // CBW, CWDE, CDQE: the A register's lower half, with its sign, to all of it.
                let v = sx(self.c.reg[RAX], osz / 2);
                self.sr(RAX, osz, v);
            }
            0x99 => {
                // CWD, CDQ, CQO: the A register's sign to all of the D register.
                let sign = self.c.reg[RAX] & msb(osz) != 0;
                self.sr(RDX, osz, if sign { !0 } else { 0 });
            }
            // FWAIT: there is never an exception waiting.
            0x9b => {}
            0x9c => self.push(self.c.rflags & 0xfc_ffff, self.ssz())?,
            0x9d => {
                // A program may not change IF or IOPL.
                let v = self.pop(self.ssz())?;
                let can = if self.p66 { 0x4dd5 } else { 0x24_4dd5 };
                self.c.rflags = self.c.rflags & !can | v & can;
            }
            0x9e => self.c.rflags = self.c.rflags & !0xd5 | self.c.reg[RAX] >> 8 & 0xd5,
            0x9f => {
                self.c.reg[RAX] = self.c.reg[RAX] & !0xff00 | (self.c.rflags & 0xd5 | 2) << 8
            }
            0xa0..=0xa3 => {
                let a = if self.asz32 { self.fnum(4)? } else { self.fnum(8)? };
                let a = a.wrapping_add(self.seg_base());
                if op < 0xa2 {
                    let v = self.rd(a, w)?;
                    self.sr(RAX, w, v);
                } else {
                    self.wr(a, w, self.c.reg[RAX])?;
                }
            }
            0xa4..=0xa7 | 0xaa..=0xaf => next = Some(self.string_op(op)?),
            0xa8 | 0xa9 => {
                let b = self.fimm(w)?;
                self.alu(4, self.gr(RAX, w), b, w);
            }
            0xb0..=0xb7 => {
                let v = self.fb()?;
                self.sr(low, 8, v);
            }
            0xb8..=0xbf => {
                let v = self.fnum(osz / 8)?;
                self.sr(low, osz, v);
            }
            0xc0 | 0xc1 | 0xd0..=0xd3 => {
                self.modrm((op < 0xd0) as u64)?;
                let n = match op {
                    0xc0 | 0xc1 => self.fb()?,
                    0xd0 | 0xd1 => 1,
                    _ => self.c.reg[RCX] & 0xff,
                };
                let quiet = op < 0xd0 && self.md == 3;
                let r = self.shift(self.reg as u8 & 7, self.rm_get(w)?, n, w, quiet);
                self.rm_set(w, r)?;
            }
            0xc2 => {
                let n = self.fnum(2)?;
                next = Some(self.pop(64)?);
                self.c.reg[RSP] = self.c.reg[RSP].wrapping_add(n);
            }
            0xc3 => next = Some(self.pop(64)?),
            0xc6 | 0xc7 => {
                self.modrm(imm_len(w))?;
                if self.reg & 7 != 0 {
                    return Err(self.not_written());
                }
                let v = self.fimm(w)?;
                self.rm_set(w, v)?;
            }
            0xc9 => {
                self.c.reg[RSP] = self.c.reg[RBP];
                let v = self.pop(self.ssz())?;
                self.sr(RBP, self.ssz(), v);
            }
            0xe0..=0xe2 => {
                // LOOPNE, LOOPE, LOOP.
                let d = self.fs8()?;
                let am = if self.asz32 { 0xffff_ffff } else { !0 };
                let left = self.c.reg[RCX].wrapping_sub(1) & am;
                self.c.reg[RCX] = left;
                if left != 0 && (op == 0xe2 || self.flag(ZF) == (op == 0xe1)) {
                    next = Some(self.ip.wrapping_add(d));
                }
            }
            0xe3 => {
                let d = self.fs8()?;
                let am = if self.asz32 { 0xffff_ffff } else { !0 };
                if self.c.reg[RCX] & am == 0 {
                    next = Some(self.ip.wrapping_add(d));
                }
            }
            0xe8 => {
                let d = self.fs32()?;
                self.push(self.ip, 64)?;
                next = Some(self.ip.wrapping_add(d));
            }
            0xe9 => {
                let d = self.fs32()?;
                next = Some(self.ip.wrapping_add(d));
            }
            0xeb => {
                let d = self.fs8()?;
                next = Some(self.ip.wrapping_add(d));
            }
            0xd8..=0xdf => self.x87(op)?,
            0xf5 => self.c.rflags ^= CF,
            0xf8 => self.c.rflags &= !CF,
            0xf9 => self.c.rflags |= CF,
            0xfc => self.c.rflags &= !DF,
            0xfd => self.c.rflags |= DF,
            0xf6 | 0xf7 => {
                // TEST, NOT, NEG, MUL, IMUL, DIV, IDIV. TEST has a constant behind the
                // operand.
                let field = (self.peek()? >> 3 & 7) as usize;
                self.modrm(if field < 2 { imm_len(w) } else { 0 })?;
                match field {
                    0 | 1 => {
                        let b = self.fimm(w)?;
                        self.alu(4, self.rm_get(w)?, b, w);
                    }
                    2 => self.rm_set(w, !self.rm_get(w)?)?,
                    3 => {
                        let r = self.alu(5, 0, self.rm_get(w)?, w);
                        self.rm_set(w, r)?;
                    }
                    _ => self.muldiv(field, self.rm_get(w)?, w)?,
                }
            }
            0xfe | 0xff => {
                self.modrm(0)?;
                let field = self.reg & 7;
                match field {
                    0 | 1 => {
                        let r = self.incdec(field == 1, self.rm_get(w)?, w);
                        self.rm_set(w, r)?;
                    }
                    2 if op == 0xff => {
                        let to = self.rm_get(64)?;
                        self.push(self.ip, 64)?;
                        next = Some(to);
                    }
                    4 if op == 0xff => next = Some(self.rm_get(64)?),
                    6 if op == 0xff => self.push(self.rm_get(self.ssz())?, self.ssz())?,
                    _ => return Err(self.not_written()),
                }
            }
            0x0f => {
                let op2 = self.fb()? as u8;
                let w2 = if op2 & 1 == 0 { 8 } else { osz };
                match op2 {
                    _ if sse::is(op2) => self.sse(op2)?,
                    0x05 => {
                        self.c.reg[RCX] = self.ip;
                        self.c.reg[11] = self.c.rflags;
                        what = Step::Syscall;
                    }
                    0x0b => return Err(Stop::Undefined),
                    // The hints, the long NOP, ENDBR64: an operand that is not used.
                    0x0d | 0x18..=0x1f => self.modrm(0)?,
                    0x31 => {
                        self.c.reg[RAX] = self.c.steps & 0xffff_ffff;
                        self.c.reg[RDX] = self.c.steps >> 32;
                    }
                    0x40..=0x4f => {
                        // The register is written either way: a 32-bit one loses its
                        // upper half even when the condition does not hold.
                        self.modrm(0)?;
                        let v = self.rm_get(osz)?;
                        let v = if self.cond(op2) { v } else { self.gr(self.reg, osz) };
                        self.sr(self.reg, osz, v);
                    }
                    0x80..=0x8f => {
                        let d = self.fs32()?;
                        if self.cond(op2) {
                            next = Some(self.ip.wrapping_add(d));
                        }
                    }
                    0x90..=0x9f => {
                        self.modrm(0)?;
                        self.rm_set(8, self.cond(op2) as u64)?;
                    }
                    0xa2 => self.cpuid(),
                    0xa3 | 0xab | 0xb3 | 0xbb => {
                        self.modrm(0)?;
                        let kind = match op2 {
                            0xa3 => 0,
                            0xab => 1,
                            0xb3 => 2,
                            _ => 3,
                        };
                        self.bit_op(kind, osz, self.gr(self.reg, osz), true)?;
                    }
                    0xba => {
                        self.modrm(1)?;
                        let n = self.fb()?;
                        if self.reg & 7 < 4 {
                            return Err(Stop::Undefined);
                        }
                        self.bit_op(self.reg as u8 & 3, osz, n, false)?;
                    }
                    0xa4 | 0xac | 0xa5 | 0xad => {
                        let by_constant = op2 & 1 == 0;
                        self.modrm(by_constant as u64)?;
                        let n = if by_constant { self.fb()? } else { self.c.reg[RCX] & 0xff };
                        let r = self.shift_double(
                            op2 < 0xa8,
                            self.rm_get(osz)?,
                            self.gr(self.reg, osz),
                            n,
                            osz,
                        );
                        self.rm_set(osz, r)?;
                    }
                    0xaf => {
                        self.modrm(0)?;
                        let r = self.imul2(self.gr(self.reg, osz), self.rm_get(osz)?, osz);
                        self.sr(self.reg, osz, r);
                    }
                    0xb0 | 0xb1 => {
                        // CMPXCHG: the A register is compared with the operand; if they
                        // are equal the operand takes the register named, and if not, A
                        // takes the operand.
                        self.modrm(0)?;
                        let cur = self.rm_get(w2)?;
                        self.alu(7, self.gr(RAX, w2), cur, w2);
                        if self.flag(ZF) {
                            self.rm_set(w2, self.gr(self.reg, w2))?;
                        } else {
                            self.rm_set(w2, cur)?;
                            self.sr(RAX, w2, cur);
                        }
                    }
                    0xb6 | 0xb7 | 0xbe | 0xbf => {
                        // MOVZX and MOVSX, from a byte or from 16 bits.
                        self.modrm(0)?;
                        let from = if op2 & 1 == 0 { 8 } else { 16 };
                        let v = self.rm_get(from)?;
                        self.sr(self.reg, osz, if op2 >= 0xbe { sx(v, from) } else { v });
                    }
                    0xb8 if self.rep == 0xf3 => {
                        // POPCNT.
                        self.modrm(0)?;
                        let n = self.rm_get(osz)?.count_ones() as u64;
                        self.sr(self.reg, osz, n);
                        self.c.rflags = self.c.rflags & !ARITH | if n == 0 { ZF } else { 0 };
                    }
                    0xbc | 0xbd => {
                        // BSF and BSR. Of a zero the register is left as it was, upper
                        // half and all. (F3h in front makes no difference on a CPU
                        // without TZCNT and LZCNT.)
                        self.modrm(0)?;
                        let v = self.rm_get(osz)?;
                        if v == 0 {
                            self.c.rflags = self.c.rflags & !ARITH | ZF | PF;
                        } else {
                            let n = if op2 == 0xbc { v.trailing_zeros() } else { 63 - v.leading_zeros() };
                            self.sr(self.reg, osz, n as u64);
                            self.c.rflags = self.c.rflags & !ARITH | szp(n as u64, 8) & PF;
                        }
                    }
                    0xc0 | 0xc1 => {
                        // XADD.
                        self.modrm(0)?;
                        let (a, b) = (self.rm_get(w2)?, self.gr(self.reg, w2));
                        let r = self.alu(0, a, b, w2);
                        self.sr(self.reg, w2, a);
                        self.rm_set(w2, r)?;
                    }
                    0xc7 => {
                        // CMPXCHG8B and, with a 64-bit operand size, CMPXCHG16B.
                        self.modrm(0)?;
                        if self.reg & 7 != 1 || self.md == 3 {
                            return Err(self.not_written());
                        }
                        let h = if osz == 64 { 64 } else { 32 };
                        let second = self.ea.wrapping_add(h as u64 / 8);
                        let (lo, hi) = (self.rd(self.ea, h)?, self.rd(second, h)?);
                        if lo == self.gr(RAX, h) && hi == self.gr(RDX, h) {
                            self.wr(self.ea, h, self.c.reg[RBX])?;
                            self.wr(second, h, self.c.reg[RCX])?;
                            self.c.rflags |= ZF;
                        } else {
                            self.sr(RAX, h, lo);
                            self.sr(RDX, h, hi);
                            self.c.rflags &= !ZF;
                        }
                    }
                    0xc8..=0xcf => {
                        let i = (op2 & 7 | (self.rex & 1) << 3) as usize;
                        let v = self.c.reg[i];
                        self.c.reg[i] =
                            if osz == 64 { v.swap_bytes() } else { (v as u32).swap_bytes() as u64 };
                    }
                    _ => return Err(self.not_written()),
                }
            }
            _ => return Err(self.not_written()),
        }
        Ok((next.unwrap_or(self.ip), what))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mem::{R as READ, W, X};

    const CODE: u64 = 0x40_0000;
    const DATA: u64 = 0x50_0000;

    /// Runs `code` until its UD2 (which changes nothing and is not counted), with a page
    /// of data that holds `data`, and gives back the CPU and that page.
    fn run(code: &[u8], data: &[u8], set: impl Fn(&mut Cpu), rep_whole: bool) -> (Cpu, Vec<u8>) {
        let mut m = Mem::reserve().unwrap();
        m.map(CODE, 4096, READ | W).unwrap();
        m.write_bytes(CODE, code).unwrap();
        m.protect(CODE, 4096, READ | X).unwrap();
        m.map(DATA, 4096, READ | W).unwrap();
        m.write_bytes(DATA, data).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        c.rep_whole = rep_whole;
        set(&mut c);
        loop {
            match step(&mut c, &mut m) {
                Ok(_) => {}
                Err(Stop::Undefined) => break,
                Err(other) => panic!("the program stopped: {other:?}"),
            }
        }
        let mut page = vec![0u8; 4096];
        m.read_bytes(DATA, &mut page).unwrap();
        (c, page)
    }

    /// The same program with every repetition a step and with a `rep` in one step:
    /// the registers, the flags, the memory and the count must all be the same.
    fn both_ways(code: &[u8], data: &[u8], set: impl Fn(&mut Cpu) + Copy) -> (Cpu, Vec<u8>) {
        let (one, page_one) = run(code, data, set, false);
        let (whole, page_whole) = run(code, data, set, true);
        assert_eq!(whole.reg, one.reg);
        assert_eq!(whole.rflags, one.rflags);
        assert_eq!(whole.rip, one.rip);
        assert_eq!(whole.steps, one.steps);
        assert_eq!(page_whole, page_one);
        (whole, page_whole)
    }

    // rep movsb; ud2, and the like.
    const REP_MOVSB: &[u8] = &[0xf3, 0xa4, 0x0f, 0x0b];
    const REP_MOVSQ: &[u8] = &[0xf3, 0x48, 0xa5, 0x0f, 0x0b];
    const REP_STOSB: &[u8] = &[0xf3, 0xaa, 0x0f, 0x0b];
    const REP_STOSW: &[u8] = &[0xf3, 0x66, 0xab, 0x0f, 0x0b];
    const REP_STOSD: &[u8] = &[0xf3, 0xab, 0x0f, 0x0b];
    const REP_STOSQ: &[u8] = &[0xf3, 0x48, 0xab, 0x0f, 0x0b];
    const STD_REP_MOVSB: &[u8] = &[0xfd, 0xf3, 0xa4, 0x0f, 0x0b];
    const REP_MOVSB_32: &[u8] = &[0x67, 0xf3, 0xa4, 0x0f, 0x0b];

    fn regs(rsi: u64, rdi: u64, rcx: u64, rax: u64) -> impl Fn(&mut Cpu) + Copy {
        move |c| {
            c.reg[RSI] = rsi;
            c.reg[RDI] = rdi;
            c.reg[RCX] = rcx;
            c.reg[RAX] = rax;
        }
    }

    #[test]
    fn a_rep_in_one_step_is_the_repetitions_one_at_a_time() {
        let text = b"0123456789abcdef";
        // Apart.
        let (c, page) = both_ways(REP_MOVSB, text, regs(DATA, DATA + 100, 16, 0));
        assert_eq!(&page[100..116], text);
        assert_eq!((c.reg[RSI], c.reg[RDI], c.reg[RCX]), (DATA + 16, DATA + 116, 0));
        // Sixteen repetitions, each counted.
        assert_eq!(c.steps, 16);
        // Downward over itself.
        let (_, page) = both_ways(REP_MOVSB, text, regs(DATA + 4, DATA, 12, 0));
        assert_eq!(&page[..16], b"456789abcdefcdef");
        // Upward over itself: what was written is read again, and the first byte fills
        // all of it. This is the case a copy of memory would get wrong.
        let (_, page) = both_ways(REP_MOVSB, text, regs(DATA, DATA + 1, 15, 0));
        assert_eq!(&page[..16], b"0000000000000000");
        let (_, page) = both_ways(REP_MOVSB, text, regs(DATA, DATA + 3, 13, 0));
        assert_eq!(&page[..16], b"0120120120120120");
        // Eight bytes at a time, apart and over itself by less than an element.
        let (c, page) = both_ways(REP_MOVSQ, text, regs(DATA, DATA + 64, 2, 0));
        assert_eq!(&page[64..80], text);
        assert_eq!((c.reg[RSI], c.reg[RDI]), (DATA + 16, DATA + 80));
        both_ways(REP_MOVSQ, text, regs(DATA, DATA + 4, 3, 0));
        // A count of zero does nothing, in one instruction.
        let (c, page) = both_ways(REP_MOVSB, text, regs(DATA, DATA + 100, 0, 0));
        assert_eq!(page[100], 0);
        assert_eq!(c.steps, 1);
        // Going down, and with 32-bit addresses: left to the one at a time.
        both_ways(STD_REP_MOVSB, text, regs(DATA + 15, DATA + 115, 16, 0));
        let (c, _) = both_ways(REP_MOVSB_32, text, regs(DATA | 7 << 32, DATA + 100 | 7 << 32, 16, 0));
        assert_eq!(c.reg[RDI], DATA + 116);
    }

    #[test]
    fn a_rep_stos_in_one_step_fills_with_the_a_register() {
        let a = 0x1122_3344_5566_7788;
        let (c, page) = both_ways(REP_STOSB, &[], regs(0, DATA + 1, 5, a));
        assert_eq!(&page[..8], [0, 0x88, 0x88, 0x88, 0x88, 0x88, 0, 0]);
        assert_eq!((c.reg[RDI], c.reg[RCX], c.steps), (DATA + 6, 0, 5));
        let (_, page) = both_ways(REP_STOSW, &[], regs(0, DATA + 1, 2, a));
        assert_eq!(&page[..6], [0, 0x88, 0x77, 0x88, 0x77, 0]);
        let (_, page) = both_ways(REP_STOSD, &[], regs(0, DATA + 2, 2, a));
        assert_eq!(&page[..11], [0, 0, 0x88, 0x77, 0x66, 0x55, 0x88, 0x77, 0x66, 0x55, 0]);
        let (c, page) = both_ways(REP_STOSQ, &[], regs(0, DATA + 8, 511, a));
        assert_eq!(page[8..].chunks(8).filter(|q| *q == a.to_le_bytes()).count(), 511);
        assert_eq!(c.reg[RDI], DATA + 4096);
    }

    #[test]
    fn a_rep_that_cannot_be_done_whole_stops_where_one_at_a_time_stops() {
        for whole in [false, true] {
            let mut m = Mem::reserve().unwrap();
            m.map(CODE, 4096, READ | W).unwrap();
            m.write_bytes(CODE, REP_STOSQ).unwrap();
            m.protect(CODE, 4096, READ | X).unwrap();
            m.map(DATA, 4096, READ | W).unwrap();
            let mut c = Cpu::new(CODE, 0);
            c.rep_whole = whole;
            // Three elements fit before the page's end; five are asked for.
            regs(0, DATA + 4096 - 24, 5, 0x55)(&mut c);
            let stop = loop {
                if let Err(stop) = step(&mut c, &mut m) {
                    break stop;
                }
            };
            assert_eq!(stop, Stop::Fault(Fault { addr: DATA + 4096, access: Access::Write }));
            assert_eq!((c.reg[RDI], c.reg[RCX], c.rip, c.steps), (DATA + 4096, 2, CODE, 3));
            assert_eq!(m.read_u64(DATA + 4096 - 8), Ok(0x55));
        }
    }
    #[test]
    fn an_instruction_with_lock_is_atomic_between_threads() {
        // again: lock add dword [rsi], 1; lock inc qword [rsi + 8]; lock sub dword
        // [rsi + 18h], 3; sub ebx, 1; jne again; ud2. Four threads, each its own
        // registers, one memory: what each added is all there at the end.
        let code = [
            0xf0, 0x83, 0x06, 0x01, 0xf0, 0x48, 0xff, 0x46, 0x08, 0xf0, 0x83, 0x6e, 0x18, 0x03, 0x83, 0xeb, 0x01, 0x75, 0xed, 0x0f, 0x0b,
        ];
        let rounds = 50_000u64;
        let mut first = Mem::reserve().unwrap();
        first.map(CODE, 4096, READ | W).unwrap();
        first.write_bytes(CODE, &code).unwrap();
        first.protect(CODE, 4096, READ | X).unwrap();
        first.map(DATA, 4096, READ | W).unwrap();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let mut m = first.view();
                std::thread::spawn(move || {
                    let mut c = Cpu::new(CODE, DATA + 4096);
                    (c.reg[RSI], c.reg[RBX]) = (DATA, rounds);
                    loop {
                        match step(&mut c, &mut m) {
                            Ok(_) => {}
                            Err(Stop::Undefined) => break,
                            Err(other) => panic!("the program stopped: {other:?}"),
                        }
                    }
                    c.steps
                })
            })
            .collect();
        for t in threads {
            assert_eq!(t.join().unwrap(), 5 * rounds);
        }
        assert_eq!(first.read_u32(DATA), Ok((4 * rounds) as u32));
        assert_eq!(first.read_u64(DATA + 8), Ok(4 * rounds));
        assert_eq!(first.read_u32(DATA + 0x18), Ok((12 * rounds) as u32).map(u32::wrapping_neg));
    }
}
