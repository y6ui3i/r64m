//! The JIT (ADR 0014): the arm64 encoder (`a64`), the translator (`translate`), the
//! runner that keeps the blocks and checks them (`run`), and here the ground they stand
//! on: the memory that generated code is written to and run from (`Code`), the one way
//! in and out (`enter`), and the handler that turns a fault in generated code into a
//! way out.
//!
//! While generated code runs:
//!
//! | host | holds |
//! |---|---|
//! | x0 to x15 | the guest's sixteen registers, in the order of their numbers |
//! | x16, x17 | scratch; x16 is the reason on the way out |
//! | x18 | not touched (the host's own on macOS) |
//! | x19 | the address of the `Cpu` |
//! | x20 | the base of the guest's memory |
//! | x21 to x23 | the record of the flags |
//! | x24 | the address of the way out; in blocks with ordered accesses (ADR 0019, point D) the host's address of each, the way out's being in the `Cpu` too |
//! | x25, x26 | scratch that the way in saves |
//! | x27 | how many instructions the blocks run to their ends have run; its top bit, that a signal waits for the thread |
//! | x28 | the address of the table of blocks (`TABLE_BITS`) |
//! | v0 to v15 | the guest's sixteen XMM registers (ADR 0015), once a block that uses them has loaded them (`XMM_LOADED`) |
//! | v16 to v31 | scratch |

// Some of its forms wait for the instructions that will use them.
#[allow(dead_code)]
pub mod a64;
pub mod run;
pub mod translate;

use crate::cpu::Cpu;
use std::io;
use std::mem::offset_of;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The host register that holds the `Cpu`'s address, the guest memory's base, and the
/// way out, while generated code runs.
pub const CPU: u8 = 19;
pub const BASE: u8 = 20;
/// The host register that holds the host's address of an ordered load or store.
pub const HOST: u8 = 24;
/// The host register that counts the instructions of the blocks run to their ends.
pub const RAN: u8 = 27;
/// The host register that holds the address of the table generated code looks a
/// block up in when where it goes is not a constant (a return, a jump through an
/// operand): two to the `TABLE_BITS` places chosen by an address's low bits, each an
/// address in the guest and where the code of its block is.
pub const TABLE: u8 = 28;
// The table's size is the thread's own (R074): it begins at 2^12 places (64 KB) and
// is made four times larger whenever as many addresses have been put out of it as it
// has places, to 2^18 at most (4 MB). (R073: a compiler's returns and calls through
// registers put each other out of a table of 2^14 two million times in one
// compilation; every thread paying for the largest was a megabyte for each of a
// server's waiting threads.)
pub const TABLE_BITS: u32 = 12;
pub const TABLE_BITS_MOST: u32 = 18;
/// In the way in's frame, from the stack pointer as generated code has it: whether the
/// XMM registers were loaded since the way in (not zero), and where the lower halves
/// of the caller's v8 to v15 are kept when they were.
pub const XMM_LOADED: u32 = 160;
pub const KEPT_D8: u32 = 96;
/// Where generated code puts why it leaves.
pub const REASON: u8 = 16;

/// Why generated code was left.
pub const LEFT_AT_END: u64 = 0;
/// The host faulted in it: `Cpu::fault_pc` says where.
pub const LEFT_BY_FAULT: u64 = 1;
/// A block left for the interpreter to run the instruction RIP is on, which the block
/// does not do in every case (a division that would fault): whatever block begins
/// there, it is the interpreter's this time.
pub const LEFT_FOR_THE_INTERPRETER: u64 = 2;

/// Goes into generated code at `code` with the guest's registers loaded from `cpu`,
/// and comes back when that code jumps to the way out (the address in x24), with the
/// registers stored again and the reason it put in x16.
///
/// # Safety
/// `code` must be code written for this convention, `base` the base of the guest's
/// memory it was written for, and `table` a table of two to the `TABLE_BITS` places
/// whose every place holds an address and code that may be jumped to.
///
/// The XMM registers are not loaded here: a block that uses them loads them where it
/// begins, if no block has since the way in (`XMM_LOADED`), and only then does the way
/// out store them. (R020: loaded and stored every time, they cost p_fill, which comes
/// and goes 160 times for every thousand of its instructions in blocks, a tenth.)
#[unsafe(naked)]
pub unsafe extern "C" fn enter(cpu: *mut Cpu, code: *const u8, base: *mut u8, table: *const [u64; 2]) -> u64 {
    core::arch::naked_asm!(
        "sub sp, sp, #176",
        "stp x19, x20, [sp]",
        "stp x21, x22, [sp, #16]",
        "stp x23, x24, [sp, #32]",
        "stp x25, x26, [sp, #48]",
        "stp x27, x28, [sp, #64]",
        "stp x29, x30, [sp, #80]",
        "mov x19, x0",
        "mov x20, x2",
        "mov x28, x3",
        "mov x17, x1",
        // The way out's address: in x24 for blocks that leave it there, and in the
        // `Cpu` for those that use x24 (ordered accesses) and for the handler of
        // faults.
        "adr x24, 2f",
        "str x24, [x19, #{out}]",
        "ldp x21, x22, [x19, #{rec}]",
        "ldr x23, [x19, #{rec} + 16]",
        "ldp x0, x1, [x19, #{reg}]",
        "ldp x2, x3, [x19, #{reg} + 16]",
        "ldp x4, x5, [x19, #{reg} + 32]",
        "ldp x6, x7, [x19, #{reg} + 48]",
        "ldp x8, x9, [x19, #{reg} + 64]",
        "ldp x10, x11, [x19, #{reg} + 80]",
        "ldp x12, x13, [x19, #{reg} + 96]",
        "ldp x14, x15, [x19, #{reg} + 112]",
        "str xzr, [sp, #{loaded}]",
        // The count of instructions, with its top bit saying that a signal waits
        // (which every block that a cycle passes through tests, R039). From here to
        // the jump the host's handler gives the bit to a signal that arrives, and
        // one that arrived before is in the thread's word, which is 0 or 1.
        "mov x27, #0",
        "ldr w16, [x19, #{kick}]",
        "orr x27, x27, x16, lsl #63",
        "br x17",
        // The way out.
        "2:",
        "stp x0, x1, [x19, #{reg}]",
        "stp x2, x3, [x19, #{reg} + 16]",
        "stp x4, x5, [x19, #{reg} + 32]",
        "stp x6, x7, [x19, #{reg} + 48]",
        "stp x8, x9, [x19, #{reg} + 64]",
        "stp x10, x11, [x19, #{reg} + 80]",
        "stp x12, x13, [x19, #{reg} + 96]",
        "stp x14, x15, [x19, #{reg} + 112]",
        "stp x21, x22, [x19, #{rec}]",
        "str x23, [x19, #{rec} + 16]",
        "str x27, [x19, #{ran}]",
        // If a block loaded the XMM registers (and kept the caller's d8 to d15).
        "ldr x17, [sp, #{loaded}]",
        "cbz x17, 3f",
        "add x17, x19, #{xmm}",
        "stp q0, q1, [x17]",
        "stp q2, q3, [x17, #32]",
        "stp q4, q5, [x17, #64]",
        "stp q6, q7, [x17, #96]",
        "stp q8, q9, [x17, #128]",
        "stp q10, q11, [x17, #160]",
        "stp q12, q13, [x17, #192]",
        "stp q14, q15, [x17, #224]",
        "ldp d8, d9, [sp, #{kept}]",
        "ldp d10, d11, [sp, #{kept} + 16]",
        "ldp d12, d13, [sp, #{kept} + 32]",
        "ldp d14, d15, [sp, #{kept} + 48]",
        "3:",
        "mov x0, x16",
        "ldp x19, x20, [sp]",
        "ldp x21, x22, [sp, #16]",
        "ldp x23, x24, [sp, #32]",
        "ldp x25, x26, [sp, #48]",
        "ldp x27, x28, [sp, #64]",
        "ldp x29, x30, [sp, #80]",
        "add sp, sp, #176",
        "ret",
        reg = const offset_of!(Cpu, reg),
        rec = const offset_of!(Cpu, flag_rec),
        ran = const offset_of!(Cpu, ran),
        xmm = const offset_of!(Cpu, xmm),
        out = const offset_of!(Cpu, way_out),
        kick = const offset_of!(Cpu, kick),
        loaded = const XMM_LOADED,
        kept = const KEPT_D8,
    )
}

// ---- The memory generated code lives in ---------------------------------------------------

/// Where each piece of generated code in this process begins and ends, for the handler
/// of faults to know its own: pairs of addresses, a zero start for a place not taken.
/// (A program has one piece. The tests have one or two each and run several at once:
/// with eight places some of them found none left, on Linux, on 2026-11-25. And
/// every thread of the guest's has a piece of its own, ADR 0019.)
static PIECES: [[AtomicUsize; 2]; 1024] = [const { [const { AtomicUsize::new(0) }; 2] }; 1024];

/// The stretch of the way in from where the count is cleared to the jump into
/// generated code: its first address and the one after its last. Found once, by
/// reading the way in's own instructions (the two that bound it are there once).
static WINDOW: [AtomicUsize; 2] = [const { AtomicUsize::new(0) }; 2];

fn find_window() {
    const CLEARS_THE_COUNT: u32 = 0xd280_001b; // mov x27, #0
    const JUMPS_IN: u32 = 0xd61f_0220; // br x17
    let code = enter as *const u32;
    let word = |i: usize| unsafe { code.add(i).read() };
    let clears = (0..96).find(|&i| word(i) == CLEARS_THE_COUNT).expect("the way in clears the count");
    let jumps = (clears..clears + 8).find(|&i| word(i) == JUMPS_IN).expect("and jumps soon after");
    WINDOW[0].store(code as usize + 4 * (clears + 1), Ordering::Release);
    WINDOW[1].store(code as usize + 4 * (jumps + 1), Ordering::Release);
}

/// Was a thread interrupted at this address running generated code, or about to (in
/// the way in, past the clearing of the count)? Then a signal for it is told to it
/// by the top bit of its x27, which the interrupter can set; anywhere else by the
/// thread's word alone.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn runs_blocks(at: usize) -> bool {
    is_generated(at) || (at >= WINDOW[0].load(Ordering::Acquire) && at < WINDOW[1].load(Ordering::Acquire))
}

/// Is this address of the host's in generated code? (For the profile.)
pub fn is_generated(at: usize) -> bool {
    PIECES.iter().any(|piece| {
        let start = piece[0].load(Ordering::Acquire);
        start != 0 && at >= start && at < piece[1].load(Ordering::Acquire)
    })
}

/// One piece of memory that can be written and run, filled from its start.
pub struct Code {
    base: *mut u32,
    /// How many instructions it holds, and how many are written.
    room: usize,
    used: usize,
    /// Its place in `PIECES`.
    place: usize,
}

impl Code {
    /// A piece of `bytes` bytes. The handler of faults is put in place with the first.
    pub fn new(bytes: usize) -> io::Result<Code> {
        handle_faults()?;
        #[cfg(target_os = "macos")]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT;
        #[cfg(not(target_os = "macos"))]
        let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                flags,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let start = p as usize;
        // The end first, so that a place with its start set is whole.
        for (place, piece) in PIECES.iter().enumerate() {
            if piece[0].load(Ordering::Acquire) == 0 && piece[1].swap(start + bytes, Ordering::AcqRel) == 0 {
                piece[0].store(start, Ordering::Release);
                return Ok(Code { base: p as *mut u32, room: bytes / 4, used: 0, place });
            }
        }
        unsafe { libc::munmap(p, bytes) };
        Err(io::Error::other("too many pieces of generated code in one process"))
    }

    /// How many instructions more it has room for.
    pub fn room(&self) -> usize {
        self.room - self.used
    }

    /// Writes instructions after those already there and tells the processor's cache of
    /// instructions; where they begin is given back. `None` if there is not the room.
    pub fn append(&mut self, words: &[u32]) -> Option<*const u8> {
        if words.len() > self.room() {
            return None;
        }
        let at = unsafe { self.base.add(self.used) };
        unsafe {
            writable(true);
            std::ptr::copy_nonoverlapping(words.as_ptr(), at, words.len());
            writable(false);
            told(at as *mut u8, words.len() * 4);
        }
        self.used += words.len();
        Some(at as *const u8)
    }

    /// Writes one instruction over one written before.
    ///
    /// # Safety
    /// `at` must be an instruction of this piece, and no thread may be running it.
    pub unsafe fn rewrite(&mut self, at: *const u8, word: u32) {
        unsafe {
            writable(true);
            (at as *mut u32).write(word);
            writable(false);
            told(at as *mut u8, 4);
        }
    }

    /// How many instructions are written.
    pub fn used(&self) -> usize {
        self.used
    }

    /// Forgets everything written: the next is written at the start again.
    pub fn empty(&mut self) {
        self.used = 0;
    }
}

impl Drop for Code {
    fn drop(&mut self) {
        PIECES[self.place][0].store(0, Ordering::Release);
        PIECES[self.place][1].store(0, Ordering::Release);
        unsafe { libc::munmap(self.base as *mut libc::c_void, self.room * 4) };
    }
}

/// On macOS a thread either writes memory mapped for generated code or runs it; on
/// Linux the memory is both at once and there is nothing to turn.
#[cfg(target_os = "macos")]
unsafe fn writable(on: bool) {
    unsafe { libc::pthread_jit_write_protect_np(!on as libc::c_int) };
}

#[cfg(not(target_os = "macos"))]
unsafe fn writable(_on: bool) {}

/// Tells the processor that instructions were written: what it had read of that memory
/// before is not what is there now.
#[cfg(target_os = "macos")]
unsafe fn told(start: *mut u8, len: usize) {
    unsafe extern "C" {
        fn sys_icache_invalidate(start: *mut libc::c_void, len: usize);
    }
    unsafe { sys_icache_invalidate(start as *mut libc::c_void, len) };
}

#[cfg(not(target_os = "macos"))]
unsafe fn told(start: *mut u8, len: usize) {
    use core::arch::asm;
    unsafe {
        // The sizes of a line of the two caches are in CTR_EL0, as powers of two of
        // words.
        let ctr: u64;
        asm!("mrs {}, ctr_el0", out(reg) ctr);
        let data_line = 4usize << (ctr >> 16 & 15);
        let code_line = 4usize << (ctr & 15);
        let (start, end) = (start as usize, start as usize + len);
        let mut at = start & !(data_line - 1);
        while at < end {
            asm!("dc cvau, {}", in(reg) at);
            at += data_line;
        }
        asm!("dsb ish");
        let mut at = start & !(code_line - 1);
        while at < end {
            asm!("ic ivau, {}", in(reg) at);
            at += code_line;
        }
        asm!("dsb ish", "isb");
    }
}

// ---- A fault in generated code -------------------------------------------------------------

/// Puts the handler in place for the faults a load or a store can end in. Once for
/// the process.
fn handle_faults() -> io::Result<()> {
    static DONE: std::sync::OnceLock<Result<(), i32>> = std::sync::OnceLock::new();
    let done = DONE.get_or_init(|| {
        find_window();
        for signal in [libc::SIGSEGV, libc::SIGBUS] {
            let mut act: libc::sigaction = unsafe { std::mem::zeroed() };
            act.sa_sigaction = on_fault as *const () as usize;
            act.sa_flags = libc::SA_SIGINFO;
            unsafe { libc::sigemptyset(&mut act.sa_mask) };
            if unsafe { libc::sigaction(signal, &act, std::ptr::null_mut()) } != 0 {
                return Err(io::Error::last_os_error().raw_os_error().unwrap_or(0));
            }
        }
        Ok(())
    });
    done.map_err(io::Error::from_raw_os_error)
}

/// The host's general registers and its program counter as the kernel saved them at a
/// fault, to be read and changed: what is changed is what the thread goes on with.
#[cfg(target_os = "macos")]
unsafe fn saved<'a>(context: *mut libc::c_void) -> (&'a mut [u64; 29], &'a mut u64) {
    let state = unsafe { &mut (*(*(context as *mut libc::ucontext_t)).uc_mcontext).__ss };
    (&mut state.__x, &mut state.__pc)
}

#[cfg(not(target_os = "macos"))]
unsafe fn saved<'a>(context: *mut libc::c_void) -> (&'a mut [u64; 31], &'a mut u64) {
    let state = unsafe { &mut (*(context as *mut libc::ucontext_t)).uc_mcontext };
    (&mut state.regs, &mut state.pc)
}

/// Sixteen hexadecimal digits, without anything a handler may not use.
fn hex(v: usize) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, digit) in out.iter_mut().enumerate() {
        *digit = b"0123456789abcdef"[v >> (60 - 4 * i) & 15];
    }
    out
}

/// A fault while generated code ran is made into that code leaving (ADR 0014, point
/// 7): where it was is written to the `Cpu`, and the thread goes on at the way out,
/// which stores the guest's registers as they were at the faulting instruction. What
/// the fault was is found afterwards, by the interpreter meeting the same instruction.
///
/// A fault anywhere else is r64m's own: the handler steps aside, and the fault happens
/// again and ends the process as it would have.
extern "C" fn on_fault(signal: libc::c_int, info: *mut libc::siginfo_t, context: *mut libc::c_void) {
    let (regs, pc) = unsafe { saved(context) };
    let at = *pc as usize;
    // A store of r64m's own into the guest's memory that the host refused: one of
    // the two routines made to be refused (src/mem.rs), which then answers so.
    if let Some(on) = crate::mem::refused_store(at) {
        *pc = on as u64;
        return;
    }
    if !is_generated(at) {
        // Said first, with what can be written from a handler: where r64m was, the
        // address it could not have, and where `on_fault` itself is (to place the
        // first against the names in the file r64m was built as).
        let address = if info.is_null() { 0 } else { unsafe { (*info).si_addr() as usize } };
        let mut line = [0u8; 120];
        let mut n = 0;
        for part in [&b"r64m: a fault of r64m's own at "[..], &hex(at), b", address ", &hex(address), b"; on_fault is at ", &hex(on_fault as *const () as usize), b"\n"] {
            line[n..n + part.len()].copy_from_slice(part);
            n += part.len();
        }
        unsafe {
            libc::write(2, line.as_ptr() as *const libc::c_void, n);
            libc::signal(signal, libc::SIG_DFL);
        }
        return;
    }
    let cpu = regs[CPU as usize] as *mut Cpu;
    unsafe { (*cpu).fault_pc = *pc };
    regs[REASON as usize] = LEFT_BY_FAULT;
    *pc = unsafe { (*cpu).way_out };
}

#[cfg(test)]
mod tests {
    use super::a64::{self, Mem as Op, Size};
    use super::*;
    use crate::mem::{Mem, PAGE, R, W};

    /// The end of a block made by hand: the reason, and the jump to the way out.
    fn leave(reason: u64) -> [u32; 3] {
        [
            a64::movz(true, REASON, reason as u16, 0),
            a64::mem_imm(Size::X, Op::Load, 17, CPU, offset_of!(Cpu, way_out) as u32),
            a64::br(17),
        ]
    }

    fn cpu() -> Cpu {
        let mut c = Cpu::new(0, 0);
        for i in 0..16 {
            c.reg[i] = 0x1111_1111_1111_1111 * i as u64 + 0x100;
        }
        c
    }

    fn run(code: &mut Code, words: &[u32], c: &mut Cpu, m: &Mem) -> u64 {
        let at = code.append(words).expect("room for the block");
        unsafe { enter(c, at, m.base(), std::ptr::null()) }
    }

    #[test]
    fn the_way_in_tells_a_block_that_a_signal_waits() {
        let m = Mem::reserve().unwrap();
        let mut code = Code::new(1 << 16).unwrap();
        // A block that begins with the look: left with 9 if the count's top bit is
        // set, else it counts three instructions and is left with 7.
        let mut block = vec![a64::tbnz_top(RAN, 5)];
        block.push(a64::arith_imm(true, false, false, RAN, RAN, 3, false));
        block.extend(leave(7));
        block.extend(leave(9));
        let mut c = cpu();
        assert_eq!((run(&mut code, &block, &mut c, &m), c.ran), (7, 3));
        c.kick = 1;
        assert_eq!((run(&mut code, &block, &mut c, &m), c.ran & !(1 << 63), c.ran >> 63), (9, 0, 1));
        // The stretch of the way in that the host's handler knows is in the way in,
        // and is the four instructions from the count's clearing to the jump.
        let (begin, end) = (WINDOW[0].load(Ordering::Acquire), WINDOW[1].load(Ordering::Acquire));
        assert_eq!(end - begin, 12);
        let way_in = enter as *const () as usize;
        assert!(begin > way_in && end < way_in + 400);
        assert!(runs_blocks(begin) && runs_blocks(end - 4) && !runs_blocks(end) && !runs_blocks(way_in));
    }

    #[test]
    fn a_block_made_by_hand_adds_two_of_the_guests_registers() {
        let m = Mem::reserve().unwrap();
        let mut code = Code::new(1 << 16).unwrap();
        let mut c = cpu();
        let before = c.reg;
        // RAX = RAX + RBX: x0 and x3.
        let mut block = vec![a64::arith(true, false, false, 0, 0, 3, 0)];
        block.extend(leave(7));
        assert_eq!(run(&mut code, &block, &mut c, &m), 7);
        assert_eq!(c.reg[0], before[0] + before[3]);
        assert_eq!(c.reg[1..], before[1..]);
    }

    #[test]
    fn every_register_goes_in_and_comes_out_in_its_own_place() {
        let m = Mem::reserve().unwrap();
        let mut code = Code::new(1 << 16).unwrap();
        let mut c = cpu();
        let before = c.reg;
        c.flag_rec = [11, 22, 33];
        // Each guest register has its own number added, and the record of the flags
        // is turned round.
        let mut block: Vec<u32> = (0..16).map(|i| a64::arith_imm(true, false, false, i, i, i as u32 + 1, false)).collect();
        block.extend([a64::mov(true, 17, 21), a64::mov(true, 21, 22), a64::mov(true, 22, 23), a64::mov(true, 23, 17)]);
        block.extend(leave(LEFT_AT_END));
        assert_eq!(run(&mut code, &block, &mut c, &m), LEFT_AT_END);
        for i in 0..16 {
            assert_eq!(c.reg[i], before[i] + i as u64 + 1, "register {i}");
        }
        assert_eq!(c.flag_rec, [22, 33, 11]);
        // And again from where it was left: nothing of the host's was lost between.
        let at = code.append(&block).unwrap();
        assert_eq!(unsafe { enter(&mut c, at, m.base(), std::ptr::null()) }, LEFT_AT_END);
        assert_eq!(c.reg[15], before[15] + 32);
        assert_eq!(c.flag_rec, [33, 11, 22]);
    }

    #[test]
    fn the_guests_memory_is_at_the_base() {
        let mut m = Mem::reserve().unwrap();
        m.map(0x40_0000, PAGE, R | W).unwrap();
        m.write_u64(0x40_0010, 0x0123_4567_89ab_cdef).unwrap();
        let mut code = Code::new(1 << 16).unwrap();
        let mut c = cpu();
        c.reg[6] = 0x40_0010;
        // RCX = [RSI]; then its low half, turned over, to [RSI + 8] as 32 bits.
        let mut block = vec![
            a64::mem_reg(Size::X, Op::Load, 1, BASE, 6),
            a64::mvn(false, 17, 1),
            a64::arith_imm(true, false, false, 16, 6, 8, false),
            a64::mem_reg(Size::W, Op::Store, 17, BASE, 16),
        ];
        block.extend(leave(LEFT_AT_END));
        assert_eq!(run(&mut code, &block, &mut c, &m), LEFT_AT_END);
        assert_eq!(c.reg[1], 0x0123_4567_89ab_cdef);
        assert_eq!(m.read_u64(0x40_0018), Ok(0x7654_3210));
    }

    #[test]
    fn a_fault_in_a_block_is_the_block_left_with_the_registers_as_they_were() {
        let mut m = Mem::reserve().unwrap();
        m.map(0x40_0000, PAGE, R).unwrap();
        let mut code = Code::new(1 << 16).unwrap();
        for (store, addr) in [(false, 0x80_0000u64), (true, 0x80_0000), (true, 0x40_0000)] {
            let mut c = cpu();
            let before = c.reg;
            c.reg[6] = addr;
            // RDX is changed, then memory is touched where it may not be, then RBX
            // would be changed.
            let mut block = vec![
                a64::arith_imm(true, false, false, 2, 2, 5, false),
                a64::mem_reg(Size::X, if store { Op::Store } else { Op::Load }, 1, BASE, 6),
                a64::arith_imm(true, false, false, 3, 3, 5, false),
            ];
            block.extend(leave(LEFT_AT_END));
            let at = code.append(&block).unwrap();
            assert_eq!(unsafe { enter(&mut c, at, m.base(), std::ptr::null()) }, LEFT_BY_FAULT, "store {store} at {addr:#x}");
            // Where: the second instruction of the block.
            assert_eq!(c.fault_pc, at as u64 + 4);
            // What ran before it is done, what comes after it is not, and nothing was
            // read into RCX.
            assert_eq!(c.reg[2], before[2] + 5);
            assert_eq!(c.reg[3], before[3]);
            assert_eq!(c.reg[1], before[1]);
        }
        // The read-only page was not written.
        assert_eq!(m.read_u64(0x40_0000), Ok(0));
        // And the handler is still there for the next.
        let mut c = cpu();
        c.reg[6] = 0x90_0000;
        let mut block = vec![a64::mem_reg(Size::B, Op::Load, 1, BASE, 6)];
        block.extend(leave(LEFT_AT_END));
        assert_eq!(run(&mut code, &block, &mut c, &m), LEFT_BY_FAULT);
    }

    #[test]
    fn a_piece_is_filled_from_its_start_and_no_further_than_its_end() {
        let mut code = Code::new(1 << 14).unwrap();
        assert_eq!(code.room(), 1 << 12);
        let first = code.append(&[a64::nop(); 3]).unwrap();
        let second = code.append(&[a64::nop()]).unwrap();
        assert_eq!(second as usize, first as usize + 12);
        assert_eq!(code.room(), (1 << 12) - 4);
        assert!(code.append(&vec![a64::nop(); (1 << 12) - 3]).is_none());
        assert!(code.append(&vec![a64::nop(); (1 << 12) - 4]).is_some());
        assert_eq!(code.room(), 0);
        code.empty();
        assert_eq!(code.append(&[a64::nop()]).unwrap(), first);
    }
}
