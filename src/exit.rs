//! Distinct process exit codes and the top-level error -> code mapping.
//!
//! Each fail-fast preflight failure gets its
//! own code so a caller (CI) can tell *why* a run failed. Later phases will
//! introduce typed errors and downcast them here; for now the only errors
//! reach us are IO/parse failures from `load`, so the mapping is a stub that
//! returns [`ExitCode::IoError`].

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

/// Map a top-level error to an exit code.
///
/// Phase 1 only produces IO/parse errors, so everything maps to
/// [`ExitCode::IoError`]. Later phases attach typed errors and downcast here to
/// return the specific codes above.
#[must_use]
pub fn exit_code_for(_error: &Error) -> ExitCode {
    ExitCode::IoError
}
