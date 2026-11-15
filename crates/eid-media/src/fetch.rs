//! Downloading tool assets (D4). The tool manager only sees the [`Fetcher`] trait; the product
//! uses [`EngineFetcher`] (HTTPS only, manifest hosts on every hop, exact size, SHA-256 checked
//! by the engine before the file is published). Tests inject a Fetcher over the local
//! `eid-testserver` in test code only.

use crate::MediaError;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One asset to fetch.
#[derive(Clone, Debug)]
pub struct FetchRequest<'a> {
    pub url: &'a str,
    /// Existing directory the file is written into (our staging folder).
    pub dir: &'a Path,
    /// Name of the file in `dir`.
    pub file_name: &'a str,
    pub size: u64,
    pub sha256: &'a str,
    pub allowed_hosts: &'a [String],
}

pub trait Fetcher: Send + Sync {
    /// Download the asset; on success the returned file has exactly `size` bytes and the
    /// SHA-256 `sha256`. On failure nothing is published under `file_name`.
    fn fetch<'a>(&'a self, req: FetchRequest<'a>) -> BoxFuture<'a, Result<PathBuf, MediaError>>;
}

/// The production fetcher: the engine with the tool-download policy.
#[derive(Clone, Default)]
pub struct EngineFetcher {
    /// Base engine configuration (timeouts, proxy...); the policy fields are overwritten.
    pub cfg: eid_engine::Config,
}

impl Fetcher for EngineFetcher {
    fn fetch<'a>(&'a self, req: FetchRequest<'a>) -> BoxFuture<'a, Result<PathBuf, MediaError>> {
        Box::pin(async move {
            crate::manifest::check_asset_url(req.url, req.allowed_hosts)?;
            engine_fetch(self.cfg.clone(), &req).await
        })
    }
}

/// The engine download with the tool policy: hosts, exact size cap and expected SHA-256.
/// Does not check the URL scheme itself ([`EngineFetcher`] and the manifest do).
pub async fn engine_fetch(
    mut cfg: eid_engine::Config,
    req: &FetchRequest<'_>,
) -> Result<PathBuf, MediaError> {
    cfg.allowed_hosts = Some(req.allowed_hosts.to_vec());
    cfg.max_total_bytes = Some(req.size);
    cfg.expected_sha256 = Some(req.sha256.to_string());
    let spec = eid_engine::JobSpec {
        url: req.url.to_string(),
        dir: req.dir.to_path_buf(),
        file_name: Some(req.file_name.to_string()),
    };
    let sink: eid_engine::EventSink = std::sync::Arc::new(|_| {});
    let o = eid_engine::download(spec, cfg, sink, eid_engine::CancellationToken::new())
        .await
        .map_err(|e| MediaError::Fetch(e.to_string()))?;
    if o.size != req.size {
        let _ = std::fs::remove_file(&o.path);
        return Err(MediaError::Fetch(format!(
            "{}: {} bytes, expected {}",
            req.file_name, o.size, req.size
        )));
    }
    if o.path.file_name().and_then(|n| n.to_str()) != Some(req.file_name) {
        // the engine numbers a name that is taken; our staging folder is fresh, so this
        // means something else is in it
        let _ = std::fs::remove_file(&o.path);
        return Err(MediaError::Fetch(format!(
            "{}: unexpected file name in the staging folder",
            req.file_name
        )));
    }
    Ok(o.path)
}
