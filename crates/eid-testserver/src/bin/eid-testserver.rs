#![forbid(unsafe_code)]
use eid_testserver::{Behavior, Content, TestServer};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let server = TestServer::start().await;
    for (path, behavior) in [
        ("/demo.bin", Behavior::well_behaved()),
        ("/no-range.bin", Behavior::no_range()),
        ("/cuts.bin", Behavior::cuts_every(32 * 1024)),
        // throttled per connection: slow enough to watch progress, pause and resume (GUI smoke)
        ("/slow.bin", Behavior::slow(48 * 1024)),
    ] {
        server.add(
            path,
            Content::Generated {
                seed: 42,
                len: 3 * 1024 * 1024,
            },
            behavior,
        );
    }
    // v0.2 window test: a link that answers with a web page (like a YouTube "watch" page)
    server.add(
        "/watch",
        Content::Bytes(b"<!doctype html><title>page</title><p>a web page, not a file".repeat(100)),
        Behavior {
            content_type: Some("text/html; charset=utf-8".into()),
            ..Behavior::well_behaved()
        },
    );
    println!("{}", server.base_url());
    tokio::signal::ctrl_c().await
}
