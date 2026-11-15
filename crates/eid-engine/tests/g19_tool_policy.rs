//! P4 G2 (D14): per-download host allowlist (initial URL and every redirect hop) and size cap,
//! used by the video tool manager. Off by default; existing behaviour is unchanged.
mod common;
use common::*;
use eid_testserver::{Behavior, Content, Redirect, TestServer};

fn gen(len: u64) -> Content {
    Content::Generated { seed: 19, len }
}

#[test]
fn host_allowlist_and_size_cap_are_pure_rules() {
    use eid_engine::http::{check_size_cap, host_allowed};
    let list = vec![
        "github.com".to_string(),
        "release-assets.githubusercontent.com".into(),
    ];
    assert!(host_allowed(None, Some("anything.example")));
    assert!(host_allowed(Some(&list), Some("github.com")));
    assert!(host_allowed(Some(&list), Some("GitHub.COM")));
    for bad in [
        Some("evil.com"),
        Some("github.com.evil.com"),
        Some("evilgithub.com"),
        Some("api.github.com"),
        Some("githubusercontent.com"),
        None,
    ] {
        assert!(!host_allowed(Some(&list), bad), "{bad:?}");
    }
    assert!(check_size_cap(None, None).is_ok());
    assert!(check_size_cap(Some(10), Some(10)).is_ok());
    assert!(check_size_cap(Some(10), Some(11)).is_err());
    assert!(
        check_size_cap(Some(10), None).is_err(),
        "unknown size with a cap"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn allowed_hosts_apply_to_the_first_url_and_every_redirect_hop() {
    let srv = TestServer::start().await;
    let port = srv.base_url().rsplit(':').next().unwrap().to_string();
    srv.add(
        "/hop.bin",
        gen(1000),
        Behavior {
            redirect: Some(Redirect::To(format!("http://localhost:{port}/b.bin"))),
            ..Behavior::default()
        },
    );
    srv.add("/b.bin", gen(1000), Behavior::default());
    srv.add("/direct.bin", gen(1000), Behavior::default());
    let only_ip = Some(vec!["127.0.0.1".to_string()]);
    // allowed host, no redirect: works (the success counterpart)
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.allowed_hosts = only_ip.clone();
    let (o, _) = get(
        &format!("http://127.0.0.1:{port}/direct.bin"),
        d.path(),
        cfg,
    )
    .await
    .unwrap();
    assert_eq!(o.size, 1000);
    // a redirect to a host outside the list is refused and nothing reaches that host
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.allowed_hosts = only_ip.clone();
    let e = get(&format!("http://127.0.0.1:{port}/hop.bin"), d.path(), cfg).await;
    assert!(e.is_err(), "redirect to localhost must be refused");
    assert!(
        srv.stats("/b.bin").requests.is_empty(),
        "the other host was contacted"
    );
    // the first URL itself is checked
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.allowed_hosts = only_ip;
    let e = get(
        &format!("http://localhost:{port}/direct.bin"),
        d.path(),
        cfg,
    )
    .await;
    assert!(e.is_err());
    // both hosts allowed: the redirect is followed
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.allowed_hosts = Some(vec!["127.0.0.1".into(), "localhost".into()]);
    let (o, _) = get(&format!("http://127.0.0.1:{port}/hop.bin"), d.path(), cfg)
        .await
        .unwrap();
    assert_eq!(o.size, 1000);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn size_cap_refuses_a_larger_file_before_writing_it() {
    let srv = TestServer::start().await;
    srv.add("/big.bin", gen(100_000), Behavior::default());
    let url = format!("{}/big.bin", srv.base_url());
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.max_total_bytes = Some(99_999);
    assert!(get(&url, d.path(), cfg).await.is_err());
    let left: Vec<_> = std::fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|n| !n.to_string_lossy().ends_with(".eid-lock"))
        .collect();
    assert!(
        left.is_empty(),
        "nothing may be left but the lock: {left:?}"
    );
    let d = tempfile::tempdir().unwrap();
    let mut cfg = fast_cfg(1);
    cfg.max_total_bytes = Some(100_000);
    let (o, _) = get(&url, d.path(), cfg).await.unwrap();
    assert_eq!(o.size, 100_000);
}
