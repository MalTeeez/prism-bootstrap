//! The one generic IO path: fetch every [`ArtifactRecord`] in parallel, verify
//! it, and write it atomically.
//!
//! Libraries, mavenFiles, the main jar, native jars, and assets all flow
//! through [`Downloader::download_all`] - there are no per-type fetchers. No-url
//! entries are asserted-local-or-fatal and never fetched; empty-hash artifacts
//! download but skip verification; writes go through `*.part` -> `rename` so an
//! interrupted run never leaves a half-file a later step would trust.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use log::{info, warn};
use reqwest::StatusCode;
use sha1::{Digest, Sha1};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};

use crate::exit::FatalError;
use crate::model::artifact::{ArtifactRecord, Role};

/// Total attempts per artifact before a download is declared fatal.
const MAX_ATTEMPTS: u32 = 4;
/// How often the progress ticker reports while a batch is in flight.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(10);

/// Knobs controlling a download run (from the CLI).
#[derive(Debug, Clone)]
pub struct DownloadOptions {
    /// Max concurrent downloads (`--jobs`).
    pub jobs: usize,
    /// Verify size + sha1 (false under `--no-verify`).
    pub verify: bool,
    /// Resolve + assert only, no network (`--dry-run`).
    pub dry_run: bool,
}

/// Owns the HTTP client and options; drives batches of downloads.
pub struct Downloader {
    client: reqwest::Client,
    options: DownloadOptions,
}

impl Downloader {
    /// Build a downloader with a redirect-following client (the ARM jinput
    /// natives are GitHub `raw` 302s).
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be constructed.
    pub fn new(options: DownloadOptions) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("prism-bootstrap/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_mins(5))
            .build()
            .context("building the HTTP client")?;
        Ok(Self { client, options })
    }

    /// Whether this is a dry run (callers skip work that needs the bytes).
    #[must_use]
    pub fn dry_run(&self) -> bool {
        self.options.dry_run
    }

    /// Download (or assert-local) every record, bounded by `--jobs`. Returns the
    /// first fatal error; remaining tasks are aborted once one fails.
    ///
    /// # Errors
    /// A surviving download/verify failure ([`FatalError::DownloadFailed`]) or a
    /// missing no-url artifact ([`FatalError::MissingLocalLib`]).
    pub async fn download_all(&self, label: &str, records: &[ArtifactRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let total = records.len();
        if self.options.dry_run {
            info!("Dry run: skipping {total} {label} download(s) (preflight only)");
        } else {
            info!("Downloading {total} {label}...");
        }

        let done = Arc::new(AtomicUsize::new(0));
        let progress = spawn_progress(label.to_owned(), total, Arc::clone(&done));
        let semaphore = Arc::new(Semaphore::new(self.options.jobs.max(1)));
        let mut set: JoinSet<Result<()>> = JoinSet::new();

        for record in records {
            let semaphore = Arc::clone(&semaphore);
            let client = self.client.clone();
            let options = self.options.clone();
            let record = record.clone();
            let done = Arc::clone(&done);
            set.spawn(async move {
                let _permit = semaphore.acquire_owned().await.expect("semaphore stays open");
                let result = download_one(&client, &record, &options).await;
                done.fetch_add(1, Ordering::Relaxed);
                result
            });
        }

        let mut first_error: Option<anyhow::Error> = None;
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                        set.abort_all();
                    }
                }
                // Tasks aborted after the first error report as cancelled.
                Err(join_error) if join_error.is_cancelled() => {}
                Err(join_error) => {
                    if first_error.is_none() {
                        first_error =
                            Some(anyhow::Error::new(join_error).context("download task panicked"));
                        set.abort_all();
                    }
                }
            }
        }
        progress.abort();

        if let Some(error) = first_error {
            return Err(error);
        }
        info!(" - {label}: {total}/{total} complete");
        Ok(())
    }
}

/// Download or assert one artifact.
async fn download_one(
    client: &reqwest::Client,
    record: &ArtifactRecord,
    options: &DownloadOptions,
) -> Result<()> {
    // No url -> assume-local: must already be on disk; never fetched. This is
    // orthogonal to the role (a Classpath lib can be assume-local). A
    // real run fails fast if it's missing; a dry run only warns, so
    // `--dry-run` still emits a command preview without the bytes on disk.
    let Some(url) = record.url.as_deref() else {
        return assert_local(record, options.dry_run);
    };
    if options.dry_run {
        return Ok(());
    }

    // Within-run idempotency: a present+valid file (or any present file under
    // --no-verify) is trustworthy thanks to the atomic write.
    if file_present_and_valid(record, options).await {
        return Ok(());
    }

    let bytes = fetch_retrying(client, url, record, options)
        .await
        .map_err(|reason| FatalError::DownloadFailed {
            coordinate: record.coordinate.clone(),
            reason: download_failure_reason(record, reason),
        })?;
    write_atomic(&record.local_path, &bytes)
        .await
        .with_context(|| format!("writing {}", record.local_path.display()))
}

/// The failure `reason` for a fetch. A fallback url (the Mojang default for a
/// library that named none) reports the full chain we tried; an explicit url
/// keeps its own message.
fn download_failure_reason(record: &ArtifactRecord, reason: String) -> String {
    if record.url_is_fallback {
        format!(
            "has no specified download url, is not present locally at {}, \
             and was not found at Mojang's library server: {reason}",
            record.local_path.display()
        )
    } else {
        reason
    }
}

/// Assert an assume-local (url-less) artifact is present. On a real run a
/// missing file is fatal; on a dry run it only warns, so a command
/// preview can still be emitted before the caller stages the file.
fn assert_local(record: &ArtifactRecord, dry_run: bool) -> Result<()> {
    if record.local_path.is_file() {
        return Ok(());
    }
    if dry_run {
        warn!(
            " - no-url library {} not present at {} - place it there before launching",
            record.coordinate,
            record.local_path.display()
        );
        return Ok(());
    }
    Err(FatalError::MissingLocalLib {
        coordinate: record.coordinate.clone(),
        path: record.local_path.clone(),
    }
    .into())
}

/// Run [`fetch_once`] up to [`MAX_ATTEMPTS`] times with backoff. Returns the
/// bytes, or the last attempt's message on exhaustion - the caller maps that to
/// its own fatal error. Shared by the artifact path (-> `DownloadFailed`) and
/// [`Fetcher::fetch_bytes`] (meta version files), so meta JSON flows through the
/// one HTTP path.
async fn fetch_retrying(
    client: &reqwest::Client,
    url: &str,
    record: &ArtifactRecord,
    options: &DownloadOptions,
) -> Result<Vec<u8>, String> {
    let mut last_message = String::new();
    for attempt in 1..=MAX_ATTEMPTS {
        match fetch_once(client, url, record, options).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                last_message = error.message;
                if error.retryable && attempt < MAX_ATTEMPTS {
                    warn!(
                        " - attempt {attempt}/{MAX_ATTEMPTS} for {} failed: {last_message}",
                        record.coordinate
                    );
                    tokio::time::sleep(backoff(attempt)).await;
                } else {
                    break;
                }
            }
        }
    }
    Err(last_message)
}

/// A minimal "fetch some bytes from a URL" capability. Callers that only need
/// raw bytes (the meta resolver) depend on this rather than the whole
/// [`Downloader`], which also lets them be unit-tested with a recorded in-memory
/// fetcher (keeping `cargo test` offline).
// Internal trait: the futures are awaited in-task (never spawned), so the Send
// bound `async fn` warns about is not needed here.
#[allow(async_fn_in_trait)]
pub trait Fetcher {
    /// Fetch `url` into memory with retries/backoff, without sha1
    /// verification or any disk write; `label` names the thing for log/error
    /// messages. Not gated by `--dry-run`/`--no-verify` (meta JSON is input we
    /// need, not an artifact download).
    ///
    /// # Errors
    /// Returns an error if every attempt fails (transport, non-success HTTP, or
    /// a surviving 5xx); the message records the last failure, so a 404 reads as
    /// `HTTP 404 ...` and a transport error as `request error: ...`.
    async fn fetch_bytes(&self, url: &str, label: &str) -> Result<Vec<u8>>;
}

impl Fetcher for Downloader {
    async fn fetch_bytes(&self, url: &str, label: &str) -> Result<Vec<u8>> {
        // Synthesize a hash-less, size-less record so verification is a no-op
        // (verify_bytes returns Ok with no sha1/size); the label rides along as
        // the coordinate for the retry/log messages. The record's role/path are
        // unused - meta bytes are parsed in memory, never written.
        let record = ArtifactRecord {
            coordinate: label.to_owned(),
            url: Some(url.to_owned()),
            url_is_fallback: false,
            sha1: None,
            size: None,
            local_path: PathBuf::new(),
            role: Role::Classpath,
            extract_exclude: Vec::new(),
        };
        fetch_retrying(&self.client, url, &record, &self.options)
            .await
            .map_err(anyhow::Error::msg)
    }
}

/// One fetch attempt: GET, classify the status, read, and verify.
async fn fetch_once(
    client: &reqwest::Client,
    url: &str,
    record: &ArtifactRecord,
    options: &DownloadOptions,
) -> Result<Vec<u8>, AttemptError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| AttemptError::retryable(format!("request error: {error}")))?;

    let status = response.status();
    if !status.is_success() {
        // 5xx and rate-limit/timeout are worth retrying; other 4xx are not.
        let retryable = status.is_server_error()
            || status == StatusCode::REQUEST_TIMEOUT
            || status == StatusCode::TOO_MANY_REQUESTS;
        return Err(AttemptError { retryable, message: format!("HTTP {status}") });
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| AttemptError::retryable(format!("read error: {error}")))?
        .to_vec();

    // A bad body can be a transient/partial fetch - retry it.
    verify_bytes(&bytes, record, options.verify)
        .map_err(|error| AttemptError::retryable(error.to_string()))?;
    Ok(bytes)
}

/// A single-attempt error plus whether it's worth retrying.
struct AttemptError {
    retryable: bool,
    message: String,
}

impl AttemptError {
    fn retryable(message: String) -> Self {
        Self { retryable: true, message }
    }
}

/// Verify size + sha1 unless verification is off or the hash is absent/empty
/// (variant c).
fn verify_bytes(bytes: &[u8], record: &ArtifactRecord, verify: bool) -> Result<()> {
    if !verify {
        return Ok(());
    }
    if let Some(expected) = record.size
        && expected > 0
    {
        let actual = bytes.len() as u64;
        if actual != expected {
            bail!("size mismatch: expected {expected} bytes, got {actual}");
        }
    }
    if let Some(expected) = &record.sha1 {
        let actual = sha1_hex(bytes);
        if !actual.eq_ignore_ascii_case(expected) {
            bail!("sha1 mismatch: expected {expected}, got {actual}");
        }
    }
    Ok(())
}

/// Whether the target already exists and (under verification) passes it.
async fn file_present_and_valid(record: &ArtifactRecord, options: &DownloadOptions) -> bool {
    if !record.local_path.is_file() {
        return false;
    }
    if !options.verify {
        return true;
    }
    match tokio::fs::read(&record.local_path).await {
        Ok(bytes) => verify_bytes(&bytes, record, true).is_ok(),
        Err(_) => false,
    }
}

/// Write bytes to `<path>.part`, then atomically rename onto `path`.
async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut part = path.to_path_buf().into_os_string();
    part.push(".part");
    let part = PathBuf::from(part);

    tokio::fs::write(&part, bytes)
        .await
        .with_context(|| format!("writing {}", part.display()))?;
    tokio::fs::rename(&part, path)
        .await
        .with_context(|| format!("renaming {} -> {}", part.display(), path.display()))?;
    Ok(())
}

/// Hex sha1 of a byte slice.
fn sha1_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Exponential backoff: 250ms, 500ms, 1s, ...
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * u64::from(2u32.pow(attempt - 1)))
}

/// Periodically report `done/total` so a long batch never waits silently
/// (~30s progress-update rule). Aborted by the caller once the batch finishes.
fn spawn_progress(label: String, total: usize, done: Arc<AtomicUsize>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
        ticker.tick().await; // the first tick fires immediately - skip it
        loop {
            ticker.tick().await;
            let completed = done.load(Ordering::Relaxed);
            if completed >= total {
                break;
            }
            info!(" - {label}: {completed}/{total} downloaded");
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::exit::{ExitCode, exit_code_for};
    use crate::model::artifact::Role;

    fn record(role: Role, path: PathBuf, sha1: Option<&str>, size: Option<u64>) -> ArtifactRecord {
        ArtifactRecord {
            coordinate: "g:a:1".to_owned(),
            url: Some("http://127.0.0.1:0/never".to_owned()),
            url_is_fallback: false,
            sha1: sha1.map(str::to_owned),
            size,
            local_path: path,
            role,
            extract_exclude: Vec::new(),
        }
    }

    fn options(verify: bool, dry_run: bool) -> DownloadOptions {
        DownloadOptions { jobs: 4, verify, dry_run }
    }

    /// A unique scratch dir under the OS temp dir for fs tests.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-dl-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha1_hex_known_vector() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn verify_passes_size_and_sha1() {
        let rec = record(Role::Classpath, PathBuf::new(),
            Some("a9993e364706816aba3e25717850c26c9cd0d89d"), Some(3));
        assert!(verify_bytes(b"abc", &rec, true).is_ok());
    }

    #[test]
    fn verify_rejects_wrong_sha1_and_size() {
        let bad_hash = record(Role::Classpath, PathBuf::new(), Some("deadbeef"), None);
        assert!(verify_bytes(b"abc", &bad_hash, true).is_err());
        let bad_size = record(Role::Classpath, PathBuf::new(), None, Some(99));
        assert!(verify_bytes(b"abc", &bad_size, true).is_err());
    }

    #[test]
    fn verify_skips_when_disabled_or_hash_absent() {
        // --no-verify: a wrong hash is not checked.
        let wrong = record(Role::Classpath, PathBuf::new(), Some("deadbeef"), Some(99));
        assert!(verify_bytes(b"abc", &wrong, false).is_ok());
        // empty/missing hash + zero size (variant c) -> nothing to check.
        let empty = record(Role::Classpath, PathBuf::new(), None, Some(0));
        assert!(verify_bytes(b"abc", &empty, true).is_ok());
    }

    #[test]
    fn missing_assume_local_is_fatal_on_a_real_run() {
        // A url-less (assume-local) classpath lib that isn't on disk is fatal on
        // a real run - keyed on the absent url, not a special role.
        let mut rec = record(Role::Classpath, PathBuf::from("/no/such/lib.jar"), None, None);
        rec.url = None;
        let error = assert_local(&rec, false).unwrap_err();
        assert_eq!(exit_code_for(&error), ExitCode::MissingLocalLib);
    }

    #[test]
    fn missing_assume_local_only_warns_on_dry_run() {
        // A dry run must still succeed (preview), only warning about the gap.
        let mut rec = record(Role::Classpath, PathBuf::from("/no/such/lib.jar"), None, None);
        rec.url = None;
        assert!(assert_local(&rec, true).is_ok());
    }

    #[test]
    fn fallback_url_failure_spells_out_the_whole_chain() {
        // A url-less library defaulted to Mojang's server: a failed fetch must
        // read as the full chain (no url, not local, not on Mojang), not a bare
        // HTTP error on a url the user never specified.
        let mut rec = record(Role::Classpath, PathBuf::from("/inst/libraries/lw.jar"), None, None);
        rec.url_is_fallback = true;
        let reason = download_failure_reason(&rec, "HTTP 404 Not Found".to_owned());
        assert!(reason.contains("has no specified download url"), "got: {reason}");
        assert!(reason.contains("/inst/libraries/lw.jar"), "got: {reason}");
        assert!(reason.contains("Mojang"), "got: {reason}");
        assert!(reason.contains("HTTP 404 Not Found"), "got: {reason}");
    }

    #[test]
    fn explicit_url_failure_keeps_the_underlying_reason_verbatim() {
        // An explicit-url artifact already names its source; don't dress it up.
        let rec = record(Role::Classpath, PathBuf::from("/inst/libraries/lib.jar"), None, None);
        assert_eq!(
            download_failure_reason(&rec, "HTTP 500 Server Error".to_owned()),
            "HTTP 500 Server Error"
        );
    }

    #[tokio::test]
    async fn dry_run_does_no_network_for_normal_artifacts() {
        // The url points nowhere; dry-run must return before touching it.
        let client = reqwest::Client::new();
        let rec = record(Role::Classpath, PathBuf::from("/tmp/never.jar"), None, None);
        assert!(download_one(&client, &rec, &options(true, true)).await.is_ok());
    }

    #[tokio::test]
    async fn write_atomic_creates_dirs_and_leaves_no_part() {
        let dir = scratch("atomic");
        let target = dir.join("nested/lib.jar");
        write_atomic(&target, b"payload").await.unwrap();

        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"payload");
        let mut part = target.clone().into_os_string();
        part.push(".part");
        assert!(!PathBuf::from(part).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- Wet (real-network) tests --------------------------------------
    // These hit real, immutable artifacts (exact url/sha1/size taken from the
    // lwjgl3ify fixture) and are excluded from the default run. Run them with:
    //   cargo test -- --ignored

    /// Build a record pointing at a real remote artifact, written under `dir`.
    fn wet_record(
        coordinate: &str,
        url: &str,
        sha1: &str,
        size: u64,
        dir: &std::path::Path,
    ) -> ArtifactRecord {
        ArtifactRecord {
            coordinate: coordinate.to_owned(),
            url: Some(url.to_owned()),
            url_is_fallback: false,
            sha1: Some(sha1.to_owned()),
            size: Some(size),
            local_path: dir.join(format!("{}.jar", coordinate.replace(':', "_"))),
            role: Role::Classpath,
            extract_exclude: Vec::new(),
        }
    }

    #[tokio::test]
    #[ignore = "wet: needs network access to libraries.minecraft.net"]
    async fn wet_downloads_and_verifies_real_artifacts() {
        let dir = scratch("wet-libs");
        let records = [
            wet_record(
                "com.mojang:netty:1.8.8",
                "https://libraries.minecraft.net/com/mojang/netty/1.8.8/netty-1.8.8.jar",
                "0a796914d1c8a55b4da9f4a8856dd9623375d8bb",
                15966,
                &dir,
            ),
            wet_record(
                "com.paulscode:codecwav:20101023",
                "https://libraries.minecraft.net/com/paulscode/codecwav/20101023/codecwav-20101023.jar",
                "12f031cfe88fef5c1dd36c563c0a3a69bd7261da",
                5618,
                &dir,
            ),
            wet_record(
                "net.sf.jopt-simple:jopt-simple:4.5",
                "https://libraries.minecraft.net/net/sf/jopt-simple/jopt-simple/4.5/jopt-simple-4.5.jar",
                "6065cc95c661255349c1d0756657be17c29a4fd3",
                61311,
                &dir,
            ),
        ];

        let downloader = Downloader::new(options(true, false)).unwrap();
        downloader
            .download_all("wet libraries", &records)
            .await
            .expect("real artifacts should download and verify");

        for record in &records {
            let meta = std::fs::metadata(&record.local_path).expect("downloaded file present");
            assert_eq!(meta.len(), record.size.unwrap());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    #[ignore = "wet: needs network; exercises a GitHub `raw` 302 redirect"]
    async fn wet_follows_github_raw_redirect() {
        let dir = scratch("wet-redirect");
        let record = wet_record(
            "net.java.jinput:jinput-platform:2.0.5:natives-linux-arm32",
            "https://github.com/theofficialgman/lwjgl3-binaries-arm32/raw/lwjgl-2.9.4/jinput-platform-2.0.5-natives-linux.jar",
            "f3c455b71c5146acb5f8a9513247fc06db182fd5",
            4521,
            &dir,
        );

        let downloader = Downloader::new(options(true, false)).unwrap();
        downloader
            .download_all("wet redirect", std::slice::from_ref(&record))
            .await
            .expect("a redirecting download should follow and verify");
        assert!(record.local_path.is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    #[ignore = "wet: needs network; proves a real sha1 mismatch is fatal"]
    async fn wet_corrupt_sha1_is_fatal_download_failed() {
        let dir = scratch("wet-corrupt");
        // A real, reachable url but a deliberately wrong expected hash -> the
        // bytes never verify, so after retries the download is fatal.
        let record = wet_record(
            "com.mojang:netty:1.8.8",
            "https://libraries.minecraft.net/com/mojang/netty/1.8.8/netty-1.8.8.jar",
            "00000000000000000000000000000000deadbeef",
            15966,
            &dir,
        );

        let downloader = Downloader::new(options(true, false)).unwrap();
        let error = downloader
            .download_all("wet corrupt", std::slice::from_ref(&record))
            .await
            .expect_err("a sha1 mismatch must fail fatally");
        assert_eq!(exit_code_for(&error), ExitCode::DownloadFailed);
        std::fs::remove_dir_all(&dir).ok();
    }
}
