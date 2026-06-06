//! Distinct process exit codes and the top-level error -> code mapping.
//!
//! Each fail-fast preflight failure gets its
//! own code so a caller (CI) can tell *why* a run failed. Later phases will
//! introduce typed errors and downcast them here; for now the only errors
//! reach us are IO/parse failures from `load`, so the mapping is a stub that
//! returns [`ExitCode::IoError`].

use std::fmt;
use std::path::PathBuf;

use anyhow::Error;

/// A process exit code, one per failure class.
///
/// Most variants are constructed only once their phase's preflight lands; they
/// are defined up front so the exit-code contract is stable and visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
#[allow(dead_code)]
pub enum ExitCode {
    /// Success: command emitted and (unless `--dry-run`) artifacts verified.
    Ok = 0,
    /// Unknown or unsupported `--platform` token.
    BadPlatform = 2,
    /// Zero or genuinely-conflicting `mainClass`.
    BadMainClass = 3,
    /// Unsatisfied `requires`/`conflicts`.
    UnsatisfiedDeps = 4,
    /// A no-url (`MMC-hint: local`) library missing from `libraries/`.
    MissingLocalLib = 5,
    /// A download failed or a SHA-1 mismatch survived retries.
    DownloadFailed = 6,
    /// Any IO/parse failure (also the current catch-all).
    IoError = 7,
}

impl ExitCode {
    /// The numeric code to pass to `std::process::exit`.
    #[must_use]
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// A failure whose exit code is meaningful. Downloader/preflight
/// code returns these so [`exit_code_for`] can map them to a distinct code;
/// everything else falls through to [`ExitCode::IoError`].
#[derive(Debug)]
pub enum FatalError {
    /// A no-url (`MMC-hint: local`) artifact is not present on disk.
    MissingLocalLib { coordinate: String, path: PathBuf },
    /// A download failed or a SHA-1/size mismatch survived all retries.
    DownloadFailed { coordinate: String, reason: String },
}

impl fmt::Display for FatalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FatalError::MissingLocalLib { coordinate, path } => write!(
                f,
                "no-url library {coordinate} is not present at {} \
                 (place it under libraries/ before running - the tool never \
                 fetches no-url entries)",
                path.display()
            ),
            FatalError::DownloadFailed { coordinate, reason } => {
                write!(f, "failed to download {coordinate}: {reason}")
            }
        }
    }
}

impl std::error::Error for FatalError {}

/// Map a top-level error to an exit code.
///
/// Typed [`FatalError`]s (anywhere in the context chain) map to their specific
/// code; anything else is an IO/parse failure -> [`ExitCode::IoError`].
#[must_use]
pub fn exit_code_for(error: &Error) -> ExitCode {
    match error.downcast_ref::<FatalError>() {
        Some(FatalError::MissingLocalLib { .. }) => ExitCode::MissingLocalLib,
        Some(FatalError::DownloadFailed { .. }) => ExitCode::DownloadFailed,
        None => ExitCode::IoError,
    }
}
