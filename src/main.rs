//! r64m: an x86-64 emulator with a JIT for arm64 hosts. A rebuild of g64m in Rust.
//!
//! So far: the memory of a program without a kernel (`mem`). Nothing runs yet.

// Until the interpreter uses all of them.
#[allow(dead_code)]
mod elf;
#[allow(dead_code)]
mod mem;

/// What `r64m` says of itself.
fn banner() -> String {
    format!("r64m {}: a rebuild of g64m in Rust; nothing runs yet", env!("CARGO_PKG_VERSION"))
}

fn fail(what: std::fmt::Arguments) -> ! {
    eprintln!("r64m: {what}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut mem = mem::Mem::reserve()
        .unwrap_or_else(|e| fail(format_args!("the host gave no address space for the guest: {e}")));
    let Some(path) = args.get(1) else {
        println!("{}", banner());
        println!(
            "guest memory: 2^{} bytes reserved, host pages of {} KB",
            mem.size().trailing_zeros(),
            mem.host_page() / 1024
        );
        return;
    };

    // Until there is an interpreter: load the program and say where it would begin.
    let img = std::fs::read(path).unwrap_or_else(|e| fail(format_args!("{path}: {e}")));
    let loaded = elf::load(&mut mem, &img).unwrap_or_else(|e| fail(format_args!("{path}: {e}")));
    let guest_args: Vec<&[u8]> = args[1..].iter().map(|a| a.as_bytes()).collect();
    let rsp = elf::first_stack(&mut mem, &loaded, &guest_args, &[])
        .unwrap_or_else(|e| fail(format_args!("{path}: {e}")));
    println!("entry {:#x}, stack pointer {:#x}, brk {:#x}", loaded.entry, rsp, loaded.brk);
    for r in mem.regions() {
        let p = |bit, c| if r.prot & bit != 0 { c } else { '-' };
        println!("{:012x}-{:012x} {}{}{}", r.start, r.end, p(mem::R, 'r'), p(mem::W, 'w'), p(mem::X, 'x'));
    }
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn the_banner_names_the_version() {
        assert!(banner().starts_with("r64m 0.1.0"));
    }
}
