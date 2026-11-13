//! G4 (ADR-0010): the engine verifies TLS. Every rejection test is paired with an acceptance
//! test using the same harness, so a "rejected" result cannot be an unrelated failure.
//! Runs where rustls is the backend (Linux). On Windows the SChannel backend is exercised by
//! the manual checklist, NOT here.
#![cfg(not(windows))]
mod common;
use common::*;
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
    let mut c = fast_cfg(1);
    c.max_attempts = 2;
    c.extra_root_certs_pem = vec![ca.cert.pem().into_bytes()];
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_accepts_with_test_root() {
    let ca = new_ca("Eid Test CA");
    let (port, served) = tls_server(&ca, Leaf::Valid("localhost")).await;
    let dir = tempfile::tempdir().unwrap();
    let (o, _) = get(
        &format!("https://localhost:{port}/ok.bin"),
        dir.path(),
        cfg_trusting(&ca),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&o.path).unwrap(), b"hello");
    assert!(served.load(Ordering::SeqCst) >= 1);
}

async fn rejected(leaf: Leaf, trust_ca: bool) {
    let ca = new_ca("Eid Test CA");
    let other = new_ca("Some Other CA");
    let (port, served) = tls_server(&ca, leaf).await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = if trust_ca {
        cfg_trusting(&ca)
    } else {
        cfg_trusting(&other)
    };
    let r = get(&format!("https://localhost:{port}/no.bin"), dir.path(), cfg).await;
    assert!(r.is_err(), "certificate problem must fail the download");
    assert_eq!(
        served.load(Ordering::SeqCst),
        0,
        "no HTTP request may be sent over an unverified connection"
    );
    assert!(only_files(dir.path()).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_rejects_selfsigned() {
    rejected(Leaf::SelfSigned("localhost"), true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_rejects_expired() {
    rejected(Leaf::Expired("localhost"), true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_rejects_wrong_host() {
    rejected(Leaf::Valid("other.example"), true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tls_rejects_untrusted_ca() {
    // valid chain and name, but the client trusts a different root
    rejected(Leaf::Valid("localhost"), false).await;
}
