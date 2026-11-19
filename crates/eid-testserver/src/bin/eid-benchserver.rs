//! Loopback server for the reproducible benchmarks in `scripts/bench/` (not a test gate).
//!
//! `eid-benchserver --size BYTES [--throttle-per-conn BPS] [--seed N] [--connect-delay-ms MS]`
//! serves (`--connect-delay-ms`: every NEW connection waits that long before its first request,
//! like the handshake round trips a reused connection would skip):
//! * `/big.bin`       well-behaved, ranges honoured, strong ETag;
//! * `/throttled.bin` the same bytes, every connection capped at `BPS` (simulates a server that
//!   limits each connection, the case where several connections help);
//! * `/no-range.bin`  the same bytes without range support (single stream).
//!
//! Prints `URL <base>` first, then `SHA256 <hex>` of the content once it is computed (streamed,
//! never materialised), then serves until stdin closes or Ctrl-C.
//!
//! v0.2: a line `STATS` on stdin prints one line `STATS {json}` with, per path, the TCP
//! connections the server accepted for it, the requests, the peak of concurrent responses and
//! the bytes sent since the previous `STATS` (the counters are then reset). The benchmark uses
//! it to measure how many connections one download opens (connection reuse, ADR-0012).
#![forbid(unsafe_code)]
use eid_testserver::{Behavior, Content, TestServer};
use std::io::Write;

fn usage() -> ! {
    eprintln!("usage: eid-benchserver --size BYTES [--throttle-per-conn BPS] [--seed N] [--connect-delay-ms MS]");
    std::process::exit(2)
}

fn main() -> std::io::Result<()> {
    let mut size: Option<u64> = None;
    let mut throttle: u64 = 4 * 1024 * 1024;
    let mut seed: u64 = 7;
    let mut connect_delay_ms: u64 = 0;
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
            "--connect-delay-ms" => connect_delay_ms = num(),
            _ => usage(),
        }
    }
    let Some(len) = size else { usage() };
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let server = TestServer::start().await;
        server.set_connect_delay(std::time::Duration::from_millis(connect_delay_ms));
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
        // Serve until the parent closes our stdin (benchmark script exit) or Ctrl-C; answer
        // `STATS` lines meanwhile.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        // the reader thread ends with stdin; it is not awaited (exit does not wait for it)
        let _reader = tokio::task::spawn_blocking(move || {
            for line in std::io::BufRead::lines(std::io::stdin().lock()) {
                let Ok(l) = line else { break };
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        loop {
            tokio::select! {
                line = rx.recv() => match line {
                    Some(l) if l.trim() == "STATS" => {
                        // plain JSON by hand: paths and numbers only (no extra dependency)
                        let parts: Vec<String> = ["/big.bin", "/throttled.bin", "/no-range.bin"]
                            .iter()
                            .map(|path| {
                                let st = server.stats(path);
                                server.reset_stats(path);
                                format!(
                                    "\"{path}\":{{\"connections\":{},\"requests\":{},\"bytes\":{},\"peak_concurrent\":{}}}",
                                    st.connections_accepted,
                                    st.requests.len(),
                                    st.bytes_sent_total,
                                    st.peak_concurrent
                                )
                            })
                            .collect();
                        println!("STATS {{{}}}", parts.join(","));
                        std::io::stdout().flush()?;
                    }
                    Some(_) => {}
                    None => break,
                },
                _ = tokio::signal::ctrl_c() => break,
            }
        }
        Ok(())
    })
}
