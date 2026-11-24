//! r64m: an x86-64 emulator with a JIT for arm64 hosts. A rebuild of g64m in Rust.
//!
//! So far: the memory of a program without a kernel (`mem`). Nothing runs yet.

mod cpu;
mod elf;
// Until the translator uses it.
#[allow(dead_code)]
mod jit;
// Some of it waits for the calls that map and unmap.
#[allow(dead_code)]
mod mem;
mod sys;

use std::io::Write;

/// What `r64m` says of itself.
fn banner() -> String {
    format!(
        "r64m {}: a rebuild of g64m in Rust; runs static programs, interpreted",
        env!("CARGO_PKG_VERSION")
    )
}

fn fail(what: std::fmt::Arguments) -> ! {
    eprintln!("r64m: {what}");
    std::process::exit(2)
}

/// One line of a trace: the registers and flags after an instruction, in the form of
/// g64m's `--trace` and of the oracle that single-steps the real CPU.
fn trace_line(out: &mut impl Write, c: &cpu::Cpu) -> std::io::Result<()> {
    const NAMES: [&str; 16] = [
        "RAX", "RCX", "RDX", "RBX", "RSP", "RBP", "RSI", "RDI", "R8", "R9", "R10", "R11", "R12",
        "R13", "R14", "R15",
    ];
    write!(out, "RIP={:016X}", c.rip)?;
    for (name, v) in NAMES.iter().zip(c.reg) {
        write!(out, " {name}={v:016X}")?;
    }
    write!(out, " F={:08X}", c.rflags)
}

/// What `--trace-xmm` adds to a line: MXCSR without its six status bits, and each XMM
/// register as `X3=high:low`, each only when it is not what it was on the line before.
struct SeenXmm {
    mxcsr: u32,
    xmm: [u64; 32],
}

impl SeenXmm {
    fn line(&mut self, out: &mut impl Write, c: &cpu::Cpu) -> std::io::Result<()> {
        if c.mxcsr & 0xffc0 != self.mxcsr {
            self.mxcsr = c.mxcsr & 0xffc0;
            write!(out, " MXCSR={:04X}", self.mxcsr)?;
        }
        for i in 0..16 {
            let (lo, hi) = (c.xmm[i * 2], c.xmm[i * 2 + 1]);
            if (lo, hi) != (self.xmm[i * 2], self.xmm[i * 2 + 1]) {
                (self.xmm[i * 2], self.xmm[i * 2 + 1]) = (lo, hi);
                write!(out, " X{i}={hi:016X}:{lo:016X}")?;
            }
        }
        Ok(())
    }
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let count = args.first().is_some_and(|a| a == "--count");
    if count {
        args.remove(0);
    }
    let trace_xmm = args.first().is_some_and(|a| a == "--trace-xmm");
    let trace = trace_xmm || args.first().is_some_and(|a| a == "--trace");
    if trace {
        args.remove(0);
    }
    let mut seen = SeenXmm { mxcsr: 0x1f80, xmm: [0; 32] };
    let mut mem = mem::Mem::reserve()
        .unwrap_or_else(|e| fail(format_args!("the host gave no address space for the guest: {e}")));
    let Some(path) = args.first() else {
        println!("{}", banner());
        println!(
            "guest memory: 2^{} bytes reserved, host pages of {} KB",
            mem.size().trailing_zeros(),
            mem.host_page() / 1024
        );
        println!("usage: r64m [--count] [--trace | --trace-xmm] program [arguments]");
        return;
    };

    let img = std::fs::read(path).unwrap_or_else(|e| fail(format_args!("{path}: {e}")));
    let loaded = elf::load(&mut mem, &img).unwrap_or_else(|e| fail(format_args!("{path}: {e}")));
    let guest_args: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
    let rsp = elf::first_stack(&mut mem, &loaded, &guest_args, &[])
        .unwrap_or_else(|e| fail(format_args!("{path}: {e}")));

    let mut c = cpu::Cpu::new(loaded.entry, rsp);
    c.rep_whole = !trace;
    let output = if trace { sys::Output::Nowhere } else { sys::Output::Host };
    let mut lines = std::io::BufWriter::new(std::io::stdout().lock());
    let code = loop {
        match cpu::step(&mut c, &mut mem) {
            Ok(cpu::Step::Next) => {}
            Ok(cpu::Step::Syscall) => {
                // The exit that ends a program is not a line of the trace: there is
                // nothing after it to show.
                if let Some(code) = sys::call(&mut c, &mem, &output) {
                    break code;
                }
            }
            Err(stop) => {
                lines.flush().ok();
                let what = match stop {
                    cpu::Stop::Fault(f) => format!("{:?} of {:#x} that is not allowed", f.access, f.addr),
                    cpu::Stop::Divide => "a division that cannot be done".to_string(),
                    cpu::Stop::Undefined => "not an instruction".to_string(),
                    cpu::Stop::Protection => "an instruction the CPU refuses".to_string(),
                    cpu::Stop::NotWritten(bytes) => format!("an instruction not written yet: {bytes}"),
                };
                // Nothing is delivered to the guest yet (ADR 0008's signals): it ends.
                fail(format_args!("at {:#x} after {} instructions: {what}", c.rip, c.steps));
            }
        }
        if trace {
            let mut line = trace_line(&mut lines, &c);
            if trace_xmm {
                line = line.and_then(|()| seen.line(&mut lines, &c));
            }
            line.and_then(|()| writeln!(lines))
                .unwrap_or_else(|e| fail(format_args!("the trace: {e}")));
        }
    };
    lines.flush().ok();
    if count {
        eprintln!("r64m: {} instructions", c.steps);
    }
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn the_banner_names_the_version() {
        assert!(banner().starts_with("r64m 0.1.0"));
    }
}
