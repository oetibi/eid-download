//! ADR-0012 TLS handshake cost bench (NOT a gate: `#[ignore]`, never in required_tests.txt).
//! Run: `scripts/bench_tls.sh` (= `cargo test -p eid-engine --test bench_tls -- --ignored --nocapture`).
//! Loopback only: measures CPU cost of TCP+TLS1.3 setup with `pool_max_idle_per_host(0)` (one fresh
//! connection per request) against plain HTTP on the same host. Real-network RTT and SChannel
//! (Windows) are NOT measured. Numbers depend on the machine; compare, do not quote as constants.
#![cfg(not(windows))]
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

struct Ca {
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn new_ca(name: &str) -> Ca {
    let key = KeyPair::generate().unwrap();
    let mut p = CertificateParams::new(vec![]).unwrap();
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.distinguished_name.push(DnType::CommonName, name);
    let cert = p.self_signed(&key).unwrap();
    Ca { cert, key }
}

enum Leaf {
    Valid(&'static str),
    Expired(&'static str),
    SelfSigned(&'static str),
}

/// Starts a one-file HTTPS server; returns (port, number of HTTP requests actually served).
async fn tls_server(ca: &Ca, leaf: Leaf) -> (u16, Arc<AtomicUsize>) {
    let key = KeyPair::generate().unwrap();
    let (name, expired, selfsigned) = match leaf {
        Leaf::Valid(n) => (n, false, false),
        Leaf::Expired(n) => (n, true, false),
        Leaf::SelfSigned(n) => (n, false, true),
    };
    let mut p = CertificateParams::new(vec![name.to_string()]).unwrap();
    if expired {
        p.not_before = rcgen::date_time_ymd(2020, 1, 1);
        p.not_after = rcgen::date_time_ymd(2021, 1, 1);
    }
    let cert = if selfsigned {
        p.self_signed(&key).unwrap()
    } else {
        p.signed_by(&key, &ca.cert, &ca.key).unwrap()
    };
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let cfg = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let served = Arc::new(AtomicUsize::new(0));
    let s2 = served.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            let served = s2.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(sock).await else {
                    return;
                };
                let mut buf = vec![0u8; 4096];
                let mut got = 0;
                while got < buf.len() {
                    let Ok(n) = tls.read(&mut buf[got..]).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    got += n;
                    if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                served.fetch_add(1, Ordering::SeqCst);
                let _ = tls
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                    )
                    .await;
                let _ = tls.shutdown().await;
            });
        }
    });
    (port, served)
}

fn cfg_trusting(ca: &Ca) -> eid_engine::Config {
    let mut c = eid_engine::Config::default();
    c.max_attempts = 2;
    c.extra_root_certs_pem = vec![ca.cert.pem().into_bytes()];
    c
}

async fn plain_server() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let mut got = 0;
                loop {
                    let Ok(n) = s.read(&mut buf[got..]).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    got += n;
                    if buf[..got].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = s
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                    )
                    .await;
                let _ = s.shutdown().await;
            });
        }
    });
    port
}

/// Mean wall-clock milliseconds per request for `n` requests spread over `par` tasks.
async fn run_n(client: &reqwest::Client, url: &str, n: usize, par: usize) -> f64 {
    let t = std::time::Instant::now();
    let per = n / par;
    let mut hs = vec![];
    for _ in 0..par {
        let (c, u) = (client.clone(), url.to_string());
        hs.push(tokio::spawn(async move {
            for _ in 0..per {
                let r = c.get(&u).send().await.unwrap();
                let _ = r.bytes().await.unwrap();
            }
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
    t.elapsed().as_secs_f64() * 1000.0 / n as f64
}

#[ignore = "benchmark, run by scripts/bench_tls.sh"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_handshake_bench() {
    let ca = new_ca("Eid Test CA");
    let (tport, _) = tls_server(&ca, Leaf::Valid("localhost")).await;
    let pport = plain_server().await;
    let client = eid_engine::http::build_client(&cfg_trusting(&ca)).unwrap();
    let turl = format!("https://localhost:{tport}/x");
    let purl = format!("http://localhost:{pport}/x");
    let _ = run_n(&client, &turl, 20, 1).await; // warm-up
    let _ = run_n(&client, &purl, 20, 1).await;
    for par in [1usize, 8] {
        let t = run_n(&client, &turl, 400, par).await;
        let p = run_n(&client, &purl, 400, par).await;
        println!(
            "BENCH par={par}: https {t:.3} ms/req, http {p:.3} ms/req, TLS extra {:.3} ms/req",
            t - p
        );
    }
}
