//! Running a program with the JIT: which addresses have a block, when one is made,
//! what is done when one is left, and the check of every block against the
//! interpreter (ADR 0014, points 2, 7, 8 and 11).

use super::translate::{translate, Built};
use super::{a64, enter, Code, CPU, LEFT_AT_END, LEFT_FOR_THE_INTERPRETER, OUT, REASON, TABLE_BITS};
use crate::cpu::{self, Cpu, Step, Stop};
use std::mem::offset_of;
use crate::mem::{Access, Mem, PAGE};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A block is made when its first address has been arrived at this often (g64m's
/// number; to be measured here), unless another number is asked for.
pub const HOT: u8 = 16;
/// A host page whose blocks have been forgotten this often for a write to it is not
/// translated from again: it holds data that changes beside its code.
const GIVEN_UP: u8 = 8;
/// How much generated code there may be before all of it is forgotten.
const CODE_BYTES: usize = 16 << 20;

/// What is known of an address that has been arrived at.
#[derive(Clone, Copy)]
enum Place {
    /// How often, with no block there yet.
    Heat(u8),
    /// The block that begins there, as its place in `blocks`.
    Block(usize),
    /// Arrived at often enough and no block made: its first instruction is not
    /// translated. The interpreter runs it in a block's stead, and the instruction
    /// after it is where the next block may begin (R016).
    StoodIn,
}

/// A hash of an address that is one multiplication: the map of places is looked in
/// for every arrival, and the standard hash is made to withstand what an address is
/// not (R016).
#[derive(Default)]
struct OfAddress(u64);

impl Hasher for OfAddress {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn write_u64(&mut self, v: u64) {
        self.0 = v.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }

    fn finish(&self) -> u64 {
        self.0 ^ self.0 >> 29
    }
}

struct Block {
    code: *const u8,
    /// For each of its instructions, where its code begins (in host instructions from
    /// the block's start) and its address in the guest.
    starts: Vec<(u32, u64)>,
    /// The host pages of the guest's memory its instructions lie in.
    pages: (u64, u64),
    /// Where its instructions begin and end in the guest.
    guest: (u64, u64),
    /// Whether its host pages are marked as holding translated code, so that a store
    /// to them is caught: only if the guest may write what it was made from (ADR
    /// 0016). Code the guest may not write does not change.
    guarded: bool,
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
    /// What is known of each address arrived at.
    places: HashMap<u64, Place, BuildHasherDefault<OfAddress>>,
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
            places: HashMap::default(),
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
        // A block begins where a jump arrives, and after an instruction that the
        // interpreter runs because no block could begin with it: there the address is
        // looked up.
        let mut arrived = true;
        self.from = 0;
        loop {
            // Is the interpreter about to run an instruction that no block begins with?
            let mut stands_in = false;
            if arrived {
                match self.places.get(&c.rip).copied() {
                    Some(Place::Block(id)) => {
                        // The block before left by a jump to this address: from now on
                        // it comes here by itself. (Not when each block is to be
                        // checked.)
                        if self.from != 0 && !self.check {
                            self.join(self.from, id);
                        }
                        // And generated code that comes to this address by a return
                        // or through an operand finds it in the table from now on.
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
                    }
                    Some(Place::StoodIn) => stands_in = true,
                    known => {
                        let heat = match known {
                            Some(Place::Heat(n)) => n + 1,
                            _ => 1,
                        };
                        if heat < self.hot {
                            self.places.insert(c.rip, Place::Heat(heat));
                        } else if self.make(c.rip, m) {
                            continue;
                        } else {
                            self.places.insert(c.rip, Place::StoodIn);
                            stands_in = true;
                        }
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
                Ok(Step::Next) => arrived = stands_in || c.rip < before || c.rip > before.wrapping_add(15),
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
                        Ok(Step::Next) => arrived = stands_in || c.rip < before || c.rip > before.wrapping_add(15),
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
        if reason == LEFT_FOR_THE_INTERPRETER {
            // RIP is on the instruction; what ran before it is counted.
            return Ok(false);
        }
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
        // If the guest may write any page of what the block rests on, the host pages
        // those lie in may not be written from now, and are counted as holding code
        // (ADR 0016: what the guest may not write needs no guard).
        let pages = m.host_pages(from, to);
        let guarded = m.guest_may_write(from & !(PAGE - 1), to.next_multiple_of(PAGE));
        if guarded {
            if m.mark_code(pages.0, pages.1, true).is_err() {
                return false;
            }
            for page in (from & !(PAGE - 1)..to).step_by(PAGE as usize) {
                *self.holds.entry(page).or_insert(0) += 1;
            }
        }
        self.places.insert(rip, Place::Block(self.blocks.len()));
        self.blocks.push(Block { code, starts, pages, guest: (from, to), guarded, incoming: Vec::new() });
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
        let (table, miss) = (&mut self.table, self.miss);
        self.places.retain(|rip, place| match *place {
            Place::Block(id) => {
                let (from, to) = blocks[id].pages;
                let kept = to <= start || from >= end;
                if !kept {
                    dropped.push(id);
                    // (Gone from the map: to be arrived at as often again before it is
                    // made again.) And not to be found by generated code.
                    let place = &mut table[Self::place(*rip)];
                    if place[0] == *rip {
                        *place = [u64::MAX, miss];
                    }
                }
                kept
            }
            // What was learnt of an address in the page was learnt of code that is
            // being written over.
            _ => *rip < start || *rip >= end,
        });
        for id in dropped {
            for site in std::mem::take(&mut self.blocks[id].incoming) {
                unsafe { self.code.rewrite(site as *const u8, a64::b(1)) };
            }
            let (from, to) = self.blocks[id].guest;
            // (A block that is not guarded was not counted. It goes with the rest of
            // its host page all the same: more than is needed, never less.)
            let counted = if self.blocks[id].guarded { from & !(PAGE - 1)..to } else { 0..0 };
            for page in counted.step_by(PAGE as usize) {
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
        for block in self.blocks.iter().filter(|b| b.guarded) {
            let _ = m.mark_code(block.pages.0, block.pages.1, false);
        }
        self.blocks.clear();
        self.places.clear();
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
    for i in 0..16 {
        let (a, b) = (&want.xmm[2 * i..2 * i + 2], &got.xmm[2 * i..2 * i + 2]);
        if a != b {
            said.push(format!("XMM{i} {:016x}{:016x} -> {:016x}{:016x}", a[1], a[0], b[1], b[0]));
        }
    }
    if want.mxcsr != got.mxcsr {
        said.push(format!("MXCSR {:#x} -> {:#x}", want.mxcsr, got.mxcsr));
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
        // The ways out of the programs' blocks were joined to the blocks they go to.
        assert!(joined.stats.joined > 0);
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

    /// How many of the guest's instructions the block at `CODE` holds, and how many
    /// records of the flags its code writes (an instruction that puts the kind of a
    /// record into x21), with every record written (`exact`) or only those needed.
    fn shape(code: &[u8], exact: bool) -> (usize, usize) {
        let m = guest(code, &[]);
        let built = translate(&m, CODE, exact).expect("a block");
        let records = built.words.iter().filter(|&&w| w & 0xffe0_001f == 0xd280_0015).count();
        (built.starts.len(), records)
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
        // A compare read by its own jump, both ways then writing the flags: no record
        // of it. The block goes on past the jump (R018): four instructions, and one
        // record, the last addition's, which the SYSCALL may read.
        //  0 cmp eax, ebx; 2 jne +2; 4 add ecx, edx (falls to 6); 6 add esi, edi; syscall
        let both_die = [0x39, 0xd8, 0x75, 0x02, 0x01, 0xd1, 0x01, 0xfe, 0x0f, 0x05];
        assert_eq!(shape(&both_die, false), (4, 1));
        // With every record written a block ends at its jump: the compare's.
        assert_eq!(shape(&both_die, true), (2, 1));
        // The same with a way that reads them (jb at the first jump's target, which
        // is another block there and has no flags of the host's): the compare's
        // record on that way; and the addition's, read by the jb when it is come to
        // in the block, on both of the jb's ways (the SYSCALL after may read them).
        //  0 cmp; 2 jne +2; 4 add ecx, edx; 6 jb +0; 8 syscall
        let one_reads = [0x39, 0xd8, 0x75, 0x02, 0x01, 0xd1, 0x72, 0x00, 0x0f, 0x05];
        assert_eq!(shape(&one_reads, false), (4, 3));
        // A jump that the host's flags cannot answer (parity) reads the record, and
        // is the interpreter's: the block is the compare alone, with its record.
        let parity = [0x39, 0xd8, 0x7a, 0x02, 0x01, 0xd1, 0x01, 0xfe, 0x0f, 0x05];
        assert_eq!(shape(&parity, false), (1, 1));
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
            // A division, which tests its operands with the host's flags: the jump
            // after it is not answered by the addition's. (EDX:EAX is 1 after the
            // addition; by 2.)
            &[0xf7, 0xf3][..],
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
        // And answered in the block, no record of the first addition is needed: the
        // block goes on past the jump, and has one record, the second addition's.
        let direct = carry_picked_after(&[]);
        assert_eq!(shape(&direct, false), (5, 1));
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

    #[test]
    fn the_a_register_and_a_constant() {
        // ADD, OR, AND, SUB, XOR, CMP and TEST of AL, AX, EAX and RAX with a
        // constant, in the short forms that name no register.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for op in [0x04u8, 0x0c, 0x24, 0x2c, 0x34, 0x3c, 0xa8] {
            list.push(vec![op, 0x81]);
            list.push(vec![op, 0x7f]);
            list.push(vec![0x66, op | 1, 0x00, 0x80]);
            list.push(vec![op | 1, 0x01, 0x00, 0x00, 0x80]);
            list.push(vec![op | 1, 0xff, 0xff, 0xff, 0x7f]);
            list.push(vec![0x48, op | 1, 0x00, 0x00, 0x00, 0x80]);
            list.push(vec![0x48, op | 1, 0x05, 0x00, 0x00, 0x00]);
        }
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        for a in [0u64, 5, 0x7f, 0x80, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0x7fff_ffff_ffff_ffff, 1 << 63, u64::MAX, 0x1234_5678_8000_7f81] {
            under_every_condition(&list, &[], &|c| c.reg[RAX] = a);
        }
    }

    #[test]
    fn conditions_read_by_setcc_and_cmovcc() {
        // After CMP, ADD and TEST of EAX and EBX (and of RAX and RBX): SETcc of DL, of
        // DH and of a byte of memory, CMOVcc of ECX and RCX from ESI, RSI and memory,
        // for every condition; then the flags are read again by a jump (so that the
        // record, too, is right after them).
        let mut list: Vec<Vec<u8>> = Vec::new();
        for first in [&[0x39u8, 0xd8][..], &[0x01, 0xd8], &[0x85, 0xd8], &[0x48, 0x39, 0xd8], &[0x48, 0x29, 0xd8], &[0x21, 0xd8]] {
            for cc in 0..16u8 {
                for then in [
                    vec![0x0f, 0x90 | cc, 0xc2],             // setcc dl
                    vec![0x0f, 0x90 | cc, 0xc6],             // setcc dh
                    vec![0x0f, 0x90 | cc, 0x46, 0x03],       // setcc byte [rsi + 3]
                    vec![0x0f, 0x40 | cc, 0xcf],             // cmovcc ecx, edi
                    vec![0x48, 0x0f, 0x40 | cc, 0xcf],       // cmovcc rcx, rdi
                    vec![0x0f, 0x40 | cc, 0x0e],             // cmovcc ecx, [rsi]
                    vec![0x48, 0x0f, 0x40 | cc, 0x4e, 0x08], // cmovcc rcx, [rsi + 8]
                ] {
                    let mut v = first.to_vec();
                    v.extend(then);
                    // What was set or moved is brought where the comparison sees it:
                    // mov [rsi + 16], rcx; mov [rsi + 24], rdx.
                    v.extend([0x48, 0x89, 0x4e, 0x10, 0x48, 0x89, 0x56, 0x18]);
                    list.push(v);
                }
            }
        }
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        let data: Vec<u8> = (1..=32).collect();
        for (a, b) in [(0u64, 0u64), (1, 2), (2, 1), (0x8000_0000, 1), (0xffff_ffff, 1), (0x7fff_ffff, 0xffff_ffff), (1 << 63, 1), (u64::MAX, u64::MAX)] {
            under_every_condition(&list, &data, &|c| {
                c.reg[RAX] = a;
                c.reg[RBX] = b;
                c.reg[crate::cpu::RCX] = 0x1111_2222_3333_4444;
                c.reg[RDX] = 0x5555_6666_7777_8888;
                c.reg[7] = 0x9999_aaaa_bbbb_cccc;
                c.reg[RSI] = DATA;
            });
        }
        // That they are in the block where the host's flags answer, and are not where
        // they do not (parity; any condition after a shift).
        let block_of = |code: &[u8]| {
            let m = guest(code, &[]);
            translate(&m, CODE, false).map(|b| b.starts.len())
        };
        assert_eq!(block_of(&[0x39, 0xd8, 0x0f, 0x94, 0xc2, 0x0f, 0x44, 0xcf, 0x0f, 0x05]), Some(3));
        assert_eq!(block_of(&[0x39, 0xd8, 0x0f, 0x9a, 0xc2, 0x0f, 0x05]), Some(1));
        assert_eq!(block_of(&[0xc1, 0xe0, 0x03, 0x0f, 0x94, 0xc2, 0x0f, 0x05]), Some(1));
        assert_eq!(block_of(&[0x0f, 0x94, 0xc2, 0x0f, 0x05]), None);
        // Read by the interpreter, they read the record: so an instruction before
        // them must have written it, though one after them writes the flags again.
        //   add eax, ebx; setp dl (parity: the host has none); add esi, edi; syscall
        //   add eax, ebx; setc dl, as the first instruction of the next block
        let set = |c: &mut Cpu| {
            c.reg[RAX] = 0xffff_ffff;
            c.reg[RBX] = 4;
        };
        let (c, _) = checked(&[0x01, 0xd8, 0x0f, 0x9a, 0xc2, 0x01, 0xfe, 0x0f, 0x05], &[], &set);
        assert_eq!(c.reg[RDX] & 0xff, 1);
        let mut late = [0x89, 0xf7].repeat(63);
        late.extend([0x01, 0xd8, 0x0f, 0x92, 0xc2, 0x01, 0xfe, 0x0f, 0x05]);
        let (c, _) = checked(&late, &[], &set);
        assert_eq!(c.reg[RDX] & 0xff, 1);
    }

    #[test]
    fn imul_and_its_flags() {
        // IMUL of two registers, of a register and memory, and by constants of 8 and
        // 32 bits, at 32 and 64 bits; under every condition.
        let list: &[&[u8]] = &[
            &[0x0f, 0xaf, 0xc3],                         // imul eax, ebx
            &[0x48, 0x0f, 0xaf, 0xc3],                   // imul rax, rbx
            &[0x0f, 0xaf, 0x06],                         // imul eax, [rsi]
            &[0x48, 0x0f, 0xaf, 0x06],                   // imul rax, [rsi]
            &[0x0f, 0xaf, 0xc0],                         // imul eax, eax
            &[0x6b, 0xc3, 0xfd],                         // imul eax, ebx, -3
            &[0x48, 0x6b, 0xc3, 0x7f],                   // imul rax, rbx, 127
            &[0x69, 0xc3, 0x00, 0x00, 0x01, 0x00],       // imul eax, ebx, 10000h
            &[0x48, 0x69, 0xc3, 0xff, 0xff, 0xff, 0x7f], // imul rax, rbx, 7fffffffh
            &[0x6b, 0x06, 0x07],                         // imul eax, [rsi], 7
            &[0x48, 0x69, 0x1e, 0x00, 0x00, 0x00, 0x80], // imul rbx, [rsi], -80000000h
            &[0x6b, 0xc0, 0x00],                         // imul eax, eax, 0
            // After a compare: a jump after the IMUL must not be answered by the
            // compare's flags in the host.
            &[0x39, 0xd8, 0x0f, 0xaf, 0xc3],
        ];
        for (a, b) in [(0u64, 0u64), (3, 5), (0xffff_ffff, 2), (0x1_0000, 0x1_0000), (0x8000_0000, 0xffff_ffff), (u64::MAX, u64::MAX), (1 << 62, 2), (1 << 63, u64::MAX), (0x7fff_ffff, 0x7fff_ffff), (0xb504_f333, 0xb504_f334)] {
            under_every_condition(list, &b.to_le_bytes(), &|c| {
                c.reg[RAX] = a;
                c.reg[RBX] = b;
                c.reg[RSI] = DATA;
            });
        }
    }

    #[test]
    fn a_block_begins_after_what_the_interpreter_ran_in_its_stead() {
        // again: add edx, 1; cpuid-free stand-in: add dx, 0x10 (16 bits with a
        // constant of 16 is translated now; use what is not: sahf, 9E); add edx, 2;
        // sub ebx, 1; jne again; syscall. SAHF is the interpreter's; what follows it
        // was the interpreter's too, up to the jump, until a block could begin there.
        //  0 again: add edx, 1      83 c2 01
        //  3        sahf            9e
        //  4        add edx, 2      83 c2 02
        //  7        sub ebx, 1      83 eb 01
        // 10        jne again       75 f4
        // 12        syscall
        let code = [0x83, 0xc2, 0x01, 0x9e, 0x83, 0xc2, 0x02, 0x83, 0xeb, 0x01, 0x75, 0xf4, 0x0f, 0x05];
        let rounds = 1000u64;
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 4).unwrap();
        let c = both(&mut jit, &mut m, CODE, &|c| c.reg[RBX] = rounds);
        assert_eq!(c.reg[RDX], 3 * rounds);
        assert_eq!(c.steps, 5 * rounds + 1);
        // All but SAHF itself ran in blocks, once the places had been arrived at four
        // times each (and four more for the place after SAHF, counted from when SAHF
        // was found to have no block).
        assert!(jit.stats.in_blocks >= 4 * (rounds - 12), "{} in blocks", jit.stats.in_blocks);
        assert!(jit.stats.in_blocks <= 4 * rounds);
        assert_eq!(jit.stats.blocks, 2);
        // And no block is made of what is arrived at fewer times than asked: the same
        // program for three rounds, with four arrivals asked.
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 4).unwrap();
        both(&mut jit, &mut m, CODE, &|c| c.reg[RBX] = 3);
        assert_eq!((jit.stats.blocks, jit.stats.in_blocks), (0, 0));
    }

    #[test]
    fn additions_and_subtractions_with_the_carry() {
        // ADC and SBB of ECX and EDX (RCX and RDX), of memory and with constants,
        // each after an instruction that leaves a carry the host's flags can say:
        // ADD, SUB, CMP of EAX and EBX, and AND (which leaves none). Under every
        // condition, so that the flags ADC and SBB themselves leave are read.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for first in [&[0x01u8, 0xd8][..], &[0x29, 0xd8], &[0x39, 0xd8], &[0x21, 0xd8], &[0x48, 0x01, 0xd8], &[0x48, 0x39, 0xd8]] {
            for then in [
                &[0x11u8, 0xf9][..],                    // adc ecx, edi
                &[0x19, 0xf9],                          // sbb ecx, edi
                &[0x48, 0x11, 0xf9],                    // adc rcx, rdi
                &[0x48, 0x19, 0xf9],                    // sbb rcx, rdi
                &[0x13, 0x0e],                          // adc ecx, [rsi]
                &[0x48, 0x1b, 0x0e],                    // sbb rcx, [rsi]
                &[0x11, 0x3e],                          // adc [rsi], edi
                &[0x48, 0x19, 0x7e, 0x08],              // sbb [rsi + 8], rdi
                &[0x83, 0xd1, 0xff],                    // adc ecx, -1
                &[0x48, 0x83, 0xd9, 0x00],              // sbb rcx, 0
                &[0x48, 0x83, 0xd1, 0x00],              // adc rcx, 0
                &[0x81, 0xd9, 0xff, 0xff, 0xff, 0x7f],  // sbb ecx, 7fffffffh
                &[0x15, 0x05, 0x00, 0x00, 0x00],        // adc eax, 5
                &[0x48, 0x1d, 0x05, 0x00, 0x00, 0x00],  // sbb rax, 5
                &[0x11, 0xc9],                          // adc ecx, ecx
                &[0x48, 0x19, 0xc9],                    // sbb rcx, rcx
                &[0x11, 0xf9, 0x11, 0xfa],              // adc ecx, edi; adc edx, edi (a chain)
            ] {
                let mut v = first.to_vec();
                v.extend(then);
                // What memory came to is brought where the comparison sees it.
                v.extend([0x4c, 0x8b, 0x06, 0x4c, 0x8b, 0x4e, 0x08]);
                list.push(v);
            }
        }
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        let data = [0xffu8, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        for (a, b, c, d) in [
            (1u64, 2u64, 0u64, 0u64),
            (0xffff_ffff, 1, 0xffff_ffff, 0),
            (0xffff_ffff, 1, 0x7fff_ffff, 0x8000_0000),
            (0, 1, 0, 0xffff_ffff),
            (u64::MAX, 1, u64::MAX, 0),
            (0, 1, 1 << 63, u64::MAX),
            (5, 5, 0x7fff_ffff_ffff_ffff, 0),
            (0x8000_0000, 0x8000_0000, 0x1234_5678, 0xedcb_a987),
        ] {
            under_every_condition(&list, &data, &|cpu| {
                cpu.reg[RAX] = a;
                cpu.reg[RBX] = b;
                cpu.reg[crate::cpu::RCX] = c;
                cpu.reg[7] = d;
                cpu.reg[RSI] = DATA;
            });
        }
        // Where the host's flags do not say what the carry is, it is the
        // interpreter's: at a block's start, and after a shift.
        let block_of = |code: &[u8]| {
            let m = guest(code, &[]);
            translate(&m, CODE, false).map(|b| b.starts.len())
        };
        assert_eq!(block_of(&[0x11, 0xf9, 0x0f, 0x05]), None);
        assert_eq!(block_of(&[0xc1, 0xe0, 0x03, 0x11, 0xf9, 0x0f, 0x05]), Some(1));
        assert_eq!(block_of(&[0x01, 0xd8, 0x11, 0xf9, 0x0f, 0x05]), Some(2));
        // And there it reads the record, which the instruction before must then have
        // written: an addition that is a block's last instruction, the ADC the next
        // block's first.
        let mut late = [0x89, 0xf7].repeat(63);
        late.extend([0x01, 0xd8, 0x11, 0xf9, 0x0f, 0x05]);
        let (c, _) = checked(&late, &[], &|c| {
            c.reg[RAX] = 0xffff_ffff;
            c.reg[RBX] = 1;
            c.reg[crate::cpu::RCX] = 10;
            c.reg[RSI] = 20;
        });
        // (The moves made EDI of ESI.) 10 + 20 + the carry is 31, of odd parity; RCX
        // itself is the SYSCALL's by now, and the flags are what it left in R11.
        assert_eq!(c.reg[11] & 4, 0);
    }

    #[test]
    fn multiplication_and_division_with_one_operand() {
        // MUL and IMUL of EBX, RBX and memory; under every condition (their flags).
        let muls: &[&[u8]] = &[
            &[0xf7, 0xe3],
            &[0xf7, 0xeb],
            &[0x48, 0xf7, 0xe3],
            &[0x48, 0xf7, 0xeb],
            &[0xf7, 0x26],
            &[0x48, 0xf7, 0x2e],
            &[0x48, 0xf7, 0xe0], // mul rax
            &[0xf7, 0xea],       // imul edx
            // After a compare: a jump after the MUL is not answered by the compare.
            &[0x39, 0xd8, 0xf7, 0xe3],
        ];
        for (a, b, d) in [(0u64, 0u64, 7u64), (3, 5, 0), (0xffff_ffff, 0xffff_ffff, 1), (u64::MAX, u64::MAX, 2), (1 << 63, 2, 3), (0x1_0000_0001, 0xffff_ffff_0000_0001, 0x8000_0000), (0x7fff_ffff, 0x7fff_ffff, u64::MAX), (0x8000_0000, 0xffff_ffff, 5)] {
            under_every_condition(muls, &b.to_le_bytes(), &|c| {
                c.reg[RAX] = a;
                c.reg[RBX] = b;
                c.reg[RDX] = d;
                c.reg[RSI] = DATA;
            });
        }
        // DIV and IDIV, where they do not fault: by a register and by memory, the
        // dividend's upper half nothing, the lower's sign, and something else that
        // still leaves a quotient that fits (the interpreter's, come back from).
        let divs: &[&[u8]] = &[
            &[0xf7, 0xf3],       // div ebx
            &[0xf7, 0xfb],       // idiv ebx
            &[0x48, 0xf7, 0xf3], // div rbx
            &[0x48, 0xf7, 0xfb], // idiv rbx
            &[0xf7, 0x36],       // div dword [rsi]
            &[0x48, 0xf7, 0x3e], // idiv qword [rsi]
            // And the flags of a compare before are still there after.
            &[0x39, 0xdf, 0xf7, 0xf3],
            &[0x39, 0xdf, 0x48, 0xf7, 0xfb],
        ];
        for (a, d, b) in [
            (100u64, 0u64, 7u64),
            (0xffff_ffff, 0, 0xffff_ffff),
            (u64::MAX, 0, 1 << 40),
            (5, 0, 0x7fff_ffff),
            // Negative by positive and by negative (the sign in the upper half).
            ((-100i64) as u64, u64::MAX, 7),
            ((-100i64) as u64, u64::MAX, (-7i64) as u64),
            (100, 0, (-7i64) as u64 & 0x7fff_ffff_ffff_ffff),
            // An upper half that is neither nothing nor the sign: 32 bits take it as
            // part of the dividend; 64 bits leave it to the interpreter.
            (0, 1, 0x4000_0000_0000_0000),
            (0x10, 3, 0x7fff_ffff_ffff_ffff),
            (0x8000_0000, 0x3fff_ffff, 0x7fff_ffff_ffff_ffff),
        ] {
            // Only where the interpreter does not fault for any of the forms.
            let fits = |code: &[u8]| {
                let mut whole = code.to_vec();
                whole.extend([0x0f, 0x05]);
                let mut m = guest(&whole, &b.to_le_bytes());
                let mut c = Cpu::new(CODE, DATA + 4096);
                (c.reg[RAX], c.reg[RDX], c.reg[RBX], c.reg[7], c.reg[RSI]) = (a, d, b, 3, DATA);
                loop {
                    match cpu::step(&mut c, &mut m) {
                        Ok(Step::Syscall) => return true,
                        Ok(_) => {}
                        Err(_) => return false,
                    }
                }
            };
            let ok: Vec<&[u8]> = divs.iter().copied().filter(|d| fits(d)).collect();
            assert!(!ok.is_empty(), "no form divides {d:#x}:{a:#x} by {b:#x}");
            under_every_condition(&ok, &b.to_le_bytes(), &|c| {
                c.reg[RAX] = a;
                c.reg[RDX] = d;
                c.reg[RBX] = b;
                c.reg[7] = 3;
                c.reg[RSI] = DATA;
            });
        }
        // Where x86 faults, the block leaves the division to the interpreter, which
        // faults: by zero; a quotient too large; the most negative number by -1.
        // The instruction before is done and counted, the division is not.
        for (code, a, d, b) in [
            (&[0x83u8, 0xc7, 0x01, 0xf7, 0xf3, 0x0f, 0x05][..], 5u64, 0u64, 0u64),
            (&[0x83, 0xc7, 0x01, 0x48, 0xf7, 0xfb, 0x0f, 0x05], 5, 0, 0),
            (&[0x83, 0xc7, 0x01, 0xf7, 0xf3, 0x0f, 0x05], 0, 5, 5),
            (&[0x83, 0xc7, 0x01, 0x48, 0xf7, 0xf3, 0x0f, 0x05], 0, 5, 5),
            (&[0x83, 0xc7, 0x01, 0xf7, 0xfb, 0x0f, 0x05], 0x8000_0000, 0xffff_ffff, 0xffff_ffff),
            (&[0x83, 0xc7, 0x01, 0x48, 0xf7, 0xfb, 0x0f, 0x05], 1 << 63, u64::MAX, u64::MAX),
        ] {
            let mut m = guest(code, &[]);
            let mut jit = Jit::new(false, 1).unwrap();
            let mut c = Cpu::new(CODE, DATA + 4096);
            (c.reg[RAX], c.reg[RDX], c.reg[RBX]) = (a, d, b);
            let Event::Stop(stop) = jit.run(&mut c, &mut m) else { panic!("{code:02x?} went on") };
            assert_eq!(stop, Stop::Divide, "{code:02x?}");
            assert_eq!((c.rip, c.steps, c.reg[7], c.reg[RAX], c.reg[RDX]), (CODE + 3, 1, 1, a, d));
        }
    }

    #[test]
    fn small_shifts_second_bytes_and_bswap() {
        // SHL, SHR, SAR (and SAL) of AL, AH, AX by constants and by one, under every
        // condition.
        let mut list: Vec<Vec<u8>> = Vec::new();
        for field in [4u8, 5, 6, 7] {
            for n in [0u8, 1, 3, 7, 8, 9, 15, 16, 17, 31, 32, 33, 0xff] {
                list.push(vec![0xc0, 0xc0 | field << 3, n]); // al
                list.push(vec![0xc0, 0xc4 | field << 3, n]); // ah
                list.push(vec![0x66, 0xc1, 0xc0 | field << 3, n]); // ax
            }
            list.push(vec![0xd0, 0xc0 | field << 3]);
            list.push(vec![0xd0, 0xc4 | field << 3]);
            list.push(vec![0x66, 0xd1, 0xc0 | field << 3]);
        }
        let list: Vec<&[u8]> = list.iter().map(|v| v.as_slice()).collect();
        for a in [0u64, 0x1234_5678_9abc_8001, 0xffff_ffff_ffff_7f80, 0x80ff, 0x4040, 0x01fe] {
            under_every_condition(&list, &[], &|c| c.reg[RAX] = a);
        }
        // That they are in blocks, and not the interpreter's doing what is compared:
        // shr dl, 3; sar ax, 7; shl ah, 1, each with the SYSCALL after it not.
        for code in [&[0xc0u8, 0xea, 0x03, 0x0f, 0x05][..], &[0x66, 0xc1, 0xf8, 0x07, 0x0f, 0x05], &[0xd0, 0xe4, 0x0f, 0x05]] {
            let m = guest(code, &[]);
            assert_eq!(translate(&m, CODE, false).map(|b| b.starts.len()), Some(1), "{code:02x?}");
        }
        // MOVZX and MOVSX from AH, CH, DH, BH; BSWAP of 32 and of 64 bits.
        let set = |c: &mut Cpu| {
            c.reg[RAX] = 0x1122_3344_5566_8f88;
            c.reg[RBX] = 0x0102_0304_0506_7f08;
        };
        let run = |code: &[u8]| {
            let mut whole = code.to_vec();
            whole.extend([0x0f, 0x05]);
            checked(&whole, &[], &set).0.reg[RDX]
        };
        assert_eq!(run(&[0x0f, 0xb6, 0xd4]), 0x8f); // movzx edx, ah
        assert_eq!(run(&[0x0f, 0xb6, 0xd7]), 0x7f); // movzx edx, bh
        assert_eq!(run(&[0x0f, 0xbe, 0xd4]), 0xffff_ff8f); // movsx edx, ah
        assert_eq!(run(&[0x0f, 0xbe, 0xd7]), 0x7f); // movsx edx, bh
        assert_eq!(run(&[0x66, 0x90, 0x89, 0xc2, 0x0f, 0xca]), 0x888f_6655); // mov edx, eax; bswap edx
        assert_eq!(run(&[0x48, 0x89, 0xc2, 0x48, 0x0f, 0xca]), 0x888f_6655_4433_2211); // bswap rdx
        assert_eq!(run(&[0x48, 0x89, 0xda, 0x49, 0x89, 0xd0, 0x49, 0x0f, 0xc8, 0x4c, 0x89, 0xc2]), 0x087f_0605_0403_0201); // bswap r8
    }

    #[test]
    fn a_block_goes_on_past_conditional_jumps() {
        // Two jumps on one compare, every pair of conditions:
        //      cmp eax, ebx; jcc1 one; jcc2 two; mov edx, 1; jmp end
        // one: mov edx, 2; jmp end
        // two: mov edx, 3
        // end: syscall
        // Whichever way is gone, RDX, the flags (read by the SYSCALL) and the count of
        // instructions must be the interpreter's.
        let mut code = Vec::new();
        let mut programs = Vec::new();
        for first in [&[0x39u8, 0xd8][..], &[0x48, 0x39, 0xd8], &[0x01, 0xd8], &[0x85, 0xd8], &[0x83, 0xe8, 0x01]] {
            for c1 in 0..16u8 {
                for c2 in 0..16u8 {
                    programs.push(CODE + code.len() as u64);
                    code.extend(first);
                    code.extend([0x70 | c1, 9, 0x70 | c2, 14, 0xba, 1, 0, 0, 0, 0xeb, 12, 0xba, 2, 0, 0, 0, 0xeb, 5, 0xba, 3, 0, 0, 0, 0x0f, 0x05]);
                }
            }
        }
        let mut seen = [0u32; 4];
        for check in [true, false] {
            let mut m = guest(&code, &[]);
            let mut jit = Jit::new(check, 1).unwrap();
            for (a, b) in [(1u64, 1u64), (1, 2), (2, 1), (0x8000_0000, 1), (0, 0xffff_ffff), (0x7fff_ffff, 0x8000_0000)] {
                for &rip in &programs {
                    let c = both(&mut jit, &mut m, rip, &|c| {
                        c.reg[RAX] = a;
                        c.reg[RBX] = b;
                    });
                    seen[c.reg[RDX] as usize] += 1;
                }
            }
        }
        // All three ends were come to, often.
        assert!(seen[1] > 500 && seen[2] > 500 && seen[3] > 500, "{seen:?}");
        // The block is the compare, both jumps, the move and the jump to the end:
        // five instructions, where it was two.
        assert_eq!(shape(&code[..27], false).0, 5);
        assert_eq!(shape(&code[..27], true).0, 2);
    }

    #[test]
    fn what_comes_after_a_jump_gone_past_is_counted_and_faults_where_it_is() {
        //  0 again: add edx, 1
        //  3        cmp edx, 5
        //  6        je out            (taken once, the fifth time round)
        //  8        mov [rsi], edx    (a store: to nowhere the third time round)
        // 10        sub ebx, 1
        // 13        jne again
        // 15        syscall
        // 17 out:   mov eax, 7; syscall
        let code = [0x83, 0xc2, 0x01, 0x83, 0xfa, 0x05, 0x74, 0x09, 0x89, 0x16, 0x83, 0xeb, 0x01, 0x75, 0xf1, 0x0f, 0x05, 0xb8, 7, 0, 0, 0, 0x0f, 0x05];
        // Taken the fifth time: four whole rounds of six, then three, and two at `out`.
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, &|c| {
            c.reg[RBX] = 100;
            c.reg[RSI] = DATA;
        });
        assert_eq!((c.reg[RAX], c.reg[RDX], c.steps), (7, 5, 4 * 6 + 3 + 2));
        assert_eq!(m.read_u32(DATA), Ok(4));
        // Never taken: three rounds of six, and the SYSCALL.
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, &|c| {
            c.reg[RBX] = 3;
            c.reg[RSI] = DATA;
        });
        assert_eq!((c.reg[RDX], c.steps), (3, 19));
        // A fault after the jump that was gone past: RIP on the store, the three
        // instructions before it counted, the flags the compare's.
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        c.reg[RBX] = 100;
        c.reg[RSI] = 0x90_0000;
        let Event::Stop(stop) = jit.run(&mut c, &mut m) else { panic!("it went on") };
        assert_eq!(stop, Stop::Fault(crate::mem::Fault { addr: 0x90_0000, access: Access::Write }));
        assert_eq!((c.rip, c.steps, c.reg[RDX]), (CODE + 8, 3, 1));
    }

    #[test]
    fn a_jump_after_a_loops_own_jump_is_another_blocks() {
        // again: add edx, 1; cmp edx, ebx; jne again; je two; add esi, edi; mov edx, 1;
        // jmp end; two: add esi, edi; mov edx, 2; end: syscall
        // The loop's jump goes to its own block's start and ends the block. The jump
        // after it is the next block's first, has no flags of the host's, and reads
        // the compare's record: which must be there, though both of that jump's ways
        // write the flags again. (Equal when the loop is left: `two`.)
        let code = [0x83, 0xc2, 0x01, 0x39, 0xda, 0x75, 0xf9, 0x74, 0x09, 0x01, 0xfe, 0xba, 1, 0, 0, 0, 0xeb, 0x07, 0x01, 0xfe, 0xba, 2, 0, 0, 0, 0x0f, 0x05];
        let (c, _) = checked(&code, &[], &|c| c.reg[RBX] = 40);
        assert_eq!(c.reg[RDX], 2);
        assert_eq!(shape(&code, false).0, 3);
    }
    // ---- SSE in blocks (ADR 0015) ----------------------------------------------------------

    /// R019's operands as doubles: numbers, zeros of both signs, infinities, two that
    /// are not numbers and quiet, two that signal, and what does not fit an integer.
    fn doubles() -> Vec<u64> {
        let mut v: Vec<u64> = [
            1.0f64, 2.0, 0.0, -0.0, -1.0, 0.1, 0.3, 3e9, -3e9, 1e19, -1e19, 2147483647.0, 2147483647.5, 2147483648.0,
            -2147483648.0, -2147483648.9, -2147483649.0, 9.3e18, 5e-324, f64::MAX, f64::INFINITY, f64::NEG_INFINITY,
        ]
        .iter()
        .map(|d| d.to_bits())
        .collect();
        v.extend([0x7ff8_0000_0000_0001, 0x7ff8_0000_0000_0002, 0x7ff0_0000_0000_0001, 0xfff0_0000_0000_0002]);
        v
    }

    /// And as singles, in the low half of a register whose upper half is something.
    fn singles() -> Vec<u64> {
        let mut v: Vec<u64> = [
            1.0f32, 2.0, 0.0, -0.0, -1.0, 0.1, 0.3, 3e9, -3e9, 1e19, -1e19, 2147483520.0, 2147483648.0, -2147483648.0,
            16777216.0, 1e-45, f32::MAX, f32::INFINITY, f32::NEG_INFINITY,
        ]
        .iter()
        .map(|f| f.to_bits() as u64)
        .collect();
        v.extend([0x7fc0_0001, 0x7fc0_0002, 0x7f80_0001, 0xff80_0002]);
        v.iter().map(|x| x | 0xabcd_ef01 << 32).collect()
    }

    /// Runs each of `list` (an instruction or a few, of which `.1` are to be in its
    /// block; SYSCALL is added) from each of `cases` beginnings, by blocks each
    /// checked and by blocks joined, held to the interpreter. `data(case)` begins the
    /// page at `DATA`. How many blocks were run to their ends and checked comes back.
    fn sse(list: &[(Vec<u8>, usize)], cases: usize, data: &dyn Fn(usize) -> Vec<u8>, set: &dyn Fn(usize, &mut Cpu)) -> u64 {
        let mut code = Vec::new();
        let mut programs = Vec::new();
        for (ins, n) in list {
            let whole = [&ins[..], &[0x0f, 0x05]].concat();
            assert_eq!(shape(&whole, true).0, *n, "{ins:02x?} is not all in a block");
            programs.push(CODE + code.len() as u64);
            code.extend(whole);
        }
        let mut checked = 0;
        for check in [true, false] {
            let mut m = guest(&code, &[]);
            let mut jit = Jit::new(check, 1).unwrap();
            for case in 0..cases {
                for &rip in &programs {
                    m.write_bytes(DATA, &data(case)).unwrap();
                    both(&mut jit, &mut m, rip, &|c| set(case, c));
                }
            }
            if check {
                checked = jit.stats.checked;
            }
        }
        checked
    }

    /// Every XMM register something of its own, so that a wrong one shows.
    fn fill_xmm(c: &mut Cpu) {
        for i in 0..32 {
            c.xmm[i] = 0x0101_0101_0101_0101 * (i as u64 + 1) ^ 0x8000_0000_0000_00ff << (i % 7);
        }
    }

    #[test]
    fn the_arithmetic_of_floats_is_the_interpreters_whatever_the_operands() {
        // op xmm1, xmm2 and op xmm1, [rsi]: the root, the sum, the product, the
        // difference, the smaller, the quotient, the larger; of doubles and of singles.
        for (pre, values) in [(0xf2u8, doubles()), (0xf3, singles())] {
            let mut list = Vec::new();
            for op in [0x51u8, 0x58, 0x59, 0x5c, 0x5d, 0x5e, 0x5f] {
                list.push((vec![pre, 0x0f, op, 0xca], 1));
                list.push((vec![pre, 0x0f, op, 0x0e], 1));
            }
            let n = values.len();
            let checked = sse(&list, n * n, &|case| values[case % n].to_le_bytes().to_vec(), &|case, c| {
                fill_xmm(c);
                c.xmm[2] = values[case / n];
                c.xmm[4] = values[case % n];
                c.reg[RSI] = DATA;
            });
            // Most of them were the block's from end to end: what is a number.
            assert!(checked as usize > list.len() * n * n / 2, "{checked} of {}", list.len() * n * n);
        }
    }

    #[test]
    fn a_result_that_is_not_a_number_is_left_to_the_interpreter() {
        // subsd xmm1, xmm2; syscall
        let code = [0xf2, 0x0f, 0x5c, 0xca, 0x0f, 0x05];
        for (a, b, want, in_block) in [
            (3.0f64.to_bits(), 1.0f64.to_bits(), 2.0f64.to_bits(), 1),
            // Infinity less infinity: x86's has the sign set, the host's has not.
            (f64::INFINITY.to_bits(), f64::INFINITY.to_bits(), 0xfff8_0000_0000_0000, 0),
            // Quiet and signalling: x86 gives the first, the host the one that signals.
            (0x7ff8_0000_0000_0001, 0xfff0_0000_0000_0002, 0x7ff8_0000_0000_0001, 0),
        ] {
            let mut m = guest(&code, &[]);
            let mut jit = Jit::new(true, 1).unwrap();
            let c = both(&mut jit, &mut m, CODE, &|c| {
                c.xmm[2] = a;
                c.xmm[3] = 7;
                c.xmm[4] = b;
            });
            assert_eq!((c.xmm[2], c.xmm[3], c.steps), (want, 7, 2));
            // The block was entered either way; it ran to its end only for the number.
            assert_eq!((jit.stats.entered, jit.stats.checked, jit.stats.in_blocks), (1, in_block, in_block));
        }
    }

    #[test]
    fn comparisons_of_floats_under_every_condition() {
        // ucomisd, comisd, ucomiss, comiss of xmm1 with xmm2 and with [rsi]; and
        // with XMM8 and XMM9.
        let d = |x: f64| x.to_bits();
        let f = |x: f32| x.to_bits() as u64 | 0x5555 << 32;
        let double: [&[u8]; 5] = [&[0x66, 0x0f, 0x2e, 0xca], &[0x66, 0x0f, 0x2f, 0xca], &[0x66, 0x0f, 0x2e, 0x0e], &[0x66, 0x0f, 0x2f, 0x0e], &[0x66, 0x45, 0x0f, 0x2e, 0xc1]];
        let single: [&[u8]; 5] = [&[0x0f, 0x2e, 0xca], &[0x0f, 0x2f, 0xca], &[0x0f, 0x2e, 0x0e], &[0x0f, 0x2f, 0x0e], &[0x45, 0x0f, 0x2e, 0xc1]];
        let nan = 0x7ff8_0000_0000_0000u64;
        let pairs = [
            (d(1.0), d(2.0)), (d(2.0), d(1.0)), (d(1.5), d(1.5)), (d(0.0), d(-0.0)), (d(-1.0), d(1.0)),
            (d(f64::INFINITY), d(1.0)), (nan, d(1.0)), (d(1.0), nan), (nan, nan), (0x7ff0_0000_0000_0001, d(0.0)),
        ];
        let nan = 0x7fc0_0000u64;
        let small = [
            (f(1.0), f(2.0)), (f(2.0), f(1.0)), (f(1.5), f(1.5)), (f(0.0), f(-0.0)), (f(-1.0), f(1.0)),
            (f(f32::INFINITY), f(1.0)), (nan, f(1.0)), (f(1.0), nan), (nan, nan), (0x7f80_0001, f(0.0)),
        ];
        for (list, pairs) in [(&double, &pairs), (&single, &small)] {
            for &(a, b) in pairs.iter() {
                under_every_condition(&list[..], &b.to_le_bytes(), &|c| {
                    fill_xmm(c);
                    (c.xmm[2], c.xmm[4], c.xmm[16], c.xmm[18]) = (a, b, a, b);
                    c.reg[RSI] = DATA;
                });
            }
        }
        // What each comes to, said outright for a double (2: the jump is taken):
        // above, below, equal, and not numbers (which is below, equal and odd at once).
        for (a, b, ja, jb, je, jp) in [(2.0, 1.0, 2, 1, 1, 1), (1.0, 2.0, 1, 2, 1, 1), (1.0, 1.0, 1, 1, 2, 1), (f64::NAN, 1.0, 1, 2, 2, 2)] {
            for (cc, want) in [(7u8, ja), (2, jb), (4, je), (10, jp)] {
                let code = [&[0x66, 0x0f, 0x2e, 0xca][..], &pick(cc)].concat();
                let (c, _) = checked(&code, &[], &|c| (c.xmm[2], c.xmm[4]) = (d(a), d(b)));
                assert_eq!(c.reg[RDX], want, "{a} and {b}, condition {cc}");
            }
        }
    }

    #[test]
    fn a_comparison_of_floats_read_by_setcc_cmovcc_and_adc() {
        // ucomisd xmm1, xmm2; then setcc dl / cmovcc edx, ebx / adc edx, 0
        let mut list = Vec::new();
        for cc in 0..16u8 {
            list.push(([&[0x66, 0x0f, 0x2e, 0xca][..], &[0x0f, 0x90 | cc, 0xc2]].concat(), 2));
            list.push(([&[0x66, 0x0f, 0x2e, 0xca][..], &[0x0f, 0x40 | cc, 0xd3]].concat(), 2));
        }
        list.push((vec![0x66, 0x0f, 0x2e, 0xca, 0x83, 0xd2, 0x00], 2));
        let pairs = [(1.0f64, 2.0f64), (2.0, 1.0), (1.0, 1.0), (f64::NAN, 1.0), (1.0, f64::NAN)];
        sse(&list, pairs.len(), &|_| Vec::new(), &|case, c| {
            (c.xmm[2], c.xmm[4]) = (pairs[case].0.to_bits(), pairs[case].1.to_bits());
            c.reg[RBX] = 0x77;
            c.reg[RDX] = 0x1100;
        });
    }

    #[test]
    fn the_hosts_flags_are_not_asked_after_arithmetic_of_floats_spent_them() {
        // cmp eax, ebx; <SSE>; then every condition. A move leaves the host's flags as
        // the compare made them; arithmetic, the smaller, and the conversions that
        // test their result do not, and the jump reads the record.
        let list: [&[u8]; 6] = [
            &[0x39, 0xd8, 0xf2, 0x0f, 0x10, 0xca],       // movsd xmm1, xmm2
            &[0x39, 0xd8, 0xf2, 0x0f, 0x58, 0xca],       // addsd xmm1, xmm2
            &[0x39, 0xd8, 0xf2, 0x0f, 0x5d, 0xca],       // minsd xmm1, xmm2
            &[0x39, 0xd8, 0xf2, 0x0f, 0x2c, 0xca],       // cvttsd2si ecx, xmm2
            &[0x39, 0xd8, 0xf2, 0x48, 0x0f, 0x2c, 0xca], // cvttsd2si rcx, xmm2
            &[0x39, 0xd8, 0xf2, 0x0f, 0x5a, 0xca],       // cvtsd2ss xmm1, xmm2
        ];
        for (a, b) in [(1u64, 2u64), (2, 1), (5, 5), (0x8000_0000, 1), (0x7fff_ffff, 0xffff_ffff)] {
            under_every_condition(&list, &[], &|c| {
                (c.reg[RAX], c.reg[RBX]) = (a, b);
                (c.xmm[2], c.xmm[4]) = (1.5f64.to_bits(), 2.5f64.to_bits());
            });
        }
        // The jump is in the compare's block after the move, and not after the
        // others: it is the interpreter's there, which reads the record.
        for (i, ins) in list.into_iter().enumerate() {
            assert_eq!(shape(&[ins, &pick(2)[..]].concat(), true).0, if i == 0 { 3 } else { 2 });
        }
    }

    #[test]
    fn a_comparison_of_floats_whose_flags_only_a_jump_reads_writes_no_record() {
        // ucomisd xmm1, xmm2; ja over; add edx, 1; over: add edx, 2; syscall
        let code = [0x66, 0x0f, 0x2e, 0xca, 0x77, 0x03, 0x83, 0xc2, 0x01, 0x83, 0xc2, 0x02, 0x0f, 0x05];
        // (MOVZ x21 with the kind of a comparison of doubles.)
        let record = a64::movz(true, 21, (cpu::REC_FLOAT << 8 | 64) as u16, 0);
        let has = |exact| translate(&guest(&code, &[]), CODE, exact).unwrap().words.contains(&record);
        assert!(has(true) && !has(false));
        for (a, b, want) in [(2.0f64, 1.0f64, 2), (1.0, 2.0, 3), (f64::NAN, 1.0, 3)] {
            let (c, _) = checked(&code, &[], &|c| (c.xmm[2], c.xmm[4]) = (a.to_bits(), b.to_bits()));
            assert_eq!(c.reg[RDX], want);
        }
    }

    #[test]
    fn integers_to_floats_and_back() {
        // cvtsi2sd and cvtsi2ss xmm1 from eax, rax, [rsi] of 32 and of 64 bits.
        let mut list = Vec::new();
        for pre in [0xf2u8, 0xf3] {
            list.push((vec![pre, 0x0f, 0x2a, 0xc8], 1));
            list.push((vec![pre, 0x48, 0x0f, 0x2a, 0xc8], 1));
            list.push((vec![pre, 0x0f, 0x2a, 0x0e], 1));
            list.push((vec![pre, 0x48, 0x0f, 0x2a, 0x0e], 1));
        }
        // What is exact, and what must be rounded (to 53 bits, to 24).
        let ints = [0u64, 1, u64::MAX, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff_8000_0000, i64::MAX as u64, 1 << 63, 0x20_0000_0000_0001, 0x20_0000_0000_0003, 16_777_217, 16_777_219, 0x1234_5678_9abc_def0];
        let n = sse(&list, ints.len(), &|case| ints[case].to_le_bytes().to_vec(), &|case, c| {
            fill_xmm(c);
            c.reg[RAX] = ints[case];
            c.reg[RSI] = DATA;
        });
        assert_eq!(n as usize, list.len() * ints.len());

        // cvttsd2si and cvttss2si to eax and to rax, from xmm1 and from [rsi]: every
        // one in its block to the end, what does not fit and what is not a number too.
        for (pre, values) in [(0xf2u8, doubles()), (0xf3, singles())] {
            let list = [
                (vec![pre, 0x0f, 0x2c, 0xc1], 1),
                (vec![pre, 0x48, 0x0f, 0x2c, 0xc1], 1),
                (vec![pre, 0x0f, 0x2c, 0x06], 1),
                (vec![pre, 0x48, 0x0f, 0x2c, 0x06], 1),
                (vec![pre, 0x4c, 0x0f, 0x2c, 0xf9], 1), // to r15
            ];
            let n = sse(&list, values.len(), &|case| values[case].to_le_bytes().to_vec(), &|case, c| {
                c.xmm[2] = values[case];
                c.reg[RAX] = 0x1111_2222_3333_4444;
                c.reg[RSI] = DATA;
            });
            assert_eq!(n as usize, list.len() * values.len());
        }
        // Said outright: what x86 gives where the host would give something else.
        for (v, want32, want64) in [
            (3e9f64, 0x8000_0000u64, 3_000_000_000u64),
            (-3e9, 0x8000_0000, -3_000_000_000i64 as u64),
            (1e19, 0x8000_0000, 1 << 63),
            (f64::NAN, 0x8000_0000, 1 << 63),
            (2147483647.5, 0x7fff_ffff, 0x7fff_ffff),
            (-1.9, 0xffff_ffff, u64::MAX),
        ] {
            // cvttsd2si eax, xmm1; cvttsd2si rbx, xmm1; syscall
            let (c, _) = checked(&[0xf2, 0x0f, 0x2c, 0xc1, 0xf2, 0x48, 0x0f, 0x2c, 0xd9, 0x0f, 0x05], &[], &|c| {
                c.xmm[2] = v.to_bits();
                c.reg[RAX] = u64::MAX;
            });
            assert_eq!((c.reg[RAX], c.reg[RBX]), (want32, want64), "{v}");
        }
    }

    #[test]
    fn between_singles_and_doubles() {
        // cvtsd2ss and cvtss2sd xmm1 from xmm2 and from [rsi].
        for (pre, values) in [(0xf2u8, doubles()), (0xf3, singles())] {
            let list = [(vec![pre, 0x0f, 0x5a, 0xca], 1), (vec![pre, 0x0f, 0x5a, 0x0e], 1), (vec![pre, 0x45, 0x0f, 0x5a, 0xf8], 1)];
            sse(&list, values.len(), &|case| values[case].to_le_bytes().to_vec(), &|case, c| {
                fill_xmm(c);
                (c.xmm[4], c.xmm[16]) = (values[case], values[case]);
                c.reg[RSI] = DATA;
            });
        }
    }

    #[test]
    fn moves_and_logic_of_the_xmm_registers() {
        let list: Vec<(Vec<u8>, usize)> = [
            // movsd and movss: register to register (the rest kept), from memory (the
            // rest zero), to memory; by both opcodes.
            &[0xf2u8, 0x0f, 0x10, 0xca][..], &[0xf2, 0x0f, 0x10, 0x0e], &[0xf2, 0x0f, 0x11, 0x0e], &[0xf2, 0x0f, 0x11, 0xca],
            &[0xf3, 0x0f, 0x10, 0xca], &[0xf3, 0x0f, 0x10, 0x0e], &[0xf3, 0x0f, 0x11, 0x0e], &[0xf3, 0x0f, 0x11, 0xca],
            // movups, movupd, movaps, movapd, movdqa, movdqu: each way.
            &[0x0f, 0x10, 0xca], &[0x0f, 0x10, 0x0e], &[0x0f, 0x11, 0x0e], &[0x0f, 0x11, 0xca],
            &[0x66, 0x0f, 0x10, 0x0e], &[0x66, 0x0f, 0x11, 0x0e],
            &[0x0f, 0x28, 0xca], &[0x0f, 0x28, 0x0e], &[0x0f, 0x29, 0x0e], &[0x66, 0x0f, 0x28, 0xca], &[0x66, 0x0f, 0x29, 0xca],
            &[0x66, 0x0f, 0x6f, 0xca], &[0x66, 0x0f, 0x6f, 0x0e], &[0x66, 0x0f, 0x7f, 0x0e], &[0x66, 0x0f, 0x7f, 0xca],
            &[0xf3, 0x0f, 0x6f, 0x0e], &[0xf3, 0x0f, 0x7f, 0x0e], &[0xf3, 0x0f, 0x6f, 0xca],
            // movd and movq with a general register and with memory.
            &[0x66, 0x0f, 0x6e, 0xc8], &[0x66, 0x48, 0x0f, 0x6e, 0xc8], &[0x66, 0x0f, 0x6e, 0x0e], &[0x66, 0x48, 0x0f, 0x6e, 0x0e],
            &[0x66, 0x0f, 0x7e, 0xc8], &[0x66, 0x48, 0x0f, 0x7e, 0xc8], &[0x66, 0x0f, 0x7e, 0x0e], &[0x66, 0x48, 0x0f, 0x7e, 0x0e],
            // movq between XMM registers and memory, by both opcodes.
            &[0xf3, 0x0f, 0x7e, 0xca], &[0xf3, 0x0f, 0x7e, 0x0e], &[0x66, 0x0f, 0xd6, 0xca], &[0x66, 0x0f, 0xd6, 0x0e],
            // and, andn, or, xor: of singles, of doubles, of integers; from memory; of
            // a register with itself.
            &[0x0f, 0x54, 0xca], &[0x0f, 0x55, 0xca], &[0x0f, 0x56, 0xca], &[0x0f, 0x57, 0xca],
            &[0x66, 0x0f, 0x54, 0xca], &[0x66, 0x0f, 0x55, 0xca], &[0x66, 0x0f, 0x56, 0xca], &[0x66, 0x0f, 0x57, 0xca],
            &[0x66, 0x0f, 0xdb, 0xca], &[0x66, 0x0f, 0xdf, 0xca], &[0x66, 0x0f, 0xeb, 0xca], &[0x66, 0x0f, 0xef, 0xca],
            &[0x0f, 0x54, 0x0e], &[0x0f, 0x55, 0x0e], &[0x66, 0x0f, 0xdf, 0x0e], &[0x66, 0x0f, 0xef, 0x0e],
            &[0x0f, 0x57, 0xc9], &[0x0f, 0x55, 0xc9], &[0x66, 0x0f, 0xef, 0xc9],
            // The upper eight: xorps xmm8, xmm9; addsd xmm8, xmm9; movsd xmm15, [rsi];
            // movaps xmm9, xmm1; movq r9, xmm10; movd xmm11, r9d; movsd [rsi + 8], xmm14
            &[0x45, 0x0f, 0x57, 0xc1], &[0xf2, 0x45, 0x0f, 0x58, 0xc1], &[0xf2, 0x44, 0x0f, 0x10, 0x3e],
            &[0x44, 0x0f, 0x28, 0xc9], &[0x66, 0x4d, 0x0f, 0x7e, 0xd1], &[0x66, 0x45, 0x0f, 0x6e, 0xd9],
            &[0xf2, 0x44, 0x0f, 0x11, 0x76, 0x08],
            // With an address that is worked out: movsd xmm1, [rsi + rax*8 + 16];
            // movups [rsi + rax + 5], xmm2
            &[0xf2, 0x0f, 0x10, 0x4c, 0xc6, 0x10], &[0x0f, 0x11, 0x54, 0x06, 0x05],
        ]
        .iter()
        .map(|i| (i.to_vec(), 1))
        .collect();
        let data: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(37) ^ 0xa5).collect();
        let n = sse(&list, 2, &|_| data.clone(), &|case, c| {
            fill_xmm(c);
            c.reg[RAX] = if case == 0 { 2 } else { 0x1234_5678_9abc_def0 & 3 };
            c.reg[RSI] = DATA;
            c.reg[9] = 0xfedc_ba98_7654_3210;
        });
        assert_eq!(n as usize, list.len() * 2);
        // Said outright, one of each kind: movsd from a register keeps the upper half
        // and from memory clears it; movd clears all above 32 bits.
        let (c, _) = checked(&[0xf2, 0x0f, 0x10, 0xca, 0xf2, 0x0f, 0x10, 0x1e, 0x66, 0x0f, 0x6e, 0xe0, 0x0f, 0x05], &9u64.to_le_bytes(), &|c| {
            fill_xmm(c);
            (c.xmm[2], c.xmm[3], c.xmm[4]) = (1, 2, 3);
            c.reg[RAX] = 0x1_0000_0005;
            c.reg[RSI] = DATA;
        });
        assert_eq!((c.xmm[2], c.xmm[3], c.xmm[6], c.xmm[7], c.xmm[8], c.xmm[9]), (3, 2, 9, 0, 5, 0));
    }

    #[test]
    fn a_fault_at_an_sse_instructions_load_or_store_changes_nothing() {
        // add edx, 1; addsd xmm1, [rsi]; ...  and  add edx, 1; movups [rsi], xmm1
        for (code, access) in [
            (&[0x83u8, 0xc2, 0x01, 0xf2, 0x0f, 0x58, 0x0e, 0x0f, 0x05][..], Access::Read),
            (&[0x83, 0xc2, 0x01, 0x0f, 0x11, 0x0e, 0x0f, 0x05], Access::Write),
        ] {
            let mut m = guest(code, &[]);
            let mut jit = Jit::new(false, 1).unwrap();
            let mut c = Cpu::new(CODE, DATA + 4096);
            fill_xmm(&mut c);
            let xmm = c.xmm;
            c.reg[RSI] = 0x90_0000;
            let Event::Stop(stop) = jit.run(&mut c, &mut m) else { panic!("it went on") };
            assert_eq!(stop, Stop::Fault(crate::mem::Fault { addr: 0x90_0000, access }));
            assert_eq!((c.rip, c.steps, c.reg[RDX], c.xmm), (CODE + 3, 1, 1, xmm));
            assert_eq!(jit.stats.faults, 1);
        }
    }
    #[test]
    fn a_block_without_sse_made_first_goes_on_to_one_with() {
        // again: add edx, 1; jmp on; on: addsd xmm1, xmm2; sub ebx, 1; jne again; syscall
        // The way in is through the first block, which does not use the XMM registers,
        // and they are not loaded there; the second loads them when it is first come
        // to, and not again while the loop goes round in generated code.
        let code = [0x83, 0xc2, 0x01, 0xeb, 0x00, 0xf2, 0x0f, 0x58, 0xca, 0x83, 0xeb, 0x01, 0x75, 0xf2, 0x0f, 0x05];
        let (c, _) = checked(&code, &[], &|c| {
            (c.xmm[2], c.xmm[3], c.xmm[4]) = (1.0f64.to_bits(), 9, 0.5f64.to_bits());
            c.reg[RBX] = 1000;
        });
        assert_eq!((c.xmm[2], c.xmm[3], c.reg[RDX]), (501.0f64.to_bits(), 9, 1000));
        // A block with no SSE in it does not begin with the loading, and one with does.
        let begins = |code: &[u8]| translate(&guest(code, &[]), CODE, false).unwrap().words[0];
        let loading = a64::mem_imm(a64::Size::X, a64::Mem::Load, 16, 31, crate::jit::XMM_LOADED);
        assert_ne!(begins(&[0x83, 0xc2, 0x01, 0x0f, 0x05]), loading);
        assert_eq!(begins(&[0xf2, 0x0f, 0x58, 0xca, 0x0f, 0x05]), loading);
    }
    #[test]
    fn packed_integers_element_by_element() {
        // PUNPCKL and PUNPCKH of each size, PCMPGT and PCMPEQ, PADD and PSUB,
        // PMULUDQ: xmm1 with xmm2, with [rsi], with itself, and xmm9 with xmm10.
        let mut list = Vec::new();
        for op in [0x60u8, 0x61, 0x62, 0x6c, 0x68, 0x69, 0x6a, 0x6d, 0x64, 0x65, 0x66, 0x74, 0x75, 0x76, 0xfc, 0xfd, 0xfe, 0xd4, 0xf8, 0xf9, 0xfa, 0xfb, 0xf4] {
            list.push((vec![0x66, 0x0f, op, 0xca], 1));
            list.push((vec![0x66, 0x0f, op, 0x0e], 1));
            list.push((vec![0x66, 0x0f, op, 0xc9], 1));
            list.push((vec![0x66, 0x45, 0x0f, op, 0xca], 1));
        }
        // Elements that are equal, that differ in their sign, that carry from one
        // element into where the next would be, and whose products need 64 bits.
        let values: [[u64; 2]; 6] = [
            [0x0102_0304_0506_0708, 0x090a_0b0c_0d0e_0f10],
            [0xffff_ffff_ffff_ffff, 0x8000_0000_7fff_ffff],
            [0x7f80_ff01_8000_7fff, 0x0000_0001_ffff_fffe],
            [0, 0x8080_8080_8080_8080],
            [0x0102_0304_0506_0708, 0x8000_0000_7fff_ffff],
            [0xffff_ffff_0000_0001, 0xdead_beef_cafe_f00d],
        ];
        let n = values.len();
        let checked = sse(&list, n * n, &|case| [values[case % n][0].to_le_bytes(), values[case % n][1].to_le_bytes()].concat(), &|case, c| {
            fill_xmm(c);
            (c.xmm[2], c.xmm[3]) = (values[case / n][0], values[case / n][1]);
            (c.xmm[4], c.xmm[5]) = (values[case % n][0], values[case % n][1]);
            (c.xmm[18], c.xmm[19]) = (values[case / n][0], values[case / n][1]);
            (c.xmm[20], c.xmm[21]) = (values[case % n][0], values[case % n][1]);
            c.reg[RSI] = DATA;
        });
        assert_eq!(checked as usize, list.len() * n * n);
        // Said outright: punpckldq, pmuludq and pcmpgtd of 32-bit elements 1, 2, 3, 4
        // with 5, 6, 0x80000007, 8.
        for (op, lo, hi) in [
            (0x62u8, 0x0000_0005_0000_0001u64, 0x0000_0006_0000_0002u64),
            (0xf4, 5, 3 * 0x8000_0007),
            (0x66, 0, 0x0000_0000_ffff_ffff),
        ] {
            let (c, _) = checked_run(&[0x66, 0x0f, op, 0xca, 0x0f, 0x05], &|c| {
                (c.xmm[2], c.xmm[3]) = (0x0000_0002_0000_0001, 0x0000_0004_0000_0003);
                (c.xmm[4], c.xmm[5]) = (0x0000_0006_0000_0005, 0x0000_0008_8000_0007);
            });
            assert_eq!((c.xmm[2], c.xmm[3]), (lo, hi), "opcode {op:#x}");
        }
    }

    /// `checked` with no data.
    fn checked_run(code: &[u8], set: &dyn Fn(&mut Cpu)) -> (Cpu, Mem) {
        checked(code, &[], set)
    }

    #[test]
    fn shuffles_by_every_constant() {
        // pshufd xmm1, xmm2 / xmm1, xmm1 / xmm1, [rsi] and shufps the same, with each
        // of the 256 constants.
        let mut list = Vec::new();
        for imm in 0..=255u8 {
            list.push((vec![0x66, 0x0f, 0x70, 0xca, imm], 1));
            list.push((vec![0x66, 0x0f, 0x70, 0xc9, imm], 1));
            list.push((vec![0x66, 0x0f, 0x70, 0x0e, imm], 1));
            list.push((vec![0x0f, 0xc6, 0xca, imm], 1));
            list.push((vec![0x0f, 0xc6, 0xc9, imm], 1));
            list.push((vec![0x0f, 0xc6, 0x0e, imm], 1));
        }
        let data: Vec<u8> = (0..16u8).map(|i| 0xa0 + i).collect();
        let n = sse(&list, 1, &|_| data.clone(), &|_, c| {
            fill_xmm(c);
            (c.xmm[2], c.xmm[3]) = (0x1111_1111_0000_0000, 0x3333_3333_2222_2222);
            (c.xmm[4], c.xmm[5]) = (0x5555_5555_4444_4444, 0x7777_7777_6666_6666);
            c.reg[RSI] = DATA;
        });
        assert_eq!(n as usize, list.len());
        // Said outright: pshufd by 1Bh turns the four end for end; shufps by DDh is
        // the odd elements of the register and then of the operand.
        let (c, _) = checked_run(&[0x66, 0x0f, 0x70, 0xda, 0x1b, 0x0f, 0xc6, 0xca, 0xdd, 0x0f, 0x05], &|c| {
            (c.xmm[2], c.xmm[3]) = (0x1111_1111_0000_0000, 0x3333_3333_2222_2222);
            (c.xmm[4], c.xmm[5]) = (0x5555_5555_4444_4444, 0x7777_7777_6666_6666);
        });
        assert_eq!((c.xmm[6], c.xmm[7]), (0x6666_6666_7777_7777, 0x4444_4444_5555_5555));
        assert_eq!((c.xmm[2], c.xmm[3]), (0x3333_3333_1111_1111, 0x7777_7777_5555_5555));
    }

    #[test]
    fn packed_shifts_by_every_count() {
        // psrlw, psraw, psllw; psrld, psrad, pslld; psrlq, psllq: of xmm1 and of
        // xmm9, by nothing, by less than the width, by the width, and by more.
        let mut list = Vec::new();
        for (op, fields) in [(0x71u8, &[2u8, 4, 6][..]), (0x72, &[2, 4, 6]), (0x73, &[2, 6])] {
            for &field in fields {
                for by in [0u8, 1, 7, 8, 15, 16, 17, 24, 31, 32, 33, 63, 64, 65, 128, 255] {
                    list.push((vec![0x66, 0x0f, op, 0xc1 | field << 3, by], 1));
                    list.push((vec![0x66, 0x41, 0x0f, op, 0xc1 | field << 3, by], 1));
                }
            }
        }
        let values: [[u64; 2]; 3] = [[0x8001_7fff_8000_0001, 0xffff_ffff_0000_0001], [0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210], [u64::MAX, 1 << 63]];
        let n = sse(&list, values.len(), &|_| Vec::new(), &|case, c| {
            fill_xmm(c);
            (c.xmm[2], c.xmm[3], c.xmm[18], c.xmm[19]) = (values[case][0], values[case][1], values[case][1], values[case][0]);
        });
        assert_eq!(n as usize, list.len() * values.len());
        // What is not read: a shift of memory, the shifts of the whole register by
        // bytes, and a shift of 64 bits with its sign.
        for code in [&[0x66u8, 0x0f, 0x72, 0x16, 1][..], &[0x66, 0x0f, 0x73, 0xd9, 1], &[0x66, 0x0f, 0x73, 0xf9, 1], &[0x66, 0x0f, 0x73, 0xe1, 1]] {
            assert!(translate(&guest(code, &[]), CODE, true).is_none(), "{code:02x?}");
        }
        // Nor the instructions of the MMX registers, which these opcodes are without
        // 66h, nor the other shuffles behind PSHUFD's opcode.
        for code in [&[0x0fu8, 0xfe, 0xca][..], &[0x0f, 0x72, 0xd1, 1], &[0xf3, 0x0f, 0x70, 0xca, 0], &[0xf2, 0x0f, 0x70, 0xca, 0], &[0x66, 0x0f, 0xc6, 0xca, 0]] {
            assert!(translate(&guest(code, &[]), CODE, true).is_none(), "{code:02x?}");
        }
    }
    #[test]
    fn code_the_guest_may_not_write_is_not_guarded() {
        // The same loop as "a store to data beside code", with its code on a page
        // the guest may only read and run, as a compiled program's is (ADR 0016): the
        // data two of the guest's pages on is in the same host page where those are
        // 16 KB, and every store goes straight to it.
        // again: add [rsi], ebx; add edx, [rsi]; sub ebx, 1; jne again; syscall
        let code = [0x01, 0x1e, 0x03, 0x16, 0x83, 0xeb, 0x01, 0x75, 0xf7, 0x0f, 0x05];
        let rounds = 300u64;
        for check in [true, false] {
            let mut m = guest(&code, &[]);
            m.map(CODE + 0x2000, 4096, R | W).unwrap();
            let mut jit = Jit::new(check, 1).unwrap();
            let c = both(&mut jit, &mut m, CODE, &|c| {
                c.reg[RBX] = rounds;
                c.reg[RSI] = CODE + 0x2000;
            });
            let total: u64 = (1..=rounds).map(|k| (k..=rounds).sum::<u64>()).sum();
            assert_eq!(c.reg[RDX], total & 0xffff_ffff);
            assert!(jit.stats.blocks >= 1 && !m.is_code(CODE) && jit.holds.is_empty());
            assert_eq!((jit.stats.beside, jit.stats.faults, jit.stats.forgotten), (0, 0, 0));
        }
        // And a block that rests on code it only read (to leave a record of the flags
        // out) is guarded if that is on a page the guest may write, though its own
        // instructions are not: add eax, ebx at a page's end, cmp eax, ecx on the next.
        let mut m = Mem::reserve().unwrap();
        m.map(CODE, 4096, R | W).unwrap();
        m.write_bytes(CODE + 4094, &[0x01, 0xd8]).unwrap();
        m.protect(CODE, 4096, R | X).unwrap();
        m.map(CODE + 4096, 4096, R | W | X).unwrap();
        m.write_bytes(CODE + 4096, &[0x39, 0xc8, 0x0f, 0x05]).unwrap();
        m.map(DATA, 4096, R | W).unwrap();
        let mut jit = Jit::new(false, 1).unwrap();
        both(&mut jit, &mut m, CODE + 4094, &|c| c.reg[RBX] = 1);
        assert!(m.is_code(CODE + 4096) && jit.holds.len() == 2);
    }
}
