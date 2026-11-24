//! Running a program with the JIT: which addresses have a block, when one is made,
//! what is done when one is left, and the check of every block against the
//! interpreter (ADR 0014, points 2, 7, 8 and 11).

use super::translate::{translate, Built};
use super::{a64, enter, Code, CPU, LEFT_AT_END, OUT, REASON, TABLE_BITS};
use crate::cpu::{self, Cpu, Step, Stop};
use std::mem::offset_of;
use crate::mem::{Access, Mem, PAGE};
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
    /// Where its instructions begin and end in the guest.
    guest: (u64, u64),
    /// The jumps in other blocks (or in this one) that were rewritten to come here.
    incoming: Vec<u64>,
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
    pub joined: u64,
    pub beside: u64,
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
    /// For each of the guest's own 4 KB pages, how many blocks have an instruction
    /// in it. A host page may be larger: a store to it need not be a store to code.
    holds: HashMap<u64, u32>,
    /// Every block is compared with the interpreter.
    check: bool,
    /// When asked for (`--jit-why`): how often the interpreter ran an instruction
    /// while the JIT was on, by the instruction's first bytes (its prefixes and
    /// opcode, and the ModRM byte's field where the opcode is a group).
    pub why: Option<HashMap<Vec<u8>, u64>>,
    /// How often an address is arrived at before its block is made.
    hot: u8,
    /// The jump the last block was left by, if it can be rewritten to go to the block
    /// at the address it left for; zero if there is none to rewrite.
    from: u64,
    /// What generated code looks in when where it goes is not a constant: for a place
    /// chosen by an address's low bits, the address and where its block's code is. A
    /// place with no block holds an address no place is chosen by and `miss`.
    table: Box<[[u64; 2]]>,
    /// Code that leaves for Rust with RIP as generated code had it in x16: what an
    /// empty place of the table sends to, so that no place can send astray.
    miss: u64,
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
        let mut jit = Jit::bare(check, hot)?;
        jit.clear_table();
        Ok(jit)
    }

    /// The code an empty place of the table sends to, written at the start of the
    /// piece (again each time it is emptied), and every place made empty.
    fn clear_table(&mut self) {
        let miss = [
            a64::mem_imm(a64::Size::X, a64::Mem::Store, a64::ZR, CPU, offset_of!(Cpu, left_from) as u32),
            a64::mem_imm(a64::Size::X, a64::Mem::Store, REASON, CPU, offset_of!(Cpu, rip) as u32),
            a64::movz(true, REASON, LEFT_AT_END as u16, 0),
            a64::br(OUT),
        ];
        self.miss = self.code.append(&miss).expect("room in a piece just emptied") as u64;
        // An address with every bit set chooses the last place; whatever is asked of
        // an empty place, its code is the way out.
        self.table.fill([u64::MAX, self.miss]);
    }

    /// The place in the table that an address chooses.
    fn place(rip: u64) -> usize {
        (rip & ((1 << TABLE_BITS) - 1)) as usize
    }

    fn bare(check: bool, hot: u8) -> std::io::Result<Jit> {
        Ok(Jit {
            code: Code::new(CODE_BYTES)?,
            blocks: Vec::new(),
            at: HashMap::new(),
            heat: HashMap::new(),
            writes: HashMap::new(),
            holds: HashMap::new(),
            check,
            why: None,
            hot: hot.max(1),
            from: 0,
            table: vec![[0, 0]; 1 << TABLE_BITS].into_boxed_slice(),
            miss: 0,
            stats: Stats::default(),
        })
    }

    /// Runs the program from where the CPU is, by blocks where there are blocks and by
    /// the interpreter where there are none, until a system call or a stop.
    pub fn run(&mut self, c: &mut Cpu, m: &mut Mem) -> Event {
        // A block begins only where a jump arrives; after one the address is looked up.
        let mut arrived = true;
        self.from = 0;
        loop {
            if arrived {
                if let Some(&id) = self.at.get(&c.rip) {
                    // The block before left by a jump to this address: from now on it
                    // comes here by itself. (Not when each block is to be checked.)
                    if self.from != 0 && !self.check {
                        self.join(self.from, id);
                    }
                    // And generated code that comes to this address by a return or
                    // through an operand finds it in the table from now on.
                    if !self.check {
                        self.table[Self::place(c.rip)] = [c.rip, self.blocks[id].code as u64];
                    }
                    match self.run_block(id, c, m) {
                        Ok(whole) => arrived = whole,
                        Err(differs) => return Event::Differs(differs),
                    }
                    // Left by a fault: the interpreter meets the instruction next,
                    // whatever block may begin there.
                    if arrived {
                        self.from = c.left_from;
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
            // The interpreter reads the flags: they are worked out from what generated
            // code left of them, which is not done after every block (R009). And a jump
            // that left a block for here is not one to the block that comes after this
            // instruction.
            self.from = 0;
            cpu::settle(c);
            let before = c.rip;
            if let Some(why) = &mut self.why {
                *why.entry(key_of(m, before)).or_insert(0) += 1;
            }
            // An instruction that faults at its store may have changed the flags or a
            // register on the way to it (ADC to memory has its carry, XADD its
            // register): what they were is kept, for the two cases below in which the
            // instruction is run again.
            let (regs, flags) = (c.reg, c.rflags);
            match cpu::step(c, m) {
                Ok(Step::Next) => arrived = c.rip < before || c.rip > before.wrapping_add(15),
                Ok(Step::Syscall) => return Event::Syscall,
                // A store to a host page that holds translated code. If it is to the
                // code (the guest's own page written has a block's instruction in it):
                // the translations are forgotten, the page can be written again, and
                // the store is made. If it is to data that only shares the host's page
                // with code (R010): the page is opened for this one instruction.
                Err(Stop::Fault(f)) if f.access == Access::Write && m.is_code(f.addr) => {
                    (c.reg, c.rflags) = (regs, flags);
                    // An instruction writes 512 bytes at most: two of the guest's pages.
                    let page = f.addr & !(PAGE - 1);
                    if self.holds.contains_key(&page) || self.holds.contains_key(&(page + PAGE)) {
                        self.forget(f.addr, m);
                        arrived = false;
                        continue;
                    }
                    let (start, end) = m.host_pages(page, page + 2 * PAGE);
                    self.stats.beside += 1;
                    let _ = m.mark_code(start, end, false);
                    let stepped = cpu::step(c, m);
                    // Closed again where code is: every host page that a block lies in.
                    for at in (start..end).step_by(m.host_page() as usize) {
                        if (at..at + m.host_page()).step_by(PAGE as usize).any(|p| self.holds.contains_key(&p)) {
                            let _ = m.mark_code(at, at + 1, true);
                        }
                    }
                    match stepped {
                        Ok(Step::Next) => arrived = c.rip < before || c.rip > before.wrapping_add(15),
                        Ok(Step::Syscall) => return Event::Syscall,
                        Err(stop) => return Event::Stop(stop),
                    }
                }
                Err(stop) => return Event::Stop(stop),
            }
        }
    }

    /// Rewrites the jump at `site` to go to a block's code.
    fn join(&mut self, site: u64, id: usize) {
        let words = (self.blocks[id].code as i64 - site as i64) / 4;
        unsafe { self.code.rewrite(site as *const u8, a64::b(words as i32)) };
        self.blocks[id].incoming.push(site);
        self.stats.joined += 1;
    }

    /// Goes into generated code at one block, which may go on to others joined to it.
    /// `Ok(true)` if it was left at a block's end; `Ok(false)` if the host faulted,
    /// and then RIP is on the instruction that did.
    fn run_block(&mut self, id: usize, c: &mut Cpu, m: &mut Mem) -> Result<bool, String> {
        let expected = if self.check { self.interpreted(id, c, m) } else { None };
        let reason = unsafe { enter(c, self.blocks[id].code, m.base(), self.table.as_ptr()) };
        // What the blocks that were run to their ends ran.
        c.steps += c.ran;
        self.stats.in_blocks += c.ran;
        self.stats.entered += 1;
        if reason != LEFT_AT_END {
            // Which block: they lie in the order they were made. Which instruction:
            // the last that begins at or before where the host was.
            let pc = c.fault_pc;
            let block = &self.blocks[self.blocks.partition_point(|b| b.code as u64 <= pc) - 1];
            let word = ((pc - block.code as u64) / 4) as u32;
            let i = block.starts.partition_point(|&(start, _)| start <= word) - 1;
            c.rip = block.starts[i].1;
            c.steps += i as u64;
            self.stats.in_blocks += i as u64;
            self.stats.faults += 1;
            return Ok(false);
        }
        if let Some((want, written)) = expected {
            self.stats.checked += 1;
            cpu::settle(c);
            if let Some(what) = differs(&want, c, &written, m) {
                let block = &self.blocks[id];
                let (n, from) = (block.starts.len(), block.starts[0].1);
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
        let Some(Built { words, starts, end, seen }) = translate(m, rip, self.check) else {
            return false;
        };
        if std::env::var_os("R64M_JIT_DUMP").is_some() {
            // For looking at what was made: each block's address, its instructions'
            // addresses, and its code as words an assembler can be given.
            eprintln!("block {rip:#x}..{end:#x} guest {:x?}", starts.iter().map(|s| s.1).collect::<Vec<_>>());
            eprintln!("{}", words.iter().map(|w| format!(".inst {w:#010x}")).collect::<Vec<_>>().join("\n"));
        }
        // What the block rests on: its own instructions, and what was read past them
        // to leave a record of the flags unwritten. A store to any of it forgets it.
        let (from, to) = seen.map_or((rip, end), |(lo, hi)| (lo.min(rip), hi.max(end)));
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
        let pages = m.host_pages(from, to);
        if m.mark_code(pages.0, pages.1, true).is_err() {
            return false;
        }
        self.at.insert(rip, self.blocks.len());
        for page in (from & !(PAGE - 1)..to).step_by(PAGE as usize) {
            *self.holds.entry(page).or_insert(0) += 1;
        }
        self.blocks.push(Block { code, starts, pages, guest: (from, to), incoming: Vec::new() });
        self.stats.blocks += 1;
        true
    }

    /// Forgets every block with an instruction in the host page that holds `addr`, and
    /// lets the page be written again. The jumps that were rewritten to come to those
    /// blocks go to their own ways out again.
    fn forget(&mut self, addr: u64, m: &mut Mem) {
        let (start, end) = m.host_pages(addr, addr + 1);
        let blocks = &self.blocks;
        // A block that reaches into the page from a neighbour keeps the neighbour
        // marked: only this page is let go, and only blocks that touch it are dropped.
        let mut dropped = Vec::new();
        let heat = &mut self.heat;
        let (table, miss) = (&mut self.table, self.miss);
        self.at.retain(|rip, id| {
            let (from, to) = blocks[*id].pages;
            let kept = to <= start || from >= end;
            if !kept {
                dropped.push(*id);
                // To be arrived at as often again before it is made again.
                heat.remove(rip);
                // And not to be found by generated code.
                let place = &mut table[Self::place(*rip)];
                if place[0] == *rip {
                    *place = [u64::MAX, miss];
                }
            }
            kept
        });
        for id in dropped {
            for site in std::mem::take(&mut self.blocks[id].incoming) {
                unsafe { self.code.rewrite(site as *const u8, a64::b(1)) };
            }
            let (from, to) = self.blocks[id].guest;
            for page in (from & !(PAGE - 1)..to).step_by(PAGE as usize) {
                if let Some(n) = self.holds.get_mut(&page) {
                    *n -= 1;
                    if *n == 0 {
                        self.holds.remove(&page);
                    }
                }
            }
        }
        // A block of a neighbouring host page may reach into this one and was dropped
        // with the rest: its other page stays marked until it is written, which is
        // more than is needed and never less.
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
        self.holds.clear();
        self.from = 0;
        self.code.empty();
        self.clear_table();
        self.stats.emptied += 1;
    }
}

/// What an instruction is counted under for `--jit-why`: its prefixes and opcode
/// (two bytes behind 0Fh, three behind 0F 38h and 0F 3Ah), and for the opcodes that
/// are groups the ModRM byte's field, as a byte of its own.
fn key_of(m: &Mem, rip: u64) -> Vec<u8> {
    let (code, have) = m.code_at(rip);
    let bytes = unsafe { std::slice::from_raw_parts(code, have) };
    let mut key = Vec::new();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], 0x26 | 0x2e | 0x36 | 0x3e | 0x64..=0x67 | 0xf0 | 0xf2 | 0xf3) {
        key.push(bytes[i]);
        i += 1;
    }
    // REX is left out but for its W, which is another instruction.
    if i < bytes.len() && bytes[i] & 0xf0 == 0x40 {
        if bytes[i] & 8 != 0 {
            key.push(0x48);
        }
        i += 1;
    }
    let Some(&op) = bytes.get(i) else { return key };
    key.push(op);
    i += 1;
    if op == 0x0f {
        if let Some(&b) = bytes.get(i) {
            key.push(b);
            i += 1;
            if matches!(b, 0x38 | 0x3a) {
                key.extend(bytes.get(i));
                i += 1;
            }
        }
    }
    let group = match key[key.len() - 1] {
        0x80..=0x83 | 0xc0 | 0xc1 | 0xd0..=0xd3 | 0xf6 | 0xf7 | 0xfe | 0xff if key.len() < 2 || key[key.len() - 2] != 0x0f => true,
        0xae | 0xba | 0xc7 | 0x71..=0x73 if key.len() >= 2 && key[key.len() - 2] == 0x0f => true,
        _ => false,
    };
    if group {
        if let Some(&modrm) = bytes.get(i) {
            key.push(0xf8 | modrm >> 3 & 7);
        }
    }
    key
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
    use crate::jit::a64;
    use crate::jit::translate::translate;
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

    /// One program, by blocks each checked and then, from the beginning again in a
    /// memory of its own, by blocks joined to one another; and it must have been run
    /// by at least one block. What comes back is the joined run's.
    fn checked(code: &[u8], data: &[u8], set: &dyn Fn(&mut Cpu)) -> (Cpu, Mem) {
        let mut m = guest(code, data);
        let mut jit = Jit::new(true, 1).unwrap();
        both(&mut jit, &mut m, CODE, set);
        assert!(jit.stats.checked >= 1, "no block was run for {code:02x?}");
        let mut m = guest(code, data);
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, set);
        assert!(jit.stats.entered >= 1);
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
        // And the same by blocks joined to one another, in a memory of their own.
        let mut joined_m = guest(&code, &[]);
        let mut joined = Jit::new(false, 1).unwrap();
        let mut taken = 0;
        for &(a, b) in pairs {
            for &rip in &programs {
                let set = |c: &mut Cpu| {
                    c.reg[RAX] = a;
                    c.reg[RBX] = b;
                };
                let c = both(&mut jit, &mut m, rip, &set);
                taken += (c.reg[RDX] == 2) as u32;
                assert_eq!(both(&mut joined, &mut joined_m, rip, &set).reg[RDX], c.reg[RDX]);
            }
        }
        // Each program is three blocks; both ways out of each first were joined.
        assert!(joined.stats.joined as usize >= programs.len(), "{} joined", joined.stats.joined);
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
        // The code must be writable to the guest for it to write itself. Checked block
        // by block, and joined.
        for check in [true, false] {
            let mut m = Mem::reserve().unwrap();
            m.map(CODE, 4096, R | W | X).unwrap();
            m.write_bytes(CODE, &code).unwrap();
            m.map(DATA, 4096, R | W).unwrap();
            let mut jit = Jit::new(check, 1).unwrap();
            let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RSI] = DATA);
            // Three times round; the second and third with the constant as written.
            assert_eq!(c.reg[RAX], 9);
            assert_eq!(m.read_u32(DATA), Ok(3));
            assert_eq!(m.read_u32(CODE + 1), Ok(9));
            // The store from a block was a fault that left the block; the page's
            // blocks were forgotten each time and made again.
            assert!(jit.stats.faults >= 1 && jit.stats.forgotten >= 1, "faults {}, forgotten {}", jit.stats.faults, jit.stats.forgotten);
            assert!(!m.is_code(DATA));
        }
    }

    #[test]
    fn a_fault_in_a_block_is_the_guests_at_that_instruction() {
        // add eax, 1; add ebx, [rsi]; add eax, 1; syscall, with RSI nowhere.
        let code = [0x83, 0xc0, 0x01, 0x03, 0x1e, 0x83, 0xc0, 0x01, 0x0f, 0x05];
        for exact in [true, false] {
            let mut m = guest(&code, &[]);
            let mut jit = Jit::new(exact, 1).unwrap();
            let mut c = Cpu::new(CODE, DATA + 4096);
            c.reg[RSI] = 0x90_0000;
            c.reg[RAX] = 0x7fff_ffff;
            let Event::Stop(stop) = jit.run(&mut c, &mut m) else { panic!("it went on") };
            assert_eq!(stop, Stop::Fault(crate::mem::Fault { addr: 0x90_0000, access: Access::Read }));
            // On the second instruction, the first done and counted.
            assert_eq!((c.rip, c.steps, c.reg[RAX], c.reg[RBX]), (CODE + 3, 1, 0x8000_0000, 0));
            assert_eq!(jit.stats.faults, 1);
            if exact {
                // With every record written, the flags are as the first instruction
                // left them (the sign and the overflow of 7FFFFFFFh and one).
                assert_eq!(c.rflags & 0x8d5, 0x894);
            } else {
                // Without: the first instruction's flags are dead (the second writes
                // them all), so no record was made of them, and what is shown at the
                // fault is what they were before. Nothing can read them (R013); a
                // signal handler, when there is one, would be shown them stale.
                assert_eq!(c.rflags & 0x8d5, 0);
            }
        }
    }

    #[test]
    fn a_loop_of_joined_blocks_stays_in_generated_code() {
        // mov edx, 0; again: add edx, 3; sub ebx, 1; jne again; syscall
        let code = [0xba, 0, 0, 0, 0, 0x83, 0xc2, 0x03, 0x83, 0xeb, 0x01, 0x75, 0xf8, 0x0f, 0x05];
        let rounds = 100_000u64;
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RBX] = rounds);
        assert_eq!(c.reg[RDX], 3 * rounds);
        // The count is every instruction's: the first, three a round, and the SYSCALL.
        assert_eq!(c.steps, 1 + 3 * rounds + 1);
        // Generated code was gone into a few times, not once a round: the loop's jump
        // back was joined to the loop's own block.
        assert!(jit.stats.entered < 10, "entered {} times", jit.stats.entered);
        assert!(jit.stats.joined >= 1);
        assert_eq!(jit.stats.in_blocks, 1 + 3 * rounds);
    }

    #[test]
    fn a_fault_after_joined_blocks_counts_what_ran() {
        // again: add edx, 1; sub ebx, 1; jne again; mov eax, [rsi]; syscall, with RSI
        // nowhere: the load is in a block of its own, come to by a joined jump.
        let code = [0x83, 0xc2, 0x01, 0x83, 0xeb, 0x01, 0x75, 0xf8, 0x8b, 0x06, 0x0f, 0x05];
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        // Twice: the second time the loop's block is gone into with the load's block
        // already joined behind it, so the fault is in a block that was not the one
        // gone into.
        for _ in 0..2 {
            let mut c = Cpu::new(CODE, DATA + 4096);
            c.reg[RBX] = 50;
            c.reg[RSI] = 0x90_0000;
            let Event::Stop(stop) = jit.run(&mut c, &mut m) else { panic!("it went on") };
            assert_eq!(stop, Stop::Fault(crate::mem::Fault { addr: 0x90_0000, access: Access::Read }));
            // Fifty rounds of three, and RIP on the load, with the flags of the last
            // SUB (zero, parity) settled for whoever looks.
            assert_eq!((c.rip, c.steps, c.reg[RDX]), (CODE + 8, 150, 50));
            assert_eq!(c.rflags & 0x8d5, 0x44);
        }
        assert_eq!(jit.stats.faults, 2);
    }

    #[test]
    fn a_block_is_not_joined_over_what_the_interpreter_runs_between() {
        // again: add edx, 1; add dx, 0x10 (16 bits: the interpreter's); jmp next;
        // next: sub ebx, 1; jne again; syscall. The first block's way out is to the
        // 16-bit instruction, not to the block at `next` that is arrived at after it.
        let code = [0x83, 0xc2, 0x01, 0x66, 0x83, 0xc2, 0x10, 0xeb, 0x00, 0x83, 0xeb, 0x01, 0x75, 0xf2, 0x0f, 0x05];
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RBX] = 20);
        assert_eq!(c.reg[RDX], 20 * 0x11);
        assert_eq!(c.steps, 20 * 5 + 1);
    }

    #[test]
    fn a_call_in_a_loop_goes_to_its_function_every_time() {
        // again: call f; sub ebx, 1; jne again; syscall;  f: add edx, 5; ret
        // A return's target is not a constant: the jump the call left by must not be
        // taken for one to wherever the return then goes.
        let code = [0xe8, 0x07, 0, 0, 0, 0x83, 0xeb, 0x01, 0x75, 0xf6, 0x0f, 0x05, 0x83, 0xc2, 0x05, 0xc3];
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RBX] = 30);
        assert_eq!(c.reg[RDX], 150);
        assert_eq!(c.reg[crate::cpu::RSP], DATA + 4096);
        assert_eq!(c.steps, 30 * 5 + 1);
        // After the first, a return finds its block in the table: generated code is
        // not left thirty times.
        assert!(jit.stats.entered < 10, "entered {} times", jit.stats.entered);
    }

    #[test]
    fn calls_and_jumps_through_a_register_and_through_memory() {
        //  0 again: call rax         (f)
        //  2        call [rsi]       (g)
        //  4        sub ebx, 1
        //  7        je done
        //  9        jmp [rsi + 8]    (again)
        // 12 done:  syscall
        // 14 f:     add edx, 5; ret
        // 18 g:     add edx, 7; ret
        let code = [0xff, 0xd0, 0xff, 0x16, 0x83, 0xeb, 0x01, 0x74, 0x03, 0xff, 0x66, 0x08, 0x0f, 0x05, 0x83, 0xc2, 0x05, 0xc3, 0x83, 0xc2, 0x07, 0xc3];
        let mut data = (CODE + 18).to_le_bytes().to_vec();
        data.extend(CODE.to_le_bytes());
        let set = |c: &mut Cpu| {
            c.reg[RAX] = CODE + 14;
            c.reg[RBX] = 40;
            c.reg[RSI] = DATA;
        };
        let (c, _) = checked(&code, &data, &set);
        assert_eq!(c.reg[RDX], 40 * 12);
        assert_eq!(c.reg[crate::cpu::RSP], DATA + 4096);
        // And joined, counted: not once a round out to Rust.
        let mut m = guest(&code, &data);
        let mut jit = Jit::new(false, 1).unwrap();
        both(&mut jit, &mut m, CODE, &set);
        assert!(jit.stats.entered < 20, "entered {} times", jit.stats.entered);
        // call [rsp + 8]: where to is read before the return address is pushed.
        //  0 call [rsp + 8]; 4 syscall; 6 f: add edx, 5; ret
        let code = [0xff, 0x54, 0x24, 0x08, 0x0f, 0x05, 0x83, 0xc2, 0x05, 0xc3];
        let mut data = vec![0u8; 0x810];
        data[0x808..].copy_from_slice(&(CODE + 6).to_le_bytes());
        let (c, m) = checked(&code, &data, &|c| c.reg[crate::cpu::RSP] = DATA + 0x800);
        assert_eq!((c.reg[RDX], c.reg[crate::cpu::RSP]), (5, DATA + 0x800));
        assert_eq!(m.read_u64(DATA + 0x7f8), Ok(CODE + 4));
    }

    #[test]
    fn two_returns_that_share_a_place_in_the_table_both_arrive() {
        // Two calls of one function from addresses 4000h apart: their return
        // addresses choose the same place, and put each other out of it every time.
        //  near:  call f; jmp far            f at near + 100h: add edx, 3; ret
        //  far:   call f; sub ebx, 1; jne near; syscall
        const FAR: u64 = CODE + (1 << TABLE_BITS);
        let mut code = vec![0u8; (1 << TABLE_BITS) + 32];
        let rel = |from_end: u64, to: u64| ((to as i64 - from_end as i64) as i32).to_le_bytes();
        code[0] = 0xe8;
        code[1..5].copy_from_slice(&rel(CODE + 5, CODE + 0x100));
        code[5] = 0xe9;
        code[6..10].copy_from_slice(&rel(CODE + 10, FAR));
        code[0x100..0x104].copy_from_slice(&[0x83, 0xc2, 0x03, 0xc3]);
        let far = (FAR - CODE) as usize;
        code[far] = 0xe8;
        code[far + 1..far + 5].copy_from_slice(&rel(FAR + 5, CODE + 0x100));
        code[far + 5..far + 8].copy_from_slice(&[0x83, 0xeb, 0x01]);
        code[far + 8] = 0x0f;
        code[far + 9] = 0x85;
        code[far + 10..far + 14].copy_from_slice(&rel(FAR + 14, CODE));
        code[far + 14..far + 16].copy_from_slice(&[0x0f, 0x05]);
        assert_eq!(Jit::place(CODE + 5), Jit::place(FAR + 5));
        let (c, _) = checked(&code, &[], &|c| c.reg[RBX] = 25);
        assert_eq!(c.reg[RDX], 25 * 6);
    }

    #[test]
    fn a_block_that_is_forgotten_is_not_found_in_the_table() {
        // On one page:   again: call r8         (the other page's function)
        //                       mov dword [far + 1], 9
        //                       add edx, eax; sub ebx, 1; jne again; syscall
        // On another:    far:   mov eax, 7; ret
        // The call is through a register: its block is found in the table, and must
        // not be found there once the page it was translated from is written. (The
        // function is not at its page's start: it must not choose the place in the
        // table that `again` chooses, which would put it out by chance.)
        const FAR: u64 = CODE + 0x1_0040;
        let mut near = vec![0x41, 0xff, 0xd0, 0xc7, 0x05, 0, 0, 0, 0, 9, 0, 0, 0, 0x01, 0xc2, 0x83, 0xeb, 0x01, 0x75, 0xec, 0x0f, 0x05];
        near[5..9].copy_from_slice(&((FAR + 1 - (CODE + 13)) as i32).to_le_bytes());
        let far = [0xb8, 7, 0, 0, 0, 0xc3];
        assert_ne!(Jit::place(FAR), Jit::place(CODE));
        for check in [true, false] {
            let mut m = Mem::reserve().unwrap();
            m.map(CODE, 4096, R | W | X).unwrap();
            m.write_bytes(CODE, &near).unwrap();
            m.map(FAR & !0xfff, 4096, R | W | X).unwrap();
            m.write_bytes(FAR, &far).unwrap();
            m.map(DATA, 4096, R | W).unwrap();
            let mut jit = Jit::new(check, 1).unwrap();
            let c = both(&mut jit, &mut m, CODE, &|c| {
                c.reg[RBX] = 4;
                c.reg[8] = FAR;
            });
            assert_eq!(c.reg[RDX], 7 + 3 * 9);
        }
    }

    #[test]
    fn a_jump_joined_to_a_block_that_is_forgotten_is_not_taken_again() {
        // On one page:   again: jmp far
        //                back:  mov dword [far + 1], 9   (writes the other page's code)
        //                       add edx, eax; sub ebx, 1; jne again; syscall
        // On another:    far:   mov eax, 7; jmp back
        // The block at `again` stays (its page is not written); the jump in it was
        // joined to the block at `far`, which is forgotten when its page is written.
        const FAR: u64 = CODE + 0x1_0000;
        let mut near = vec![0xe9, 0, 0, 0, 0, 0xc7, 0x05, 0, 0, 0, 0, 9, 0, 0, 0, 0x01, 0xc2, 0x83, 0xeb, 0x01, 0x75, 0xea, 0x0f, 0x05];
        near[1..5].copy_from_slice(&((FAR - (CODE + 5)) as i32).to_le_bytes());
        near[7..11].copy_from_slice(&((FAR + 1 - (CODE + 15)) as i32).to_le_bytes());
        let mut far = vec![0xb8, 7, 0, 0, 0, 0xe9, 0, 0, 0, 0];
        far[6..10].copy_from_slice(&((CODE as i64 + 5 - (FAR as i64 + 10)) as i32).to_le_bytes());
        for check in [true, false] {
            let mut m = Mem::reserve().unwrap();
            m.map(CODE, 4096, R | W | X).unwrap();
            m.write_bytes(CODE, &near).unwrap();
            m.map(FAR, 4096, R | W | X).unwrap();
            m.write_bytes(FAR, &far).unwrap();
            m.map(DATA, 4096, R | W).unwrap();
            let mut jit = Jit::new(check, 1).unwrap();
            let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RBX] = 4);
            // Seven the first time round, nine the three after.
            assert_eq!(c.reg[RDX], 7 + 3 * 9);
            assert!(jit.stats.forgotten >= 1);
        }
    }

    #[test]
    fn a_store_to_data_beside_code_keeps_the_code() {
        // The code and, two of the guest's pages on, its data: one host page where
        // the host's pages are 16 KB, two where they are 4 KB.
        // again: add [rsi], ebx (the store); add edx, [rsi]; sub ebx, 1; jne again;
        // syscall
        let code = [0x01, 0x1e, 0x03, 0x16, 0x83, 0xeb, 0x01, 0x75, 0xf7, 0x0f, 0x05];
        let rounds = 300u64;
        for check in [true, false] {
            let mut m = Mem::reserve().unwrap();
            m.map(CODE, 4096, R | W | X).unwrap();
            m.write_bytes(CODE, &code).unwrap();
            m.map(CODE + 0x2000, 4096, R | W).unwrap();
            m.map(DATA, 4096, R | W).unwrap();
            let mut jit = Jit::new(check, 1).unwrap();
            let c = both(&mut jit, &mut m, CODE, &|c| {
                c.reg[RBX] = rounds;
                c.reg[RSI] = CODE + 0x2000;
            });
            // The sum of the sums of 300 down to each round's count.
            let total: u64 = (1..=rounds).map(|k| (k..=rounds).sum::<u64>()).sum();
            assert_eq!(c.reg[RDX], total & 0xffff_ffff);
            assert_eq!(m.read_u32(CODE + 0x2000), Ok((rounds * (rounds + 1) / 2) as u32));
            // Nothing was forgotten for it, however often it was written, and the
            // loop's other instructions went on running in their block.
            assert_eq!(jit.stats.forgotten, 0);
            if m.host_page() > 4096 {
                // Where the host cannot tell the two pages apart, every store was
                // made beside the code.
                assert_eq!(jit.stats.beside, rounds);
            } else {
                assert_eq!((jit.stats.beside, jit.stats.faults), (0, 0));
            }
            assert!(m.is_code(CODE));
        }
    }

    #[test]
    fn a_store_that_runs_from_data_into_code_forgets_the_code() {
        // The code begins at a page's start, and a store of eight bytes begins four
        // bytes before it: its upper half lands on the code's first four bytes.
        // again: mov [rsi], rcx; sub ebx, 1; jne again; syscall, at CODE + 4096, with
        // RSI just under it.
        let code = [0x48, 0x89, 0x0e, 0x83, 0xeb, 0x01, 0x75, 0xf8, 0x0f, 0x05];
        let mut m = Mem::reserve().unwrap();
        m.map(CODE, 8192, R | W | X).unwrap();
        // The four bytes of the store that land on the code are what is there
        // already: the program goes on unchanged.
        m.write_bytes(CODE + 4096, &code).unwrap();
        let low = u32::from_le_bytes([code[0], code[1], code[2], code[3]]) as u64;
        m.map(DATA, 4096, R | W).unwrap();
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE + 4096, &|c| {
            c.reg[RBX] = 3;
            c.reg[RSI] = CODE + 4096 - 4;
            c.reg[crate::cpu::RCX] = low << 32 | 0xdddd_dddd;
        });
        assert_eq!(c.reg[RBX], 0);
        assert_eq!(m.read_u32(CODE + 4096 - 4), Ok(0xdddd_dddd));
        // Each round the store reached the code's own page: forgotten each time.
        assert_eq!(jit.stats.forgotten, 3);
        assert_eq!(jit.stats.beside, 0);
    }

    #[test]
    fn an_instruction_run_again_after_its_store_faulted_begins_as_it_began() {
        // Each stores to data that lies beside the code (one host page where pages
        // are 16 KB) or to the code's own page, so that the store faults the first
        // time and the instruction is run again: adc [rsi], ebx with the carry set
        // (its own carry out must not be its carry in); xadd [rsi], ebx (the register
        // it changes is one of its operands); sbb; cmpxchg; pop [rsi] (RSP).
        let programs: &[&[u8]] = &[
            &[0xf9, 0x11, 0x1e, 0x0f, 0x05],
            &[0x0f, 0xc1, 0x1e, 0x0f, 0x05],
            &[0xf9, 0x19, 0x1e, 0x0f, 0x05],
            &[0x0f, 0xb1, 0x1e, 0x0f, 0x05],
            &[0x6a, 0x77, 0x8f, 0x06, 0x0f, 0x05],
            &[0xf9, 0xd1, 0x16, 0x0f, 0x05],
        ];
        for (i, code) in programs.iter().enumerate() {
            for data_at in [CODE + 0x2000, CODE + 0x800] {
                let mut m = Mem::reserve().unwrap();
                m.map(CODE, 0x3000, R | W | X).unwrap();
                m.write_bytes(CODE, code).unwrap();
                m.write_u32(data_at, 0xffff_fff0).unwrap();
                m.map(DATA, 4096, R | W).unwrap();
                // A block on the code's page first, so that the page is marked: the
                // program is arrived at once before it is run for the comparison.
                let mut jit = Jit::new(false, 1).unwrap();
                let set = |c: &mut Cpu| {
                    c.reg[RSI] = data_at;
                    c.reg[RBX] = 0x1f;
                    c.reg[RAX] = 0xffff_fff0;
                };
                // (A program of the interpreter's instructions only makes no block:
                // one that is translated is put before it.)
                let mut with_block = vec![0x83, 0xc2, 0x01];
                with_block.extend(*code);
                m.write_bytes(CODE, &with_block).unwrap();
                both(&mut jit, &mut m, CODE, &set);
                assert!(jit.stats.blocks >= 1, "program {i}: no block");
                assert!(jit.stats.beside + jit.stats.forgotten >= 1 || m.host_page() == 4096 && data_at != CODE + 0x800, "program {i}: the store did not fault");
            }
        }
    }

    /// The end of a small program that shows its flags: a jump on condition `cc` that
    /// picks RDX, and SYSCALL.
    fn pick(cc: u8) -> [u8; 16] {
        [0x70 | cc, 7, 0xba, 1, 0, 0, 0, 0xeb, 5, 0xba, 2, 0, 0, 0, 0x0f, 0x05]
    }

    /// Runs each instruction of `list` under every condition from the registers that
    /// `set` gives, by blocks each checked and by blocks joined, held to the
    /// interpreter (registers, flags, memory, count). `data` begins the page at RSI.
    fn under_every_condition(list: &[&[u8]], data: &[u8], set: &dyn Fn(&mut Cpu)) {
        // All of them one after another in one memory, each at its own address.
        let mut code = Vec::new();
        let mut programs = Vec::new();
        for ins in list {
            for cc in 0..16 {
                programs.push(CODE + code.len() as u64);
                code.extend(*ins);
                code.extend(pick(cc));
            }
        }
        for check in [true, false] {
            let mut m = guest(&code, data);
            let mut jit = Jit::new(check, 1).unwrap();
            for &rip in &programs {
                // The data is written by some: put back for each.
                m.write_bytes(DATA, data).unwrap();
                both(&mut jit, &mut m, rip, set);
            }
            assert!(jit.stats.entered as usize >= programs.len());
        }
    }

    #[test]
    fn operands_of_8_and_16_bits() {
        // op r/m, r and op r, r/m on AL and BL, on AH and BL, on AX and BX, and on
        // memory; with constants; each under every condition, on pairs that make each
        // flag come out both ways at 8 and at 16 bits.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for op in [0x00u8, 0x08, 0x20, 0x28, 0x30, 0x38, 0x84] {
            list.push(vec![op, 0xd8]); // al, bl
            list.push(vec![op, 0xdc]); // ah, bl
            list.push(vec![op, 0xfc]); // ah, bh
            list.push(vec![op, 0x1e]); // [rsi], bl
            list.push(vec![op, 0x7e, 0x01]); // [rsi + 1], bh
            list.push(vec![0x40, op, 0xf8]); // al, dil (REX: no AH)
            list.push(vec![0x66, op | 1, 0xd8]); // ax, bx
            list.push(vec![0x66, op | 1, 0x1e]); // [rsi], bx
            if op != 0x84 {
                list.push(vec![op | 2, 0xc3]); // al, bl the other way
                list.push(vec![op | 2, 0x26]); // ah, [rsi]
                list.push(vec![0x66, op | 3, 0x06]); // ax, [rsi]
            }
        }
        for field in [0u8, 1, 4, 5, 6, 7] {
            list.push(vec![0x80, 0xc0 | field << 3, 0x81]); // al, 81h
            list.push(vec![0x80, 0xc4 | field << 3, 0x7f]); // ah, 7fh
            list.push(vec![0x80, 0x06 | field << 3, 0xff]); // byte [rsi], ffh
            list.push(vec![0x66, 0x81, 0xc0 | field << 3, 0x00, 0x80]); // ax, 8000h
            list.push(vec![0x66, 0x83, 0xc0 | field << 3, 0xff]); // ax, -1
            list.push(vec![0x66, 0x81, 0x06 | field << 3, 0xff, 0x7f]); // word [rsi], 7fffh
        }
        list.push(vec![0xf6, 0xc3, 0x80]); // test bl, 80h
        list.push(vec![0xf6, 0xc7, 0x01]); // test bh, 1
        list.push(vec![0x66, 0xf7, 0xc3, 0x00, 0x80]); // test bx, 8000h
        list.push(vec![0xf6, 0x06, 0x80]); // test byte [rsi], 80h
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        for (a, b) in [(0x1122_3344_5566_0000u64, 0u64), (0x7f7f, 0x0101), (0x8080, 0x8080), (0xffff, 0x0001), (0x00ff, 0xff01), (0x1234, 0x1234), (0xaaaa_aaaa_aaaa_8001, 0x5555_5555_5555_7fff)] {
            under_every_condition(&list, &[0x80, 0x7f, 0xff, 0x01], &|c| {
                c.reg[RAX] = a;
                c.reg[RBX] = b;
                // DIL is not BH, which has its number without REX.
                c.reg[7] = !b ^ 0x35;
                c.reg[RSI] = DATA;
            });
        }
    }

    #[test]
    fn moves_of_8_and_16_bits_leave_the_rest() {
        let set = |c: &mut Cpu| {
            c.reg[RAX] = 0x1111_2222_3333_4455;
            c.reg[RBX] = 0xaaaa_bbbb_cccc_ddee;
            c.reg[RSI] = DATA;
        };
        let data = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        let run = |code: &[u8]| {
            let mut whole = code.to_vec();
            whole.extend([0x0f, 0x05]);
            let (c, m) = checked(&whole, &data, &set);
            (c.reg[RAX], m.read_u64(DATA).unwrap())
        };
        let (a, d) = (0x1111_2222_3333_4455u64, 0x0807_0605_0403_0201u64);
        // To a register: only its low byte, its second byte, or its low 16 bits.
        assert_eq!(run(&[0x88, 0xd8]), (a & !0xff | 0xee, d)); // mov al, bl
        assert_eq!(run(&[0x88, 0xfc]), (a & !0xff00 | 0xdd00, d)); // mov ah, bh
        assert_eq!(run(&[0x88, 0xdc]), (a & !0xff00 | 0xee00, d)); // mov ah, bl
        assert_eq!(run(&[0x88, 0xf8]), (a & !0xff | 0xdd, d)); // mov al, bh
        assert_eq!(run(&[0x66, 0x89, 0xd8]), (a & !0xffff | 0xddee, d)); // mov ax, bx
        assert_eq!(run(&[0x8a, 0x46, 0x02]), (a & !0xff | 0x03, d)); // mov al, [rsi + 2]
        assert_eq!(run(&[0x8a, 0x66, 0x02]), (a & !0xff00 | 0x0300, d)); // mov ah, [rsi + 2]
        assert_eq!(run(&[0x66, 0x8b, 0x46, 0x02]), (a & !0xffff | 0x0403, d)); // mov ax, [rsi + 2]
        assert_eq!(run(&[0xc6, 0xc0, 0x99]), (a & !0xff | 0x99, d)); // mov al, 99h
        assert_eq!(run(&[0xc6, 0xc4, 0x99]), (a & !0xff00 | 0x9900, d)); // mov ah, 99h
        assert_eq!(run(&[0x66, 0xc7, 0xc0, 0x34, 0x12]), (a & !0xffff | 0x1234, d)); // mov ax, 1234h
        // To memory: one byte or two, the neighbours left.
        assert_eq!(run(&[0x88, 0x5e, 0x03]), (a, d & !0xff00_0000 | 0xee00_0000)); // mov [rsi + 3], bl
        assert_eq!(run(&[0x88, 0x7e, 0x03]), (a, d & !0xff00_0000 | 0xdd00_0000)); // mov [rsi + 3], bh
        assert_eq!(run(&[0x66, 0x89, 0x5e, 0x03]), (a, d & !0xff_ff00_0000 | 0xdd_ee00_0000)); // mov [rsi + 3], bx
        assert_eq!(run(&[0xc6, 0x46, 0x03, 0x99]), (a, d & !0xff00_0000 | 0x9900_0000)); // mov byte [rsi + 3], 99h
        assert_eq!(run(&[0x66, 0xc7, 0x46, 0x03, 0x34, 0x12]), (a, d & !0xff_ff00_0000 | 0x12_3400_0000));
        // A displacement too large for one instruction, with AH: both need scratch.
        // mov [rsi + 10000h], ah, with RSI that far under the data.
        let (c, m) = checked(&[0x88, 0xa6, 0x00, 0x00, 0x01, 0x00, 0x0f, 0x05], &data, &|c| {
            c.reg[RAX] = a;
            c.reg[RSI] = DATA - 0x1_0000;
        });
        assert_eq!((c.reg[RAX], m.read_u64(DATA).unwrap()), (a, d & !0xff | 0x44));
        // NOT of a byte, of its second byte, of 16 bits, of memory.
        assert_eq!(run(&[0xf6, 0xd0]), (a ^ 0xff, d));
        assert_eq!(run(&[0xf6, 0xd4]), (a ^ 0xff00, d));
        assert_eq!(run(&[0x66, 0xf7, 0xd0]), (a ^ 0xffff, d));
        assert_eq!(run(&[0xf6, 0x56, 0x01]), (a, d ^ 0xff00));
        assert_eq!(run(&[0x66, 0xf7, 0x16]), (a, d ^ 0xffff));
        assert_eq!(run(&[0xf7, 0x16]), (a, d ^ 0xffff_ffff));
        assert_eq!(run(&[0x48, 0xf7, 0x16]), (a, !d));
        assert_eq!(run(&[0xf7, 0xd0]), (!a & 0xffff_ffff, d));
        assert_eq!(run(&[0x48, 0xf7, 0xd0]), (!a, d));
        // CWDE, CDQE; CDQ and CQO (seen in RDX, moved to RAX).
        assert_eq!(run(&[0x98]).0, 0x4455);
        assert_eq!(run(&[0x48, 0x98]).0, 0x3333_4455);
        assert_eq!(run(&[0xb8, 0x00, 0x80, 0x00, 0x00, 0x98]).0, 0xffff_8000);
        assert_eq!(run(&[0xb8, 0x00, 0x00, 0x00, 0x80, 0x48, 0x98]).0, 0xffff_ffff_8000_0000);
        assert_eq!(run(&[0xb8, 0x00, 0x00, 0x00, 0x80, 0x99, 0x89, 0xd0]).0, 0xffff_ffff);
        assert_eq!(run(&[0x99, 0x48, 0x89, 0xd0]).0, 0);
        assert_eq!(run(&[0x48, 0x89, 0xd8, 0x48, 0x99, 0x48, 0x89, 0xd0]).0, u64::MAX);
    }

    #[test]
    fn shifts_and_their_flags() {
        // SHL, SHR, SAR (and SAL, which is SHL) of EAX and of RAX by constants and by
        // one, each under every condition.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for field in [4u8, 5, 6, 7] {
            for n in [0u8, 1, 2, 7, 31, 32, 33, 63, 64, 0xff] {
                list.push(vec![0xc1, 0xc0 | field << 3, n]);
                list.push(vec![0x48, 0xc1, 0xc0 | field << 3, n]);
            }
            list.push(vec![0xd1, 0xc0 | field << 3]);
            list.push(vec![0x48, 0xd1, 0xc0 | field << 3]);
            // By CL: of EAX, of RAX, and of RCX itself.
            list.push(vec![0xd3, 0xc0 | field << 3]);
            list.push(vec![0x48, 0xd3, 0xc0 | field << 3]);
            list.push(vec![0x48, 0xd3, 0xc1 | field << 3]);
            // And with a compare before, whose flags a shift by nothing leaves.
            list.push(vec![0x39, 0xd8, 0xd3, 0xc0 | field << 3]);
            list.push(vec![0x39, 0xd8, 0x48, 0xd3, 0xc0 | field << 3]);
        }
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        for value in [0u64, 1, 0x8000_0000, 0x4000_0000, 0xffff_ffff, 1 << 63, 0x4000_0000_0000_0001, 0xc000_0000_c000_0001, u64::MAX] {
            for count in [0u64, 1, 5, 31, 32, 33, 63, 64, 0x141] {
                under_every_condition(&list, &[], &|c| {
                    c.reg[RAX] = value;
                    c.reg[RBX] = value ^ 1;
                    c.reg[crate::cpu::RCX] = count;
                });
            }
        }
    }

    #[test]
    fn rotations_where_their_flags_are_not_read_and_where_they_are() {
        // ROL and ROR by constants, by one and by CL, of EAX and RAX: followed by an
        // instruction that writes all the flags (translated: the block goes on through
        // the rotation), and followed at once by a jump that reads them (not
        // translated: the interpreter's). Either way the end is the interpreter's.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for field in [0u8, 1] {
            for n in [0u8, 1, 7, 13, 31, 32, 33, 63, 64] {
                for tail in [&[0x39, 0xd8][..], &[][..], &[0x89, 0xc7, 0x85, 0xff][..]] {
                    for rex in [&[][..], &[0x48][..]] {
                        let mut v = rex.to_vec();
                        v.extend([0xc1, 0xc0 | field << 3, n]);
                        v.extend(tail);
                        list.push(v);
                    }
                }
            }
            for tail in [&[0x39, 0xd8][..], &[][..]] {
                for first in [&[0xd1][..], &[0x48, 0xd1][..], &[0xd3][..], &[0x48, 0xd3][..]] {
                    let mut v = first.to_vec();
                    v.push(0xc0 | field << 3);
                    v.extend(tail);
                    list.push(v);
                }
            }
        }
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        for value in [1u64, 0x8000_0001, 0x1234_5678_9abc_def0, 1 << 63, u64::MAX - 1] {
            for count in [0u64, 1, 13, 31, 32, 63, 64, 0x7f] {
                under_every_condition(&list, &[], &|c| {
                    c.reg[RAX] = value;
                    c.reg[RBX] = 0x1234;
                    c.reg[crate::cpu::RCX] = count;
                });
            }
        }
        // That a rotation whose flags die is in a block, and one whose flags are read
        // is not: rol eax, 7; cmp eax, ebx; jne; ... against rol eax, 7; jc ...
        let block_of = |code: &[u8]| {
            let m = guest(code, &[]);
            translate(&m, CODE, false).map(|b| b.starts.len())
        };
        assert_eq!(block_of(&[0xc1, 0xc0, 0x07, 0x39, 0xd8, 0x75, 0x00, 0x0f, 0x05]), Some(3));
        assert_eq!(block_of(&[0x89, 0xd8, 0xc1, 0xc0, 0x07, 0x72, 0x00, 0x0f, 0x05]), Some(1));
        assert_eq!(block_of(&[0xc1, 0xc0, 0x07, 0x72, 0x00, 0x0f, 0x05]), None);
        // A shift by CL between may leave the flags: the rotation's are not dead.
        assert_eq!(block_of(&[0x89, 0xd8, 0xc1, 0xc0, 0x07, 0xd3, 0xe0, 0x0f, 0x05]), Some(1));
    }

    #[test]
    fn bit_scans_and_negation() {
        // BSF, BSR, and both with F3h in front (the same, on a CPU without TZCNT and
        // LZCNT), of EBX and RBX into EAX and RAX, of memory; of a zero the register is
        // left. NEG of EAX and of RAX.
        let list: &[&[u8]] = &[
            &[0x0f, 0xbc, 0xc3],
            &[0x0f, 0xbd, 0xc3],
            &[0x48, 0x0f, 0xbc, 0xc3],
            &[0x48, 0x0f, 0xbd, 0xc3],
            &[0xf3, 0x0f, 0xbc, 0xc3],
            &[0xf3, 0x48, 0x0f, 0xbd, 0xc3],
            &[0x0f, 0xbc, 0x06],
            &[0x48, 0x0f, 0xbd, 0x06],
            &[0x48, 0x0f, 0xbc, 0xc0],
            &[0x48, 0x0f, 0xbd, 0xdb],
            &[0xf7, 0xd8],
            &[0x48, 0xf7, 0xd8],
            &[0xf7, 0xdb],
        ];
        for value in [0u64, 1, 0x80, 0x8000_0000, 0x1_0000_0000, 1 << 63, 0xffff_0000_0000_0000, 0x7fff_ffff, u64::MAX] {
            under_every_condition(list, &value.to_le_bytes(), &|c| {
                c.reg[RAX] = 0x1122_3344_5566_7788;
                c.reg[RBX] = value;
                c.reg[RSI] = DATA;
            });
            under_every_condition(&[&[0xf7, 0xd8], &[0x48, 0xf7, 0xd8]], &[], &|c| c.reg[RAX] = value);
        }
    }

    #[test]
    fn flags_that_are_dead_are_not_recorded_and_live_ones_are() {
        use crate::jit::translate::translate;
        let words = |code: &[u8], exact: bool| {
            let m = guest(code, &[]);
            translate(&m, CODE, exact).unwrap().words.len()
        };
        // add eax, ebx; add ecx, edx; syscall: the first add's flags die at the second,
        // whose own are read by whatever comes after. One record fewer than exact.
        let two = [0x01, 0xd8, 0x01, 0xd1, 0x0f, 0x05];
        assert_eq!(words(&two, true) - words(&two, false), 3);
        // add eax, ebx; mov ecx, eax; syscall: nothing writes them again. All kept.
        let live = [0x01, 0xd8, 0x89, 0xc1, 0x0f, 0x05];
        assert_eq!(words(&live, true), words(&live, false));
        // Shifts in a row: each of three but the last without its record (a constant
        // and three moves each).
        let shifts = [0xc1, 0xe0, 0x03, 0xc1, 0xe8, 0x05, 0xc1, 0xe0, 0x07, 0xc1, 0xf8, 0x02, 0x0f, 0x05];
        assert_eq!(words(&shifts, true) - words(&shifts, false), 3 * 4);
        // A compare read by its own jump, both ways then writing the flags: no record.
        //  0 cmp eax, ebx; 2 jne +2; 4 add ecx, edx (falls to 6); 6 add esi, edi; syscall
        let both_die = [0x39, 0xd8, 0x75, 0x02, 0x01, 0xd1, 0x01, 0xfe, 0x0f, 0x05];
        assert_eq!(words(&both_die, true) - words(&both_die, false), 3);
        // The same with one way that reads them (jb at the jump's target): recorded.
        //  0 cmp; 2 jne +2; 4 add ecx, edx; 6 jb ...
        let one_reads = [0x39, 0xd8, 0x75, 0x02, 0x01, 0xd1, 0x72, 0x00, 0x0f, 0x05];
        assert_eq!(words(&one_reads, true), words(&one_reads, false));
        // A jump that the host's flags cannot answer (parity) reads the record.
        let parity = [0x39, 0xd8, 0x7a, 0x02, 0x01, 0xd1, 0x01, 0xfe, 0x0f, 0x05];
        assert_eq!(words(&parity, true), words(&parity, false));
        // Across a jump and a call to constants: add; jmp +0; add: dead.
        let across = [0x01, 0xd8, 0xeb, 0x00, 0x01, 0xd1, 0x0f, 0x05];
        assert_eq!(words(&across, true) - words(&across, false), 3);
        // Before a return: live.
        let ret = [0x01, 0xd8, 0xc3];
        assert_eq!(words(&ret, true), words(&ret, false));
        // What was read to decide is said, so that a store there is known of.
        let m = guest(&across, &[]);
        let built = translate(&m, CODE, false).unwrap();
        assert_eq!((built.end, built.seen), (CODE + 4, Some((CODE + 2, CODE + 6))));
    }

    #[test]
    fn a_dead_flag_made_live_by_a_store_to_the_code_after_is_recorded_again() {
        // first: add eax, ebx; jmp second      (the add's flags die at `second`'s add)
        // second: add edx, 1; ... the program then writes `second`'s first instruction
        // into one that reads the carry (adc edx, 1 is 83 D2 01), and comes round.
        //  0  first:  add eax, ebx          01 d8
        //  2          jmp second            eb 00
        //  4  second: add edx, 1            83 c2 01    <- becomes adc edx, 1
        //  7          sub ecx, 1            83 e9 01
        // 10          je done               74 09
        // 12          mov byte [second + 1], 0xd2   c6 05 f2 ff ff ff d2
        // 19          jmp first             eb eb
        // 21  done:   syscall
        let code = [0x01, 0xd8, 0xeb, 0x00, 0x83, 0xc2, 0x01, 0x83, 0xe9, 0x01, 0x74, 0x09, 0xc6, 0x05, 0xf2, 0xff, 0xff, 0xff, 0xd2, 0xeb, 0xeb, 0x0f, 0x05];
        for check in [true, false] {
            let mut m = Mem::reserve().unwrap();
            m.map(CODE, 4096, R | W | X).unwrap();
            m.write_bytes(CODE, &code).unwrap();
            m.map(DATA, 4096, R | W).unwrap();
            let mut jit = Jit::new(check, 1).unwrap();
            // EAX + EBX carries every time: once `second` is ADC it adds two.
            let c = both(&mut jit, &mut m, CODE, &|c| {
                c.reg[RAX] = 0xffff_ffff;
                c.reg[RBX] = 0xffff_ffff;
                c.reg[crate::cpu::RCX] = 3;
            });
            assert_eq!(m.read_u8(CODE + 5), Ok(0xd2));
            assert_eq!(c.reg[RDX], 1 + 2 + 2);
        }
    }

    /// A program that ends by picking RDX on the carry, by way of `between`, with both
    /// ways then writing the flags afresh:
    ///      add eax, ebx; (between); jc two
    ///      add esi, edi; mov edx, 1; jmp end
    /// two: add esi, edi; mov edx, 2
    /// end: syscall
    fn carry_picked_after(between: &[u8]) -> Vec<u8> {
        let mut code = vec![0x01, 0xd8];
        code.extend(between);
        code.extend([0x72, 0x09, 0x01, 0xfe, 0xba, 1, 0, 0, 0, 0xeb, 0x07, 0x01, 0xfe, 0xba, 2, 0, 0, 0, 0x0f, 0x05]);
        code
    }

    #[test]
    fn flags_read_where_the_hosts_cannot_answer_are_recorded() {
        // The addition carries; whoever reads the carry must find it. In each of
        // these the jump on it cannot be answered by the host's flags of the
        // addition, so it reads the record, which must therefore have been written,
        // though both of the jump's ways write the flags again.
        let set = |c: &mut Cpu| {
            c.reg[RAX] = 0xffff_ffff;
            c.reg[RBX] = 2;
        };
        for between in [
            // A jump to the next instruction: the jump on carry begins another block.
            &[0xeb, 0x00][..],
            // A shift by CL (by nothing here): it keeps x86's flags but the host's
            // are no longer asked after it.
            &[0xd3, 0xe7][..],
            // A rotation, whose own flags are dead only if the carry is not read:
            // it is read, so the block ends before it.
            &[0xc1, 0xc7, 0x00][..],
            // Sixty moves: the jump is past the end of what one block holds.
            &[0x89, 0xf7].repeat(70)[..],
        ] {
            let (c, _) = checked(&carry_picked_after(between), &[], &set);
            assert_eq!(c.reg[RDX], 2, "after {between:02x?}");
        }
        // The addition late in a block: 61 moves before it, three after, and the jump
        // on the carry falls past the 64 instructions a block holds, into the next.
        let mut late = [0x89, 0xf7].repeat(61);
        late.extend(carry_picked_after(&[0x89, 0xf7, 0x89, 0xf7, 0x89, 0xf7]));
        assert_eq!(checked(&late, &[], &set).0.reg[RDX], 2);
        // The addition the last instruction a block holds: the jump on its carry is
        // the next block's first, and no record may be left for it to write.
        let mut last = [0x89, 0xf7].repeat(63);
        last.extend(carry_picked_after(&[]));
        assert_eq!(checked(&last, &[], &set).0.reg[RDX], 2);
        // And answered in the block, no record is needed: one fewer than exact.
        let direct = carry_picked_after(&[]);
        let m = guest(&direct, &[]);
        let words = |exact| translate(&m, CODE, exact).unwrap().words.len();
        assert_eq!(words(true) - words(false), 3);
        assert_eq!(checked(&direct, &[], &set).0.reg[RDX], 2);
    }

    #[test]
    fn code_read_to_leave_a_record_out_is_what_the_block_rests_on() {
        // As the test of dead flags made live, with `second` on the next host page
        // from `first` (which lies at the end of its own): the block at `first` has
        // no instruction on that page, only what it read there to leave its record
        // out. A store there must forget it all the same.
        //  first:  add eax, ebx; jmp second            at the end of a host page
        //  second: add edx, 1 (becomes adc edx, 1); sub ecx, 1; je done;
        //          mov byte [second + 1], 0xd2; jmp first;  done: syscall
        const SECOND: u64 = CODE + 0x4000;
        const FIRST: u64 = SECOND - 4;
        let first = [0x01, 0xd8, 0xeb, 0x00];
        let mut second = vec![0x83, 0xc2, 0x01, 0x83, 0xe9, 0x01, 0x74, 0x0c, 0xc6, 0x05, 0, 0, 0, 0, 0xd2, 0xe9, 0, 0, 0, 0, 0x0f, 0x05];
        second[10..14].copy_from_slice(&(1i32 - 15).to_le_bytes());
        second[16..20].copy_from_slice(&((FIRST as i64 - (SECOND as i64 + 20)) as i32).to_le_bytes());
        for check in [true, false] {
            let mut m = Mem::reserve().unwrap();
            m.map(CODE, 0x8000, R | W | X).unwrap();
            m.write_bytes(FIRST, &first).unwrap();
            m.write_bytes(SECOND, &second).unwrap();
            m.map(DATA, 4096, R | W).unwrap();
            assert_ne!(m.host_pages(FIRST, FIRST + 1), m.host_pages(SECOND, SECOND + 1));
            let mut jit = Jit::new(check, 1).unwrap();
            let c = both(&mut jit, &mut m, FIRST, &|c| {
                c.reg[RAX] = 0xffff_ffff;
                c.reg[RBX] = 0xffff_ffff;
                c.reg[crate::cpu::RCX] = 3;
            });
            assert_eq!(m.read_u8(SECOND + 1), Ok(0xd2));
            assert_eq!(c.reg[RDX], 1 + 2 + 2);
        }
    }

    #[test]
    fn a_record_left_for_the_jump_is_the_record() {
        // Each is followed at once by a jump on every condition in turn, and then by
        // SYSCALL, which reads the flags: so the record that was not written by the
        // instruction is written by the jump, on both ways, and must be the same.
        // Two registers; one register twice (where the second operand is lost and the
        // record cannot wait); constants of 8 and of 32 bits; at 32 and at 64 bits.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for op in [0x01u8, 0x09, 0x21, 0x29, 0x31, 0x39, 0x85] {
            for modrm in [0xd8u8, 0xc0, 0xdb] {
                list.push(vec![op, modrm]);
                list.push(vec![0x48, op, modrm]);
                if op != 0x85 {
                    list.push(vec![op | 2, modrm]);
                    list.push(vec![0x48, op | 2, modrm]);
                }
            }
        }
        for field in [0u8, 1, 4, 5, 6, 7] {
            list.push(vec![0x83, 0xc0 | field << 3, 0xff]);
            list.push(vec![0x48, 0x83, 0xc0 | field << 3, 0x7f]);
            list.push(vec![0x81, 0xc0 | field << 3, 0x00, 0x00, 0x00, 0x80]);
            list.push(vec![0x48, 0x81, 0xc3 | field << 3, 0xff, 0xff, 0xff, 0x7f]);
        }
        list.push(vec![0xf7, 0xc0, 0x00, 0x00, 0x00, 0x80]); // test eax, 80000000h
        list.push(vec![0x48, 0xf7, 0xc3, 0x00, 0x00, 0x00, 0x80]); // test rbx, -80000000h
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        for (a, b) in [(0u64, 0u64), (1, u64::MAX), (0x7fff_ffff, 1), (0x8000_0000, 0x8000_0000), (0xffff_ffff, 0xffff_ffff), (1 << 63, 1 << 63), (0x7fff_ffff_ffff_ffff, 0x7fff_ffff_ffff_ffff), (0x1234_5678_9abc_def0, 0x0fed_cba9_8765_4321)] {
            under_every_condition(&list, &[], &|c| {
                c.reg[RAX] = a;
                c.reg[RBX] = b;
            });
        }
        // That it is left: a loop's compare and jump, the way back writing the flags
        // and the way on reading them, has its record on the way on only, so the
        // block is three host instructions shorter than with every record written
        // (written once, where exact writes it before the jump for both ways).
        //  0 again: add edx, 3; 3 cmp edx, ebx; 5 jne again; 7 syscall
        let m = guest(&[0x83, 0xc2, 0x03, 0x39, 0xda, 0x75, 0xf9, 0x0f, 0x05], &[]);
        let words = |exact| translate(&m, CODE, exact).unwrap().words.len();
        // The add's record is left out (dead at the compare), and the compare's is
        // written on the way on only. And since R015 the jump goes to the block's
        // start itself (one way out the fewer: nine instructions), and the constant 3
        // is carried by the ADD (one more): eleven fewer than with every record
        // written and every way a way out.
        assert_eq!(words(true) - words(false), 11);
        // The loop is: add, cmp, the count, the jump back.
        let built = translate(&m, CODE, false).unwrap();
        assert_eq!(built.words[3], a64::b_cond(a64::Cond::Ne, -3));
    }

    #[test]
    fn a_loop_of_one_block_goes_round_in_generated_code() {
        // again: add edx, 3; xor esi, edx; sub ebx, 1; jne again; syscall. The jump
        // back goes to the block's start itself: one entry, and the count is every
        // round's.
        let code = [0x83, 0xc2, 0x03, 0x31, 0xd6, 0x83, 0xeb, 0x01, 0x75, 0xf6, 0x0f, 0x05];
        let rounds = 50_000u64;
        for check in [true, false] {
            let mut m = guest(&code, &[]);
            let mut jit = Jit::new(check, 1).unwrap();
            let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RBX] = rounds);
            assert_eq!(c.reg[RDX], 3 * rounds);
            assert_eq!(c.steps, 4 * rounds + 1);
            assert_eq!(jit.stats.in_blocks, 4 * rounds);
            if !check {
                assert_eq!(jit.stats.entered, 1);
            }
        }
        // (The way back cannot need a record: everything from the block's start to
        // the instruction that set the flags is the block's own, which has no reader
        // of them before its last jump.)
    }

    #[test]
    fn the_hosts_flags_are_made_where_a_jump_asks_them_though_no_record_is() {
        // xor eax, ebx (and or, and sub with a constant); jz two; both ways then write
        // the flags afresh, so no record is written at all: but the jump asks the
        // host's flags, which the host's XOR and OR do not set by themselves.
        //      (op); jz two; add esi, edi; mov edx, 1; jmp end
        // two: add esi, edi; mov edx, 2;  end: syscall
        let tail = [0x74, 0x09, 0x01, 0xfe, 0xba, 1, 0, 0, 0, 0xeb, 0x07, 0x01, 0xfe, 0xba, 2, 0, 0, 0, 0x0f, 0x05];
        for (op, a, b, want) in [
            (&[0x31, 0xd8][..], 5u64, 5u64, 2u64),
            (&[0x31, 0xd8][..], 5, 4, 1),
            (&[0x09, 0xd8][..], 0, 0, 2),
            (&[0x09, 0xd8][..], 0, 8, 1),
            (&[0x48, 0x31, 0xd8][..], 1 << 40, 1 << 40, 2),
            (&[0x83, 0xe8, 0x05][..], 5, 0, 2),
            (&[0x83, 0xf8, 0x05][..], 6, 0, 1),
        ] {
            let mut code = op.to_vec();
            code.extend(tail);
            let (c, _) = checked(&code, &[], &|c| {
                c.reg[RAX] = a;
                c.reg[RBX] = b;
            });
            assert_eq!(c.reg[RDX], want, "{op:02x?} of {a:#x} and {b:#x}");
        }
    }
}
