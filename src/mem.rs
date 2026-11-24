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

use std::io;
use std::ptr;

/// The guest's page: x86's.
pub const PAGE: u64 = 4096;

/// Protection bits, with Linux's values for `PROT_READ`, `PROT_WRITE`, `PROT_EXEC`.
pub const R: u8 = 1;
pub const W: u8 = 2;
pub const X: u8 = 4;

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

#[derive(Debug)]
pub enum MapError {
    /// Not on a page, empty, or not inside the reservation (Linux's EINVAL or ENOMEM).
    Range,
    /// A change of protection over pages that are not all mapped (Linux's ENOMEM).
    NotMapped,
    /// The host refused. The list and the host may now disagree: not to be gone on from.
    Host(io::Error),
}

pub struct Mem {
    base: *mut u8,
    size: u64,
    host_page: u64,
    /// In order of address, none overlapping, neighbours of one protection joined.
    regions: Vec<Region>,
}

/// x86 has no page that can be written or run but not read: any mapping with a
/// protection can be read. A fetch needs `X` (the NX bit is on in every 64-bit Linux).
fn allows(prot: u8, access: Access) -> bool {
    match access {
        Access::Read => prot != 0,
        Access::Write => prot & W != 0,
        Access::Fetch => prot & X != 0,
    }
}

/// The host never runs the guest's bytes, so it is asked for reading and writing only.
fn host_prot(prot: u8) -> libc::c_int {
    let mut p = libc::PROT_NONE;
    if prot != 0 {
        p |= libc::PROT_READ;
    }
    if prot & W != 0 {
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
                return Ok(Mem { base: p as *mut u8, size, host_page, regions: Vec::new() });
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

    // ---- the guest's view: mapping -------------------------------------------------

    /// Maps `[start, start + len)` afresh, as `mmap` with `MAP_FIXED` and no file does:
    /// whatever was there is gone and the pages read as zero.
    pub fn map(&mut self, start: u64, len: u64, prot: u8) -> Result<(), MapError> {
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
    pub fn unmap(&mut self, start: u64, len: u64) -> Result<(), MapError> {
        let end = self.range(start, len)?;
        self.carve(start, end);
        self.host_sync(start, end).map_err(MapError::Host)
    }

    /// Changes the protection of `[start, start + len)`, all of which must be mapped,
    /// as `mprotect` does. The contents stay.
    pub fn protect(&mut self, start: u64, len: u64, prot: u8) -> Result<(), MapError> {
        let end = self.range(start, len)?;
        if self.walk(start, end, |_| true).is_some() {
            return Err(MapError::NotMapped);
        }
        self.carve(start, end);
        self.insert(Region { start, end, prot });
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
    pub fn read<const N: usize>(&self, addr: u64) -> Result<[u8; N], Fault> {
        let mut bytes = [0u8; N];
        self.read_bytes(addr, &mut bytes)?;
        Ok(bytes)
    }

    /// Writes `N` bytes, or writes none and says which byte could not be written.
    pub fn write<const N: usize>(&mut self, addr: u64, bytes: [u8; N]) -> Result<(), Fault> {
        self.write_bytes(addr, &bytes)
    }

    pub fn read_bytes(&self, addr: u64, into: &mut [u8]) -> Result<(), Fault> {
        self.copy_out(addr, into, Access::Read)
    }

    /// Reads bytes to be run: the pages must be executable.
    pub fn fetch(&self, addr: u64, into: &mut [u8]) -> Result<(), Fault> {
        self.copy_out(addr, into, Access::Fetch)
    }

    pub fn write_bytes(&mut self, addr: u64, from: &[u8]) -> Result<(), Fault> {
        self.check(addr, from.len() as u64, Access::Write)?;
        // The list allowed it, and the host is at least as open as the list.
        unsafe { ptr::copy_nonoverlapping(from.as_ptr(), self.host(addr), from.len()) };
        Ok(())
    }

    fn copy_out(&self, addr: u64, into: &mut [u8], access: Access) -> Result<(), Fault> {
        self.check(addr, into.len() as u64, access)?;
        unsafe { ptr::copy_nonoverlapping(self.host(addr), into.as_mut_ptr(), into.len()) };
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
    pub fn check(&self, addr: u64, len: u64, access: Access) -> Result<(), Fault> {
        // No region reaches past the reservation, so an end that would wrap is cut off
        // at the top and faults there like any other unmapped byte.
        let end = addr.saturating_add(len);
        match self.walk(addr, end, |prot| allows(prot, access)) {
            None => Ok(()),
            Some(at) => Err(Fault { addr: at, access }),
        }
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
    fn drop(&mut self) {
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
}
