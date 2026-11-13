//! Test helper: prints argv[1] exactly (used by the shell-quoting injection matrix).
use std::io::Write;
fn main() {
    let a = std::env::args().nth(1).unwrap_or_default();
    let mut o = std::io::stdout();
    let _ = o.write_all(a.as_bytes());
}
