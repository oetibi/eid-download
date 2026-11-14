//! `eid-native-host`: started by the browser, speaks Native Messaging on stdin/stdout.
//! stdout is the protocol channel: **nothing here may print to it** (a guard test scans for
//! print macros); diagnostics go to stderr.

use eid_native_host::{run, Exit, HostConfig};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let cfg = match HostConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("eid-native-host: cannot determine the IPC endpoint: {e}");
            std::process::exit(2);
        }
    };
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let code = match run(&mut stdin, &mut stdout, &cfg).await {
        Exit::Eof => 0,
        Exit::ProtocolError => 1,
    };
    std::process::exit(code);
}
