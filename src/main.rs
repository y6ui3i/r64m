//! r64m: an x86-64 emulator with a JIT for arm64 hosts. A rebuild of g64m in Rust.
//!
//! So far: the memory of a program without a kernel (`mem`). Nothing runs yet.

// Until the loader and the interpreter use all of it.
#[allow(dead_code)]
mod mem;

/// What `r64m` says of itself.
fn banner() -> String {
    format!("r64m {}: a rebuild of g64m in Rust; nothing runs yet", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", banner());
    match mem::Mem::reserve() {
        Ok(m) => println!(
            "guest memory: 2^{} bytes reserved, host pages of {} KB",
            m.size().trailing_zeros(),
            m.host_page() / 1024
        ),
        Err(e) => {
            eprintln!("r64m: the host gave no address space for the guest: {e}");
            std::process::exit(1);
        }
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
