//! The memory of a program without a kernel (ADR 0008).
//!
//! One reservation of the host's address space, as large as the host gives, with no
//! access at first. The guest's address `g` is the host's `base + g`. What the guest
//! maps is made accessible inside it; everything else stays a fault of the host's.
//!
//! Two views of the same memory are kept:
//!
//! - the list of regions: what the guest mapped, to the guest's 4 KB page, each with
//!   its protection. The interpreter asks it before every access, so the interpreter is
//!   exact on any host (ADR 0010);
//! - the host's protection, which is what generated code will rely on. A host page is
//!   as open as the most open guest page in it, so on a host with 16 KB pages it is
//!   coarser than the list (ADR 0008, point B).

use std::cell::Cell;
use std::io;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The guest's page: x86's.
pub const PAGE: u64 = 4096;

/// Protection bits, with Linux's values for `PROT_READ`, `PROT_WRITE`, `PROT_EXEC`.
pub const R: u8 = 1;
pub const W: u8 = 2;
pub const X: u8 = 4;
/// Not the guest's: the page holds code that has been translated (ADR 0008, point 3).
/// While it is set nothing may write the page, the host included, whatever `W` says.
pub const C: u8 = 8;

/// What an access was, for the fault it may end in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Fetch,
}

/// The fault a real CPU would raise: the first byte that could not be had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub addr: u64,
    pub access: Access,
}

/// A run of the guest's pages mapped with one protection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub prot: u8,
}

/// Copies `len` bytes to `to`, a byte at a time, and answers 1; or, if the host
/// refuses a store, answers 0: the handler of faults sends the thread on at the
/// routine's second ending (`refused_store`). Some of the bytes may have been
/// written by then.
#[unsafe(naked)]
unsafe extern "C" fn store_or_not(to: *mut u8, from: *const u8, len: usize) -> u64 {
    core::arch::naked_asm!(
        "cbz x2, 3f",
        "2: ldrb w3, [x1], #1",
        "strb w3, [x0], #1",
        "subs x2, x2, #1",
        "b.ne 2b",
        "3: mov x0, #1",
        "ret",
        "mov x0, #0",
        "ret",
    )
}

/// Adds nothing to the byte at `at`, as one act (`ldaddb wzr, w3, [x0]`), and
/// answers 1; or 0 if the host refuses the store, as `store_or_not` does.
#[unsafe(naked)]
unsafe extern "C" fn may_store(at: *mut u8) -> u64 {
    core::arch::naked_asm!(".inst 0x383f0003", "mov x0, #1", "ret", "mov x0, #0", "ret")
}

/// For the handler of faults: if the host's program counter is in one of the two
/// routines above, where the thread is to go on (the ending that answers 0).
pub fn refused_store(pc: usize) -> Option<usize> {
    let (copy, ask) = (store_or_not as *const () as usize, may_store as *const () as usize);
    if (copy..copy + 28).contains(&pc) {
        Some(copy + 28)
    } else if (ask..ask + 12).contains(&pc) {
        Some(ask + 12)
    } else {
        None
    }
}

#[derive(Debug)]
pub enum MapError {
    /// Not on a page, empty, or not inside the reservation (Linux's EINVAL or ENOMEM).
    Range,
    /// A change of protection over pages that are not all mapped (Linux's ENOMEM).
    NotMapped,
    /// The host refused. The list and the host may now disagree: not to be gone on from.
    Host(io::Error),
}

/// The list of regions as every thread of the process is to see it (ADR 0019): the
/// list itself, changed only by whoever holds it, and a number that goes up at each
/// change, by which a thread knows that its own copy is old.
struct Shared {
    regions: Mutex<Vec<Region>>,
    version: AtomicU64,
    /// How many stores any thread has made into host pages that hold guarded code:
    /// by this the others know to drop their blocks from such pages.
    guarded_writes: AtomicU64,
    /// Every thread of the process has the processor's own x86 order of memory (R035):
    /// plain loads and stores keep it, and nothing need be ordered by instructions.
    /// False until the first thread has asked and been given it; false for good once
    /// any thread is refused.
    x86_order: std::sync::atomic::AtomicU8,
}

/// `Shared::x86_order`: not asked yet, given to every thread so far, refused to one.
const NOT_ASKED: u8 = 0;
const GIVEN: u8 = 1;
const REFUSED: u8 = 2;

pub struct Mem {
    base: *mut u8,
    size: u64,
    host_page: u64,
    /// In order of address, none overlapping, neighbours of one protection joined.
    regions: Vec<Region>,
    /// For each kind of access, the two regions the last ones were found in, so that
    /// the next one nearby is not searched for (R005; two since R006: a program goes
    /// to and fro between its stack and its data). Emptied whenever the list changes.
    near: [[Cell<Region>; 2]; 3],
    /// While there is one, what every write through `write_bytes` wrote over: where,
    /// and the bytes that were there (for `--jit-check`, which runs the same
    /// instructions twice from the same memory).
    log: Option<Vec<(u64, Vec<u8>)>>,
    /// Once the process has a second thread: the list all of them share, and the
    /// number of the version this thread's copy (`regions`) was taken at.
    shared: Option<Arc<Shared>>,
    seen: u64,
}

// The memory is the process's; a `Mem` is one thread's handle on it, and goes with
// that thread.
unsafe impl Send for Mem {}

const NOWHERE: Region = Region { start: 0, end: 0, prot: 0 };

/// x86 has no page that can be written or run but not read: any mapping with a
/// protection can be read. A fetch needs `X` (the NX bit is on in every 64-bit Linux).
fn allows(prot: u8, access: Access) -> bool {
    match access {
        Access::Read => prot != 0,
        Access::Write => prot & (W | C) == W,
        Access::Fetch => prot & X != 0,
    }
}

/// The host never runs the guest's bytes, so it is asked for reading and writing only.
fn host_prot(prot: u8) -> libc::c_int {
    let mut p = libc::PROT_NONE;
    if prot != 0 {
        p |= libc::PROT_READ;
    }
    if prot & (W | C) == W {
        p |= libc::PROT_WRITE;
    }
    p
}

impl Mem {
    /// Reserves the guest's address space: 2^47 bytes if the host gives them (all that
    /// an x86 program can address), else the largest power of two it does give. On
    /// macOS no more than 2^40 is asked for: a reservation there costs time in
    /// proportion to its size (ADR 0013).
    pub fn reserve() -> io::Result<Mem> {
        let host_page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
        let mut refused = io::Error::other("no size tried");
        let most = if cfg!(target_os = "macos") { 40 } else { 47 };
        for bits in (36..=most).rev() {
            let size = 1u64 << bits;
            let p = unsafe {
                libc::mmap(
                    ptr::null_mut(),
                    size as usize,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            if p != libc::MAP_FAILED {
                return Ok(Mem {
                    base: p as *mut u8,
                    size,
                    host_page,
                    regions: Vec::new(),
                    near: [const { [const { Cell::new(NOWHERE) }; 2] }; 3],
                    log: None,
                    shared: None,
                    seen: 0,
                });
            }
            refused = io::Error::last_os_error();
        }
        Err(refused)
    }

    /// Where the guest's address 0 is in the host.
    pub fn base(&self) -> *mut u8 {
        self.base
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn host_page(&self) -> u64 {
        self.host_page
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    // ---- more than one thread ------------------------------------------------------

    /// Another handle on the same memory, for a new thread: from now on the list of
    /// regions is shared, and each handle holds a copy that it renews when the list
    /// has changed.
    pub fn view(&mut self) -> Mem {
        let shared = self.shared.get_or_insert_with(|| {
            Arc::new(Shared { regions: Mutex::new(self.regions.clone()), version: AtomicU64::new(1), guarded_writes: AtomicU64::new(0), x86_order: std::sync::atomic::AtomicU8::new(NOT_ASKED) })
        });
        if self.seen == 0 {
            self.seen = 1;
        }
        let shared = shared.clone();
        self.sync();
        Mem {
            base: self.base,
            size: self.size,
            host_page: self.host_page,
            regions: self.regions.clone(),
            near: [const { [const { Cell::new(NOWHERE) }; 2] }; 3],
            log: None,
            seen: self.seen,
            shared: Some(shared),
        }
    }

    /// Renews this thread's copy of the list if another thread has changed it. To be
    /// called between instructions: an instruction that is running keeps the list it
    /// began with.
    #[inline(always)]
    pub fn sync(&mut self) {
        if let Some(shared) = &self.shared {
            if shared.version.load(Ordering::Acquire) != self.seen {
                self.renew();
            }
        }
    }

    /// Has the process more than one thread's handle on this memory?
    pub fn is_shared(&self) -> bool {
        self.shared.is_some()
    }

    /// Must loads and stores be ordered by the instructions that make them: is the
    /// memory shared between threads that have not all the processor's own x86 order?
    #[inline(always)]
    pub fn must_order(&self) -> bool {
        self.shared.as_ref().is_some_and(|shared| shared.x86_order.load(Ordering::Relaxed) != GIVEN)
    }

    /// A thread of this memory asked for the processor's x86 order and says whether
    /// it was given. One refusal is for all: the memory is one.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn thread_has_x86_order(&self, given: bool) {
        if let Some(shared) = &self.shared {
            if !given {
                shared.x86_order.store(REFUSED, Ordering::Relaxed);
            } else {
                let _ = shared.x86_order.compare_exchange(NOT_ASKED, GIVEN, Ordering::Relaxed, Ordering::Relaxed);
            }
        }
    }

    /// How many stores into guarded code the threads of this memory have made; and
    /// one more of them, made by this thread. Nothing where there is one thread.
    #[inline(always)]
    pub fn guarded_writes(&self) -> u64 {
        self.shared.as_ref().map_or(0, |shared| shared.guarded_writes.load(Ordering::Acquire))
    }

    pub fn wrote_guarded(&self) -> u64 {
        self.shared.as_ref().map_or(0, |shared| shared.guarded_writes.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// After an access of the interpreter's to the guest's memory: where there are
    /// other threads, nothing of this thread's that comes after may be seen or see
    /// before it, which is more than x86 asks and so enough (ADR 0019, point D). The
    /// interpreter's accesses are few beside the blocks'.
    #[inline(always)]
    fn ordered(&self) {
        if self.must_order() {
            std::sync::atomic::fence(Ordering::SeqCst);
        }
    }

    #[cold]
    fn renew(&mut self) {
        let shared = self.shared.clone().expect("called for a shared list");
        let master = shared.regions.lock().unwrap_or_else(|e| e.into_inner());
        self.regions = master.clone();
        self.seen = shared.version.load(Ordering::Acquire);
        self.forget_near();
    }

    /// A change of the list: alone, or, where the list is shared, with the list held,
    /// made on its newest state, and given to the others when done.
    fn changing<T>(&mut self, change: impl FnOnce(&mut Mem) -> T) -> T {
        let Some(shared) = self.shared.clone() else {
            return change(self);
        };
        let mut master = shared.regions.lock().unwrap_or_else(|e| e.into_inner());
        if shared.version.load(Ordering::Acquire) != self.seen {
            self.regions = master.clone();
            self.forget_near();
        }
        let done = change(self);
        *master = self.regions.clone();
        self.seen = shared.version.fetch_add(1, Ordering::AcqRel) + 1;
        done
    }

    // ---- the guest's view: mapping -------------------------------------------------

    /// Maps `[start, start + len)` afresh: whatever was there is gone and the pages
    /// read as zero.
    pub fn map(&mut self, start: u64, len: u64, prot: u8) -> Result<(), MapError> {
        self.changing(|m| m.map_alone(start, len, prot))
    }

    /// Maps `[start, start + len)` as memory that is shared (R048): the host's own
    /// shared mapping put at the place, of the file `fd` from `offset`, or of no file
    /// (`None`). What is written is seen by every process that has the same mapping
    /// (a child made by `fork` among them) and reaches the file. Only where the
    /// host's page is the guest's: elsewhere a host page could hold part of a shared
    /// mapping and part of something else, and it is refused (`MapError::Range`).
    /// From then on the region is like any other: protected, unmapped and mapped
    /// over as the others are.
    pub fn map_shared(&mut self, start: u64, len: u64, prot: u8, file: Option<(i32, u64)>) -> Result<(), MapError> {
        self.changing(|m| m.map_host_alone(start, len, prot, file, libc::MAP_SHARED))
    }

    /// Maps `[start, start + len)` as a private mapping of the file `fd` from
    /// `offset` (R053): the host's own, put at the place, so that the file's pages
    /// are the kernel's one copy until this process writes them, as they are for
    /// the same program on the real machine. What is written stays the process's.
    /// A page wholly past the file's end is there and faults when touched, as on
    /// Linux. Only where the host's page is the guest's (`MapError::Range` if not,
    /// with nothing changed: the caller then copies the file's bytes as before).
    pub fn map_file(&mut self, start: u64, len: u64, prot: u8, fd: i32, offset: u64) -> Result<(), MapError> {
        self.changing(|m| m.map_host_alone(start, len, prot, Some((fd, offset)), libc::MAP_PRIVATE))
    }

    fn map_host_alone(&mut self, start: u64, len: u64, prot: u8, file: Option<(i32, u64)>, kind: libc::c_int) -> Result<(), MapError> {
        let end = self.range(start, len)?;
        if self.host_page != PAGE {
            return Err(MapError::Range);
        }
        self.carve(start, end);
        let (flags, fd, offset) = match file {
            Some((fd, offset)) => (kind | libc::MAP_FIXED, fd, offset as libc::off_t),
            None => (kind | libc::MAP_ANON | libc::MAP_FIXED, -1, 0),
        };
        let p = unsafe { libc::mmap(self.host(start) as *mut libc::c_void, (end - start) as usize, libc::PROT_NONE, flags, fd, offset) };
        if p == libc::MAP_FAILED {
            // Nothing is there now: what was carved out is handed back as it is
            // when unmapped.
            let e = io::Error::last_os_error();
            self.host_sync(start, end).map_err(MapError::Host)?;
            return Err(MapError::Host(e));
        }
        self.insert(Region { start, end, prot });
        self.host_sync(start, end).map_err(MapError::Host)
    }

    /// Says that `[start, start + len)` is mapped with `prot`, the host having been
    /// given what is there by someone else (a System V segment attached at the
    /// place): the list is told, and the host's protection made what the list asks.
    pub fn adopt(&mut self, start: u64, len: u64, prot: u8, attach: impl FnOnce(*mut u8) -> io::Result<()>) -> Result<(), MapError> {
        self.changing(|m| {
            let end = m.range(start, len)?;
            if m.host_page != PAGE {
                return Err(MapError::Range);
            }
            m.carve(start, end);
            if let Err(e) = attach(m.host(start)) {
                m.host_sync(start, end).map_err(MapError::Host)?;
                return Err(MapError::Host(e));
            }
            m.insert(Region { start, end, prot });
            m.host_sync(start, end).map_err(MapError::Host)
        })
    }

    /// Unmaps `[start, start + len)`.
    pub fn unmap(&mut self, start: u64, len: u64) -> Result<(), MapError> {
        self.changing(|m| m.unmap_alone(start, len))
    }

    /// Changes the protection of `[start, start + len)`, all of which must be mapped.
    pub fn protect(&mut self, start: u64, len: u64, prot: u8) -> Result<(), MapError> {
        self.changing(|m| m.protect_alone(start, len, prot))
    }

    /// Says of the host pages that hold `[start, end)` that code from them has been
    /// translated (`on`), or that it no longer is.
    pub fn mark_code(&mut self, start: u64, end: u64, on: bool) -> Result<(), MapError> {
        self.changing(|m| m.mark_code_alone(start, end, on))
    }

    /// Maps `[start, start + len)` afresh, as `mmap` with `MAP_FIXED` and no file does:
    /// whatever was there is gone and the pages read as zero.
    fn map_alone(&mut self, start: u64, len: u64, prot: u8) -> Result<(), MapError> {
        let end = self.range(start, len)?;
        self.carve(start, end);

        // Host pages that lie wholly inside are taken afresh from the host. What is
        // left at either end shares a host page with a neighbour and is cleared by hand.
        let inner_start = self.host_up(start);
        let inner_end = self.host_down(end);
        if inner_start < inner_end {
            self.host_fresh(inner_start, inner_end).map_err(MapError::Host)?;
            self.host_clear(start, inner_start).map_err(MapError::Host)?;
            self.host_clear(inner_end, end).map_err(MapError::Host)?;
        } else {
            self.host_clear(start, end).map_err(MapError::Host)?;
        }

        self.insert(Region { start, end, prot });
        self.host_sync(start, end).map_err(MapError::Host)
    }

    /// Unmaps `[start, start + len)`. Pages that were not mapped are passed over, as
    /// `munmap` does.
    fn unmap_alone(&mut self, start: u64, len: u64) -> Result<(), MapError> {
        let end = self.range(start, len)?;
        self.carve(start, end);
        self.host_sync(start, end).map_err(MapError::Host)
    }

    /// Changes the protection of `[start, start + len)`, all of which must be mapped,
    /// as `mprotect` does. The contents stay.
    fn protect_alone(&mut self, start: u64, len: u64, prot: u8) -> Result<(), MapError> {
        let end = self.range(start, len)?;
        if self.walk(start, end, |_| true).is_some() {
            return Err(MapError::NotMapped);
        }
        // What is marked as holding translated code stays marked.
        let pieces: Vec<Region> = self
            .regions
            .iter()
            .filter(|r| r.start < end && start < r.end)
            .map(|r| Region { start: r.start.max(start), end: r.end.min(end), prot: prot | r.prot & C })
            .collect();
        self.carve(start, end);
        for r in pieces {
            self.insert(r);
        }
        self.host_sync(start, end).map_err(MapError::Host)
    }

    /// The end of a range the guest names, rounded up to a page; or what is wrong.
    fn range(&self, start: u64, len: u64) -> Result<u64, MapError> {
        if start % PAGE != 0 || len == 0 {
            return Err(MapError::Range);
        }
        let end = start
            .checked_add(len)
            .and_then(|e| e.checked_next_multiple_of(PAGE))
            .ok_or(MapError::Range)?;
        if end > self.size {
            return Err(MapError::Range);
        }
        Ok(end)
    }

    // ---- the guest's view: access ---------------------------------------------------

    /// Reads `N` bytes, or says which byte could not be read.
    #[inline(always)]
    pub fn read<const N: usize>(&self, addr: u64) -> Result<[u8; N], Fault> {
        let mut bytes = [0u8; N];
        self.read_bytes(addr, &mut bytes)?;
        Ok(bytes)
    }

    /// Writes `N` bytes, or writes none and says which byte could not be written.
    #[inline(always)]
    pub fn write<const N: usize>(&mut self, addr: u64, bytes: [u8; N]) -> Result<(), Fault> {
        self.write_bytes(addr, &bytes)
    }

    #[inline(always)]
    pub fn read_bytes(&self, addr: u64, into: &mut [u8]) -> Result<(), Fault> {
        self.copy_out(addr, into, Access::Read)
    }

    /// Reads bytes to be run: the pages must be executable.
    pub fn fetch(&self, addr: u64, into: &mut [u8]) -> Result<(), Fault> {
        self.copy_out(addr, into, Access::Fetch)
    }

    /// Where the bytes to be run at `addr` lie in the host, and how many of the
    /// fifteen an instruction may have are there to be run (fewer only where what can
    /// be run ends). They are to be read before the guest's memory is next changed.
    pub fn code_at(&self, addr: u64) -> (*const u8, usize) {
        let mut have = 15;
        while have > 0 && self.check(addr, have, Access::Fetch).is_err() {
            have -= 1;
        }
        // With none to be had the address may be anywhere, and is not turned into a
        // pointer inside the reservation.
        (if have == 0 { self.base } else { self.host(addr) }, have as usize)
    }

    #[inline(always)]
    pub fn write_bytes(&mut self, addr: u64, from: &[u8]) -> Result<(), Fault> {
        self.check(addr, from.len() as u64, Access::Write)?;
        if self.log.is_some() {
            self.note(addr, from.len());
        }
        if self.shared.is_some() {
            return self.store_shared(addr, from);
        }
        // The list allowed it, and the host is at least as open as the list.
        unsafe { ptr::copy_nonoverlapping(from.as_ptr(), self.host(addr), from.len()) };
        self.ordered();
        Ok(())
    }

    /// A store into memory that other threads have too. This thread's list allowed
    /// it; but another thread may since have marked the page as holding translated
    /// code, and then the host refuses the store (a thread of a JVM writes methods
    /// into pages another is running from, all the time: R047). So the store is one
    /// that can be refused: then the list is looked at again, and the answer is the
    /// list's, which the caller meets as it meets any store beside code.
    #[cold]
    fn store_shared(&mut self, addr: u64, from: &[u8]) -> Result<(), Fault> {
        for _ in 0..3 {
            if unsafe { store_or_not(self.host(addr), from.as_ptr(), from.len()) } != 0 {
                self.ordered();
                return Ok(());
            }
            self.renew();
            self.check(addr, from.len() as u64, Access::Write)?;
        }
        Err(Fault { addr, access: Access::Write })
    }

    /// Writes the low `bits` of `new` at `addr` if what is there is still `old`, as
    /// one act that no other thread's store can fall into; says whether it was.
    /// (An address that is not a multiple of the width has no such act on the host:
    /// there it is written as any store is.)
    pub fn swap_if(&mut self, addr: u64, bits: u32, old: u64, new: u64) -> Result<bool, Fault> {
        use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU8};
        let bytes = bits as usize / 8;
        self.check(addr, bytes as u64, Access::Write)?;
        if self.log.is_some() {
            self.note(addr, bytes);
        }
        let at = self.host(addr);
        if addr % bytes as u64 != 0 {
            return self.write_bytes(addr, &new.to_le_bytes()[..bytes]).map(|()| true);
        }
        // Shared: the host is asked first whether it lets the place be written (an
        // addition of nothing, as one act), as `store_shared` asks. (Between the
        // asking and the exchange the page can still be marked: not closed.)
        if self.shared.is_some() && unsafe { may_store(at) } == 0 {
            self.renew();
            self.check(addr, bytes as u64, Access::Write)?;
            return Err(Fault { addr, access: Access::Write });
        }
        const ORDER: Ordering = Ordering::SeqCst;
        Ok(unsafe {
            match bits {
                8 => (*(at as *const AtomicU8)).compare_exchange(old as u8, new as u8, ORDER, ORDER).is_ok(),
                16 => (*(at as *const AtomicU16)).compare_exchange(old as u16, new as u16, ORDER, ORDER).is_ok(),
                32 => (*(at as *const AtomicU32)).compare_exchange(old as u32, new as u32, ORDER, ORDER).is_ok(),
                _ => (*(at as *const AtomicU64)).compare_exchange(old, new, ORDER, ORDER).is_ok(),
            }
        })
    }

    #[cold]
    fn note(&mut self, addr: u64, len: usize) {
        let mut old = vec![0u8; len];
        unsafe { ptr::copy_nonoverlapping(self.host(addr), old.as_mut_ptr(), len) };
        self.log.as_mut().unwrap().push((addr, old));
    }

    /// Begins to keep what writes write over. (`fill` and `copy_up` are not kept:
    /// whoever keeps a log has every repetition a step.)
    pub fn log_writes(&mut self) {
        self.log = Some(Vec::new());
    }

    /// Ends the keeping and puts back everything written since it began, the last
    /// first. Gives back where the writes were and what each wrote, in their order
    /// (so the last to a byte is what stood there at the end).
    pub fn undo_writes(&mut self) -> Vec<(u64, Vec<u8>)> {
        let log = self.log.take().unwrap_or_default();
        let mut written = Vec::with_capacity(log.len());
        for (addr, old) in log.iter().rev() {
            let mut new = vec![0u8; old.len()];
            unsafe {
                ptr::copy_nonoverlapping(self.host(*addr), new.as_mut_ptr(), old.len());
                ptr::copy_nonoverlapping(old.as_ptr(), self.host(*addr), old.len());
            }
            written.push((*addr, new));
        }
        written.reverse();
        written
    }

    /// Says of the host pages that hold `[start, end)` that code from them has been
    /// translated (`on`), or that it no longer is: while it is, nothing may write
    /// them. It is the host's page that is marked, since that is what the host can
    /// protect: on a host with 16 KB pages three more of the guest's go with each.
    fn mark_code_alone(&mut self, start: u64, end: u64, on: bool) -> Result<(), MapError> {
        let (start, end) = (self.host_down(start), self.host_up(end).min(self.size));
        let inside: Vec<Region> = self
            .regions
            .iter()
            .filter(|r| r.start < end && start < r.end)
            .map(|r| Region {
                start: r.start.max(start),
                end: r.end.min(end),
                prot: if on { r.prot | C } else { r.prot & !C },
            })
            .collect();
        for r in inside {
            self.carve(r.start, r.end);
            self.insert(r);
        }
        self.host_sync(start, end).map_err(MapError::Host)
    }

    /// May the guest write any of `[start, end)`? (By what it mapped and protected:
    /// the mark of translated code does not change the answer.)
    pub fn guest_may_write(&self, start: u64, end: u64) -> bool {
        self.regions.iter().any(|r| r.start < end && start < r.end && r.prot & W != 0)
    }

    /// Does the page at `addr` hold translated code?
    pub fn is_code(&self, addr: u64) -> bool {
        self.regions.get(self.first_ending_after(addr)).is_some_and(|r| r.start <= addr && r.prot & C != 0)
    }

    /// The start and end of the host pages that hold `[start, end)`.
    pub fn host_pages(&self, start: u64, end: u64) -> (u64, u64) {
        (self.host_down(start), self.host_up(end))
    }

    /// Writes the low `bytes` bytes of `v`, `n` times over, from `addr` up: all of it or,
    /// if any of it cannot be written, none.
    pub fn fill(&mut self, addr: u64, v: u64, bytes: u64, n: u64) -> Result<(), Fault> {
        let len = n.checked_mul(bytes).unwrap_or(u64::MAX);
        self.check(addr, len, Access::Write)?;
        if self.shared.is_some() {
            // An element at a time, each a store that can be refused.
            let element = v.to_le_bytes();
            for i in 0..n {
                self.store_shared(addr + i * bytes, &element[..bytes as usize])?;
            }
            return Ok(());
        }
        let at = self.host(addr);
        unsafe {
            match bytes {
                1 => ptr::write_bytes(at, v as u8, n as usize),
                2 => std::slice::from_raw_parts_mut(at as *mut [u8; 2], n as usize).fill((v as u16).to_le_bytes()),
                4 => std::slice::from_raw_parts_mut(at as *mut [u8; 4], n as usize).fill((v as u32).to_le_bytes()),
                _ => std::slice::from_raw_parts_mut(at as *mut [u8; 8], n as usize).fill(v.to_le_bytes()),
            }
        }
        Ok(())
    }

    /// Copies `len` bytes from `from` to `to` as a copy from the lowest address up
    /// does, which is what a forward `rep movs` does: all of it or none. `None` when
    /// the two overlap so that such a copy would read what it has itself written; the
    /// caller then goes an element at a time.
    pub fn copy_up(&mut self, to: u64, from: u64, len: u64) -> Option<Result<(), Fault>> {
        if to > from && to - from < len {
            return None;
        }
        if let Err(f) = self.check(from, len, Access::Read).and_then(|()| self.check(to, len, Access::Write)) {
            return Some(Err(f));
        }
        if self.shared.is_some() {
            let held = unsafe { std::slice::from_raw_parts(self.host(from), len as usize) }.to_vec();
            return Some(self.store_shared(to, &held));
        }
        unsafe { ptr::copy(self.host(from), self.host(to), len as usize) };
        Some(Ok(()))
    }

    #[inline(always)]
    fn copy_out(&self, addr: u64, into: &mut [u8], access: Access) -> Result<(), Fault> {
        self.check(addr, into.len() as u64, access)?;
        unsafe { ptr::copy_nonoverlapping(self.host(addr), into.as_mut_ptr(), into.len()) };
        self.ordered();
        Ok(())
    }

    pub fn read_u8(&self, addr: u64) -> Result<u8, Fault> {
        self.read(addr).map(u8::from_le_bytes)
    }

    pub fn read_u16(&self, addr: u64) -> Result<u16, Fault> {
        self.read(addr).map(u16::from_le_bytes)
    }

    pub fn read_u32(&self, addr: u64) -> Result<u32, Fault> {
        self.read(addr).map(u32::from_le_bytes)
    }

    pub fn read_u64(&self, addr: u64) -> Result<u64, Fault> {
        self.read(addr).map(u64::from_le_bytes)
    }

    pub fn write_u8(&mut self, addr: u64, v: u8) -> Result<(), Fault> {
        self.write(addr, v.to_le_bytes())
    }

    pub fn write_u16(&mut self, addr: u64, v: u16) -> Result<(), Fault> {
        self.write(addr, v.to_le_bytes())
    }

    pub fn write_u32(&mut self, addr: u64, v: u32) -> Result<(), Fault> {
        self.write(addr, v.to_le_bytes())
    }

    pub fn write_u64(&mut self, addr: u64, v: u64) -> Result<(), Fault> {
        self.write(addr, v.to_le_bytes())
    }

    /// Is every byte of `[addr, addr + len)` mapped so as to allow `access`? If not,
    /// the fault names the first that is not. An access may run over several regions.
    #[inline(always)]
    pub fn check(&self, addr: u64, len: u64, access: Access) -> Result<(), Fault> {
        // Nearly always in one of the two regions the last accesses of this kind were in.
        for near in &self.near[access as usize] {
            let last = near.get();
            if addr >= last.start && addr < last.end && len <= last.end - addr {
                return Ok(());
            }
        }
        self.search(addr, len, access)
    }

    /// `check` when the access is not where the last one was.
    #[cold]
    fn search(&self, addr: u64, len: u64, access: Access) -> Result<(), Fault> {
        let near = &self.near[access as usize];
        // No region reaches past the reservation, so an end that would wrap is cut off
        // at the top and faults there like any other unmapped byte.
        let end = addr.saturating_add(len);
        if let Some(at) = self.walk(addr, end, |prot| allows(prot, access)) {
            return Err(Fault { addr: at, access });
        }
        // Remembered if it lies in one region (a region found here allows the access),
        // in place of the one found longer ago.
        if let Some(r) = self.regions.get(self.first_ending_after(addr)) {
            if r.start <= addr && end <= r.end {
                near[1].set(near[0].get());
                near[0].set(*r);
            }
        }
        Ok(())
    }

    /// The first address in `[start, end)` that is unmapped or whose protection `ok`
    /// refuses.
    fn walk(&self, start: u64, end: u64, ok: impl Fn(u8) -> bool) -> Option<u64> {
        let mut at = start;
        let mut i = self.first_ending_after(at);
        while at < end {
            match self.regions.get(i) {
                Some(r) if r.start <= at && ok(r.prot) => {
                    at = r.end;
                    i += 1;
                }
                _ => return Some(at),
            }
        }
        None
    }

    #[inline(always)]
    fn host(&self, addr: u64) -> *mut u8 {
        // Callers have checked `addr` against the list, which lies inside the
        // reservation.
        unsafe { self.base.add(addr as usize) }
    }

    // ---- the list -------------------------------------------------------------------

    /// The index of the first region that ends after `addr`: the one that holds `addr`
    /// if any does, else the next one up.
    fn first_ending_after(&self, addr: u64) -> usize {
        self.regions.partition_point(|r| r.end <= addr)
    }

    /// Takes `[start, end)` out of the list, cutting the regions that reach into it.
    fn carve(&mut self, start: u64, end: u64) {
        self.forget_near();
        let mut kept = Vec::with_capacity(self.regions.len() + 1);
        for r in &self.regions {
            if r.end <= start || r.start >= end {
                kept.push(*r);
                continue;
            }
            if r.start < start {
                kept.push(Region { start: r.start, end: start, prot: r.prot });
            }
            if r.end > end {
                kept.push(Region { start: end, end: r.end, prot: r.prot });
            }
        }
        self.regions = kept;
    }

    /// Puts a region into a place that `carve` has emptied, and joins it to a neighbour
    /// that touches it with the same protection.
    fn insert(&mut self, new: Region) {
        self.forget_near();
        let mut i = self.first_ending_after(new.start);
        self.regions.insert(i, new);
        if i + 1 < self.regions.len() && self.joins(i) {
            self.regions[i].end = self.regions[i + 1].end;
            self.regions.remove(i + 1);
        }
        if i > 0 && self.joins(i - 1) {
            i -= 1;
            self.regions[i].end = self.regions[i + 1].end;
            self.regions.remove(i + 1);
        }
    }

    fn forget_near(&mut self) {
        for near in self.near.iter().flatten() {
            near.set(NOWHERE);
        }
    }

    fn joins(&self, i: usize) -> bool {
        let (a, b) = (self.regions[i], self.regions[i + 1]);
        a.end == b.start && a.prot == b.prot
    }

    // ---- the host's view ------------------------------------------------------------

    fn host_down(&self, addr: u64) -> u64 {
        addr & !(self.host_page - 1)
    }

    fn host_up(&self, addr: u64) -> u64 {
        self.host_down(addr + self.host_page - 1)
    }

    /// Hands host pages back and takes them again: no access, and zero when next read.
    fn host_fresh(&self, start: u64, end: u64) -> io::Result<()> {
        let p = unsafe {
            libc::mmap(
                self.host(start) as *mut libc::c_void,
                (end - start) as usize,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE | libc::MAP_FIXED,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    fn host_protect(&self, start: u64, end: u64, prot: libc::c_int) -> io::Result<()> {
        let rc = unsafe {
            libc::mprotect(self.host(start) as *mut libc::c_void, (end - start) as usize, prot)
        };
        if rc != 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }

    /// Clears `[start, end)` where it cannot be taken afresh because its host pages are
    /// shared. The host pages are left open; the `host_sync` that follows closes them.
    fn host_clear(&self, start: u64, end: u64) -> io::Result<()> {
        if start >= end {
            return Ok(());
        }
        self.host_protect(
            self.host_down(start),
            self.host_up(end),
            libc::PROT_READ | libc::PROT_WRITE,
        )?;
        unsafe { ptr::write_bytes(self.host(start), 0, (end - start) as usize) };
        Ok(())
    }

    /// Makes the host's protection of every host page touching `[start, end)` what the
    /// list now asks: as open as the most open guest page in it, and handed back if no
    /// guest page is left in it.
    fn host_sync(&self, start: u64, end: u64) -> io::Result<()> {
        let hp = self.host_page;
        let end = self.host_up(end);
        let mut at = self.host_down(start);
        while at < end {
            let i = self.first_ending_after(at);
            // How far the same answer holds from `at`, and the answer: a protection, or
            // none because no guest page is there.
            let (to, prot) = match self.regions.get(i) {
                // A region covers this host page whole, and every one up to its last.
                Some(r) if r.start <= at && r.end >= at + hp => (self.host_down(r.end), Some(r.prot)),
                // A region begins or ends inside this host page: all that touch it count.
                Some(r) if r.start < at + hp => {
                    let mut all = 0;
                    for r in &self.regions[i..] {
                        if r.start >= at + hp {
                            break;
                        }
                        all |= r.prot;
                    }
                    (at + hp, Some(all))
                }
                // Nothing until a region further up, or nothing at all.
                Some(r) => (self.host_down(r.start), None),
                None => (end, None),
            };
            let to = to.min(end);
            match prot {
                Some(prot) => self.host_protect(at, to, host_prot(prot))?,
                None => self.host_fresh(at, to)?,
            }
            at = to;
        }
        Ok(())
    }
}

impl Drop for Mem {
    /// The reservation goes with the last handle on it, not with any one thread's.
    fn drop(&mut self) {
        if self.shared.as_ref().is_some_and(|shared| Arc::strong_count(shared) > 1) {
            return;
        }
        unsafe { libc::munmap(self.base as *mut libc::c_void, self.size as usize) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RW: u8 = R | W;
    const RX: u8 = R | X;

    /// An address clear of anything else in these tests, on a host page of any size.
    const A: u64 = 0x40_0000;

    fn mem() -> Mem {
        Mem::reserve().expect("the host gives a reservation")
    }

    fn fault(addr: u64, access: Access) -> Fault {
        Fault { addr, access }
    }

    /// Does the host itself refuse a read (`write` false) or a write at `addr`? Asked
    /// of a child, which dies of it or does not.
    fn host_refuses(m: &Mem, addr: u64, write: bool) -> bool {
        let p = m.host(addr);
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork");
            if pid == 0 {
                if write {
                    ptr::write_volatile(p, 1);
                } else {
                    ptr::read_volatile(p);
                }
                libc::_exit(0);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            libc::WIFSIGNALED(status)
        }
    }

    #[test]
    fn the_reservation_is_large_and_closed() {
        let m = mem();
        assert!(m.size() >= 1 << 36);
        assert!(m.host_page() == 4096 || m.host_page() == 16384);
        assert!(m.regions().is_empty());
        assert_eq!(m.read_u8(0), Err(fault(0, Access::Read)));
        assert_eq!(m.read_u64(A), Err(fault(A, Access::Read)));
        assert!(host_refuses(&m, A, false));
        assert!(host_refuses(&m, m.size() - 1, false));
    }

    #[test]
    fn what_is_mapped_reads_as_zero_and_keeps_what_is_written() {
        let mut m = mem();
        m.map(A, 2 * PAGE, RW).unwrap();
        assert_eq!(m.read_u64(A), Ok(0));
        assert_eq!(m.read_u64(A + 2 * PAGE - 8), Ok(0));
        m.write_u64(A + 8, 0x1122_3344_5566_7788).unwrap();
        assert_eq!(m.read_u64(A + 8), Ok(0x1122_3344_5566_7788));
        // Little-endian, as x86 is.
        assert_eq!(m.read_u8(A + 8), Ok(0x88));
        assert_eq!(m.read_u16(A + 9), Ok(0x6677));
        assert_eq!(m.read_u32(A + 12), Ok(0x1122_3344));
        m.write_u8(A, 0xab).unwrap();
        m.write_u16(A + 1, 0xcdef).unwrap();
        m.write_u32(A + 3, 0x0123_4567).unwrap();
        assert_eq!(m.read::<7>(A), Ok([0xab, 0xef, 0xcd, 0x67, 0x45, 0x23, 0x01]));
    }

    #[test]
    fn the_guests_address_is_the_hosts_base_plus_it() {
        let mut m = mem();
        m.map(A, PAGE, RW).unwrap();
        m.write_u32(A + 0x10, 0xdead_beef).unwrap();
        let there = unsafe { ptr::read_unaligned(m.base().add((A + 0x10) as usize) as *const u32) };
        assert_eq!(there, 0xdead_beef);
    }

    #[test]
    fn a_fault_names_the_first_byte_that_could_not_be_had() {
        let mut m = mem();
        m.map(A, PAGE, RW).unwrap();
        let top = A + PAGE;
        // The last byte is there; an access that runs off the end faults at the end.
        assert_eq!(m.read_u8(top - 1), Ok(0));
        assert_eq!(m.read_u64(top - 3), Err(fault(top, Access::Read)));
        assert_eq!(m.write_u64(top - 3, !0), Err(fault(top, Access::Write)));
        // And wrote nothing of it.
        assert_eq!(m.read::<3>(top - 3), Ok([0, 0, 0]));
        // One that begins below the mapping faults where it begins.
        assert_eq!(m.read_u32(A - 2), Err(fault(A - 2, Access::Read)));
        // Past the reservation, and at the top of all addresses, there is only a fault.
        assert_eq!(m.read_u8(m.size()), Err(fault(m.size(), Access::Read)));
        assert_eq!(m.read_u64(u64::MAX - 3), Err(fault(u64::MAX - 3, Access::Read)));
    }

    #[test]
    fn an_access_may_run_over_regions_that_touch() {
        let mut m = mem();
        m.map(A, PAGE, RW).unwrap();
        m.map(A + PAGE, PAGE, R).unwrap();
        assert_eq!(m.regions().len(), 2);
        m.write_u32(A + PAGE - 4, 0x0403_0201).unwrap();
        assert_eq!(m.read_u64(A + PAGE - 4), Ok(0x0403_0201));
        // A write stops at the page that may only be read.
        assert_eq!(m.write_u64(A + PAGE - 4, 0), Err(fault(A + PAGE, Access::Write)));
        assert_eq!(m.read_u32(A + PAGE - 4), Ok(0x0403_0201));
    }

    #[test]
    fn protection_is_x86s() {
        let mut m = mem();
        m.map(A, PAGE, RW).unwrap();
        m.write_bytes(A, &[0x90, 0xc3]).unwrap();
        let mut code = [0u8; 2];
        // Data is not run.
        assert_eq!(m.fetch(A, &mut code), Err(fault(A, Access::Fetch)));
        m.protect(A, PAGE, RX).unwrap();
        m.fetch(A, &mut code).unwrap();
        assert_eq!(code, [0x90, 0xc3]);
        assert_eq!(m.write_u8(A, 0), Err(fault(A, Access::Write)));
        // What can be written or run can be read, whatever was asked for.
        m.protect(A, PAGE, X).unwrap();
        assert_eq!(m.read_u8(A), Ok(0x90));
        m.protect(A, PAGE, W).unwrap();
        assert_eq!(m.read_u8(A), Ok(0x90));
        // Mapped with no protection: there, and closed.
        m.protect(A, PAGE, 0).unwrap();
        assert_eq!(m.read_u8(A), Err(fault(A, Access::Read)));
        assert_eq!(m.regions(), &[Region { start: A, end: A + PAGE, prot: 0 }]);
        // The contents stayed through all of it.
        m.protect(A, PAGE, R).unwrap();
        assert_eq!(m.read_u16(A), Ok(0xc390));
    }

    #[test]
    fn the_list_is_cut_and_joined() {
        let mut m = mem();
        m.map(A, 8 * PAGE, RW).unwrap();
        assert_eq!(m.regions(), &[Region { start: A, end: A + 8 * PAGE, prot: RW }]);
        // A change in the middle makes three.
        m.protect(A + 2 * PAGE, 2 * PAGE, R).unwrap();
        assert_eq!(
            m.regions(),
            &[
                Region { start: A, end: A + 2 * PAGE, prot: RW },
                Region { start: A + 2 * PAGE, end: A + 4 * PAGE, prot: R },
                Region { start: A + 4 * PAGE, end: A + 8 * PAGE, prot: RW },
            ]
        );
        // Changing it back makes one again.
        m.protect(A + 2 * PAGE, 2 * PAGE, RW).unwrap();
        assert_eq!(m.regions(), &[Region { start: A, end: A + 8 * PAGE, prot: RW }]);
        // A hole in the middle makes two; a length is rounded up to a page.
        m.unmap(A + 3 * PAGE, 1).unwrap();
        assert_eq!(
            m.regions(),
            &[
                Region { start: A, end: A + 3 * PAGE, prot: RW },
                Region { start: A + 4 * PAGE, end: A + 8 * PAGE, prot: RW },
            ]
        );
        assert_eq!(m.read_u8(A + 3 * PAGE), Err(fault(A + 3 * PAGE, Access::Read)));
        // A change of protection over a hole is refused, and changes nothing.
        assert!(matches!(m.protect(A, 8 * PAGE, R), Err(MapError::NotMapped)));
        assert_eq!(m.regions().len(), 2);
        // Unmapping what is not mapped is not an error.
        m.unmap(A + 3 * PAGE, PAGE).unwrap();
        m.unmap(A, 8 * PAGE).unwrap();
        assert!(m.regions().is_empty());
    }

    #[test]
    fn a_range_must_be_on_a_page_and_inside() {
        let mut m = mem();
        assert!(matches!(m.map(A + 1, PAGE, RW), Err(MapError::Range)));
        assert!(matches!(m.map(A, 0, RW), Err(MapError::Range)));
        assert!(matches!(m.map(m.size(), PAGE, RW), Err(MapError::Range)));
        assert!(matches!(m.map(m.size() - PAGE, 2 * PAGE, RW), Err(MapError::Range)));
        assert!(matches!(m.map(A, u64::MAX, RW), Err(MapError::Range)));
        assert!(m.regions().is_empty());
        // The first page and the last are inside.
        m.map(0, PAGE, RW).unwrap();
        m.map(m.size() - PAGE, PAGE, RW).unwrap();
        m.write_u8(0, 1).unwrap();
        m.write_u8(m.size() - 1, 2).unwrap();
        assert_eq!(m.read_u8(m.size() - 1), Ok(2));
    }

    #[test]
    fn mapping_again_gives_zero_and_leaves_the_neighbours() {
        let mut m = mem();
        // Sixteen guest pages: on any host, some host pages lie wholly inside the middle
        // eight and, on a host with 16 KB pages, the odd offsets below share one.
        m.map(A, 16 * PAGE, RW).unwrap();
        for page in 0..16 {
            m.write_u64(A + page * PAGE, 0x1000 + page).unwrap();
            m.write_u64(A + page * PAGE + PAGE - 8, 0x2000 + page).unwrap();
        }
        // Over the middle, beginning and ending off a 16 KB boundary.
        m.map(A + 3 * PAGE, 10 * PAGE, RW).unwrap();
        for page in 0..16 {
            let (first, last) = if (3..13).contains(&page) {
                (0, 0)
            } else {
                (0x1000 + page, 0x2000 + page)
            };
            assert_eq!(m.read_u64(A + page * PAGE), Ok(first), "page {page}");
            assert_eq!(m.read_u64(A + page * PAGE + PAGE - 8), Ok(last), "page {page}");
        }
        // One guest page alone, unmapped and mapped again.
        m.write_u64(A + 5 * PAGE, 7).unwrap();
        m.unmap(A + 5 * PAGE, PAGE).unwrap();
        m.write_u64(A + 4 * PAGE, 8).unwrap();
        m.map(A + 5 * PAGE, PAGE, RW).unwrap();
        assert_eq!(m.read_u64(A + 5 * PAGE), Ok(0));
        assert_eq!(m.read_u64(A + 4 * PAGE), Ok(8));
    }

    #[test]
    fn a_fill_writes_all_of_it_or_none() {
        let mut m = mem();
        m.map(A, 2 * PAGE, RW).unwrap();
        m.fill(A + 3, 0xab, 1, 5).unwrap();
        assert_eq!(m.read::<10>(A), Ok([0, 0, 0, 0xab, 0xab, 0xab, 0xab, 0xab, 0, 0]));
        // Wider elements, off their own boundary, low byte first; only the low bytes of
        // the value count.
        m.fill(A + 17, 0x9911_2233, 2, 3).unwrap();
        assert_eq!(m.read::<8>(A + 16), Ok([0, 0x33, 0x22, 0x33, 0x22, 0x33, 0x22, 0]));
        m.fill(A + 33, 0x55_1122_3344, 4, 2).unwrap();
        assert_eq!(m.read::<10>(A + 32), Ok([0, 0x44, 0x33, 0x22, 0x11, 0x44, 0x33, 0x22, 0x11, 0]));
        m.fill(A + 65, 0x0102_0304_0506_0708, 8, 2).unwrap();
        assert_eq!(m.read_u64(A + 65), Ok(0x0102_0304_0506_0708));
        assert_eq!(m.read_u64(A + 73), Ok(0x0102_0304_0506_0708));
        assert_eq!(m.read_u8(A + 81), Ok(0));
        // Up to the last byte that is mapped; one more and nothing is written.
        let top = A + 2 * PAGE;
        m.fill(top - 16, 0x77, 8, 2).unwrap();
        assert_eq!(m.fill(top - 16, 0x88, 8, 3), Err(fault(top, Access::Write)));
        assert_eq!(m.read_u64(top - 8), Ok(0x77));
        // A count that no memory could hold is a fault, not a wrapped length.
        assert_eq!(m.fill(A, 0, 8, u64::MAX), Err(fault(top, Access::Write)));
        m.fill(A, 1, 8, 0).unwrap();
    }

    #[test]
    fn a_copy_up_is_a_forward_copy_or_is_not_made() {
        let mut m = mem();
        m.map(A, 2 * PAGE, RW).unwrap();
        m.write_bytes(A, b"abcdefgh").unwrap();
        // Apart, and downward over itself: as a copy from the lowest byte up gives.
        assert!(matches!(m.copy_up(A + 100, A, 8), Some(Ok(()))));
        assert_eq!(m.read::<8>(A + 100), Ok(*b"abcdefgh"));
        assert!(matches!(m.copy_up(A + 98, A + 100, 8), Some(Ok(()))));
        assert_eq!(m.read::<10>(A + 98), Ok(*b"abcdefghgh"));
        // Touching is not overlapping.
        assert!(matches!(m.copy_up(A + 8, A, 8), Some(Ok(()))));
        assert_eq!(m.read::<16>(A), Ok(*b"abcdefghabcdefgh"));
        // Upward over itself, such a copy would read what it wrote: not made.
        assert!(m.copy_up(A + 1, A, 8).is_none());
        assert!(m.copy_up(A + 7, A, 8).is_none());
        assert_eq!(m.read::<16>(A), Ok(*b"abcdefghabcdefgh"));
        // A source or a target that is not all there: a fault and nothing copied.
        let top = A + 2 * PAGE;
        assert_eq!(m.copy_up(A + 200, top - 4, 8), Some(Err(fault(top, Access::Read))));
        assert_eq!(m.copy_up(top - 4, A, 8), Some(Err(fault(top, Access::Write))));
        assert_eq!(m.read_u32(top - 4), Ok(0));
        m.protect(A + PAGE, PAGE, R).unwrap();
        assert_eq!(m.copy_up(A + PAGE, A, 8), Some(Err(fault(A + PAGE, Access::Write))));
    }

    #[test]
    fn what_is_remembered_of_the_last_accesses_does_not_outlive_a_change() {
        let mut m = mem();
        m.map(A, PAGE, RW).unwrap();
        m.map(A + 4 * PAGE, PAGE, RW).unwrap();
        // To and fro between two regions, then a third: each is found.
        m.map(A + 8 * PAGE, PAGE, RW).unwrap();
        for at in [A, A + 4 * PAGE, A, A + 8 * PAGE, A + 4 * PAGE, A] {
            m.write_u64(at, at).unwrap();
            assert_eq!(m.read_u64(at), Ok(at));
        }
        // What was just read and written is then closed, cut, and taken away.
        m.protect(A, PAGE, R).unwrap();
        assert_eq!(m.write_u8(A, 1), Err(fault(A, Access::Write)));
        assert_eq!(m.read_u64(A), Ok(A));
        m.protect(A, PAGE, 0).unwrap();
        assert_eq!(m.read_u8(A), Err(fault(A, Access::Read)));
        m.read_u8(A + 4 * PAGE).unwrap();
        m.unmap(A + 4 * PAGE, PAGE).unwrap();
        assert_eq!(m.read_u8(A + 4 * PAGE), Err(fault(A + 4 * PAGE, Access::Read)));
        // An access that begins in a remembered region and runs out of it is not let by.
        m.read_u8(A + 8 * PAGE).unwrap();
        assert_eq!(m.read_u64(A + 9 * PAGE - 4), Err(fault(A + 9 * PAGE, Access::Read)));
        // Code likewise.
        m.map(A + 12 * PAGE, PAGE, R | X).unwrap();
        assert_eq!(m.code_at(A + 12 * PAGE).1, 15);
        assert_eq!(m.code_at(A + 13 * PAGE - 3).1, 3);
        m.protect(A + 12 * PAGE, PAGE, R).unwrap();
        assert_eq!(m.code_at(A + 12 * PAGE).1, 0);
    }

    #[test]
    fn a_page_with_translated_code_is_written_by_no_one() {
        let mut m = mem();
        let hp = m.host_page();
        m.map(A, 2 * hp, RW).unwrap();
        m.map(A + 2 * hp, hp, RX).unwrap();
        m.write_u64(A + 8, 5).unwrap();
        // Marked from the middle of the first host page to the middle of the third:
        // whole host pages are.
        assert!(m.guest_may_write(A, A + 1) && m.guest_may_write(A + 2 * hp - 1, A + 3 * hp));
        assert!(!m.guest_may_write(A + 2 * hp, A + 3 * hp) && !m.guest_may_write(A + 3 * hp, A + 4 * hp));
        m.mark_code(A + hp - 1, A + 2 * hp + 1, true).unwrap();
        // (The mark does not change what the guest may do.)
        assert!(m.guest_may_write(A, A + 1) && !m.guest_may_write(A + 2 * hp, A + 3 * hp));
        assert!(m.is_code(A) && m.is_code(A + hp) && m.is_code(A + 3 * hp - 1));
        assert!(!m.is_code(A + 3 * hp) && !m.is_code(A - 1));
        // Read and run as before, written by neither the interpreter nor the host.
        assert_eq!(m.read_u64(A + 8), Ok(5));
        assert_eq!(m.code_at(A + 2 * hp).1, 15);
        assert_eq!(m.write_u64(A + 8, 6), Err(fault(A + 8, Access::Write)));
        assert_eq!(m.fill(A, 0, 8, 1), Err(fault(A, Access::Write)));
        assert!(host_refuses(&m, A + 8, true));
        assert!(!host_refuses(&m, A + 8, false));
        // A change of protection by the guest does not take the mark off.
        m.protect(A, hp, RW).unwrap();
        assert!(m.is_code(A));
        assert_eq!(m.write_u64(A + 8, 6), Err(fault(A + 8, Access::Write)));
        // Unmarked, it is what it was: the guest's protection, the contents.
        m.mark_code(A, A + 3 * hp, false).unwrap();
        assert!(!m.is_code(A) && !m.is_code(A + 2 * hp));
        m.write_u64(A + 8, 6).unwrap();
        assert!(!host_refuses(&m, A + 8, true));
        assert_eq!(m.write_u8(A + 2 * hp, 0), Err(fault(A + 2 * hp, Access::Write)));
        assert_eq!(m.regions(), &[
            Region { start: A, end: A + 2 * hp, prot: RW },
            Region { start: A + 2 * hp, end: A + 3 * hp, prot: RX },
        ]);
        // A mark over a hole marks what is there.
        m.unmap(A + hp, hp).unwrap();
        m.mark_code(A, A + 3 * hp, true).unwrap();
        assert!(m.is_code(A) && !m.is_code(A + hp) && m.is_code(A + 2 * hp));
    }

    #[test]
    fn writes_that_are_kept_can_be_put_back() {
        let mut m = mem();
        m.map(A, PAGE, RW).unwrap();
        m.write_bytes(A, b"abcdefgh").unwrap();
        m.log_writes();
        m.write_u16(A + 2, 0x5a59).unwrap();
        m.write_u8(A + 3, b'!').unwrap();
        m.write_u32(A + 100, 7).unwrap();
        assert_eq!(m.read::<8>(A), Ok(*b"abY!efgh"));
        let written = m.undo_writes();
        // Everything as it was, and what each write wrote, in their order: the first as
        // it was written, before the second wrote over half of it.
        assert_eq!(m.read::<8>(A), Ok(*b"abcdefgh"));
        assert_eq!(m.read_u32(A + 100), Ok(0));
        assert_eq!(written, vec![(A + 2, b"YZ".to_vec()), (A + 3, b"!".to_vec()), (A + 100, vec![7, 0, 0, 0])]);
        // Nothing is kept after.
        m.write_u8(A, b'z').unwrap();
        assert!(m.undo_writes().is_empty());
        assert_eq!(m.read_u8(A), Ok(b'z'));
    }

    #[test]
    fn the_host_is_as_open_as_the_list_to_the_host_page() {
        let mut m = mem();
        let hp = m.host_page();
        // Three host pages: one to write, one to read only, one mapped and closed.
        m.map(A, hp, RW).unwrap();
        m.map(A + hp, hp, R).unwrap();
        m.map(A + 2 * hp, hp, 0).unwrap();
        assert!(!host_refuses(&m, A, true));
        assert!(!host_refuses(&m, A + hp, false));
        assert!(host_refuses(&m, A + hp, true));
        assert!(host_refuses(&m, A + 2 * hp, false));
        // Code can be read by the host and is not written by it.
        m.protect(A, hp, RX).unwrap();
        assert!(!host_refuses(&m, A, false));
        assert!(host_refuses(&m, A, true));
        // Unmapped, the host closes it again.
        m.unmap(A + hp, hp).unwrap();
        assert!(host_refuses(&m, A + hp, false));
    }

    #[test]
    fn a_host_page_shared_is_as_open_as_its_most_open_guest_page() {
        let mut m = mem();
        let hp = m.host_page();
        if hp == PAGE {
            // Nothing is shared on a host with the guest's page.
            return;
        }
        // The first guest page of a host page to write, the second closed.
        m.map(A, PAGE, RW).unwrap();
        m.map(A + PAGE, PAGE, 0).unwrap();
        // The list is exact.
        assert_eq!(m.read_u8(A + PAGE), Err(fault(A + PAGE, Access::Read)));
        assert_eq!(m.read_u8(A + 2 * PAGE), Err(fault(A + 2 * PAGE, Access::Read)));
        // The host is not (ADR 0008, point B): the whole host page is open.
        assert!(!host_refuses(&m, A + PAGE, true));
        assert!(!host_refuses(&m, A + 2 * PAGE, true));
        // Until the last guest page in it is gone.
        m.unmap(A, PAGE).unwrap();
        assert!(host_refuses(&m, A + PAGE, false));
        m.unmap(A + PAGE, PAGE).unwrap();
        assert!(host_refuses(&m, A, false));
        // And what was written there by the way is not found by the next mapping.
        m.map(A, PAGE, RW).unwrap();
        m.write_u64(A, 99).unwrap();
        m.unmap(A, PAGE).unwrap();
        m.map(A, hp, RW).unwrap();
        assert_eq!(m.read_u64(A), Ok(0));
    }
    #[test]
    fn two_handles_on_one_memory_see_each_others_changes() {
        let mut first = mem();
        first.map(A, PAGE, RW).unwrap();
        first.write_u64(A, 7).unwrap();
        assert!(!first.is_shared());
        let mut second = first.view();
        assert!(first.is_shared() && second.is_shared());
        // What was there is there for both: the memory is one.
        assert_eq!(second.read_u64(A), Ok(7));
        second.write_u64(A, 8).unwrap();
        assert_eq!(first.read_u64(A), Ok(8));
        // A mapping made through one is the other's when it next looks; until it
        // looks, its own list is the old one.
        second.map(A + 4 * PAGE, PAGE, RW).unwrap();
        assert!(first.write_u64(A + 4 * PAGE, 1).is_err());
        first.sync();
        first.write_u64(A + 4 * PAGE, 1).unwrap();
        assert_eq!(second.read_u64(A + 4 * PAGE), Ok(1));
        // A change made on an old list is made on the newest: nothing of the
        // other's is lost.
        second.map(A + 8 * PAGE, PAGE, RW).unwrap();
        first.unmap(A, PAGE).unwrap();
        second.sync();
        assert!(second.read_u64(A).is_err());
        second.write_u64(A + 8 * PAGE, 2).unwrap();
        assert_eq!(first.read_u64(A + 8 * PAGE), Ok(2));
        // Stores into guarded code are counted for all of them.
        assert_eq!((first.guarded_writes(), second.wrote_guarded(), first.guarded_writes()), (0, 1, 1));
    }

    #[test]
    fn a_write_made_only_if_nothing_came_between() {
        let mut m = mem();
        m.map(A, PAGE, RW).unwrap();
        m.write_u64(A, 0x1111_2222_3333_4444).unwrap();
        // The low 32 bits are still what was read: written. Then they are not.
        assert_eq!(m.swap_if(A, 32, 0x3333_4444, 0xaaaa_bbbb), Ok(true));
        assert_eq!(m.swap_if(A, 32, 0x3333_4444, 0xcccc_dddd), Ok(false));
        assert_eq!(m.read_u64(A), Ok(0x1111_2222_aaaa_bbbb));
        for (bits, old, new, after) in [(8u32, 0xbbu64, 0x01u64, 0x1111_2222_aaaa_bb01u64), (16, 0xbb01, 0x0203, 0x1111_2222_aaaa_0203), (64, 0x1111_2222_aaaa_0203, 9, 9)] {
            assert_eq!(m.swap_if(A, bits, old, new), Ok(true));
            assert_eq!(m.read_u64(A), Ok(after));
        }
        // Where it may not be written, a fault, and nothing written.
        m.protect(A, PAGE, R).unwrap();
        assert_eq!(m.swap_if(A, 64, 9, 1), Err(fault(A, Access::Write)));
        assert_eq!(m.read_u64(A), Ok(9));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_shared_mapping_is_the_files_pages_and_a_childs_too() {
        let mut m = Mem::reserve().unwrap();
        // (Only where the host's page is the guest's.)
        if m.host_page() != PAGE {
            assert!(matches!(m.map_shared(A, PAGE, RW, None), Err(MapError::Range)));
            return;
        }
        let fd = unsafe { libc::memfd_create(c"r64m".as_ptr(), 0) };
        assert!(fd >= 0 && unsafe { libc::ftruncate(fd, 2 * PAGE as i64) } == 0);
        let in_file = |at: i64| {
            let mut v = [0u8; 8];
            assert_eq!(unsafe { libc::pread(fd, v.as_mut_ptr() as *mut libc::c_void, 8, at) }, 8);
            u64::from_le_bytes(v)
        };
        // What is written is in the file, and in another mapping of it.
        m.map_shared(A, 2 * PAGE, RW, Some((fd, 0))).unwrap();
        m.write_u64(A + PAGE + 8, 7).unwrap();
        assert_eq!(in_file(PAGE as i64 + 8), 7);
        m.map_shared(A + 8 * PAGE, PAGE, R, Some((fd, PAGE))).unwrap();
        assert_eq!(m.read_u64(A + 8 * PAGE + 8), Ok(7));
        assert!(m.write_u64(A + 8 * PAGE, 1).is_err());
        // Protected and unmapped as any region; the file keeps what it was given.
        m.protect(A, PAGE, R).unwrap();
        assert!(m.write_u64(A, 1).is_err());
        m.unmap(A, 2 * PAGE).unwrap();
        assert!(m.read_u64(A + PAGE + 8).is_err());
        assert_eq!(in_file(PAGE as i64 + 8), 7);
        // With no file: what a child writes, its parent reads.
        m.map_shared(A + 16 * PAGE, PAGE, RW, None).unwrap();
        m.map(A + 17 * PAGE, PAGE, RW).unwrap();
        let (shared, private) = (m.host(A + 16 * PAGE) as *mut u64, m.host(A + 17 * PAGE) as *mut u64);
        match unsafe { libc::fork() } {
            0 => unsafe {
                shared.write(9);
                private.write(9);
                libc::_exit(0)
            },
            child => {
                assert!(child > 0);
                let mut status = 0;
                assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            }
        }
        assert_eq!((m.read_u64(A + 16 * PAGE), m.read_u64(A + 17 * PAGE)), (Ok(9), Ok(0)));
        // Mapped over afresh, it is the guest's own again and reads as nothing.
        m.map(A + 16 * PAGE, PAGE, RW).unwrap();
        assert_eq!(m.read_u64(A + 16 * PAGE), Ok(0));
        // A private mapping of the file (R053): the file's bytes are read, what is
        // written stays this mapping's, and the file's own later change is seen in
        // a page not yet written (as Linux has it) and not in one that was.
        m.map_file(A + 24 * PAGE, 2 * PAGE, RW, fd, 0).unwrap();
        assert_eq!(m.read_u64(A + 25 * PAGE + 8), Ok(7));
        m.write_u64(A + 25 * PAGE + 8, 11).unwrap();
        assert_eq!(in_file(PAGE as i64 + 8), 7);
        let five = 5u64.to_le_bytes();
        assert_eq!(unsafe { libc::pwrite(fd, five.as_ptr() as *const libc::c_void, 8, 16) }, 8);
        assert_eq!(unsafe { libc::pwrite(fd, five.as_ptr() as *const libc::c_void, 8, PAGE as i64 + 16) }, 8);
        assert_eq!((m.read_u64(A + 24 * PAGE + 16), m.read_u64(A + 25 * PAGE + 16)), (Ok(5), Ok(0)));
        // Read only, it is not written; protected otherwise and unmapped as any.
        m.map_file(A + 32 * PAGE, PAGE, R, fd, PAGE).unwrap();
        assert!(m.write_u64(A + 32 * PAGE, 1).is_err());
        m.protect(A + 32 * PAGE, PAGE, RW).unwrap();
        m.write_u64(A + 32 * PAGE, 1).unwrap();
        assert_eq!(in_file(PAGE as i64), 0);
        m.unmap(A + 24 * PAGE, 2 * PAGE).unwrap();
        assert!(m.read_u64(A + 24 * PAGE).is_err());
        // A descriptor that is no file is refused, and nothing is left there.
        assert!(matches!(m.map_file(A + 40 * PAGE, PAGE, R, -1, 0), Err(MapError::Host(_))));
        assert!(m.read_u64(A + 40 * PAGE).is_err());
        unsafe { libc::close(fd) };
    }
}
