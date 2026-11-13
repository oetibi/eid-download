//! SockDiag counts this process's client sockets to a port by ownership (Linux only; inside
//! eid-platform, so the platform cfg is allowed here).
#![cfg(target_os = "linux")]
use eid_platform::net::SockDiag;
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn wait_for(diag: &mut SockDiag, port: u16, want: usize) -> usize {
    let t = Instant::now();
    loop {
        let n = diag.client_sockets_to_port(port).expect("query works");
        if n == want || t.elapsed() > Duration::from_secs(3) {
            return n;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn sockdiag_counts_client_sockets_by_ownership_and_sees_closes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let other_port = other.local_addr().unwrap().port();
    let mut diag = SockDiag::open().expect("netlink SOCK_DIAG must be available on Linux");
    assert_eq!(wait_for(&mut diag, port, 0), 0);
    let mut clients: Vec<TcpStream> = (0..5)
        .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
        .collect();
    // keep the server-side ends alive: they must NOT be counted (their remote port is ephemeral)
    let accepted: Vec<TcpStream> = (0..5).map(|_| listener.accept().unwrap().0).collect();
    assert_eq!(wait_for(&mut diag, port, 5), 5, "5 client sockets");
    assert_eq!(
        diag.client_sockets_to_port(other_port),
        Some(0),
        "other port has none"
    );
    clients.truncate(3);
    assert_eq!(
        wait_for(&mut diag, port, 3),
        3,
        "count falls the moment sockets close"
    );
    drop(clients);
    assert_eq!(wait_for(&mut diag, port, 0), 0, "all closed");
    drop(accepted);
}
