#![allow(dead_code)]
use eid_engine::{CancellationToken, Config, Event, EventSink, JobSpec, Outcome};
use eid_testserver::{Behavior, Content, TestServer};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn fast_cfg(connections: usize) -> Config {
    Config {
        connections,
        min_split: 64 * 1024,
        ramp_interval: Duration::from_millis(5),
        checkpoint_interval: Duration::from_millis(40),
        stall_timeout: Duration::from_secs(5),
        max_attempts: 6,
        backoff_base: Duration::from_millis(5),
        backoff_cap: Duration::from_millis(40),
        retry_after_cap: Duration::from_secs(5),
        write_chunk: 32 * 1024,
        use_system_proxy: false,
        compute_sha256: true,
        ..Config::default()
    }
}

#[derive(Clone, Default)]
pub struct Log(pub Arc<Mutex<Vec<Event>>>);
impl Log {
    pub fn sink(&self) -> EventSink {
        let l = self.0.clone();
        Arc::new(move |e| l.lock().unwrap().push(e))
    }
    pub fn count(&self, f: impl Fn(&Event) -> bool) -> usize {
        self.0.lock().unwrap().iter().filter(|e| f(e)).count()
    }
}

pub async fn serve(path: &str, content: Content, b: Behavior) -> (TestServer, String) {
    let s = TestServer::start().await;
    s.add(path, content, b);
    let url = format!("{}{}", s.base_url(), path);
    (s, url)
}

pub async fn get(url: &str, dir: &Path, cfg: Config) -> eid_engine::Result<(Outcome, Log)> {
    let log = Log::default();
    let o = eid_engine::download(
        JobSpec {
            url: url.to_string(),
            dir: dir.to_path_buf(),
            file_name: None,
        },
        cfg,
        log.sink(),
        CancellationToken::new(),
    )
    .await?;
    Ok((o, log))
}

pub fn sha_of_file(p: &Path) -> String {
    use sha2::{Digest, Sha256};
    let b = std::fs::read(p).unwrap();
    eid_engine::state::hex(&Sha256::digest(b))
}

/// No leftovers other than the expected final file(s).
pub fn only_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}
