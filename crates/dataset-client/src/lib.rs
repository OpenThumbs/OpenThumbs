//! Rust SDK for the dataset store.
//!
//! Upload path: hash locally → ask server which blobs are missing → upload only
//! those (single PUT for small files, resumable parallel multipart for large)
//! → create an immutable version from the manifest.
//!
//! Long operations report into a shared [`Progress`] so UIs can show transfers.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use dataset_core::{BlobHash, BlobHasher, ManifestFile};
use futures_util::{stream, StreamExt, TryStreamExt};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const SINGLE_PUT_MAX: u64 = 32 << 20;
const MIN_PART: u64 = 32 << 20;
const MAX_PARTS: u64 = 9_000;
const PARALLEL: usize = 4;
const RETRIES: u32 = 4;

/// Shared byte counters for a long-running transfer.
#[derive(Default)]
pub struct Progress {
    pub total: AtomicU64,
    pub done: AtomicU64,
    phase: Mutex<String>,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn set_phase(&self, phase: impl Into<String>) {
        *self.phase.lock().unwrap() = phase.into();
    }

    pub fn phase(&self) -> String {
        self.phase.lock().unwrap().clone()
    }

    /// Start a new phase with a fresh byte total.
    pub fn reset(&self, phase: impl Into<String>, total: u64) {
        self.set_phase(phase);
        self.total.store(total, Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
    }

    fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }

    fn sub(&self, n: u64) {
        self.done.fetch_sub(n.min(self.done.load(Ordering::Relaxed)), Ordering::Relaxed);
    }

    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedFile {
    pub path: String,
    pub blob: BlobHash,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolved {
    pub dataset: String,
    pub version: String,
    pub manifest_hash: BlobHash,
    pub uri: String,
    pub files: Vec<ResolvedFile>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct VersionSpec {
    pub parent: Option<String>,
    pub branch: Option<String>,
    pub schema_hash: Option<String>,
    pub metadata: Value,
    pub producer: Option<dataset_core::Producer>,
    pub inputs: Vec<String>,
}

/// A locally hashed file ready to be pushed.
#[derive(Debug, Clone)]
pub struct LocalFile {
    pub manifest: ManifestFile,
    pub abs: PathBuf,
}

/// Percent-encode one path segment (refs like `training/latest` contain `/`).
pub fn seg(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Client {
    pub fn new(base: &str, token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        Self { http, base: base.trim_end_matches('/').to_string(), token }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        let r = self.http.request(method, format!("{}/api{path}", self.base));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }

    async fn check(resp: Response) -> Result<Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        bail!("HTTP {status}: {body}")
    }

    async fn json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut r = self.req(method, path);
        if let Some(b) = body {
            r = r.json(&b);
        }
        Ok(Self::check(r.send().await?).await?.json().await?)
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        self.json(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.json(Method::POST, path, Some(body)).await
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<String> {
        let v = self.post("/auth/token", json!({ "username": username, "password": password, "name": "ds-cli" })).await?;
        Ok(v["token"].as_str().context("no token in response")?.to_string())
    }

    pub async fn resolve(&self, dataset: &str, reference: &str) -> Result<Resolved> {
        Ok(serde_json::from_value(self.get(&format!("/datasets/{}/resolve/{}", seg(dataset), seg(reference))).await?)?)
    }

    pub async fn missing(&self, blobs: &[BlobHash]) -> Result<Vec<BlobHash>> {
        let mut out = Vec::new();
        for chunk in blobs.chunks(5000) {
            let v = self.post("/blobs/missing", json!({ "blobs": chunk })).await?;
            out.extend(serde_json::from_value::<Vec<BlobHash>>(v["missing"].clone())?);
        }
        Ok(out)
    }

    // ---------------- upload ----------------

    /// Upload a local file as a blob with a known hash. Resumes interrupted multipart uploads.
    pub async fn upload_file(&self, path: &Path, hash: &BlobHash, size: u64, progress: Option<&Progress>) -> Result<()> {
        if size <= SINGLE_PUT_MAX {
            let data = Bytes::from(tokio::fs::read(path).await?);
            retry(|| async {
                Self::check(self.req(Method::PUT, &format!("/blobs/{hash}")).body(data.clone()).send().await?).await?;
                Ok(())
            })
            .await?;
            if let Some(p) = progress {
                p.add(size);
            }
            return Ok(());
        }

        let state_file = upload_state_dir().join(hash.hex());
        let mut upload_id = tokio::fs::read_to_string(&state_file).await.ok().map(|s| s.trim().to_string());
        let mut done = std::collections::HashSet::new();
        if let Some(id) = &upload_id {
            match self.get(&format!("/uploads/{id}")).await {
                Ok(v) => {
                    for p in v["parts"].as_array().into_iter().flatten() {
                        done.insert((p["number"].as_u64().unwrap_or(0), p["size"].as_u64().unwrap_or(0)));
                    }
                    tracing::info!("resuming upload {id} ({} parts already on server)", done.len());
                }
                Err(_) => upload_id = None,
            }
        }
        let id = match upload_id {
            Some(id) => id,
            None => {
                let v = self.post("/uploads", json!({ "sha256": hash, "size": size })).await?;
                if v["exists"].as_bool() == Some(true) {
                    if let Some(p) = progress {
                        p.add(size);
                    }
                    return Ok(());
                }
                let id = v["id"].as_str().context("no upload id")?.to_string();
                tokio::fs::create_dir_all(state_file.parent().unwrap()).await?;
                tokio::fs::write(&state_file, &id).await?;
                id
            }
        };

        let part_size = MIN_PART.max(size.div_ceil(MAX_PARTS));
        let parts = size.div_ceil(part_size);
        if let Some(p) = progress {
            p.add(done.iter().map(|(_, s)| s).sum());
        }
        let path = Arc::new(path.to_path_buf());
        stream::iter(1..=parts)
            .filter(|n| {
                let len = part_size.min(size - (n - 1) * part_size);
                std::future::ready(!done.contains(&(*n, len)))
            })
            .map(|n| {
                let (path, id) = (path.clone(), id.clone());
                async move {
                    let offset = (n - 1) * part_size;
                    let len = part_size.min(size - offset);
                    let data = read_range(&path, offset, len).await?;
                    retry(|| async {
                        let r = self.req(Method::PUT, &format!("/uploads/{id}/parts/{n}")).body(data.clone()).send().await?;
                        Self::check(r).await?;
                        Ok(())
                    })
                    .await?;
                    if let Some(p) = progress {
                        p.add(len);
                    }
                    tracing::info!("{}: part {n}/{parts}", path.display());
                    anyhow::Ok(())
                }
            })
            .buffer_unordered(PARALLEL)
            .try_collect::<Vec<_>>()
            .await?;

        let complete_path = format!("/uploads/{id}/complete");
        let v = retry(|| self.post(&complete_path, json!({}))).await?;
        let _ = tokio::fs::remove_file(&state_file).await;
        if v["hash"].as_str().map(BlobHash::parse).transpose()?.as_ref() != Some(hash) {
            bail!("server stored a different hash: {v}");
        }
        Ok(())
    }

    /// Upload missing blobs for already-hashed files and create a version.
    pub async fn push_hashed(&self, dataset: &str, files: &[LocalFile], spec: VersionSpec, progress: &Progress) -> Result<Value> {
        progress.set_phase("Checking server for existing blobs");
        let hashes: Vec<BlobHash> = files.iter().map(|f| f.manifest.blob.clone()).collect();
        let missing: std::collections::HashSet<BlobHash> = self.missing(&hashes).await?.into_iter().collect();
        let mut to_upload: Vec<&LocalFile> = Vec::new();
        let mut queued = std::collections::HashSet::new();
        for f in files {
            if missing.contains(&f.manifest.blob) && queued.insert(&f.manifest.blob) {
                to_upload.push(f);
            }
        }
        let upload_bytes: u64 = to_upload.iter().map(|f| f.manifest.size).sum();
        let total: u64 = files.iter().map(|f| f.manifest.size).sum();
        tracing::info!(
            "{} files, {} bytes; uploading {} new blobs ({} bytes), reusing the rest",
            files.len(),
            total,
            to_upload.len(),
            upload_bytes
        );
        progress.reset(format!("Uploading {} new blobs", to_upload.len()), upload_bytes);
        stream::iter(to_upload)
            .map(|f| async move {
                self.upload_file(&f.abs, &f.manifest.blob, f.manifest.size, Some(progress))
                    .await
                    .with_context(|| f.abs.display().to_string())
            })
            .buffer_unordered(PARALLEL)
            .try_collect::<Vec<_>>()
            .await?;

        progress.set_phase("Creating version");
        let manifest: Vec<&ManifestFile> = files.iter().map(|f| &f.manifest).collect();
        let mut body = serde_json::to_value(&spec)?;
        body["files"] = serde_json::to_value(&manifest)?;
        self.post(&format!("/datasets/{}/versions", seg(dataset)), body).await
    }

    /// Hash `dir`, upload missing blobs, and create a new version of `dataset`.
    pub async fn push_dir(&self, dataset: &str, dir: &Path, spec: VersionSpec) -> Result<Value> {
        let progress = Progress::new();
        let files = hash_dir(dir, &progress).await?;
        self.push_hashed(dataset, &files, spec, &progress).await
    }

    // ---------------- download ----------------

    /// Download a blob to `dest`, resuming a partial `.part` file with Range requests,
    /// and verify its hash before moving it into place.
    pub async fn download_blob(&self, blob: &BlobHash, size: u64, dest: &Path, progress: Option<&Progress>) -> Result<()> {
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let part = dest.with_extension(match dest.extension() {
            Some(e) => format!("{}.part", e.to_string_lossy()),
            None => "part".into(),
        });
        // Bytes of this file currently credited to `progress` (adjusted on retries).
        let counted = AtomicU64::new(0);
        let credit = |now: u64| {
            if let Some(p) = progress {
                let before = counted.swap(now, Ordering::Relaxed);
                if now >= before { p.add(now - before) } else { p.sub(before - now) }
            }
        };
        retry(|| async {
            let mut have = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
            credit(have.min(size));
            if have < size {
                let mut r = self.req(Method::GET, &format!("/blobs/{blob}"));
                if have > 0 {
                    r = r.header(reqwest::header::RANGE, format!("bytes={have}-"));
                }
                let resp = Self::check(r.send().await?).await?;
                let append = have > 0 && resp.status() == StatusCode::PARTIAL_CONTENT;
                if !append {
                    have = 0;
                    credit(0);
                }
                let mut file = tokio::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(append)
                    .truncate(!append)
                    .open(&part)
                    .await?;
                let mut body = resp.bytes_stream();
                while let Some(chunk) = body.next().await {
                    let chunk = chunk?;
                    file.write_all(&chunk).await?;
                    have += chunk.len() as u64;
                    credit(have.min(size));
                }
                file.sync_all().await?;
            }
            let actual = hash_file(&part).await?.0;
            if &actual != blob {
                tokio::fs::remove_file(&part).await?;
                credit(0);
                bail!("checksum mismatch for {}: got {actual}", dest.display());
            }
            Ok(())
        })
        .await?;
        tokio::fs::rename(&part, dest).await?;
        Ok(())
    }

    /// Materialize a version into `dir`, skipping files already present with the right hash.
    pub async fn pull(&self, dataset: &str, reference: &str, dir: &Path) -> Result<Resolved> {
        self.pull_with(dataset, reference, dir, &Progress::new()).await
    }

    pub async fn pull_with(&self, dataset: &str, reference: &str, dir: &Path, progress: &Arc<Progress>) -> Result<Resolved> {
        progress.set_phase("Resolving version");
        let resolved = self.resolve(dataset, reference).await?;
        progress.reset(format!("Downloading {} files", resolved.files.len()), resolved.files.iter().map(|f| f.size).sum());
        // Owned data per task keeps the future `Send` for use from spawned tasks.
        stream::iter(resolved.files.clone())
            .map(|f| {
                let (client, dest, progress) = (self.clone(), dir.join(&f.path), progress.clone());
                async move {
                    if !dataset_core::validate_path(&f.path) {
                        bail!("refusing unsafe path {:?}", f.path);
                    }
                    if tokio::fs::metadata(&dest).await.map(|m| m.len()).ok() == Some(f.size)
                        && hash_file(&dest).await?.0 == f.blob
                    {
                        progress.add(f.size);
                        return Ok(());
                    }
                    client.download_blob(&f.blob, f.size, &dest, Some(&progress)).await?;
                    tracing::info!("{}", f.path);
                    Ok(())
                }
            })
            .buffer_unordered(PARALLEL)
            .try_collect::<Vec<_>>()
            .await?;
        progress.set_phase("Done");
        Ok(resolved)
    }
}

// ---------------- helpers ----------------

async fn retry<T, F, Fut>(mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 0;
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if attempt < RETRIES => {
                attempt += 1;
                let wait = Duration::from_millis(500 * 2u64.pow(attempt));
                tracing::warn!("attempt {attempt} failed: {e:#}; retrying in {wait:?}");
                tokio::time::sleep(wait).await;
            }
            Err(e) => return Err(e),
        }
    }
}

fn upload_state_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("ds/uploads")
}

async fn read_range(path: &Path, offset: u64, len: u64) -> Result<Bytes> {
    let mut f = tokio::fs::File::open(path).await?;
    f.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).await?;
    Ok(Bytes::from(buf))
}

/// Hash a file on a blocking thread (sha256 is CPU-bound).
pub async fn hash_file(path: &Path) -> Result<(BlobHash, u64)> {
    hash_file_with(path, None).await
}

async fn hash_file_with(path: &Path, progress: Option<Arc<Progress>>) -> Result<(BlobHash, u64)> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut f = std::fs::File::open(&path).with_context(|| path.display().to_string())?;
        let mut h = BlobHasher::new();
        let mut buf = vec![0u8; 8 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            if let Some(p) = &progress {
                p.add(n as u64);
            }
        }
        let len = h.len();
        Ok((h.finish(), len))
    })
    .await?
}

/// Hash every regular file under `dir` in parallel. Paths are `/`-separated and relative.
pub async fn hash_dir(dir: &Path, progress: &Arc<Progress>) -> Result<Vec<LocalFile>> {
    let mut paths = Vec::new();
    let mut total = 0u64;
    for entry in walkdir::WalkDir::new(dir).follow_links(true).sort_by_file_name() {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(dir)?;
        let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
        if !dataset_core::validate_path(&rel) {
            bail!("unsupported path {rel:?}");
        }
        total += entry.metadata()?.len();
        paths.push((rel, entry.into_path()));
    }
    progress.reset(format!("Hashing {} files", paths.len()), total);
    let parallel = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    stream::iter(paths)
        .map(|(rel, abs)| {
            let progress = progress.clone();
            async move {
                let (blob, size) = hash_file_with(&abs, Some(progress)).await?;
                anyhow::Ok(LocalFile { manifest: ManifestFile { path: rel, blob, size }, abs })
            }
        })
        .buffered(parallel)
        .try_collect()
        .await
}
