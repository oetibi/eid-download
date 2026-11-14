//! Test double for the app: understands `serve --endpoint NAME --token-file PATH`, listens on
//! that endpoint, answers ping/add, exits on `shutdown` (and, as a safety net, after 90 s).
//! Used by the native-host tests to check that the host really launches a detached program.

use eid_ipc::{start, Command, Endpoint, ServeOptions};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if args.first().map(String::as_str) != Some("serve") {
        eprintln!("fake_app: expected `serve`");
        std::process::exit(2);
    }
    let (Some(name), Some(token)) = (get("--endpoint"), get("--token-file")) else {
        eprintln!("fake_app: --endpoint and --token-file are required");
        std::process::exit(2);
    };
    let stop = Arc::new(tokio::sync::Notify::new());
    let s2 = stop.clone();
    let handler = move |cmd: Command| match cmd {
        Command::Shutdown {} => {
            s2.notify_one();
            Ok(serde_json::json!({}))
        }
        Command::Add { url, .. } => Ok(serde_json::json!({"id": 1, "url": url, "fake": true})),
        _ => Ok(serde_json::json!({"pong": true, "fake": true})),
    };
    let ep = Endpoint {
        name,
        token_path: token.into(),
    };
    let running = match start(&ep, Arc::new(handler), ServeOptions::default()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fake_app: {e}");
            std::process::exit(1);
        }
    };
    tokio::select! {
        _ = stop.notified() => {}
        _ = tokio::time::sleep(Duration::from_secs(90)) => {}
    }
    tokio::time::sleep(Duration::from_millis(150)).await; // let the shutdown reply go out
    running.stop().await;
}
