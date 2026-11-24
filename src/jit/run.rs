//! Running a program with the JIT: which addresses have a block, when one is made,
//! what is done when one is left, and the check of every block against the
//! interpreter (ADR 0014, points 2, 7, 8 and 11).

use super::translate::{translate, Built};
use super::{enter, Code, LEFT_AT_END};
use crate::cpu::{self, Cpu, Step, Stop};
use crate::mem::{Access, Mem};
use std::collections::HashMap;

/// A block is made when its first address has been arrived at this often (g64m's
/// number; to be measured here), unless another number is asked for.
pub const HOT: u8 = 16;
/// A host page whose blocks have been forgotten this often for a write to it is not
/// translated from again: it holds data that changes beside its code.
const GIVEN_UP: u8 = 8;
/// How much generated code there may be before all of it is forgotten.
const CODE_BYTES: usize = 16 << 20;

struct Block {
    code: *const u8,
    /// For each of its instructions, where its code begins (in host instructions from
    /// the block's start) and its address in the guest.
    starts: Vec<(u32, u64)>,
    /// The host pages of the guest's memory its instructions lie in.
    pages: (u64, u64),
}

/// What is counted for `--jit-stats`.
#[derive(Default)]
pub struct Stats {
    pub blocks: u64,
    pub entered: u64,
    pub in_blocks: u64,
    pub faults: u64,
    pub forgotten: u64,
    pub emptied: u64,
    pub checked: u64,
}

pub struct Jit {
    code: Code,
    blocks: Vec<Block>,
    /// The block that begins at an address, as its place in `blocks`.
    at: HashMap<u64, usize>,
    /// How often an address has been arrived at with no block there.
    heat: HashMap<u64, u8>,
    /// How often a host page's blocks were forgotten for a write.
    writes: HashMap<u64, u8>,
    /// Every block is compared with the interpreter.
    check: bool,
    /// How often an address is arrived at before its block is made.
    hot: u8,
    pub stats: Stats,
}

/// Why `run` came back.
pub enum Event {
    Syscall,
    Stop(Stop),
    /// A block and the interpreter did not agree (`--jit-check`): what differed.
    Differs(String),
}

impl Jit {
    pub fn new(check: bool, hot: u8) -> std::io::Result<Jit> {
        Ok(Jit {
            code: Code::new(CODE_BYTES)?,
            blocks: Vec::new(),
            at: HashMap::new(),
            heat: HashMap::new(),
            writes: HashMap::new(),
            check,
            hot: hot.max(1),
            stats: Stats::default(),
        })
    }

    /// Runs the program from where the CPU is, by blocks where there are blocks and by
    /// the interpreter where there are none, until a system call or a stop.
    pub fn run(&mut self, c: &mut Cpu, m: &mut Mem) -> Event {
        // A block begins only where a jump arrives; after one the address is looked up.
        let mut arrived = true;
        loop {
            if arrived {
                if let Some(&id) = self.at.get(&c.rip) {
                    match self.run_block(id, c, m) {
                        Ok(whole) => arrived = whole,
                        Err(differs) => return Event::Differs(differs),
                    }
                    // Left by a fault: the interpreter meets the instruction next,
                    // whatever block may begin there.
                    if arrived {
                        continue;
                    }
                } else {
                    let heat = self.heat.entry(c.rip).or_insert(0);
                    *heat = heat.saturating_add(1);
                    if *heat == self.hot && self.make(c.rip, m) {
                        continue;
                    }
                }
            }
            let before = c.rip;
            match cpu::step(c, m) {
                Ok(Step::Next) => arrived = c.rip < before || c.rip > before.wrapping_add(15),
                Ok(Step::Syscall) => return Event::Syscall,
                // A store to a page whose code was translated: the translations are
                // forgotten, the page can be written again, and the store is made.
                Err(Stop::Fault(f)) if f.access == Access::Write && m.is_code(f.addr) => {
                    self.forget(f.addr, m);
                    arrived = false;
                }
                Err(stop) => return Event::Stop(stop),
            }
        }
    }

    /// Runs one block. `Ok(true)` if it ran to its end; `Ok(false)` if the host
    /// faulted in it, and then RIP is on the instruction that did.
    fn run_block(&mut self, id: usize, c: &mut Cpu, m: &mut Mem) -> Result<bool, String> {
        let expected = if self.check { self.interpreted(id, c, m) } else { None };
        let block = &self.blocks[id];
        let reason = unsafe { enter(c, block.code, m.base()) };
        cpu::settle(c);
        self.stats.entered += 1;
        if reason != LEFT_AT_END {
            // Which instruction: the last that begins at or before where the host was.
            let word = ((c.fault_pc - block.code as u64) / 4) as u32;
            let i = block.starts.partition_point(|&(start, _)| start <= word) - 1;
            c.rip = block.starts[i].1;
            c.steps += i as u64;
            self.stats.in_blocks += i as u64;
            self.stats.faults += 1;
            return Ok(false);
        }
        let n = block.starts.len() as u64;
        c.steps += n;
        self.stats.in_blocks += n;
        if let Some((want, written)) = expected {
            self.stats.checked += 1;
            if let Some(what) = differs(&want, c, &written, m) {
                let from = block.starts[0].1;
                return Err(format!("the block of {n} instructions at {from:#x}: {what}"));
            }
        }
        Ok(true)
    }

    /// For `--jit-check`: what the interpreter makes of a block's instructions. The
    /// CPU and the memory are put back as they were; what comes back is the CPU as the
    /// interpreter left it and what it wrote. `None` if the interpreter did not get
    /// through them all (a fault): then there is nothing to compare a whole block with.
    fn interpreted(&self, id: usize, c: &mut Cpu, m: &mut Mem) -> Option<(Cpu, Vec<(u64, Vec<u8>)>)> {
        let n = self.blocks[id].starts.len();
        let start = c.clone();
        m.log_writes();
        let mut whole = true;
        for _ in 0..n {
            if !matches!(cpu::step(c, m), Ok(Step::Next)) {
                whole = false;
                break;
            }
        }
        let written = m.undo_writes();
        let end = std::mem::replace(c, start);
        whole.then_some((end, written))
    }

    /// Translates the block at `rip` and keeps it. False if nothing was made.
    fn make(&mut self, rip: u64, m: &mut Mem) -> bool {
        let page = m.host_pages(rip, rip + 1).0;
        if self.writes.get(&page).is_some_and(|&n| n >= GIVEN_UP) {
            return false;
        }
        let Some(Built { words, starts, end }) = translate(m, rip) else {
            return false;
        };
        let code = match self.code.append(&words) {
            Some(code) => code,
            None => {
                self.empty(m);
                match self.code.append(&words) {
                    Some(code) => code,
                    None => return false,
                }
            }
        };
        // From now the pages its instructions lie in may not be written.
        let pages = m.host_pages(rip, end);
        if m.mark_code(pages.0, pages.1, true).is_err() {
            return false;
        }
        self.at.insert(rip, self.blocks.len());
        self.blocks.push(Block { code, starts, pages });
        self.stats.blocks += 1;
        true
    }

    /// Forgets every block with an instruction in the host page that holds `addr`, and
    /// lets the page be written again.
    fn forget(&mut self, addr: u64, m: &mut Mem) {
        let (start, end) = m.host_pages(addr, addr + 1);
        let blocks = &self.blocks;
        // A block that reaches into the page from a neighbour keeps the neighbour
        // marked: only this page is let go, and only blocks that touch it are dropped.
        self.at.retain(|_, id| {
            let (from, to) = blocks[*id].pages;
            to <= start || from >= end
        });
        let _ = m.mark_code(start, end, false);
        *self.writes.entry(start).or_insert(0) += 1;
        self.stats.forgotten += 1;
    }

    /// Forgets all generated code: the piece of memory is full.
    fn empty(&mut self, m: &mut Mem) {
        for block in &self.blocks {
            let _ = m.mark_code(block.pages.0, block.pages.1, false);
        }
        self.blocks.clear();
        self.at.clear();
        self.heat.clear();
        self.code.empty();
        self.stats.emptied += 1;
    }
}

/// What differs between the CPU as the interpreter left it (`want`) and as the block
/// did, and between what the interpreter wrote and what is in memory now.
fn differs(want: &Cpu, got: &Cpu, written: &[(u64, Vec<u8>)], m: &Mem) -> Option<String> {
    const NAMES: [&str; 16] = [
        "RAX", "RCX", "RDX", "RBX", "RSP", "RBP", "RSI", "RDI", "R8", "R9", "R10", "R11", "R12",
        "R13", "R14", "R15",
    ];
    let mut said = Vec::new();
    for i in 0..16 {
        if want.reg[i] != got.reg[i] {
            said.push(format!("{} {:#x} -> {:#x}", NAMES[i], want.reg[i], got.reg[i]));
        }
    }
    for (name, a, b) in [("RIP", want.rip, got.rip), ("RFLAGS", want.rflags, got.rflags), ("the count", want.steps, got.steps)] {
        if a != b {
            said.push(format!("{name} {a:#x} -> {b:#x}"));
        }
    }
    // What the interpreter left at each place it wrote: the last write to a byte is
    // the one that stands, so they are laid over one another in their order.
    let mut bytes: HashMap<u64, u8> = HashMap::new();
    for (addr, new) in written {
        for (i, b) in new.iter().enumerate() {
            bytes.insert(addr + i as u64, *b);
        }
    }
    let mut wrong: Vec<u64> = bytes.iter().filter(|&(&a, &b)| m.read_u8(a) != Ok(b)).map(|(&a, _)| a).collect();
    wrong.sort();
    if let Some(first) = wrong.first() {
        said.push(format!("memory at {first:#x}: {:#x} -> {:?} ({} bytes differ)", bytes[first], m.read_u8(*first), wrong.len()));
    }
    if said.is_empty() { None } else { Some(format!("the interpreter -> the block: {}", said.join(", "))) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::{RAX, RBX, RDX, RSI};
    use crate::mem::{R, W, X};

    const CODE: u64 = 0x40_0000;
    const DATA: u64 = 0x60_0000;

    /// A guest's memory with `code` at `CODE` (to be run, not written) and a page of
    /// data at `DATA` that begins with `data`.
    fn guest(code: &[u8], data: &[u8]) -> Mem {
        let mut m = Mem::reserve().unwrap();
        let len = (code.len() as u64).next_multiple_of(4096);
        m.map(CODE, len, R | W).unwrap();
        m.write_bytes(CODE, code).unwrap();
        m.protect(CODE, len, R | X).unwrap();
        m.map(DATA, 4096, R | W).unwrap();
        m.write_bytes(DATA, data).unwrap();
        m
    }

    /// Runs from `rip` to the next SYSCALL twice from the same beginning: by the
    /// interpreter alone, and by blocks made at once and each checked against the
    /// interpreter. Both must end the same; the CPU is given back.
    fn both(jit: &mut Jit, m: &mut Mem, rip: u64, set: &dyn Fn(&mut Cpu)) -> Cpu {
        let mut alone = Cpu::new(rip, DATA + 4096);
        set(&mut alone);
        m.log_writes();
        while cpu::step(&mut alone, m).expect("the interpreter runs it") != Step::Syscall {}
        let wrote = m.undo_writes();

        let mut c = Cpu::new(rip, DATA + 4096);
        set(&mut c);
        match jit.run(&mut c, m) {
            Event::Syscall => {}
            Event::Stop(stop) => panic!("at {rip:#x}: stopped: {stop:?}"),
            Event::Differs(what) => panic!("at {rip:#x}: {what}"),
        }
        if let Some(what) = differs(&alone, &c, &wrote, m) {
            panic!("at {rip:#x}, the whole run: {what}");
        }
        c
    }

    /// One program, checked; and it must have been run by at least one block.
    fn checked(code: &[u8], data: &[u8], set: &dyn Fn(&mut Cpu)) -> (Cpu, Mem) {
        let mut m = guest(code, data);
        let mut jit = Jit::new(true, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, set);
        assert!(jit.stats.checked >= 1, "no block was run for {code:02x?}");
        (c, m)
    }

    #[test]
    fn every_condition_after_every_kind_of_flags() {
        // (rax, rbx): what makes each flag come out both ways, at both widths.
        let pairs: &[(u64, u64)] = &[
            (0, 0), (1, 2), (2, 1), (5, 5), (0x7fff_ffff, 1), (0x8000_0000, 1), (0xffff_ffff, 1),
            (0x8000_0000, 0x8000_0000), (0x7fff_ffff, 0xffff_ffff), (1, 0xffff_ffff),
            (0x7fff_ffff_ffff_ffff, 1), (1 << 63, 1), (u64::MAX, 1), (u64::MAX, u64::MAX),
            (1 << 63, 1 << 63), (0x7fff_ffff_ffff_ffff, u64::MAX), (3, 0x1_0000_0003), (0xf0, 0x0f),
        ];
        // op r/m, r with RAX the operand and RBX the register: ADD, OR, AND, SUB, XOR,
        // CMP, TEST.
        let ops = [0x01u8, 0x09, 0x21, 0x29, 0x31, 0x39, 0x85];
        // Every program one after another in one memory, each at its own address:
        //   op; jcc taken; mov edx, 1; jmp end; taken: mov edx, 2; end: syscall
        // (RDX, since SYSCALL itself writes RCX.)
        let mut code = Vec::new();
        let mut programs = Vec::new();
        for op in ops {
            for wide in [false, true] {
                for cc in 0..16u8 {
                    programs.push(CODE + code.len() as u64);
                    if wide {
                        code.push(0x48);
                    }
                    code.extend([op, 0xd8, 0x70 | cc, 7, 0xba, 1, 0, 0, 0, 0xeb, 5, 0xba, 2, 0, 0, 0, 0x0f, 0x05]);
                }
            }
        }
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(true, 1).unwrap();
        let mut taken = 0;
        for &(a, b) in pairs {
            for &rip in &programs {
                let c = both(&mut jit, &mut m, rip, &|c| {
                    c.reg[RAX] = a;
                    c.reg[RBX] = b;
                });
                taken += (c.reg[RDX] == 2) as u32;
            }
        }
        let all = (pairs.len() * programs.len()) as u32;
        // Both ways were gone, and often: a condition and its opposite are each other's.
        assert_eq!(taken * 2, all);
        // Every program whose jump is translated ran as a block each time: all but
        // those on parity, and "below or equal" after an addition.
        let (per_op, parity, add_be) = (2 * 16, 7 * 2 * 2, 2 * 2);
        assert_eq!(programs.len(), 7 * per_op);
        let whole = (programs.len() - parity - add_be) * pairs.len();
        assert!(jit.stats.checked as usize >= whole, "{} blocks checked, {whole} looked for", jit.stats.checked);
    }

    #[test]
    fn moves_that_widen_and_constants() {
        let set = |c: &mut Cpu| {
            c.reg[RAX] = 0x1111_2222_3333_4444;
            // What is above its low 32 bits is not their sign.
            c.reg[RBX] = 0x1_8000_ff80;
            c.reg[RSI] = DATA;
        };
        let data = [0x80u8, 0xff, 0x00, 0x80, 0x7f, 0x01, 0x02, 0x03, 0xfe, 0xff, 0xff, 0xff];
        let run = |code: &[u8]| {
            let mut whole = code.to_vec();
            whole.extend([0x0f, 0x05]);
            checked(&whole, &data, &set).0.reg[RAX]
        };
        // mov eax, 0x80000000: no sign above; mov rax, -0x80000000: the sign.
        assert_eq!(run(&[0xc7, 0xc0, 0, 0, 0, 0x80]), 0x8000_0000);
        assert_eq!(run(&[0x48, 0xc7, 0xc0, 0, 0, 0, 0x80]), 0xffff_ffff_8000_0000);
        assert_eq!(run(&[0xb8, 0xff, 0xff, 0xff, 0xff]), 0xffff_ffff);
        assert_eq!(run(&[0x48, 0xb8, 1, 2, 3, 4, 5, 6, 7, 0x88]), 0x8807_0605_0403_0201);
        // movsx and movzx from a byte and from 16 bits, of memory and of a register,
        // to 32 bits (nothing above) and to 64.
        assert_eq!(run(&[0x0f, 0xbe, 0x06]), 0xffff_ff80);
        assert_eq!(run(&[0x48, 0x0f, 0xbe, 0x06]), 0xffff_ffff_ffff_ff80);
        assert_eq!(run(&[0x0f, 0xbf, 0x06]), 0xffff_ff80);
        assert_eq!(run(&[0x48, 0x0f, 0xbf, 0x46, 0x02]), 0xffff_ffff_ffff_8000);
        assert_eq!(run(&[0x0f, 0xb6, 0x06]), 0x80);
        assert_eq!(run(&[0x48, 0x0f, 0xb7, 0x06]), 0xff80);
        assert_eq!(run(&[0x0f, 0xbe, 0xc3]), 0xffff_ff80);
        assert_eq!(run(&[0x48, 0x0f, 0xbe, 0xc3]), 0xffff_ffff_ffff_ff80);
        assert_eq!(run(&[0x0f, 0xbf, 0xc3]), 0xffff_ff80);
        assert_eq!(run(&[0x48, 0x0f, 0xbf, 0xc3]), 0xffff_ffff_ffff_ff80);
        assert_eq!(run(&[0x0f, 0xb6, 0xc3]), 0x80);
        assert_eq!(run(&[0x0f, 0xb7, 0xc3]), 0xff80);
        // movsxd: the sign of 32 bits to 64, from a register and from memory; without
        // REX.W it is a move of 32 bits.
        assert_eq!(run(&[0x48, 0x63, 0xc3]), 0xffff_ffff_8000_ff80);
        assert_eq!(run(&[0x48, 0x63, 0x46, 0x08]), 0xffff_ffff_ffff_fffe);
        assert_eq!(run(&[0x48, 0x63, 0x46, 0x04]), 0x0302_017f);
        assert_eq!(run(&[0x63, 0xc3]), 0x8000_ff80);
        assert_eq!(run(&[0x63, 0x46, 0x08]), 0xffff_fffe);
        // lea: 64 bits, and 32 with nothing above; mov of 32 bits likewise.
        assert_eq!(run(&[0x48, 0x8d, 0x44, 0x5b, 0x10]), 0x1_8000_ff80u64 * 3 + 0x10);
        assert_eq!(run(&[0x8d, 0x44, 0x5b, 0x10]), 0x8002_fe90);
        assert_eq!(run(&[0x8d, 0x04, 0x9d, 0xf0, 0xff, 0xff, 0xff]), 0x0003_fdf0);
        assert_eq!(run(&[0x89, 0xd8]), 0x8000_ff80);
        assert_eq!(run(&[0x8b, 0x06]), 0x8000_ff80);
    }

    #[test]
    fn arithmetic_on_memory_and_with_constants() {
        let set = |c: &mut Cpu| {
            c.reg[RAX] = 0x1_0000_0005;
            c.reg[RBX] = 0xffff_ffff_ffff_fffb;
            c.reg[RSI] = DATA;
        };
        let data = [0xfbu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 5, 0, 0, 0, 1, 0, 0, 0];
        // Each: the instruction, a jump on a condition of its flags that picks RDX,
        // and the end. The check compares the flags; this compares where it went.
        let run = |ins: &[u8], cc: u8| {
            let mut whole = ins.to_vec();
            whole.extend([0x70 | cc, 7, 0xba, 1, 0, 0, 0, 0xeb, 5, 0xba, 2, 0, 0, 0, 0x0f, 0x05]);
            let (c, m) = checked(&whole, &data, &set);
            (c.reg[RAX], c.reg[RDX] == 2, m.read_u64(DATA).unwrap(), m.read_u64(DATA + 8).unwrap())
        };
        // add [rsi], eax: 32 bits of memory, carry out; add [rsi], rax: 64, overflow.
        assert_eq!(run(&[0x01, 0x06], 2), (0x1_0000_0005, true, 0x7fff_ffff_0000_0000, 0x1_0000_0005));
        assert_eq!(run(&[0x48, 0x01, 0x06], 0), (0x1_0000_0005, true, 0x8000_0001_0000_0000, 0x1_0000_0005));
        // The rest are held to the interpreter alone (`checked` compares registers,
        // flags and memory), under every condition, so that each flag is asked both
        // ways: sub rax, [rsi + 8]; cmp [rsi + 8], rax; cmp [rsi + 12], eax; test; and;
        // or; xor; then with constants of 8 bits with their sign and of 32, to a
        // register and to memory.
        let more: &[&[u8]] = &[
            &[0x48, 0x2b, 0x46, 0x08],
            &[0x48, 0x39, 0x46, 0x08],
            &[0x39, 0x46, 0x0c],
            &[0x85, 0x46, 0x0c],
            &[0x48, 0x21, 0x5e, 0x08],
            &[0x09, 0x1e],
            &[0x48, 0x33, 0x06],
            &[0x48, 0x03, 0x1e],
            &[0x3b, 0x1e],
            &[0x83, 0xc0, 0xfb],
            &[0x48, 0x83, 0xe8, 0x06],
            &[0x48, 0x81, 0xc0, 0, 0, 0, 0x80],
            &[0x81, 0x46, 0x08, 0xfb, 0xff, 0xff, 0x7f],
            &[0x48, 0x83, 0x3e, 0xff],
            &[0x83, 0x0e, 0x04],
            &[0x48, 0x81, 0x26, 0xff, 0xff, 0xff, 0x7f],
            &[0x81, 0x36, 0x04, 0, 0, 0x80],
            &[0x48, 0x83, 0x6e, 0x08, 0x06],
            &[0x48, 0x85, 0x06],
        ];
        for ins in more {
            for cc in 0..16 {
                run(ins, cc);
            }
        }
        // A store of 32 bits leaves the four bytes after it.
        assert_eq!(run(&[0x89, 0x06], 4).2, 0x7fff_ffff_0000_0005);
        // What was certainly written and not: cmp writes nothing, or writes its result.
        assert_eq!(run(&[0x48, 0x39, 0x46, 0x08], 4).2, 0x7fff_ffff_ffff_fffb);
        assert_eq!(run(&[0x83, 0x0e, 0x04], 4).2, 0x7fff_ffff_ffff_ffff);
        // mov [rsi + 8], imm: 32 bits, and 64 from 32 with their sign.
        assert_eq!(run(&[0xc7, 0x46, 0x08, 0, 0, 0, 0x80], 5).3, 0x1_8000_0000);
        assert_eq!(run(&[0x48, 0xc7, 0x46, 0x08, 0, 0, 0, 0x80], 5).3, 0xffff_ffff_8000_0000);
    }

    #[test]
    fn the_stack_calls_and_returns() {
        // call f; mov ebx, eax; syscall;  f: push rsi; push -2; pop rax; pop rdx; ret
        let code = [0xe8, 4, 0, 0, 0, 0x89, 0xc3, 0x0f, 0x05, 0x56, 0x6a, 0xfe, 0x58, 0x5a, 0xc3];
        let (c, m) = checked(&code, &[], &|c| c.reg[RSI] = 0x1234);
        assert_eq!((c.reg[RAX], c.reg[RBX], c.reg[RDX]), (u64::MAX - 1, 0xffff_fffe, 0x1234));
        // RSP is where it began, and what was pushed is under it.
        assert_eq!(c.reg[crate::cpu::RSP], DATA + 4096);
        assert_eq!(m.read_u64(DATA + 4096 - 8), Ok(CODE + 5));
        assert_eq!(m.read_u64(DATA + 4096 - 16), Ok(0x1234));
        assert_eq!(m.read_u64(DATA + 4096 - 24), Ok(u64::MAX - 1));
    }

    #[test]
    fn a_store_to_translated_code_forgets_it_and_is_made() {
        // again: mov eax, 7 (the 7 is what gets written over); inc of a counter in
        // memory by add; mov byte-wise is not translated, so the store is the
        // interpreter's: mov dword [rip + again + 1 - next], 9; cmp dword [rsi], 3;
        // jne again; syscall
        let mut code = vec![0xb8, 7, 0, 0, 0, 0x83, 0x06, 0x01];
        let store_at = code.len();
        code.extend([0xc7, 0x05, 0, 0, 0, 0, 9, 0, 0, 0]);
        let disp = 1i32 - (store_at as i32 + 10);
        code[store_at + 2..store_at + 6].copy_from_slice(&disp.to_le_bytes());
        code.extend([0x83, 0x3e, 0x03, 0x75]);
        code.push((-(code.len() as i32 + 1)) as u8);
        code.extend([0x0f, 0x05]);
        // The code must be writable to the guest for it to write itself.
        let mut m = Mem::reserve().unwrap();
        m.map(CODE, 4096, R | W | X).unwrap();
        m.write_bytes(CODE, &code).unwrap();
        m.map(DATA, 4096, R | W).unwrap();
        let mut jit = Jit::new(true, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RSI] = DATA);
        // Three times round; the second and third with the constant as written.
        assert_eq!(c.reg[RAX], 9);
        assert_eq!(m.read_u32(DATA), Ok(3));
        assert_eq!(m.read_u32(CODE + 1), Ok(9));
        // The store from a block was a fault that left the block; the page's blocks
        // were forgotten each time and made again.
        assert!(jit.stats.faults >= 1 && jit.stats.forgotten >= 1, "faults {}, forgotten {}", jit.stats.faults, jit.stats.forgotten);
        assert!(!m.is_code(DATA));
    }

    #[test]
    fn a_fault_in_a_block_is_the_guests_at_that_instruction() {
        // add eax, 1; add ebx, [rsi]; add eax, 1; syscall, with RSI nowhere.
        let code = [0x83, 0xc0, 0x01, 0x03, 0x1e, 0x83, 0xc0, 0x01, 0x0f, 0x05];
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        c.reg[RSI] = 0x90_0000;
        c.reg[RAX] = 0x7fff_ffff;
        let Event::Stop(stop) = jit.run(&mut c, &mut m) else { panic!("it went on") };
        assert_eq!(stop, Stop::Fault(crate::mem::Fault { addr: 0x90_0000, access: Access::Read }));
        // On the second instruction, the first done and counted, its flags as it left
        // them (the sign and the overflow of 7FFFFFFFh and one).
        assert_eq!((c.rip, c.steps, c.reg[RAX], c.reg[RBX]), (CODE + 3, 1, 0x8000_0000, 0));
        assert_eq!(c.rflags & 0x8d5, 0x894);
        assert_eq!(jit.stats.faults, 1);
    }
}
