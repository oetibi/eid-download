//! Loopback server for the reproducible benchmarks in `scripts/bench/` (not a test gate).
//!
//! `eid-benchserver --size BYTES [--throttle-per-conn BPS] [--seed N]` serves:
//! * `/big.bin`       well-behaved, ranges honoured, strong ETag;
//! * `/throttled.bin` the same bytes, every connection capped at `BPS` (simulates a server that
//!   limits each connection, the case where several connections help);
//! * `/no-range.bin`  the same bytes without range support (single stream).
//!
//! Prints `URL <base>` first, then `SHA256 <hex>` of the content once it is computed (streamed,
//! never materialised), then serves until stdin closes or Ctrl-C.
#![forbid(unsafe_code)]
use eid_testserver::{Behavior, Content, TestServer};
use std::io::Write;

fn usage() -> ! {
    eprintln!("usage: eid-benchserver --size BYTES [--throttle-per-conn BPS] [--seed N]");
    std::process::exit(2)
}

fn main() -> std::io::Result<()> {
    let mut size: Option<u64> = None;
    let mut throttle: u64 = 4 * 1024 * 1024;
    let mut seed: u64 = 7;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut num = || {
            it.next()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or_else(|| usage())
        };
        match a.as_str() {
            "--size" => size = Some(num()),
            "--throttle-per-conn" => throttle = num(),
            "--seed" => seed = num(),
            _ => usage(),
        }
    }
    let Some(len) = size else { usage() };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let server = TestServer::start().await;
        let content = Content::Generated { seed, len };
        server.add("/big.bin", content.clone(), Behavior::well_behaved());
        server.add(
            "/throttled.bin",
            content.clone(),
            Behavior::slow(throttle.max(1)),
        );
        server.add("/no-range.bin", content.clone(), Behavior::no_range());
        println!("URL {}", server.base_url());
        std::io::stdout().flush()?;
        let sha = tokio::task::spawn_blocking(move || content.sha256_hex())
            .await
            .map_err(std::io::Error::other)?;
        println!("SHA256 {sha}");
        std::io::stdout().flush()?;
        // Serve until the parent closes our stdin (benchmark script exit) or Ctrl-C.
        let stdin_closed = tokio::task::spawn_blocking(|| {
            let mut sink = Vec::new();
            let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut sink);
        });
        tokio::select! {
            _ = stdin_closed => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        Ok(())
    })
}
