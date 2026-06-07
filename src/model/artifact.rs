//! `ArtifactRecord` - the generic resolved unit the IO phases share.
//!
//! `resolve` turns each `Library`/`mavenFile`/`mainJar` (and later each asset)
//! into one of these: its on-disk [`Role`], its absolute `local_path`, and its
//! download `url` (or `None` for assume-local entries). Download (phase 4),
//! natives (phase 5), and the emitter (phase 6) all operate on this one shape.

use std::path::PathBuf;

/// What an artifact is *for* - decides whether it lands on the classpath, gets
/// extracted, is fetched-but-excluded, or must already be present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// On the classpath: plain libs, modern self-extracting natives, main jar.
    Classpath,
    /// Downloaded into `libraries/` but off the classpath.
    MavenFile,
    /// Legacy native jar to extract into `natives/`.
    NativeExtract,
    /// An asset-store object (produced by the phase-4 asset pipeline).
    Asset,
    /// No resolvable url: must already exist locally, else phase 4 fails
    /// Never fetched.
    NoUrl,
}

/// One resolved artifact: where it comes from, where it goes, and its role.
#[derive(Debug, Clone)]
pub struct ArtifactRecord {
    /// The maven coordinate (or asset name) this record resolves - for
    /// diagnostics, dedup tracing, and the optional `resolution.lock`.
    pub coordinate: String,
    /// Download source, verbatim from the patch; `None` for assume-local.
    pub url: Option<String>,
    /// Expected SHA-1, or `None` to skip verification (empty hash).
    pub sha1: Option<String>,
    /// Declared size in bytes, if known.
    pub size: Option<u64>,
    /// Absolute on-disk destination under the instance directory.
    pub local_path: PathBuf,
    /// The artifact's role (see [`Role`]).
    pub role: Role,
    /// Path prefixes to skip when unzipping (only meaningful for
    /// `NativeExtract`; empty otherwise). Carries the library's `extract.exclude`
    /// so phase 5 can honor it without re-consulting the profile.
    pub extract_exclude: Vec<String>,
}
