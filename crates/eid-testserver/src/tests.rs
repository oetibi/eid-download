use super::*;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{timeout, Duration, Instant},
};

const DATA: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
struct Reply {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}
async fn connect(s: &TestServer) -> TcpStream {
    timeout(
        Duration::from_secs(5),
        TcpStream::connect(s.base_url().trim_start_matches("http://")),
    )
    .await
    .unwrap()
    .unwrap()
}
async fn headers(socket: &mut TcpStream) -> Reply {
    let mut raw = Vec::new();
    loop {
        let b = timeout(Duration::from_secs(5), socket.read_u8())
            .await
            .unwrap()
            .unwrap();
        raw.push(b);
        if raw.ends_with(b"\r\n\r\n") {
            break;
        }
        assert!(raw.len() < 65536);
    }
    let raw = String::from_utf8(raw).unwrap();
    let mut lines = raw.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter_map(|s| s.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Reply {
        status,
        headers,
        body: Vec::new(),
    }
}
async fn request(s: &TestServer, method: &str, extra: &str) -> Reply {
    let mut socket = connect(s).await;
    socket
        .write_all(
            format!("{method} /f HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{extra}\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut r = headers(&mut socket).await;
    timeout(Duration::from_secs(5), socket.read_to_end(&mut r.body))
        .await
        .unwrap()
        .unwrap();
    r
}
async fn fixture(b: Behavior) -> TestServer {
    let s = TestServer::start().await;
    s.add("/f", Content::Bytes(DATA.to_vec()), b);
    s
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn body(r: &Reply, expected: &[u8]) {
    assert_eq!(digest(&r.body), digest(expected));
    assert_eq!(r.body, expected);
}

#[test]
fn generated_determinism_and_random_access() {
    for seed in [0, 1, 42, u64::MAX] {
        let c = Content::Generated { seed, len: 5000 };
        let mut state = seed.max(1);
        let mut expected = Vec::new();
        while expected.len() < 5000 {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            expected.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
        }
        expected.truncate(5000);
        assert_eq!(c.read_range(0, 5000), expected, "seed={seed}");
        for start in [0, 1, 7, 8, 99, 4097, 4999] {
            assert_eq!(
                c.read_range(start, 113),
                expected[start as usize..(start as usize + 113).min(5000)],
                "seed={seed}"
            );
        }
        assert!(c.read_range(u64::MAX, usize::MAX).is_empty());
    }
    assert!(Content::Bytes(vec![]).is_empty());
    assert_eq!(Content::Bytes(vec![]).sha256_hex(), digest(&[]));
}

#[tokio::test]
async fn generated_three_mb_stream_sha256_and_ranges() {
    let c = Content::Generated {
        seed: 42,
        len: 3 * 1024 * 1024,
    };
    let expected = c.sha256_hex();
    let s = TestServer::start().await;
    s.add("/f", c.clone(), Behavior::default());
    let r = request(&s, "GET", "").await;
    assert_eq!(digest(&r.body), expected);
    let r = request(&s, "GET", "Range: bytes=131071-197003\r\n").await;
    body(&r, &c.read_range(131071, 197003 - 131071 + 1));
}

#[tokio::test]
async fn ranges_head_and_invalid_ranges() {
    let s = fixture(Behavior::well_behaved()).await;
    let h = request(&s, "HEAD", "Range: bytes=1-3\r\n").await;
    assert_eq!(h.status, 200);
    assert_eq!(h.headers["content-length"], DATA.len().to_string());
    assert!(h.body.is_empty());
    for (range, expected) in [
        ("1-3", &DATA[1..4]),
        ("30-", &DATA[30..]),
        ("-4", &DATA[32..]),
        ("0-999", DATA),
    ] {
        let r = request(&s, "GET", &format!("Range: bytes={range}\r\n")).await;
        assert_eq!(r.status, 206);
        body(&r, expected);
    }
    for range in [
        "99-",
        "3-1",
        "-0",
        "0-1,3-4",
        "18446744073709551616-",
        "x-y",
    ] {
        let r = request(&s, "GET", &format!("Range: bytes={range}\r\n")).await;
        assert_eq!(r.status, 416);
        assert_eq!(r.headers["content-range"], "bytes */36");
    }
    s.set_content("/f", Content::Bytes(vec![]));
    assert_eq!(request(&s, "GET", "Range: bytes=0-\r\n").await.status, 416);
}

#[tokio::test]
async fn range_fault_modes() {
    let s = fixture(Behavior::no_range()).await;
    let r = request(&s, "GET", "Range: bytes=2-4\r\n").await;
    assert_eq!(r.status, 200);
    body(&r, DATA);
    assert!(!r.headers.contains_key("accept-ranges"));
    for (mode, status) in [
        (RangeMode::Reject416, 416),
        (RangeMode::WrongContentRange, 206),
        (RangeMode::IgnoreIfRange, 206),
    ] {
        s.set_behavior(
            "/f",
            Behavior {
                range: mode.clone(),
                ..Behavior::default()
            },
        );
        let extra = if mode == RangeMode::IgnoreIfRange {
            "If-Range: \"wrong\"\r\n"
        } else {
            ""
        };
        let r = request(&s, "GET", &format!("Range: bytes=2-4\r\n{extra}")).await;
        assert_eq!(r.status, status);
        if status == 206 {
            body(&r, &DATA[2..5]);
        }
        if mode == RangeMode::WrongContentRange {
            assert_eq!(r.headers["content-range"], "bytes 3-5/36");
        }
    }
}

#[tokio::test]
async fn etags_if_range_and_http_date() {
    let date = "Wed, 21 Oct 2015 07:28:00 GMT";
    let s = fixture(Behavior {
        last_modified: Some(date.into()),
        ..Behavior::default()
    })
    .await;
    let h = request(&s, "HEAD", "").await;
    for (validator, status) in [
        (h.headers["etag"].as_str(), 206),
        ("\"wrong\"", 200),
        (date, 206),
        ("Wed, 21 Oct 2015 07:29:00 GMT", 200),
    ] {
        let r = request(
            &s,
            "GET",
            &format!("Range: bytes=1-3\r\nIf-Range: {validator}\r\n"),
        )
        .await;
        assert_eq!(r.status, status);
        body(&r, if status == 206 { &DATA[1..4] } else { DATA });
    }
    for (etag, expected) in [
        (EtagMode::None, None),
        (EtagMode::Strong("fixed".into()), Some("\"fixed\"")),
        (EtagMode::Weak("fixed".into()), Some("W/\"fixed\"")),
    ] {
        s.set_behavior(
            "/f",
            Behavior {
                etag,
                ..Behavior::default()
            },
        );
        let r = request(&s, "GET", "Range: bytes=1-3\r\nIf-Range: W/\"fixed\"\r\n").await;
        assert_eq!(r.status, 200);
        assert_eq!(r.headers.get("etag").map(String::as_str), expected);
    }
}

#[tokio::test]
async fn length_lies_and_head_lies() {
    for (lie, advertised, actual) in [
        (LengthLie::Header { delta: 7 }, 43, 36),
        (LengthLie::Header { delta: -7 }, 29, 36),
        (LengthLie::BodyShort { missing: 9 }, 36, 27),
        (LengthLie::Header { delta: i64::MIN }, 0, 36),
        (LengthLie::BodyShort { missing: u64::MAX }, 36, 0),
    ] {
        let s = fixture(Behavior {
            length_lie: lie,
            ..Behavior::default()
        })
        .await;
        let r = request(&s, "GET", "").await;
        assert_eq!(r.headers["content-length"], advertised.to_string());
        body(&r, &DATA[..actual]);
    }
    let s = fixture(Behavior {
        head_lies: true,
        ..Behavior::default()
    })
    .await;
    let h = request(&s, "HEAD", "").await;
    let g = request(&s, "GET", "").await;
    assert_eq!(h.headers["content-length"], "37");
    assert!(!h.headers.contains_key("accept-ranges"));
    assert_eq!(g.headers["content-length"], "36");
    body(&g, DATA);
}

#[tokio::test]
async fn cuts_exact_bytes_and_selected_connection() {
    let s = fixture(Behavior::cuts_every(7)).await;
    for _ in 0..2 {
        body(&request(&s, "GET", "").await, &DATA[..7]);
    }
    assert_eq!(s.stats("/f").bytes_sent_total, 14);
    let s = fixture(Behavior {
        cut: Cut::AfterBytes {
            conn_index: 2,
            n: 9,
        },
        ..Behavior::default()
    })
    .await;
    body(&request(&s, "GET", "").await, DATA);
    body(&request(&s, "GET", "").await, &DATA[..9]);
    body(&request(&s, "GET", "").await, DATA);
}

#[tokio::test]
async fn random_cuts_reproducible() {
    let mut lengths = Vec::new();
    for _ in 0..2 {
        let s = fixture(Behavior {
            cut: Cut::Random {
                seed: 123,
                min: 3,
                max: 15,
            },
            ..Behavior::default()
        })
        .await;
        let mut run = Vec::new();
        for _ in 0..5 {
            let r = request(&s, "GET", "").await;
            assert!((3..=15).contains(&r.body.len()), "seed=123");
            body(&r, &DATA[..r.body.len()]);
            run.push(r.body.len());
        }
        lengths.push(run);
    }
    assert_eq!(lengths[0], lengths[1], "seed=123");
}

#[tokio::test]
async fn keep_alive_pipelining_stats_and_reset() {
    let s = fixture(Behavior::default()).await;
    let mut socket = connect(&s).await;
    socket.write_all(b"GET /f HTTP/1.1\r\nHost: localhost\r\nRange: bytes=1-3\r\n\r\nGET /f HTTP/1.1\r\nHost: localhost\r\n\r\n").await.unwrap();
    for expected in [&DATA[1..4], DATA] {
        let mut r = headers(&mut socket).await;
        r.body
            .resize(r.headers["content-length"].parse().unwrap(), 0);
        socket.read_exact(&mut r.body).await.unwrap();
        body(&r, expected);
    }
    let stats = s.stats("/f");
    assert_eq!(stats.connections_accepted, 1);
    assert_eq!(stats.peak_concurrent, 1);
    assert_eq!(stats.bytes_sent_total, 39);
    assert_eq!(stats.requests[0].conn_id, stats.requests[1].conn_id);
    assert_eq!(stats.requests[0].range_header.as_deref(), Some("bytes=1-3"));
    assert_eq!(stats.requests[0].bytes_sent, 3);
    s.reset_stats("/f");
    assert!(s.stats("/f").requests.is_empty());
    socket
        .write_all(b"HEAD /f HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(headers(&mut socket).await.status, 200);
    assert_eq!(s.stats("/f").connections_accepted, 1);
}

#[tokio::test]
async fn concurrent_limits_peak_and_retry_after() {
    for (limit, status) in [
        (LimitStatus::TooManyRequests, 429),
        (LimitStatus::Forbidden, 403),
    ] {
        let s = fixture(Behavior {
            throttle_bps: Some(0),
            max_concurrent: Some((1, limit)),
            retry_after_secs: Some(17),
            ..Behavior::default()
        })
        .await;
        let mut held = connect(&s).await;
        held.write_all(b"GET /f HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(headers(&mut held).await.status, 200);
        let r = request(&s, "GET", "").await;
        assert_eq!(r.status, status);
        if status == 429 {
            assert_eq!(r.headers["retry-after"], "17");
        }
        assert_eq!(s.stats("/f").peak_concurrent, 2);
        assert_eq!(s.stats("/f").connections_accepted, 2);
    }
}

#[tokio::test(start_paused = true)]
async fn stall_pauses_once_and_live_behavior_change() {
    let s = fixture(Behavior {
        stall: Some(Stall {
            after_bytes: 5,
            duration: Duration::from_secs(10),
            once: true,
        }),
        ..Behavior::default()
    })
    .await;
    let mut socket = connect(&s).await;
    socket
        .write_all(b"GET /f HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut r = headers(&mut socket).await;
    let mut first = [0; 5];
    socket.read_exact(&mut first).await.unwrap();
    assert_eq!(&first, &DATA[..5]);
    assert!(timeout(Duration::from_secs(1), socket.read_u8())
        .await
        .is_err());
    s.reset_stats("/f");
    r.body.extend_from_slice(&first);
    socket.read_to_end(&mut r.body).await.unwrap();
    body(&r, DATA);
    assert!(s.stats("/f").requests.is_empty());
    let began = Instant::now();
    body(&request(&s, "GET", "").await, DATA);
    assert!(began.elapsed() < Duration::from_secs(1));
    s.set_behavior("/f", Behavior::no_range());
    assert_eq!(request(&s, "GET", "Range: bytes=1-3\r\n").await.status, 200);
}

#[tokio::test(start_paused = true)]
async fn throttle_and_first_slow_connections() {
    let s = fixture(Behavior::slow(12)).await;
    let began = Instant::now();
    body(&request(&s, "GET", "").await, DATA);
    assert!(began.elapsed() >= Duration::from_secs(3));
    let s = fixture(Behavior {
        slow_connections: Some((1, 12)),
        ..Behavior::default()
    })
    .await;
    let began = Instant::now();
    body(&request(&s, "GET", "").await, DATA);
    assert!(began.elapsed() >= Duration::from_secs(3));
    let began = Instant::now();
    body(&request(&s, "GET", "").await, DATA);
    assert!(began.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn single_use_and_initial_failures() {
    let s = fixture(Behavior {
        single_use: true,
        ..Behavior::default()
    })
    .await;
    body(&request(&s, "GET", "").await, DATA);
    assert_eq!(request(&s, "GET", "").await.status, 410);
    let s = fixture(Behavior {
        fail_first_requests: 2,
        retry_after_secs: Some(3),
        ..Behavior::default()
    })
    .await;
    for _ in 0..2 {
        let r = request(&s, "GET", "").await;
        assert_eq!(r.status, 503);
        assert_eq!(r.headers["retry-after"], "3");
    }
    body(&request(&s, "GET", "").await, DATA);
    let s = fixture(Behavior {
        disconnect_before_headers: 2,
        ..Behavior::default()
    })
    .await;
    for _ in 0..2 {
        let mut socket = connect(&s).await;
        socket
            .write_all(b"GET /f HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
    }
    body(&request(&s, "GET", "").await, DATA);
    assert_eq!(s.stats("/f").requests[0].status, 0);
}

#[tokio::test]
async fn change_after_requests_bytes_and_set_content_update_etag() {
    for (requests, bytes_sent) in [(Some(1), None), (None, Some(DATA.len() as u64))] {
        let s = fixture(Behavior {
            change_after: Some(ChangeAfter {
                requests,
                bytes_sent,
                new_content: Content::Bytes(b"new".to_vec()),
            }),
            ..Behavior::default()
        })
        .await;
        let old = request(&s, "GET", "").await;
        body(&old, DATA);
        let new = request(
            &s,
            "GET",
            &format!("Range: bytes=1-\r\nIf-Range: {}\r\n", old.headers["etag"]),
        )
        .await;
        assert_eq!(new.status, 200);
        body(&new, b"new");
        assert_ne!(old.headers["etag"], new.headers["etag"]);
        s.set_content("/f", Content::Generated { seed: 7, len: 100 });
        let generated = request(&s, "GET", "").await;
        assert_ne!(new.headers["etag"], generated.headers["etag"]);
        body(
            &generated,
            &Content::Generated { seed: 7, len: 100 }.read_range(0, 100),
        );
    }
}

#[tokio::test]
async fn raw_disposition_type_gzip_and_arabic_path() {
    let disposition = "attachment; filename=\"../CON\u{202e}.txt:ads\"";
    let s = fixture(Behavior {
        content_disposition: Some(disposition.into()),
        content_type: Some("application/octet-stream".into()),
        gzip_always: true,
        ..Behavior::default()
    })
    .await;
    let r = request(&s, "GET", "Accept-Encoding: identity\r\n").await;
    assert_eq!(r.headers["content-disposition"], disposition);
    assert_eq!(r.headers["content-type"], "application/octet-stream");
    assert_eq!(r.headers["content-encoding"], "gzip");
    body(&r, DATA);
    s.add("/عربي", Content::Bytes(DATA.to_vec()), Behavior::default());
    let mut socket = connect(&s).await;
    socket
        .write_all("GET /عربي HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n".as_bytes())
        .await
        .unwrap();
    let mut r = headers(&mut socket).await;
    socket.read_to_end(&mut r.body).await.unwrap();
    body(&r, DATA);
}

#[tokio::test]
async fn redirect_variants_and_loop() {
    let s = fixture(Behavior::default()).await;
    for (redirect, location) in [
        (Redirect::To("/target".into()), "/target".to_owned()),
        (Redirect::Loop, "/f".to_owned()),
        (
            Redirect::ToOtherHost("http://example.invalid/f".into()),
            "http://example.invalid/f".to_owned(),
        ),
        (Redirect::ToHttpsDowngrade, format!("{}/f", s.base_url())),
    ] {
        s.set_behavior(
            "/f",
            Behavior {
                redirect: Some(redirect),
                ..Behavior::default()
            },
        );
        for _ in 0..2 {
            let r = request(&s, "GET", "").await;
            assert_eq!(r.status, 302);
            assert_eq!(r.headers["location"], location);
            assert!(r.body.is_empty());
        }
    }
}

#[tokio::test]
async fn drop_aborts_stalled_connections() {
    let s = fixture(Behavior::slow(0)).await;
    let mut socket = connect(&s).await;
    socket
        .write_all(b"GET /f HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    headers(&mut socket).await;
    drop(s);
    assert_eq!(
        timeout(Duration::from_secs(5), socket.read(&mut [0; 1]))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn unsupported_methods_missing_paths_and_bad_requests() {
    let s = fixture(Behavior::default()).await;
    assert_eq!(request(&s, "POST", "").await.status, 405);
    let mut socket = connect(&s).await;
    socket
        .write_all(b"GET /missing HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    assert_eq!(headers(&mut socket).await.status, 404);
    for raw in [
        "GET /f HTTP/1.0\r\n\r\n".to_owned(),
        "GET /f HTTP/1.1\r\nRange: bytes=1-2\r\nRange: bytes=2-3\r\n\r\n".to_owned(),
        "GET /f HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".to_owned(),
        format!("GET /f HTTP/1.1\r\nX: {}\r\n\r\n", "x".repeat(20000)),
    ] {
        let mut socket = connect(&s).await;
        let _ = socket.write_all(raw.as_bytes()).await;
        let result = timeout(Duration::from_secs(5), socket.read(&mut [0; 1]))
            .await
            .unwrap();
        assert!(matches!(result, Ok(0) | Err(_)));
    }
}

#[tokio::test]
async fn header_delay_withholds_headers_then_serves_only_the_selected_requests() {
    let s = fixture(Behavior {
        header_delay: Some(HeaderDelay {
            skip: 1,
            count: 1,
            delay: Duration::from_millis(400),
            partial: true,
        }),
        ..Behavior::default()
    })
    .await;
    // request 1: not delayed
    let t = Instant::now();
    body(&request(&s, "GET", "").await, DATA);
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
    // request 2: bytes arrive (a partial status line) but no complete header block for 400 ms
    let mut socket = connect(&s).await;
    socket
        .write_all(b"GET /f HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut first = [0u8; 14];
    timeout(Duration::from_secs(2), socket.read_exact(&mut first))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&first, b"HTTP/1.1 200 O");
    let started = Instant::now();
    let mut rest = vec![0u8; 1];
    timeout(Duration::from_secs(5), socket.read_exact(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "headers arrived after only {:?}",
        started.elapsed()
    );
    drop(socket);
    // request 3: not delayed again
    let t = Instant::now();
    body(&request(&s, "GET", "").await, DATA);
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
}
