//! Running a program with the JIT: which addresses have a block, when one is made,
//! what is done when one is left, and the check of every block against the
//! interpreter (ADR 0014, points 2, 7, 8 and 11).

use super::translate::{self, translate_as, Built};
use super::{a64, enter, Code, CPU, LEFT_AT_END, LEFT_FOR_THE_INTERPRETER, REASON, TABLE_BITS, TABLE_BITS_MOST};
use crate::cpu::{self, Cpu, Step, Stop};
use std::mem::offset_of;
use crate::mem::{Access, Mem, PAGE};
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A block is made when its first address has been arrived at this often, unless
/// another number is asked for. Measured in R031 on `cc1`: making a block costs
/// about what interpreting it twenty times does, so g64m's 16 waited too long; 1 to
/// 4 are alike on a long compilation and 1 is worse on a short one.
pub const HOT: u8 = 4;
/// A host page whose blocks have been forgotten this often for a write to it is not
/// translated from again: it holds data that changes beside its code.
const GIVEN_UP: u8 = 8;
/// How much generated code there may be before all of it is forgotten.
const CODE_BYTES: usize = 64 << 20;
/// How often an instruction is begun again because the host refused a store that
/// the list allows (R057), before the refusal is taken for one that stays.
const MOST_REFUSALS: u32 = 1000;

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
    /// the block's start) and its address in the guest, as bytes after the block's
    /// first instruction (`first`). Half the size of the two as they are made, and
    /// no room kept for more (R051: with the room, 6.9 MB of a thread's 22.7).
    starts: Box<[(u32, u32)]>,
    /// The guest's address of its first instruction.
    first: u64,
    /// The host pages of the guest's memory its instructions lie in.
    pages: (u64, u64),
    /// Where its instructions begin and end in the guest.
    guest: (u64, u64),
    /// Whether its host pages are marked as holding translated code, so that a store
    /// to them is caught: only if the guest may write what it was made from (ADR
    /// 0016). Code the guest may not write does not change.
    guarded: bool,
    /// The jumps in other blocks (or in this one) that were rewritten to come here.
    incoming: Vec<(u64, u64)>,
    /// The landings of returns in it (R046): the guest's return address and the
    /// host's address of the landing.
    pads: Vec<(u64, u64)>,
}

impl Block {
    /// The guest's address of its instruction `i`.
    fn at(&self, i: usize) -> u64 {
        self.first + self.starts[i].1 as u64
    }
}

/// A block as `--jit-count` keeps it (R076): its counters, and for each of its
/// instructions the length of its code and its kind.
pub struct Counted {
    entry: u32,
    exits: Vec<(u32, u32)>,
    lens: Vec<u32>,
    kinds: Vec<Vec<u8>>,
}

/// What `--jit-count` says: for each kind of instruction how often it ran and how
/// many of the host's instructions that was; and in all, how often blocks were
/// begun.
pub fn count_report(blocks: &[Counted]) -> (Vec<(Vec<u8>, u64, u64)>, u64) {
    let counts = std::ptr::addr_of!(translate::COUNTS) as *const u64;
    let read = |c: u32| unsafe { counts.add(c as usize).read() };
    let mut by_kind: HashMap<Vec<u8>, (u64, u64)> = HashMap::new();
    let mut begun = 0;
    for b in blocks {
        let entered = read(b.entry);
        begun += entered;
        // Instruction i ran each time the block was begun but for the ways out
        // taken after fewer than i + 1 instructions.
        let mut left = vec![0u64; b.lens.len() + 1];
        for &(c, n) in &b.exits {
            left[(n as usize).min(b.lens.len())] += read(c);
        }
        let mut gone = 0;
        for i in 0..b.lens.len() {
            gone += left[i];
            let ran = entered.saturating_sub(gone);
            let k = by_kind.entry(b.kinds[i].clone()).or_default();
            k.0 += ran;
            k.1 += ran * b.lens[i] as u64;
        }
    }
    let mut rows: Vec<_> = by_kind.into_iter().map(|(k, (n, h))| (k, n, h)).collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    (rows, begun)
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
    /// Why generated code was left (R073): for the interpreter; at a way out not yet
    /// joined to the block it goes to; by an address looked up in the table and not
    /// found there; and of those, how often the place held another address, which
    /// was put out for this one.
    pub left_for_interpreter: u64,
    pub left_unjoined: u64,
    pub left_missed: u64,
    pub missed_taken: u64,
    /// How often the table was made larger (R074).
    pub grown: u64,
    /// The guest's instructions translated, over all blocks made (R079).
    pub translated: u64,
    /// What a way out not yet joined arrived at (R075): a block (it is joined now),
    /// an address where no block can begin, or one not arrived at often enough yet.
    pub unjoined_to_block: u64,
    pub unjoined_to_stood_in: u64,
    pub unjoined_to_cold: u64,
    /// How long the making of blocks took, and the running of generated code, in
    /// nanoseconds: the rest of a run is the interpreter and r64m's own keeping.
    pub making: u64,
    pub running: u64,
    /// Blocks taken from the cache of translations (ADR 0021), not translated.
    pub from_cache: u64,
}

pub struct Jit {
    code: Code,
    blocks: Vec<Block>,
    /// What is known of each address arrived at.
    places: HashMap<u64, Place, BuildHasherDefault<OfAddress>>,
    /// The landing of a return to each address that is some call's return address
    /// (R046): what the table gives for it, in place of the block there.
    pads: HashMap<u64, u64, BuildHasherDefault<OfAddress>>,
    /// How often a host page's blocks were forgotten for a write.
    writes: HashMap<u64, u8>,
    /// For each of the guest's own 4 KB pages, how many blocks have an instruction
    /// in it. A host page may be larger: a store to it need not be a store to code.
    holds: HashMap<u64, u32>,
    /// The guest's pages that anything is known of (`places`) or that a block rests
    /// on, since generated code was last emptied: so that a change of mapping
    /// elsewhere, which is nearly all of them, is seen at once to concern nothing
    /// (R029: `cc1` maps memory many thousand times, and each was a walk through
    /// every place known).
    known: std::collections::BTreeSet<u64>,
    /// Blocks are made with ordered loads and stores (ADR 0019, point D): from when
    /// the memory is first shared with another thread, or because it was asked for
    /// (`--jit-ordered`, to measure what it costs).
    pub ordered: bool,
    /// How many stores into guarded code, by any thread, this thread has taken
    /// account of (`Mem::guarded_writes`).
    writes_seen: u64,
    /// Whether the making of blocks and the running of generated code are timed
    /// (`--jit-stats`).
    pub timed: bool,
    /// Every block is compared with the interpreter.
    check: bool,
    /// When asked for (`--jit-why`): how often the interpreter ran an instruction
    /// while the JIT was on, by the instruction's first bytes (its prefixes and
    /// opcode, and the ModRM byte's field where the opcode is a group).
    pub why: Option<HashMap<Vec<u8>, u64>>,
    /// With it: of those, the ones run at an address where no block can begin, which
    /// every way out to it leaves generated code for (R075).
    pub why_stood: Option<HashMap<Vec<u8>, u64>>,
    /// How often an address is arrived at before its block is made.
    hot: u8,
    /// The jump the last block was left by, if it can be rewritten to go to the block
    /// at the address it left for; zero if there is none to rewrite.
    from: u64,
    /// What generated code looks in when where it goes is not a constant: for a place
    /// chosen by an address's low bits, the address and where its block's code is. A
    /// place with no block holds an address no place is chosen by and `miss`.
    table: Box<[[u64; 2]]>,
    /// Where generated code has the instruction of each lookup that says the table's
    /// size, to be written again when it grows (R074); and how many addresses have
    /// been put out of the table since it last grew.
    lookups: Vec<u64>,
    put_out: u64,
    /// With `--jit-count`: each block made, for the report (R076).
    pub counted: Vec<Counted>,
    /// Code that leaves for Rust with RIP as generated code had it in x16: what an
    /// empty place of the table sends to, so that no place can send astray.
    miss: u64,
    pub stats: Stats,
}

/// Whether each thread says what its blocks hold as they grow (`--jit-memory`).
pub static SAY_MEMORY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Why `run` came back.
pub enum Event {
    Syscall,
    /// A signal waits for the thread: nothing more is run until it is delivered.
    Signal,
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
            a64::mem_imm(a64::Size::X, a64::Mem::Load, 17, CPU, offset_of!(Cpu, way_out) as u32),
            a64::br(17),
        ];
        self.miss = self.code.append(&miss).expect("room in a piece just emptied") as u64;
        // An address with every bit set chooses the last place; whatever is asked of
        // an empty place, its code is the way out.
        self.table.fill([u64::MAX, self.miss]);
    }

    /// The place in a table of `len` places that an address chooses.
    fn place(len: usize, rip: u64) -> usize {
        (rip as usize) & (len - 1)
    }

    /// The table four times larger (R074): what it holds put in the new places, and
    /// every lookup in generated code told the new size.
    fn grow(&mut self) {
        let bits = self.table.len().trailing_zeros();
        if bits >= TABLE_BITS_MOST {
            return;
        }
        let bits = (bits + 2).min(TABLE_BITS_MOST);
        let mut table = vec![[u64::MAX, self.miss]; 1 << bits].into_boxed_slice();
        for &[rip, to] in self.table.iter() {
            if rip != u64::MAX {
                table[Self::place(table.len(), rip)] = [rip, to];
            }
        }
        self.table = table;
        let word = translate::lookup_word(bits);
        for &at in &self.lookups {
            unsafe { self.code.rewrite(at as *const u8, word) };
        }
        self.put_out = 0;
        self.stats.grown += 1;
    }

    fn bare(check: bool, hot: u8) -> std::io::Result<Jit> {
        Ok(Jit {
            code: Code::new(CODE_BYTES)?,
            blocks: Vec::new(),
            places: HashMap::default(),
            pads: HashMap::default(),
            known: Default::default(),
            // (The tests can be run with every block ordered: R64M_ORDERED.)
            ordered: cfg!(test) && std::env::var_os("R64M_ORDERED").is_some(),
            writes_seen: 0,
            timed: false,
            writes: HashMap::new(),
            holds: HashMap::new(),
            check,
            why: None,
            why_stood: None,
            hot: hot.max(1),
            from: 0,
            table: vec![[0, 0]; 1 << TABLE_BITS].into_boxed_slice(),
            lookups: Vec::new(),
            put_out: 0,
            counted: Vec::new(),
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
        // How often in a row the host has refused a store that the list allows (see
        // the arms for it below).
        let mut refused = 0u32;
        self.from = 0;
        loop {
            // What other threads have done since the last look: to the list of
            // regions, and to code that is guarded (ADR 0019). A thread that stored
            // into such code could drop only its own blocks; every other thread drops
            // its guarded ones here, when it next comes by.
            if c.kick != 0 {
                // With the flags worked out: the handler's frame holds them.
                cpu::settle(c);
                return Event::Signal;
            }
            m.sync();
            if !self.ordered && m.must_order() {
                // A second thread, on a host that does not order memory as x86 does
                // by itself: every block from here on is ordered, and those made
                // before are dropped.
                self.ordered = true;
                self.empty(m);
                arrived = true;
            }
            let writes = m.guarded_writes();
            if writes != self.writes_seen {
                self.writes_seen = writes;
                if !self.holds.is_empty() {
                    self.empty(m);
                    arrived = true;
                }
            }
            // Is the interpreter about to run an instruction that no block begins with?
            let mut stands_in = false;
            // Or one that a block left to it from inside (a case of it that is not
            // translated, a fault)? After it the address is looked up again: what
            // follows may be a long way from the next jump, and was the
            // interpreter's all that way before (R043: sixty instructions for every
            // ADC that could not be told its carry, in code written without jumps).
            let mut left_inside = false;
            if arrived {
                if self.from != 0 {
                    match self.places.get(&c.rip) {
                        Some(Place::Block(_)) => self.stats.unjoined_to_block += 1,
                        Some(Place::StoodIn) => self.stats.unjoined_to_stood_in += 1,
                        _ => self.stats.unjoined_to_cold += 1,
                    }
                }
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
                            let to = self.pads.get(&c.rip).copied().unwrap_or(self.blocks[id].code as u64);
                            let place = &mut self.table[Self::place(self.table.len(), c.rip)];
                            let put_out = self.from == 0 && place[0] != c.rip && place[0] != u64::MAX;
                            *place = [c.rip, to];
                            if put_out {
                                self.stats.missed_taken += 1;
                                self.put_out += 1;
                                if self.put_out >= self.table.len() as u64 {
                                    self.grow();
                                }
                            }
                        }
                        match self.run_block(id, c, m) {
                            Ok(whole) => {
                                arrived = whole;
                                left_inside = !whole;
                            }
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
                        // A block kept by an earlier process (ADR 0021) is taken where
                        // it would have been made, not before: made at an address's
                        // first arrival, blocks fail Java's keytool (R084, R085).
                        if heat < self.hot {
                            if self.places.insert(c.rip, Place::Heat(heat)).is_none() {
                                self.know(c.rip);
                            }
                        } else if super::cache::on() && self.timed_from_cache(c.rip, m) {
                            continue;
                        } else if self.timed_make(c.rip, m) {
                            continue;
                        } else {
                            if self.places.insert(c.rip, Place::StoodIn).is_none() {
                                self.know(c.rip);
                            }
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
            let record = c.flag_rec[0];
            cpu::settle(c);
            let before = c.rip;
            if let Some(why) = &mut self.why {
                *why.entry(key_of(m, before)).or_insert(0) += 1;
            }
            if let (true, Some(why)) = (stands_in, &mut self.why_stood) {
                // With the kind of record the flags were left in (its kind and width),
                // or none (RFLAGS has them), as two more bytes.
                let mut key = key_of(m, before);
                key.extend([0xee, (record >> 8) as u8, record as u8]);
                *why.entry(key).or_insert(0) += 1;
            }
            // An instruction that faults at its store may have changed the flags or a
            // register on the way to it (ADC to memory has its carry, XADD its
            // register): what they were is kept, for the two cases below in which the
            // instruction is run again.
            let (regs, flags) = (c.reg, c.rflags);
            match cpu::step(c, m) {
                Ok(Step::Next) => {
                    refused = 0;
                    arrived = stands_in || left_inside || c.rip < before || c.rip > before.wrapping_add(15);
                }
                Ok(Step::Syscall) => return Event::Syscall,
                // A store to a host page that holds translated code. If it is to the
                // code (the guest's own page written has a block's instruction in it):
                // the translations are forgotten, the page can be written again, and
                // the store is made. If it is to data that only shares the host's page
                // with code (R010): the page is opened for this one instruction.
                // (Asked of the list as it is now: another thread may have marked
                // the page since this one last looked, and the host then refused a
                // store that this thread's copy of the list allowed.)
                Err(Stop::Fault(f)) if f.access == Access::Write && { m.sync(); m.is_code(f.addr) } => {
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
                    self.writes_seen = m.wrote_guarded();
                    let _ = m.mark_code(start, end, false);
                    let stepped = cpu::step(c, m);
                    // Closed again where code is: every host page that a block lies in.
                    for at in (start..end).step_by(m.host_page() as usize) {
                        if (at..at + m.host_page()).step_by(PAGE as usize).any(|p| self.holds.contains_key(&p)) {
                            let _ = m.mark_code(at, at + 1, true);
                        }
                    }
                    match stepped {
                        Ok(Step::Next) => arrived = stands_in || left_inside || c.rip < before || c.rip > before.wrapping_add(15),
                        Ok(Step::Syscall) => return Event::Syscall,
                        // Closed again before the store was made: another thread
                        // made a block from the page in that moment (R055: the
                        // JVM writes code beside code its other threads run, and
                        // was sent this fault as its own, one run in six under
                        // load). The instruction is begun again, as it began.
                        Err(Stop::Fault(again)) if again.access == Access::Write && { m.sync(); m.is_code(again.addr) } => {
                            (c.reg, c.rflags) = (regs, flags);
                            arrived = false;
                            continue;
                        }
                        Err(Stop::Fault(f)) if f.access == Access::Write && refused < MOST_REFUSALS && m.is_shared() && m.guest_may_write(f.addr, f.addr + 1) => {
                            (c.reg, c.rflags) = (regs, flags);
                            refused += 1;
                            std::thread::yield_now();
                            arrived = false;
                            continue;
                        }
                        Err(stop) => return Event::Stop(stop),
                    }
                }
                // Refused by the host where the list, as it is now, lets the guest
                // write, and no mark is on the page: it was closed when the store
                // was made and has been opened since, by another thread that stores
                // beside code and opens the page for its one instruction (R057: the
                // JVM was sent this as its own fault, one run in a hundred under
                // load). Not the guest's fault whatever the mark says at this
                // moment: the instruction is begun again. A refusal that stays
                // (nothing known does that) is said after a thousand tries.
                Err(Stop::Fault(f)) if f.access == Access::Write && refused < MOST_REFUSALS && m.is_shared() && m.guest_may_write(f.addr, f.addr + 1) => {
                    (c.reg, c.rflags) = (regs, flags);
                    refused += 1;
                    std::thread::yield_now();
                    arrived = false;
                    continue;
                }
                Err(stop) => return Event::Stop(stop),
            }
        }
    }

    /// For `--profile`: of a host address in this thread's generated code, the block
    /// it is in (by the guest's address of its first instruction), the guest's
    /// instruction, and how many host instructions it is from the block's start.
    pub fn where_is(&self, pc: usize) -> Option<(u64, u64, u32)> {
        let block = &self.blocks[self.blocks.partition_point(|b| b.code as usize <= pc).checked_sub(1)?];
        let word = ((pc - block.code as usize) / 4) as u32;
        let i = block.starts.partition_point(|&(start, _)| start <= word).saturating_sub(1);
        Some((block.first, block.at(i), word))
    }

    /// For `--profile`: of a host address in this thread's generated code, the kind
    /// of the guest's instruction it is in (as `key_of` names kinds), or nothing
    /// named where it is in a block's own beginning, before its first instruction.
    pub fn kind_at(&self, pc: usize, m: &Mem) -> Option<Vec<u8>> {
        let block = &self.blocks[self.blocks.partition_point(|b| b.code as usize <= pc).checked_sub(1)?];
        let word = ((pc - block.code as usize) / 4) as u32;
        let i = block.starts.partition_point(|&(start, _)| start <= word);
        Some(if i == 0 { Vec::new() } else { key_of(m, block.at(i - 1)) })
    }

    /// For `--profile`: every kind of instruction that was translated, how often,
    /// and into how many of the host's instructions in all. A block's last
    /// instruction is counted to where the next block begins, so the way out of the
    /// block is in it; the very last block made is left out for having no end known.
    /// With them, under no name, the blocks' own beginnings.
    pub fn costs(&self, m: &Mem) -> HashMap<Vec<u8>, (u64, u64)> {
        let mut all: HashMap<Vec<u8>, (u64, u64)> = HashMap::new();
        for pair in self.blocks.windows(2) {
            let (block, next) = (&pair[0], &pair[1]);
            let end = ((next.code as usize).wrapping_sub(block.code as usize) / 4) as u32;
            let Some(&(first, _)) = block.starts.first() else { continue };
            let entry = all.entry(Vec::new()).or_default();
            *entry = (entry.0 + 1, entry.1 + first as u64);
            for (i, &(start, _)) in block.starts.iter().enumerate() {
                let until = block.starts.get(i + 1).map_or(end, |&(next, _)| next);
                let kind = all.entry(key_of(m, block.at(i))).or_default();
                *kind = (kind.0 + 1, kind.1 + until.saturating_sub(start) as u64);
            }
        }
        all
    }

    /// And a block's code as it stands (to the next block's start, or 400
    /// instructions), with where each of the guest's instructions begins.
    pub fn code_of(&self, rip: u64) -> Option<(Vec<u32>, Vec<(u32, u64)>)> {
        let Some(&Place::Block(id)) = self.places.get(&rip) else { return None };
        let block = &self.blocks[id];
        let end = self.blocks.get(id + 1).map_or(400, |next| ((next.code as usize).wrapping_sub(block.code as usize) / 4).min(400));
        let words = unsafe { std::slice::from_raw_parts(block.code as *const u32, end) }.to_vec();
        Some((words, block.starts.iter().map(|&(word, after)| (word, block.first + after as u64)).collect()))
    }

    /// Rewrites the jump at `site` to go to a block's code. Where it went before (the
    /// code that hands its address to r64m, after its own block's end: R072) is kept,
    /// to point it back there if the block is forgotten.
    fn join(&mut self, site: u64, id: usize) {
        let before = unsafe { (site as *const u32).read() };
        let unjoined = site.wrapping_add((((before << 6) as i32 >> 6) as i64 * 4) as u64);
        let words = (self.blocks[id].code as i64 - site as i64) / 4;
        unsafe { self.code.rewrite(site as *const u8, a64::b(words as i32)) };
        self.blocks[id].incoming.push((site, unjoined));
        self.stats.joined += 1;
    }

    /// Goes into generated code at one block, which may go on to others joined to it.
    /// `Ok(true)` if it was left at a block's end; `Ok(false)` if the host faulted,
    /// and then RIP is on the instruction that did.
    fn run_block(&mut self, id: usize, c: &mut Cpu, m: &mut Mem) -> Result<bool, String> {
        // (With more than one thread a block cannot be held to the interpreter: what
        // another thread writes between the two runs makes them differ rightly.)
        let expected = if self.check && !m.is_shared() { self.interpreted(id, c, m) } else { None };
        let start = self.timed.then(std::time::Instant::now);
        let reason = unsafe { enter(c, self.blocks[id].code, m.base(), self.table.as_ptr()) };
        if let Some(start) = start {
            self.stats.running += start.elapsed().as_nanos() as u64;
        }
        // What the blocks that were run to their ends ran.
        // (Without the count's top bit, which says that a signal waits.)
        let ran = c.ran & !(1 << 63);
        c.steps += ran;
        self.stats.in_blocks += ran;
        self.stats.entered += 1;
        match reason {
            LEFT_FOR_THE_INTERPRETER => self.stats.left_for_interpreter += 1,
            LEFT_AT_END if c.left_from != 0 => self.stats.left_unjoined += 1,
            LEFT_AT_END => self.stats.left_missed += 1,
            _ => {}
        }
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
            c.rip = block.at(i);
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
                let (n, from) = (block.starts.len(), block.first);
                // With the block's own bytes: what it is can then be read at once.
                let (code, have) = m.code_at(from);
                let bytes = unsafe { std::slice::from_raw_parts(code, have.min((block.guest.1 - from) as usize).min(48)) };
                return Err(format!("the block of {n} instructions at {from:#x} ({bytes:02x?}): {what}"));
            }
        }
        Ok(true)
    }

    /// For `--jit-check`: what the interpreter makes of a block's instructions. The
    /// CPU is not touched and the memory is put back as it was; what comes back is the CPU as the
    /// interpreter left it and what it wrote. `None` if the interpreter did not get
    /// through them all (a fault): then there is nothing to compare a whole block with.
    fn interpreted(&self, id: usize, c: &Cpu, m: &mut Mem) -> Option<(Cpu, Vec<(u64, Vec<u8>)>)> {
        let n = self.blocks[id].starts.len();
        // On a copy: the thread's own `Cpu` holds the word the host's handler sets
        // when a signal arrives, and is not to be written over with what it was
        // before. (It was, until R043: a signal that arrived while the interpreter
        // ran here was noted and never looked for, and a guest in a loop without
        // calls went round for ever. About two runs in a hundred under load.)
        let mut alone = c.clone();
        // The flags as the record says them: the pass through the block before may
        // have been one that was not compared, and then nothing worked them out.
        cpu::settle(&mut alone);
        m.log_writes();
        let mut whole = true;
        for _ in 0..n {
            let (at, count) = (alone.rip, alone.reg[cpu::RCX]);
            if !matches!(cpu::step(&mut alone, m), Ok(Step::Next)) {
                whole = false;
                break;
            }
            // A repeated move or store that the interpreter does one element at a
            // time (its two places overlap): it stays where it is and has counted
            // RCX down by one. A block does all of it or leaves it (R054), and there
            // may be millions to do: this pass through the block is not compared.
            if alone.rip == at && alone.reg[cpu::RCX] != count {
                whole = false;
                break;
            }
        }
        let written = m.undo_writes();
        whole.then_some((alone, written))
    }

    fn timed_from_cache(&mut self, rip: u64, m: &mut Mem) -> bool {
        if !self.timed {
            return self.from_cache(rip, m);
        }
        let start = std::time::Instant::now();
        let made = self.from_cache(rip, m);
        self.stats.making += start.elapsed().as_nanos() as u64;
        made
    }

    fn timed_make(&mut self, rip: u64, m: &mut Mem) -> bool {
        if !self.timed {
            return self.make(rip, m);
        }
        let start = std::time::Instant::now();
        let made = self.make(rip, m);
        self.stats.making += start.elapsed().as_nanos() as u64;
        made
    }

    /// Translates the block at `rip` and keeps it. False if nothing was made.
    fn make(&mut self, rip: u64, m: &mut Mem) -> bool {
        let page = m.host_pages(rip, rip + 1).0;
        if self.writes.get(&page).is_some_and(|&n| n >= GIVEN_UP) {
            return false;
        }
        let Some(built) = translate_as(m, rip, self.check, self.ordered) else {
            return false;
        };
        self.keep_block(rip, m, built, true)
    }

    /// The block at `rip` from the cache (ADR 0021), if one is kept there and the
    /// guest's bytes are what it was made from.
    fn from_cache(&mut self, rip: u64, m: &mut Mem) -> bool {
        if self.check || translate::COUNTING.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let page = m.host_pages(rip, rip + 1).0;
        if self.writes.get(&page).is_some_and(|&n| n >= GIVEN_UP) {
            return false;
        }
        let Some(built) = super::cache::find(super::cache::key(rip, self.ordered), m) else {
            return false;
        };
        self.stats.from_cache += 1;
        self.keep_block(rip, m, built, false)
    }

    /// Keeps a block: its code written, its lookups, landings and pages set up.
    /// `fresh` if it was just translated (and not taken from the cache): then it is
    /// kept for the next process too, if a cache is in use and the guest may not
    /// write its code.
    fn keep_block(&mut self, rip: u64, m: &mut Mem, built: Built, fresh: bool) -> bool {
        // What the block rests on: its own instructions, and what was read past them
        // to leave a record of the flags unwritten. A store to any of it forgets it.
        let (from, to) = built.seen.map_or((rip, built.end), |(lo, hi)| (lo.min(rip), hi.max(built.end)));
        // Whether the guest may write any page of it (ADR 0016), asked once (R087).
        let guarded = m.guest_may_write(from & !(PAGE - 1), to.next_multiple_of(PAGE));
        if fresh && !guarded && super::cache::on() && !self.check {
            super::cache::keep(super::cache::key(rip, self.ordered), rip, &built, m);
        }
        let Built { mut words, starts, end, pads, lookups, counted, body, .. } = built;
        if std::env::var_os("R64M_JIT_DUMP").is_some() {
            // For looking at what was made: each block's address, its instructions'
            // addresses, and its code as words an assembler can be given.
            eprintln!("block {rip:#x}..{end:#x} guest {:x?}", starts.iter().map(|s| s.1).collect::<Vec<_>>());
            eprintln!("{}", words.iter().map(|w| format!(".inst {w:#010x}")).collect::<Vec<_>>().join("\n"));
        }
        // Its lookups take as many bits as the table has now.
        let bits = self.table.len().trailing_zeros();
        if bits != TABLE_BITS {
            for &at in &lookups {
                words[at as usize] = translate::lookup_word(bits);
            }
        }
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
        self.lookups.extend(lookups.iter().map(|&at| code as u64 + 4 * at as u64));
        // With `--jit-count`: what each of its instructions is and how long its code is.
        if let Some((entry, exits)) = counted {
            let lens = (0..starts.len()).map(|i| starts.get(i + 1).map_or(body, |n| n.0) - starts[i].0).collect();
            let kinds = starts.iter().map(|&(_, at)| key_of(m, at)).collect();
            self.counted.push(Counted { entry, exits, lens, kinds });
        }
        // If the guest may write any page of what the block rests on, the host pages
        // those lie in may not be written from now, and are counted as holding code
        // (ADR 0016: what the guest may not write needs no guard).
        let pages = m.host_pages(from, to);
        for page in (from & !(PAGE - 1)..to).step_by(PAGE as usize) {
            self.known.insert(page);
        }
        if guarded {
            if m.mark_code(pages.0, pages.1, true).is_err() {
                return false;
            }
            for page in (from & !(PAGE - 1)..to).step_by(PAGE as usize) {
                *self.holds.entry(page).or_insert(0) += 1;
            }
        }
        if self.places.insert(rip, Place::Block(self.blocks.len())).is_none() {
            self.know(rip);
        }
        // A return to one of this block's calls is to find the landing after that
        // call, not the block at the address: from now on, and where the table
        // already says the block. A call that another block holds too (blocks
        // overlap) keeps the landing it has: this block's `bl` is made a jump to
        // that block's, so that what the host remembers at the call is where the
        // return will go. (The two are forgotten together: both have the call's
        // instruction, and blocks go by the page.)
        let mut own = Vec::new();
        for &(word, next) in &pads {
            let bl = code as u64 + 4 * word as u64;
            match self.pads.get(&next) {
                Some(&landing) => unsafe { self.code.rewrite(bl as *const u8, a64::b(((landing - 4) as i64 - bl as i64) as i32 / 4)) },
                None => {
                    self.pads.insert(next, bl + 4);
                    own.push((next, bl + 4));
                    let place = &mut self.table[Self::place(self.table.len(), next)];
                    if place[0] == next && !self.check {
                        place[1] = bl + 4;
                    }
                }
            }
        }
        // (With no room kept for more: there will be none.)
        own.shrink_to_fit();
        let pads = own;
        let starts = starts.iter().map(|&(word, at)| (word, (at - rip) as u32)).collect();
        self.blocks.push(Block { code, starts, first: rip, pages, guest: (from, to), guarded, incoming: Vec::new(), pads });
        self.stats.blocks += 1;
        if fresh {
            self.stats.translated += self.blocks.last().map_or(0, |b| b.starts.len() as u64);
        }
        if SAY_MEMORY.load(std::sync::atomic::Ordering::Relaxed) && self.blocks.len().is_multiple_of(2000) {
            self.say_memory();
        }
        true
    }

    /// What this thread's blocks hold, part by part, in bytes (`--jit-memory`): the
    /// code written, what is kept about each block, each map, and the table. A map's
    /// own cost is taken as its room times an entry's size and a byte of control.
    fn say_memory(&self) {
        use std::mem::size_of;
        let code = (self.code.used() * 4) as u64;
        let records = (self.blocks.capacity() * size_of::<Block>()) as u64;
        let starts: u64 = self.blocks.iter().map(|b| (b.starts.len() * size_of::<(u32, u32)>()) as u64).sum();
        let instructions: u64 = self.blocks.iter().map(|b| b.starts.len() as u64).sum();
        let incoming: u64 = self.blocks.iter().map(|b| (b.incoming.capacity() * 16) as u64).sum();
        let pads: u64 = self.blocks.iter().map(|b| (b.pads.capacity() * 16) as u64).sum();
        // The allocator's own for each vector that holds anything: taken as 16 bytes.
        let pieces = self.blocks.iter().map(|b| (!b.starts.is_empty()) as u64 + (b.incoming.capacity() != 0) as u64 + (b.pads.capacity() != 0) as u64).sum::<u64>();
        let places = (self.places.capacity() * (size_of::<(u64, Place)>() + 1)) as u64;
        let pad_map = (self.pads.capacity() * 17) as u64;
        let holds = (self.holds.capacity() * 13) as u64 + (self.writes.capacity() * 10) as u64;
        let known = (self.known.len() * 12) as u64;
        let table = (self.table.len() * 16 + self.lookups.capacity() * 8) as u64;
        let all = code + records + starts + incoming + pads + pieces * 16 + places + pad_map + holds + known + table;
        eprintln!(
            "r64m: memory of thread {}: {} blocks, {} guest instructions, {} places known; code {code}, block records {records}, starts {starts}, incoming {incoming}, pads {pads}, {pieces} vectors, places {places}, pad map {pad_map}, pages {holds}, known {known}, table {table}; in all {all}",
            crate::sys::thread_number(),
            self.blocks.len(),
            instructions,
            self.places.len(),
        );
    }

    /// Forgets every block with an instruction in the host page that holds `addr`, and
    /// lets the page be written again. The jumps that were rewritten to come to those
    /// blocks go to their own ways out again.
    fn forget(&mut self, addr: u64, m: &mut Mem) {
        // The other threads are told: their blocks from this page may be of what is
        // no longer there.
        self.writes_seen = m.wrote_guarded();
        let start = self.drop_page(addr, m);
        *self.writes.entry(start).or_insert(0) += 1;
        self.stats.forgotten += 1;
    }

    /// Notes that something is known of the page `rip` is in.
    fn know(&mut self, rip: u64) {
        self.known.insert(rip & !(PAGE - 1));
    }

    /// The guest's pages `[start, end)` were unmapped, mapped anew or given another
    /// protection: what was made from them, or learnt of addresses in them, is of
    /// what is no longer there. (It may be made again: if the pages can now be
    /// written, guarded; ADR 0016, point 2.)
    pub fn guest_changed(&mut self, start: u64, end: u64, m: &mut Mem) {
        // Nearly always nothing is known of those pages: memory for data.
        if self.known.range(start & !(PAGE - 1)..end).next().is_none() {
            return;
        }
        let mut pages: Vec<u64> = self
            .places
            .values()
            .filter_map(|place| match place {
                Place::Block(id) => {
                    let block = &self.blocks[*id];
                    (block.guest.0 < end && start < block.guest.1).then_some(block.pages.0)
                }
                _ => None,
            })
            .collect();
        pages.sort();
        pages.dedup();
        for page in pages {
            self.drop_page(page, m);
        }
        self.places.retain(|rip, place| matches!(place, Place::Block(_)) || *rip < start || *rip >= end);
        self.from = 0;
    }

    /// Drops every block with an instruction in the host page that holds `addr`; the
    /// page's start is given back.
    fn drop_page(&mut self, addr: u64, m: &mut Mem) -> u64 {
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
                    let place = &mut table[Self::place(table.len(), *rip)];
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
            for (site, unjoined) in std::mem::take(&mut self.blocks[id].incoming) {
                unsafe { self.code.rewrite(site as *const u8, a64::b(((unjoined as i64 - site as i64) / 4) as i32)) };
            }
            // Its landings are of calls that are no longer there.
            for (next, pad) in std::mem::take(&mut self.blocks[id].pads) {
                if self.pads.get(&next) == Some(&pad) {
                    self.pads.remove(&next);
                }
                let place = &mut self.table[Self::place(self.table.len(), next)];
                if *place == [next, pad] {
                    *place = [u64::MAX, self.miss];
                }
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
        start
    }

    /// Forgets all generated code: the piece of memory is full.
    fn empty(&mut self, m: &mut Mem) {
        for block in self.blocks.iter().filter(|b| b.guarded) {
            let _ = m.mark_code(block.pages.0, block.pages.1, false);
        }
        self.blocks.clear();
        self.pads.clear();
        self.places.clear();
        self.known.clear();
        self.holds.clear();
        self.from = 0;
        self.code.empty();
        self.lookups.clear();
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
            Event::Signal => panic!("at {rip:#x}: a signal, which no test sends"),
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
    fn the_table_grows_and_what_was_made_before_finds_its_way() {
        // One function called from 16 places 2^12 apart, round after round: their
        // return addresses all choose one place of the first table, and four of a
        // table four times larger, and put each other out at every return. The table
        // grows twice, to where each has a place; the blocks made before still find
        // the landings, which they could only do by lookups told the new size. The
        // sum is the same as the interpreter's.
        //   at k * 1000h (k < 16): call f; jmp (k + 1) * 1000h  (the last: sub ebx, 1;
        //   jne 0; syscall)       f at 10000h + 100h: add edx, 3; ret
        const STEP: u64 = 1 << TABLE_BITS;
        const N: u64 = 16;
        let f = CODE + N * STEP + 0x100;
        let mut code = vec![0u8; (N * STEP + 0x110) as usize];
        let rel = |from_end: u64, to: u64| ((to as i64 - from_end as i64) as i32).to_le_bytes();
        for k in 0..N {
            let at = CODE + k * STEP;
            let i = (k * STEP) as usize;
            code[i] = 0xe8;
            code[i + 1..i + 5].copy_from_slice(&rel(at + 5, f));
            if k + 1 < N {
                code[i + 5] = 0xe9;
                code[i + 6..i + 10].copy_from_slice(&rel(at + 10, at + STEP));
            } else {
                code[i + 5..i + 10].copy_from_slice(&[0x83, 0xeb, 0x01, 0x0f, 0x85]);
                code[i + 10..i + 14].copy_from_slice(&rel(at + 14, CODE));
                code[i + 14..i + 16].copy_from_slice(&[0x0f, 0x05]);
            }
        }
        let fi = (f - CODE) as usize;
        code[fi..fi + 4].copy_from_slice(&[0x83, 0xc2, 0x03, 0xc3]);
        let set = |c: &mut Cpu| {
            c.reg[RBX] = 6000;
            c.reg[RDX] = 0;
        };
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let c = both(&mut jit, &mut m, CODE, &set);
        assert_eq!(c.reg[RDX], 6000 * N * 3);
        assert!(jit.stats.grown >= 1, "the table did not grow: {} put out", jit.stats.missed_taken);
        // Grown until the return addresses have a place each: about 21,000 returns
        // missed on the way, and none after. (A lookup still taking the first
        // table's bits would miss at every one of the 96,000.)
        assert_eq!(jit.table.len(), 1 << (TABLE_BITS + 4));
        assert!(jit.stats.left_missed < 30_000, "{} returns not found in the table", jit.stats.left_missed);
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
        assert_eq!(Jit::place(1 << TABLE_BITS, CODE + 5), Jit::place(1 << TABLE_BITS, FAR + 5));
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
        assert_ne!(Jit::place(1 << TABLE_BITS, FAR), Jit::place(1 << TABLE_BITS, CODE));
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
    fn a_store_refused_while_another_thread_had_the_page_closed_is_made() {
        // again: mov [rsi], ebx; sub ebx, 1; jne again; syscall. Its data is far
        // from its code. Another thread marks the data's page as holding code and
        // takes the mark off, over and over, as a thread does that stores beside
        // code of its own: the store is refused now and then, and when this thread
        // asks, the mark is sometimes there and sometimes gone again. Either way
        // the fault is not the guest's (R057).
        let code = [0x89, 0x1e, 0x83, 0xeb, 0x01, 0x75, 0xf9, 0x0f, 0x05];
        let far = CODE + 0x10_0000;
        let mut m = Mem::reserve().unwrap();
        m.map(CODE, 4096, R | W | X).unwrap();
        m.write_bytes(CODE, &code).unwrap();
        m.map(far, 4096, R | W).unwrap();
        m.map(DATA, 4096, R | W).unwrap();
        let mut other = m.view();
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|s| {
            s.spawn(|| {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    other.mark_code(far, far + 1, true).unwrap();
                    other.mark_code(far, far + 1, false).unwrap();
                }
            });
            let mut jit = Jit::new(false, 1).unwrap();
            let mut c = Cpu::new(CODE, DATA + 4096);
            c.reg[RBX] = 300_000;
            c.reg[RSI] = far;
            let event = jit.run(&mut c, &mut m);
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            assert!(matches!(event, Event::Syscall), "the guest was handed {:?}", match event { Event::Stop(stop) => format!("{stop:?}"), _ => "something else".to_string() });
            assert_eq!(c.reg[RBX], 0);
        });
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
    fn a_kept_block_is_taken_only_where_every_byte_it_rests_on_is_the_same() {
        use crate::jit::cache;
        // add eax, ebx; jmp +0 | add ecx, edx; syscall: the block is the first two,
        // and it read the third to find the add's flags dead (R084, ADR 0021).
        let across = [0x01, 0xd8, 0xeb, 0x00, 0x01, 0xd1, 0x0f, 0x05];
        let m = guest(&across, &[]);
        let built = translate(&m, CODE, false).unwrap();
        assert!(built.read.contains(&(CODE + 4, CODE + 6)), "{:x?}", built.read);
        let e = cache::entry(CODE, &built, &m).unwrap();
        let back = cache::read(&e, &m).expect("the same bytes: taken");
        assert_eq!((&back.words, &back.starts, back.end, back.seen, &back.pads, &back.lookups, back.body), (&built.words, &built.starts, built.end, built.seen, &built.pads, &built.lookups, built.body));
        // A byte of the block's own changed (add eax, ebx to add eax, edx), and one of
        // what it read past its end (add ecx, edx to add ebx, edx): not taken.
        for (at, byte) in [(1, 0xd0), (5, 0xd3)] {
            let mut other = across;
            other[at] = byte;
            assert!(cache::read(&e, &guest(&other, &[])).is_none(), "byte {at} changed");
        }
        // In a file, by key; a second block added keeps the first.
        let file = cache::merged(&[], 0, &[(cache::key(CODE, false), e.clone())]).unwrap();
        assert!(cache::find_in(&file, 1, cache::key(CODE, false), &m).is_some());
        assert!(cache::find_in(&file, 1, cache::key(CODE, true), &m).is_none(), "made ordered is another key");
        let second = translate(&m, CODE + 4, false).unwrap();
        let file = cache::merged(&file, 1, &[(cache::key(CODE + 4, false), cache::entry(CODE + 4, &second, &m).unwrap())]).unwrap();
        assert_eq!(cache::find_in(&file, 2, cache::key(CODE, false), &m).map(|b| b.words), Some(built.words));
        assert_eq!(cache::find_in(&file, 2, cache::key(CODE + 4, false), &m).map(|b| b.end), Some(second.end));
        // Other code at the same address, made again and added: the new entry is
        // taken, and the file keeps no bytes of the one it replaced (R087: such
        // orphans grew one file to 1.1 GB).
        let mut other = across;
        other[1] = 0xd0;
        let o = guest(&other, &[]);
        let again = cache::entry(CODE, &translate(&o, CODE, false).unwrap(), &o).unwrap();
        let before = file.len();
        let file = cache::merged(&file, 2, &[(cache::key(CODE, false), again.clone())]).unwrap();
        assert!(cache::find_in(&file, 2, cache::key(CODE, false), &o).is_some(), "the new entry");
        assert!(cache::find_in(&file, 2, cache::key(CODE, false), &m).is_none(), "not the old");
        assert!(cache::find_in(&file, 2, cache::key(CODE + 4, false), &m).is_some(), "the other block kept");
        assert_eq!(file.len(), before - e.len() + again.len());
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
        // The loop is: the look for a signal, add, cmp, the count, the jump back to
        // the look.
        let built = translate(&m, CODE, false).unwrap();
        assert_eq!(built.words[4], a64::b_cond(a64::Cond::Ne, -4));
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
    fn jumps_after_a_shift_and_a_multiplication_are_in_the_block() {
        // R075. Each of the jumps on zero and sign after a shift by a constant (SHL,
        // SHR, SAR of 32 and 64 bits), and on overflow and carry after MUL (32 and
        // 64), by blocks checked against the interpreter, with values that take each
        // way; and the jump in the block with the instruction before it.
        //   <first>; j<cc> +3; add edx, 1; add edx, 16; syscall
        let mut cases: Vec<Vec<u8>> = Vec::new();
        for first in [&[0xc1, 0xe8, 0x03][..], &[0x48, 0xc1, 0xe8, 0x3f], &[0xc1, 0xe0, 0x1f], &[0x48, 0xc1, 0xf8, 0x05], &[0xd1, 0xe8], &[0x48, 0xd1, 0xe0]] {
            for cc in [4u8, 5, 8, 9] {
                let mut v = first.to_vec();
                v.extend([0x70 | cc, 0x03, 0x83, 0xc2, 0x01, 0x83, 0xc2, 0x10, 0x0f, 0x05]);
                cases.push(v);
            }
        }
        for first in [&[0xf7, 0xe3][..], &[0x48, 0xf7, 0xe3]] {
            for cc in [0u8, 1, 2, 3] {
                let mut v = first.to_vec();
                v.extend([0x70 | cc, 0x03, 0x83, 0xc2, 0x01, 0x83, 0xc2, 0x10, 0x0f, 0x05]);
                cases.push(v);
            }
        }
        for code in &cases {
            for (a, b) in [(0u64, 0u64), (1, 1), (8, 3), (0x8000_0000, 2), (0xffff_ffff, 0xffff_ffff), (1 << 63, 2), (u64::MAX, 1), (7, 1 << 40)] {
                let set = |c: &mut Cpu| {
                    c.reg[RAX] = a;
                    c.reg[RBX] = b;
                    c.reg[RDX] = 0;
                };
                checked(code, &[], &set);
            }
            let m = guest(code, &[]);
            assert!(translate(&m, CODE, false).is_some_and(|b| b.starts.len() >= 2), "{code:02x?}: the jump is not in the block");
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
        // they do not (parity; after a shift, any condition but zero and sign: R075).
        let block_of = |code: &[u8]| {
            let m = guest(code, &[]);
            translate(&m, CODE, false).map(|b| b.starts.len())
        };
        assert_eq!(block_of(&[0x39, 0xd8, 0x0f, 0x94, 0xc2, 0x0f, 0x44, 0xcf, 0x0f, 0x05]), Some(3));
        assert_eq!(block_of(&[0x39, 0xd8, 0x0f, 0x9a, 0xc2, 0x0f, 0x05]), Some(1));
        assert_eq!(block_of(&[0xc1, 0xe0, 0x03, 0x0f, 0x94, 0xc2, 0x0f, 0x05]), Some(2));
        assert_eq!(block_of(&[0xc1, 0xe0, 0x03, 0x0f, 0x92, 0xc2, 0x0f, 0x05]), Some(1));
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
        // Where the host's flags do not say what the carry is, it is taken from the
        // record if the record says its carry was given, and the instruction is left
        // to the interpreter from inside the block if not (R043; before, the block
        // ended before it): at a block's start, and after a shift.
        let block_of = |code: &[u8]| {
            let m = guest(code, &[]);
            translate(&m, CODE, false).map(|b| b.starts.len())
        };
        assert_eq!(block_of(&[0x11, 0xf9, 0x0f, 0x05]), Some(1));
        assert_eq!(block_of(&[0xc1, 0xe0, 0x03, 0x11, 0xf9, 0x0f, 0x05]), Some(2));
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
            // Into the upper half: movlhps xmm1, xmm2; movhps and movhpd xmm1, [rsi].
            &[0x0f, 0x16, 0xca], &[0x0f, 0x16, 0x0e], &[0x66, 0x0f, 0x16, 0x0e], &[0x45, 0x0f, 0x16, 0xc1],
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
        // (Ordered, an access of 16 bytes that is not aligned and crosses a line of
        // 16 is refused by the host and made by the interpreter: one of these, twice.)
        let ordered = std::env::var_os("R64M_ORDERED").is_some();
        assert_eq!(n as usize, list.len() * 2 - if ordered { 2 } else { 0 });
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
        // And of the whole register by bytes: psrldq and pslldq.
        for (op, fields) in [(0x71u8, &[2u8, 4, 6][..]), (0x72, &[2, 4, 6]), (0x73, &[2, 3, 6, 7])] {
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
        // What is not read: a shift of memory, and a shift of 64 bits with its sign.
        for code in [&[0x66u8, 0x0f, 0x72, 0x16, 1][..], &[0x66, 0x0f, 0x73, 0xe1, 1]] {
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
    #[test]
    fn a_rotation_at_a_blocks_end_is_checked_as_the_interpreter_has_it() {
        // 63 times mov eax, eax; then ror edi, 9 as the 64th instruction, which is the
        // most a block holds; then add edx, ebx, which writes the flags the rotation
        // changed, and syscall. With every block checked the rotation's flags are
        // compared at its block's end, so it may not be in that block; without, it is,
        // and nothing reads them.
        let mut code = [0x89u8, 0xc0].repeat(63);
        code.extend([0xc1, 0xcf, 0x09, 0x01, 0xda, 0x0f, 0x05]);
        let (c, _) = checked(&code, &[], &|c| {
            c.reg[crate::cpu::RDI] = 0x8000_0101;
            c.reg[RBX] = 1;
        });
        assert_eq!((c.reg[crate::cpu::RDI], c.reg[RDX], c.steps), (0x80c0_0000, 1, 66));
        assert_eq!((shape(&code, true).0, shape(&code, false).0), (63, 64));
    }
    #[test]
    fn operands_relative_to_fs_and_gs() {
        // What a function compiled with the stack protector does: mov rax, fs:[28h];
        // sub rax, fs:[28h]. And: mov fs:[rsi + 8], rbx; add eax, gs:[rbx*4 + 10h];
        // movsd xmm1, fs:[30h]; lea rax, fs:[rsi + 8], which is the address without
        // the segment.
        let list = [
            (vec![0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0, 0, 0], 1),
            (vec![0x64, 0x48, 0x2b, 0x04, 0x25, 0x28, 0, 0, 0], 1),
            (vec![0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0, 0, 0, 0x64, 0x48, 0x2b, 0x04, 0x25, 0x28, 0, 0, 0, 0x75, 0x02, 0x89, 0xc2], 3),
            (vec![0x64, 0x48, 0x89, 0x5e, 0x08], 1),
            (vec![0x65, 0x03, 0x04, 0x9d, 0x10, 0, 0, 0], 1),
            (vec![0xf2, 0x64, 0x0f, 0x10, 0x0c, 0x25, 0x30, 0, 0, 0], 1),
            (vec![0x64, 0x48, 0x8d, 0x46, 0x08], 1),
        ];
        let data: Vec<u8> = (0..=255u8).collect();
        let n = sse(&list, 2, &|_| data.clone(), &|case, c| {
            fill_xmm(c);
            c.fs_base = DATA + 0x40 + 8 * case as u64;
            c.gs_base = DATA + 0x80;
            (c.reg[RAX], c.reg[RBX], c.reg[RSI]) = (0x1111_2222_3333_4444, 3, 0x20);
        });
        // (One of them is two blocks where each is checked: it has a jump.)
        assert_eq!(n as usize, (list.len() + 1) * 2);
        // Said outright: the word at FS + 28h, and LEA's answer.
        let (c, m) = checked(&[0x64, 0x48, 0x8b, 0x04, 0x25, 0x28, 0, 0, 0, 0x64, 0x48, 0x8d, 0x5e, 0x08, 0x0f, 0x05], &data, &|c| {
            c.fs_base = DATA + 0x40;
            c.reg[RSI] = 0x20;
        });
        assert_eq!((c.reg[RAX], c.reg[RBX]), (m.read_u64(DATA + 0x68).unwrap(), 0x28));
    }

    #[test]
    fn bit_tests_where_only_the_carry_is_asked_for() {
        // bt, bts, btr, btc of EAX and of RAX, by EBX/RBX and by a constant.
        let mut tests: Vec<Vec<u8>> = Vec::new();
        for rex in [&[][..], &[0x48][..]] {
            for op in [0xa3u8, 0xab, 0xb3, 0xbb] {
                tests.push([rex, &[0x0f, op, 0xd8]].concat());
            }
            for field in 4..8u8 {
                for n in [0u8, 5, 31, 32, 63, 200] {
                    tests.push([rex, &[0x0f, 0xba, 0xc0 | field << 3, n]].concat());
                }
            }
        }
        let values = [(0u64, 0u64), (u64::MAX, 5), (0x8000_0000_0000_0020, 5), (0x8000_0000_0000_0020, 63), (0x1_0000_0000, 32), (0xffff_fffe, 64), (0x20, 0xffff_ff05), (1 << 31, 31)];
        let set = |case: usize, c: &mut Cpu| {
            (c.reg[RAX], c.reg[RBX]) = values[case];
            c.reg[RDX] = 0x100;
            // Flags that a bit test leaves and the instruction after writes over.
            c.rflags |= 0x8c4;
        };
        // Read by setc and by adc, then all written over: in one block, checked.
        let mut list = Vec::new();
        for t in &tests {
            list.push(([&t[..], &[0x0f, 0x92, 0xc1, 0x83, 0xc2, 0x05]].concat(), 3));
            list.push(([&t[..], &[0x83, 0xd2, 0x00]].concat(), 2));
        }
        let n = sse(&list, values.len(), &|_| Vec::new(), &set);
        assert_eq!(n as usize, list.len() * values.len());
        // Read by a jump on the carry, with both ways writing the flags over: jc
        // over; add edx, 1; over: add edx, 2. The block goes on past the jump, which
        // a checked block does not: held to the interpreter by the whole run.
        for t in &tests {
            let code = [&t[..], &[0x72, 0x03, 0x83, 0xc2, 0x01, 0x83, 0xc2, 0x02, 0x0f, 0x05]].concat();
            assert_eq!(shape(&code, false).0, 4, "{t:02x?}");
            let mut m = guest(&code, &[]);
            let mut jit = Jit::new(false, 1).unwrap();
            for case in 0..values.len() {
                both(&mut jit, &mut m, CODE, &|c| set(case, c));
            }
            assert_eq!(jit.stats.in_blocks as usize, values.iter().map(|&(a, b)| if carry_of(t, a, b) { 3 } else { 4 }).sum::<usize>());
        }
        // Where the other flags are read after it (every condition, and nothing
        // writes them again), the record says that the carry was given since: of
        // the flags as they stood, and of a comparison's before it (cmp ecx, edx).
        let alone: Vec<&[u8]> = tests.iter().step_by(5).map(|t| &t[..]).collect();
        let after: Vec<Vec<u8>> = tests.iter().step_by(5).map(|t| [&[0x39, 0xd1][..], t].concat()).collect();
        let after: Vec<&[u8]> = after.iter().map(|t| &t[..]).collect();
        for &(a, b) in &values[..4] {
            for (i, list) in [&alone, &after].into_iter().enumerate() {
                for (x, y) in [(1u64, 2u64), (2, 1), (5, 5), (0x8000_0000, 1)] {
                    under_every_condition(list, &[], &|c| {
                        (c.reg[RAX], c.reg[RBX], c.reg[crate::cpu::RCX], c.reg[RDX]) = (a, b, x, y);
                        c.rflags |= 0x8c4;
                    });
                }
                // And it is in a block, with the compare if there is one.
                for t in list.iter() {
                    assert_eq!(shape(&[t, &pick(4)[..]].concat(), true).0, 1 + i, "{t:02x?}");
                }
            }
        }
        // Not read: of memory, and the rest of the group.
        for code in [&[0x0fu8, 0xa3, 0x1e][..], &[0x0f, 0xba, 0x26, 1], &[0x0f, 0xba, 0xc0, 1]] {
            assert!(translate(&guest(code, &[]), CODE, false).is_none(), "{code:02x?}");
        }
    }

    /// The carry a bit test of `a` by `b` (or by its own constant) gives.
    fn carry_of(test: &[u8], a: u64, b: u64) -> bool {
        let wide = test[0] == 0x48;
        let rest = &test[wide as usize..];
        let n = if rest[1] == 0xba { rest[3] as u64 } else { b } & if wide { 63 } else { 31 };
        a >> n & 1 != 0
    }

    #[test]
    fn a_no_operation_of_two_bytes_is_in_its_block() {
        // xchg ax, ax; add edx, 1; syscall
        let code = [0x66, 0x90, 0x83, 0xc2, 0x01, 0x0f, 0x05];
        let (c, _) = checked(&code, &[], &|c| c.reg[RAX] = 0x1234_5678);
        assert_eq!((c.reg[RAX], c.reg[RDX], shape(&code, true).0), (0x1234_5678, 1, 2));
    }
    #[test]
    fn a_change_of_mapping_forgets_what_was_made_there_and_nothing_else() {
        // mov edx, 7; syscall at CODE; then the page is mapped anew with mov edx, 9.
        let mut m = guest(&[0xba, 7, 0, 0, 0, 0x0f, 0x05], &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let run = |jit: &mut Jit, m: &mut Mem| {
            let mut c = Cpu::new(CODE, DATA + 4096);
            assert!(matches!(jit.run(&mut c, m), Event::Syscall));
            c.reg[RDX]
        };
        assert_eq!(run(&mut jit, &mut m), 7);
        // A change somewhere else (memory for data, which is nearly every change):
        // the block is kept and run again.
        m.map(DATA + 0x10_0000, 4096, R | W).unwrap();
        jit.guest_changed(DATA + 0x10_0000, DATA + 0x10_1000, &mut m);
        assert_eq!((run(&mut jit, &mut m), jit.stats.blocks, jit.stats.entered), (7, 1, 2));
        // The code's own page mapped anew with other code: what was made is dropped,
        // and the new code is what runs.
        m.map(CODE, 4096, R | W).unwrap();
        m.write_bytes(CODE, &[0xba, 9, 0, 0, 0, 0x0f, 0x05]).unwrap();
        m.protect(CODE, 4096, R | X).unwrap();
        jit.guest_changed(CODE, CODE + 4096, &mut m);
        assert_eq!((run(&mut jit, &mut m), jit.stats.blocks), (9, 2));
        // A range that only begins before the code's page reaches it too.
        m.map(CODE, 4096, R | W).unwrap();
        m.write_bytes(CODE, &[0xba, 11, 0, 0, 0, 0x0f, 0x05]).unwrap();
        m.protect(CODE, 4096, R | X).unwrap();
        jit.guest_changed(CODE - 0x10_0000, CODE + 1, &mut m);
        assert_eq!((run(&mut jit, &mut m), jit.stats.blocks), (11, 3));
    }
    #[test]
    fn only_a_block_that_a_cycle_passes_through_looks_for_a_signal() {
        // The look: a branch if the top bit of the count is set (wherever to).
        let look = a64::tbnz_top(27, 0);
        let begins = |code: &[u8]| translate(&guest(code, &[]), CODE, false).unwrap().words[0] & !(0x3fff << 5);
        // A straight run, and a jump forward: no look.
        assert_ne!(begins(&[0x83, 0xc2, 0x01, 0x0f, 0x05]), look);
        assert_ne!(begins(&[0x83, 0xc2, 0x01, 0xeb, 0x02, 0x90, 0x90, 0x0f, 0x05]), look);
        // A jump back to itself, a jump back to before itself, a return, a call
        // through a register: each looks.
        assert_eq!(begins(&[0x83, 0xea, 0x01, 0x75, 0xfb, 0x0f, 0x05]), look);
        assert_eq!(begins(&[0x83, 0xea, 0x01, 0xeb, 0xf0]), look);
        assert_eq!(begins(&[0x83, 0xc2, 0x01, 0xc3]), look);
        assert_eq!(begins(&[0x83, 0xc2, 0x01, 0xff, 0xd0]), look);
        // A loop is left when the word is set, at its start, with what it had done.
        let code = [0x83, 0xc2, 0x01, 0x83, 0xeb, 0x01, 0x75, 0xf8, 0x0f, 0x05];
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        c.reg[RBX] = 1000;
        c.kick = 1;
        assert!(matches!(jit.run(&mut c, &mut m), Event::Signal));
        assert_eq!((c.rip, c.steps), (CODE, 0));
        c.kick = 0;
        assert!(matches!(jit.run(&mut c, &mut m), Event::Syscall));
        assert_eq!((c.reg[RDX], c.reg[RBX]), (1000, 0));
    }

    #[test]
    fn inc_and_dec_keep_the_carry_and_write_the_rest() {
        // inc/dec of EAX and RAX; every condition after; the carry before both ways.
        //   stc or clc is not translated, so the carry comes from cmp ebx, 1 (EBX 0 or 1):
        //   cmp ebx, 1; inc/dec; jcc taken; mov edx, 1; jmp end; taken: mov edx, 2;
        //   end: adc edx, 0; syscall
        // The ADC reads the carry the INC kept, from the record: the jump before it
        // ended the block the INC was in.
        let values = [0u64, 1, 2, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0x7fff_ffff_ffff_ffff, 1 << 63, u64::MAX, 0x1_0000_0000];
        let mut code = Vec::new();
        let mut programs = Vec::new();
        for form in [&[0xff, 0xc0][..], &[0xff, 0xc8], &[0x48, 0xff, 0xc0], &[0x48, 0xff, 0xc8]] {
            for cc in 0..16u8 {
                programs.push(CODE + code.len() as u64);
                code.extend([0x83, 0xfb, 0x01]);
                code.extend(form);
                code.extend([0x70 | cc, 7, 0xba, 1, 0, 0, 0, 0xeb, 5, 0xba, 2, 0, 0, 0, 0x83, 0xd2, 0x00, 0x0f, 0x05]);
            }
        }
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(true, 1).unwrap();
        let mut joined = Jit::new(false, 1).unwrap();
        for &at in &programs {
            for &v in &values {
                for carry in [0u64, 1] {
                    let set = |c: &mut Cpu| (c.reg[RAX], c.reg[RBX]) = (v, 1 - carry);
                    both(&mut jit, &mut m, at, &set);
                    let c = both(&mut joined, &mut m, at, &set);
                    assert!(matches!(c.reg[RDX], 1..=3));
                }
            }
        }
        assert!(jit.stats.checked as usize >= programs.len());
        // Of memory, at both widths, the flags read by SETcc in the same block and
        // then by a jump on the carry in the next.
        //   cmp ebx, 1; inc/dec [rsi]; seto dl; jmp +0; jc over; add edx, 4; over: syscall
        for form in [&[0xff, 0x06][..], &[0xff, 0x0e], &[0x48, 0xff, 0x06], &[0x48, 0xff, 0x0e]] {
            for &v in &values {
                for carry in [0u64, 1] {
                    let code = [&[0x83, 0xfb, 0x01][..], form, &[0x0f, 0x90, 0xc2, 0xeb, 0x00, 0x72, 0x03, 0x83, 0xc2, 0x04, 0x0f, 0x05]].concat();
                    let (c, m) = checked(&code, &v.to_le_bytes(), &|c| {
                        (c.reg[RSI], c.reg[RBX], c.reg[RDX]) = (DATA, 1 - carry, 0);
                    });
                    let wide = form[0] == 0x48;
                    let dec = form[form.len() - 1] == 0x0e;
                    let mask = if wide { u64::MAX } else { 0xffff_ffff };
                    let want = if dec { v.wrapping_sub(1) } else { v.wrapping_add(1) } & mask | v & !mask;
                    assert_eq!(m.read_u64(DATA).unwrap(), want, "{form:02x?} of {v:#x}");
                    let sign = if wide { 1 << 63 } else { 1 << 31 };
                    let over = if dec { v & mask == sign } else { v & mask == sign - 1 };
                    assert_eq!(c.reg[RDX], over as u64 + 4 * (1 - carry), "{form:02x?} of {v:#x}, carry {carry}");
                }
            }
        }
    }

    #[test]
    fn a_carry_goes_round_a_loop_through_dec_and_a_jump_without_leaving_blocks() {
        // Four words at RSI added to (or taken from) by four at RDI, as hand-written
        // code does it: the carry of one round is the next round's, kept by DEC (or
        // INC) across the jump.
        //   cmp ebx, 1          (the carry in: EBX 0 sets it)
        //   top: mov rax, [rsi]; adc/sbb rax, [rdi]; mov [rsi], rax
        //        lea rsi, [rsi+8]; lea rdi, [rdi+8]; dec ecx / inc rcx; jnz top
        //   setc dl; syscall
        let a = [u64::MAX, u64::MAX, 0, 5];
        let b = [1u64, 0, 0, 7];
        let data: Vec<u8> = a.iter().chain(&b).flat_map(|v| v.to_le_bytes()).collect();
        for sub in [false, true] {
            for (count, start) in [(&[0xff, 0xc9][..], 4u64), (&[0x48, 0xff, 0xc1], (-4i64) as u64)] {
                for carry in [0u64, 1] {
                    let mut code = vec![0x83, 0xfb, 0x01, 0x48, 0x8b, 0x06, 0x48, if sub { 0x1b } else { 0x13 }, 0x07, 0x48, 0x89, 0x06];
                    code.extend([0x48, 0x8d, 0x76, 0x08, 0x48, 0x8d, 0x7f, 0x08]);
                    code.extend(count);
                    let back = 3i64 - (code.len() as i64 + 2);
                    code.extend([0x75, back as u8, 0x0f, 0x92, 0xc2, 0x0f, 0x05]);
                    let set = |c: &mut Cpu| {
                        (c.reg[RSI], c.reg[7], c.reg[1], c.reg[RBX], c.reg[RDX]) = (DATA, DATA + 32, start, 1 - carry, 0);
                    };
                    let (c, m) = checked(&code, &data, &set);
                    // What it should be, a word at a time.
                    let mut want = [0u64; 4];
                    let mut k = carry;
                    for i in 0..4 {
                        let (r, k1) = if sub { a[i].overflowing_sub(b[i]) } else { a[i].overflowing_add(b[i]) };
                        let (r, k2) = if sub { r.overflowing_sub(k) } else { r.overflowing_add(k) };
                        (want[i], k) = (r, (k1 | k2) as u64);
                    }
                    let got: Vec<u64> = (0..4).map(|i| m.read_u64(DATA + 8 * i).unwrap()).collect();
                    assert_eq!((&got[..], c.reg[RDX]), (&want[..], k), "sub {sub}, carry {carry}");
                    // And all of it in blocks but the two at the end (SETC asks for
                    // the carry, which the host's flags after a DEC do not say, and
                    // the SYSCALL): nothing in the loop is the interpreter's.
                    let mut m = guest(&code, &data);
                    let mut jit = Jit::new(false, 1).unwrap();
                    let mut c = Cpu::new(CODE, DATA + 4096);
                    set(&mut c);
                    assert!(matches!(jit.run(&mut c, &mut m), Event::Syscall));
                    assert_eq!(jit.stats.in_blocks, c.steps - 2, "sub {sub}, by {count:02x?}");
                }
            }
        }
    }

    #[test]
    fn leave_a_push_of_an_operand_a_hint_and_a_prefix_that_does_nothing() {
        // push rbp; mov rbp, rsp; sub rsp, 32; leave
        // push qword [rsi]; push r8 (FFh /6); pop rax; pop rbx
        // prefetcht0 [rsi]; 67h mul rbx; 67h mov rdi, rax; syscall
        let code = [
            0x55, 0x48, 0x89, 0xe5, 0x48, 0x83, 0xec, 0x20, 0xc9, 0xff, 0x36, 0x41, 0xff, 0xf0, 0x58, 0x5b, 0x0f, 0x18, 0x0e, 0x67, 0x48,
            0xf7, 0xe3, 0x67, 0x48, 0x89, 0xc7, 0x0f, 0x05,
        ];
        let (c, _) = checked(&code, &0x1_0000_0003u64.to_le_bytes(), &|c| {
            (c.reg[RSI], c.reg[8], c.reg[5]) = (DATA, 0x2_0000_0005, 0x1234);
        });
        assert_eq!((c.reg[5], c.reg[4]), (0x1234, DATA + 4096));
        assert_eq!((c.reg[RAX], c.reg[RDX], c.reg[7]), (0x0000_000b_0000_000f, 2, 0x0000_000b_0000_000f));
        let mut m = guest(&code, &0x1_0000_0003u64.to_le_bytes());
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        c.reg[RSI] = DATA;
        assert!(matches!(jit.run(&mut c, &mut m), Event::Syscall));
        assert_eq!(jit.stats.in_blocks, c.steps - 1);
    }

    #[test]
    fn inc_and_dec_whose_flags_nothing_reads() {
        // inc eax; add edx, 1; syscall: the flags are written over at once.
        let (c, _) = checked(&[0xff, 0xc0, 0x83, 0xc2, 0x01, 0x0f, 0x05], &[], &|c| (c.reg[RAX], c.reg[RDX]) = (0xffff_ffff, 6));
        assert_eq!((c.reg[RAX], c.reg[RDX]), (0, 7));
        // A count down with both ways of the jump writing the flags over:
        //   top: add edx, 3; dec rbx; jnz top; add edx, 1; syscall
        let code = [0x83, 0xc2, 0x03, 0x48, 0xff, 0xcb, 0x75, 0xf8, 0x83, 0xc2, 0x01, 0x0f, 0x05];
        let (c, _) = checked(&code, &[], &|c| (c.reg[RBX], c.reg[RDX]) = (50, 0));
        assert_eq!((c.reg[RBX], c.reg[RDX]), (0, 151));
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        c.reg[RBX] = 50;
        assert!(matches!(jit.run(&mut c, &mut m), Event::Syscall));
        assert_eq!(jit.stats.in_blocks, c.steps - 1);
    }

    #[test]
    fn an_adc_takes_its_carry_from_the_block_before_and_what_is_left_goes_back_to_blocks() {
        // add rax, rbx; jmp +0; adc rdx, 0; add esi, 1; syscall: the ADC begins a
        // block and finds the addition's record. All of it is in blocks.
        let wide = [0x48, 0x01, 0xd8, 0xeb, 0x00, 0x48, 0x83, 0xd2, 0x00, 0x83, 0xc6, 0x01, 0x0f, 0x05];
        // The same with an addition of 32 bits, whose record the ADC is not told the
        // carry by: it is the interpreter's, and the instruction after it is in a
        // block again (before R043 the interpreter went on to the next jump).
        let narrow = [0x01, 0xd8, 0xeb, 0x00, 0x48, 0x83, 0xd2, 0x00, 0x83, 0xc6, 0x01, 0x0f, 0x05];
        for (code, interpreted) in [(&wide[..], 1), (&narrow[..], 2)] {
            for (a, b) in [(u64::MAX, 1u64), (5, 6), (0xffff_ffff, 1), (1 << 63, 1 << 63)] {
                let set = |c: &mut Cpu| (c.reg[RAX], c.reg[RBX], c.reg[RDX], c.reg[RSI]) = (a, b, 10, 0);
                let (c, _) = checked(code, &[], &set);
                let carry = if code.len() == wide.len() { a.checked_add(b).is_none() } else { (a as u32).checked_add(b as u32).is_none() };
                assert_eq!((c.reg[RDX], c.reg[RSI]), (10 + carry as u64, 1));
                let mut m = guest(code, &[]);
                let mut jit = Jit::new(false, 1).unwrap();
                let mut c = Cpu::new(CODE, DATA + 4096);
                set(&mut c);
                assert!(matches!(jit.run(&mut c, &mut m), Event::Syscall));
                assert_eq!(jit.stats.in_blocks, c.steps - interpreted);
            }
        }
    }

    #[test]
    fn the_bytes_top_bits_a_lower_half_from_memory_and_the_smaller_and_larger_bytes() {
        // pmovmskb eax, xmm1; pmovmskb r8d, xmm2; pmovmskb ebx, xmm8
        // movlpd xmm3, [rsi]; movlps xmm4, [rsi+8]
        // pminub xmm5, xmm1; pmaxub xmm6, [rsi]; syscall
        let code = [
            0x66, 0x0f, 0xd7, 0xc1, 0x66, 0x44, 0x0f, 0xd7, 0xc2, 0x66, 0x41, 0x0f, 0xd7, 0xd8, 0x66, 0x0f, 0x12, 0x1e, 0x0f, 0x12, 0x66,
            0x08, 0x66, 0x0f, 0xda, 0xe9, 0x66, 0x0f, 0xde, 0x36, 0x0f, 0x05,
        ];
        let data: Vec<u8> = (0..16u8).map(|i| i.wrapping_mul(37) ^ 0x5a).collect();
        let values: [(u64, u64); 6] = [
            (0, 0),
            (u64::MAX, u64::MAX),
            (0x8000_0000_0000_0080, 0x0080_0000_0000_8000),
            (0x7f7f_7f7f_7f7f_7f7f, 0x8080_8080_8080_8080),
            (0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210),
            (0xff00_ff00_ff00_ff00, 0x00ff_00ff_00ff_00ff),
        ];
        let mask = |(lo, hi): (u64, u64)| (0..16).fold(0u64, |m, i| m | ((if i < 8 { lo >> (8 * i + 7) } else { hi >> (8 * (i - 8) + 7) }) & 1) << i);
        for (k, &v) in values.iter().enumerate() {
            let (w, x) = (values[(k + 1) % 6], values[(k + 2) % 6]);
            let (c, _) = checked(&code, &data, &|c| {
                fill_xmm(c);
                (c.xmm[2], c.xmm[3]) = v;
                (c.xmm[4], c.xmm[5]) = w;
                (c.xmm[16], c.xmm[17]) = x;
                (c.reg[RSI], c.reg[RAX], c.reg[RBX], c.reg[8]) = (DATA, u64::MAX, u64::MAX, u64::MAX);
            });
            assert_eq!((c.reg[RAX], c.reg[8], c.reg[RBX]), (mask(v), mask(w), mask(x)), "of {v:x?}");
        }
        let mut m = guest(&code, &data);
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        c.reg[RSI] = DATA;
        assert!(matches!(jit.run(&mut c, &mut m), Event::Syscall));
        assert_eq!(jit.stats.in_blocks, c.steps - 1);
    }

    #[test]
    fn a_return_address_has_one_landing_whichever_block_holds_its_call() {
        // mov ebx, 3; top: add edx, 1; call f; sub ebx, 1; jnz top; syscall
        // f: add esi, 2; ret
        // The block from the start and the block from `top` both hold the call: the
        // return address has one landing (R046), and it goes when the code does.
        let code = [0xbb, 3, 0, 0, 0, 0x83, 0xc2, 0x01, 0xe8, 0x07, 0, 0, 0, 0x83, 0xeb, 0x01, 0x75, 0xf3, 0x0f, 0x05, 0x83, 0xc6, 0x02, 0xc3];
        let (c, _) = checked(&code, &[], &|c| (c.reg[RDX], c.reg[RSI]) = (0, 0));
        assert_eq!((c.reg[RDX], c.reg[RSI], c.reg[RBX]), (3, 6, 0));
        let mut m = guest(&code, &[]);
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        assert!(matches!(jit.run(&mut c, &mut m), Event::Syscall));
        assert_eq!((c.reg[RDX], c.reg[RSI]), (3, 6));
        assert_eq!(jit.stats.in_blocks, c.steps - 1);
        assert_eq!(jit.pads.keys().copied().collect::<Vec<_>>(), [CODE + 13]);
        // Where the table has the return address, it has the landing: not the block.
        let block = match jit.places[&(CODE + 13)] {
            Place::Block(id) => jit.blocks[id].code as u64,
            _ => panic!("no block at the return address"),
        };
        let place = jit.table[Jit::place(jit.table.len(), CODE + 13)];
        assert_eq!((place[0], place[1]), (CODE + 13, jit.pads[&(CODE + 13)]));
        assert_ne!(place[1], block);
        jit.guest_changed(CODE, CODE + 4096, &mut m);
        assert!(jit.pads.is_empty());
        assert_ne!(jit.table[Jit::place(jit.table.len(), CODE + 13)][0], CODE + 13);
    }

    #[test]
    fn a_compare_and_swap_and_a_swap_with_memory_are_in_blocks() {
        // [lock] cmpxchg [rsi], rbx / ebx; sete dl; jc +3; add edx, 4; syscall:
        // equal and not, the accumulator below and above what memory holds.
        let values = [0u64, 1, 5, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0x1_0000_0005, 1 << 63, u64::MAX];
        let after = [0x0f, 0x94, 0xc2, 0x72, 0x03, 0x83, 0xc2, 0x04, 0x0f, 0x05];
        for (form, bits) in [(&[0xf0, 0x48, 0x0f, 0xb1, 0x1e][..], 64), (&[0x48, 0x0f, 0xb1, 0x1e], 64), (&[0xf0, 0x0f, 0xb1, 0x1e], 32), (&[0x0f, 0xb1, 0x1e], 32)] {
            let mask = if bits == 64 { u64::MAX } else { 0xffff_ffff };
            for &held in &values {
                for &expected in &values {
                    let code = [form, &after[..]].concat();
                    let (c, m) = checked(&code, &held.to_le_bytes(), &|c| {
                        (c.reg[RAX], c.reg[RBX], c.reg[RSI], c.reg[RDX]) = (expected, 0x1234_5678_9abc_def0, DATA, 0);
                    });
                    let same = held & mask == expected & mask;
                    let now = m.read_u64(DATA).unwrap();
                    if same {
                        assert_eq!((now & mask, now & !mask, c.reg[RAX]), (0x1234_5678_9abc_def0 & mask, held & !mask, expected));
                    } else {
                        assert_eq!((now, c.reg[RAX]), (held, if bits == 64 { held } else { held & mask }));
                    }
                    assert_eq!(c.reg[RDX] & 1, same as u64);
                    // All of it in blocks: the interpreter ran the SYSCALL only.
                    assert_eq!(c.steps, if c.reg[RDX] & 4 != 0 { 5 } else { 4 });
                }
            }
        }
        // xchg [rsi], rbx / ebx; add edx, 1; syscall: the flags are left, and a jump
        // after it reads those of the comparison before.
        //   cmp edx, 1; xchg ...; jb +3; add edx, 4; syscall
        for (form, bits) in [(&[0x48, 0x87, 0x1e][..], 64), (&[0x87, 0x1e], 32), (&[0xf0, 0x48, 0x87, 0x1e], 64)] {
            let mask = if bits == 64 { u64::MAX } else { 0xffff_ffff };
            for &held in &values {
                for below in [0u64, 2] {
                    let code = [&[0x83, 0xfa, 0x01][..], form, &[0x72, 0x03, 0x83, 0xc2, 0x04, 0x0f, 0x05]].concat();
                    let (c, m) = checked(&code, &held.to_le_bytes(), &|c| (c.reg[RBX], c.reg[RSI], c.reg[RDX]) = (0x1111_2222_3333_4444, DATA, below));
                    let now = m.read_u64(DATA).unwrap();
                    assert_eq!((now & mask, now & !mask, c.reg[RBX]), (0x1111_2222_3333_4444 & mask, held & !mask, held & mask));
                    assert_eq!(c.reg[RDX], if below == 0 { 0 } else { 6 });
                }
            }
        }
    }

    #[test]
    fn a_short_repeated_move_or_store_is_a_loop_in_its_block() {
        use crate::cpu::{RCX, RDI};
        let data: Vec<u8> = (0..4096u32).map(|i| (i * 7 + 1) as u8).collect();
        // rep movsq, movsd, movsb, stosq, stosd, stosb; then add edx, 1; syscall.
        let forms: [&[u8]; 6] = [&[0xf3, 0x48, 0xa5], &[0xf3, 0xa5], &[0xf3, 0xa4], &[0xf3, 0x48, 0xab], &[0xf3, 0xab], &[0xf3, 0xaa]];
        for form in forms {
            // None, one, a few, the most a block does, and what is the interpreter's;
            // to a place apart, and one byte on (each element's store is read by
            // the next but one's load).
            for count in [0u64, 1, 2, 5, 63, 64, 65, 200] {
                for to in [DATA + 2048, DATA + 1] {
                    let code = [form, &[0x83, 0xc2, 0x01, 0x0f, 0x05][..]].concat();
                    let set = |c: &mut Cpu| {
                        (c.reg[RAX], c.reg[RCX], c.reg[RSI], c.reg[RDI], c.reg[RDX]) = (0x0102_0304_0506_0708, count, DATA, to, 0);
                        // As a program is run: all of a repetition in one step.
                        c.rep_whole = true;
                    };
                    // (Where the two places overlap the interpreter goes an element
                    // at a time, and the checker passes such a block over: there
                    // the whole run is held to the interpreter's, not each block.)
                    let c = if to == DATA + 1 {
                        both(&mut Jit::new(false, 1).unwrap(), &mut guest(&code, &data), CODE, &set)
                    } else {
                        checked(&code, &data, &set).0
                    };
                    // (RCX is the SYSCALL's by now.)
                    let bytes: u64 = if form.contains(&0x48) { 8 } else if form[1] & 1 == 1 { 4 } else { 1 };
                    assert_eq!((c.reg[RDI], c.reg[RDX]), (to + count * bytes, 1));
                    // As the interpreter counts: one for each repetition, one for none.
                    assert_eq!(c.steps, count.max(1) + 2);
                }
            }
        }
        // The flags of a comparison before it are read by a jump after it, and
        // by what follows the jump (uvicorn's start met this):
        //   cmp qword [rbx], 0; rep movsb; jne +7; mov edx, 1; jmp +5; mov edx, 2;
        //   adc edx, 0; sete al; syscall
        let code = [0x48, 0x83, 0x3b, 0x00, 0xf3, 0xa4, 0x75, 0x07, 0xba, 1, 0, 0, 0, 0xeb, 0x05, 0xba, 2, 0, 0, 0, 0x83, 0xd2, 0x00, 0x0f, 0x94, 0xc0, 0x0f, 0x05];
        for count in [0u64, 3, 63, 64, 100] {
            for (held, way) in [(0u64, 1u64), (5, 2)] {
                let mut data = data.clone();
                data[3000..3008].copy_from_slice(&held.to_le_bytes());
                let (c, _) = checked(&code, &data, &|c| {
                    (c.reg[RBX], c.reg[RCX], c.reg[RSI], c.reg[RDI], c.reg[RDX]) = (DATA + 3000, count, DATA, DATA + 2048, 0);
                    c.rep_whole = true;
                });
                // (The comparison's carry is clear; SETE reads the ADC's flags.)
                assert_eq!((c.reg[RDX], c.reg[RAX] & 1), (way, 0), "{count} repetitions, {held} compared");
            }
        }
        // The same not checked block by block, where a record that nothing in the
        // block reads is left out: with too many repetitions for the block the
        // interpreter runs the jump, and must find the comparison's record.
        //   cmp qword [rbx], 0; rep movsb; jne +5; add edx, 1; syscall; add edx, 2; syscall
        // (Both ways write the flags over at once: nothing but the jump reads them.)
        let code = [0x48, 0x83, 0x3b, 0x00, 0xf3, 0xa4, 0x75, 0x05, 0x83, 0xc2, 0x01, 0x0f, 0x05, 0x83, 0xc2, 0x02, 0x0f, 0x05];
        for count in [3u64, 64, 100] {
            for (held, way) in [(0u64, 1u64), (5, 2)] {
                let mut data = data.clone();
                data[3000..3008].copy_from_slice(&held.to_le_bytes());
                let set = |c: &mut Cpu| {
                    (c.reg[RBX], c.reg[RCX], c.reg[RSI], c.reg[RDI], c.reg[RDX]) = (DATA + 3000, count, DATA, DATA + 2048, 0);
                    c.rep_whole = true;
                };
                let c = both(&mut Jit::new(false, 1).unwrap(), &mut guest(&code, &data), CODE, &set);
                assert_eq!(c.reg[RDX], way, "{count} repetitions, {held} compared");
            }
        }
        // Going down (std is the interpreter's, and so then is the move): as the
        // interpreter has it.
        //   std; rep movsb; add edx, 1; syscall
        let (c, _) = checked(&[0xfd, 0xf3, 0xa4, 0x83, 0xc2, 0x01, 0x0f, 0x05], &data, &|c| {
            (c.reg[RCX], c.reg[RSI], c.reg[RDI], c.reg[RDX]) = (5, DATA + 100, DATA + 200, 0);
            c.rep_whole = true;
        });
        assert_eq!((c.reg[RSI], c.reg[RDX]), (DATA + 95, 1));
        // A fault part of the way: what was done stays done, the registers say how
        // far, and the count of instructions is the interpreter's. (Where the
        // host's page is the guest's: on a larger one the host lets generated code
        // store past the guest's page, as it does for every store.)
        if guest(&[0x90], &[]).host_page() != 4096 {
            return;
        }
        //   add edx, 1; rep stosq (to the last 24 bytes of the page and beyond)
        let code = [0x83, 0xc2, 0x01, 0xf3, 0x48, 0xab, 0x0f, 0x05];
        let set = |c: &mut Cpu| {
            (c.reg[RAX], c.reg[RCX], c.reg[RDI], c.reg[RDX]) = (9, 10, DATA + 4096 - 24, 0);
            c.rep_whole = true;
        };
        let mut alone = Cpu::new(CODE, DATA + 4096);
        set(&mut alone);
        let mut m = guest(&code, &data);
        let stopped = loop {
            match cpu::step(&mut alone, &mut m) {
                Ok(_) => {}
                Err(stop) => break stop,
            }
        };
        let mut m = guest(&code, &data);
        let mut jit = Jit::new(false, 1).unwrap();
        let mut c = Cpu::new(CODE, DATA + 4096);
        set(&mut c);
        let Event::Stop(stop) = jit.run(&mut c, &mut m) else { panic!("no fault") };
        assert_eq!(format!("{stop:?}"), format!("{stopped:?}"));
        assert_eq!((c.reg[RCX], c.reg[RDI], c.rip, c.steps), (alone.reg[RCX], alone.reg[RDI], alone.rip, alone.steps));
        assert_eq!((c.reg[RCX], m.read_u64(DATA + 4096 - 8)), (7, Ok(9)));
        assert!(jit.stats.faults >= 1);
    }
}
