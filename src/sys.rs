//! The system calls of a program without a kernel: r64m answers them as Linux would.
//!
//! So far what g64m answers: `write` to the standard output and error, and the two
//! exits. Everything else is ENOSYS.

use crate::cpu::{Cpu, RAX, RDI, RDX, RSI};
use crate::mem::Mem;
use std::io::Write;

const EBADF: u64 = 9;
const EFAULT: u64 = 14;
const ENOSYS: u64 = 38;

/// Where what the program writes goes.
pub enum Output {
    /// To r64m's own standard output and error.
    Host,
    /// Nowhere, as under the oracle that takes the traces: a trace has the output to
    /// itself.
    Nowhere,
}

/// Answers the call the registers describe. `Some` is the program's exit code.
pub fn call(c: &mut Cpu, m: &Mem, out: &Output) -> Option<i32> {
    let (a, b, d) = (c.reg[RDI], c.reg[RSI], c.reg[RDX]);
    let result: Result<u64, u64> = match c.reg[RAX] {
        1 => write(m, out, a, b, d),
        60 | 231 => return Some((a & 0xff) as i32),
        _ => Err(ENOSYS),
    };
    c.reg[RAX] = match result {
        Ok(v) => v,
        Err(errno) => errno.wrapping_neg(),
    };
    None
}

fn write(m: &Mem, out: &Output, fd: u64, at: u64, len: u64) -> Result<u64, u64> {
    if fd != 1 && fd != 2 {
        return Err(EBADF);
    }
    // No more at once than Linux writes at once.
    let len = len.min(0x7fff_f000);
    let mut bytes = vec![0u8; len as usize];
    m.read_bytes(at, &mut bytes).map_err(|_| EFAULT)?;
    if let Output::Host = out {
        let done = if fd == 1 {
            std::io::stdout().write_all(&bytes)
        } else {
            std::io::stderr().write_all(&bytes)
        };
        done.map_err(|e| e.raw_os_error().unwrap_or(5) as u64)?;
    }
    Ok(len)
}
