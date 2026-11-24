//! r64m: an x86-64 emulator with a JIT for arm64 hosts, the Rust port of g64m.
//!
//! Nothing is ported yet. This is the skeleton the port starts from.

/// What `r64m` says of itself.
fn banner() -> String {
    format!(
        "r64m {}: the Rust port of g64m; nothing is ported yet",
        env!("CARGO_PKG_VERSION")
    )
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::banner;

    #[test]
    fn the_banner_names_the_version() {
        assert!(banner().starts_with("r64m 0.1.0"));
    }
}
