//! JDK selection.
//!
//! Either honor an explicit `--java <path>` or pick a JDK from `PATH` whose
//! major version satisfies the profile's `compatibleJavaMajors`. Provisioning
//! JDKs is out of scope - we only select and point at one, failing
//! clearly if nothing on `PATH` matches.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use log::{info, warn};

/// Choose the JDK to launch with.
///
/// With `--java` set, that path is used verbatim (we still log its version for
/// the user). Otherwise every `java` discoverable on `PATH` is probed and the
/// first whose major satisfies `compatible_majors` is chosen; an empty
/// `compatible_majors` accepts any working `java`.
///
/// # Errors
/// Returns an error if the explicit path cannot be probed, or if no `PATH`
/// `java` matches the required majors.
pub fn select_java(java_override: Option<&Path>, compatible_majors: &[u32]) -> Result<PathBuf> {
    if let Some(path) = java_override {
        let major = probe_java_major(path)
            .with_context(|| format!("probing the --java binary at {}", path.display()))?;
        announce_selection(path, major, compatible_majors);
        return Ok(path.to_path_buf());
    }

    // Probe each candidate on PATH, keeping the first that satisfies the majors.
    let candidates = discover_path_javas();
    if candidates.is_empty() {
        bail!("no `java` found on PATH - install a JDK or pass --java <path>");
    }

    let mut probed: Vec<(PathBuf, u32)> = Vec::new();
    for candidate in candidates {
        match probe_java_major(&candidate) {
            Ok(major) => probed.push((candidate, major)),
            // A broken entry on PATH shouldn't abort the search.
            Err(error) => warn!(" - skipping unprobeable java {}: {error}", candidate.display()),
        }
    }

    for (path, major) in &probed {
        if compatible_majors.is_empty() || compatible_majors.contains(major) {
            announce_selection(path, *major, compatible_majors);
            return Ok(path.clone());
        }
    }

    let found: Vec<String> = probed.iter().map(|(_, major)| major.to_string()).collect();
    Err(anyhow!(
        "no JDK on PATH satisfies compatibleJavaMajors {compatible_majors:?} \
         (found majors: [{}]) - pass --java <path> to a matching JDK",
        found.join(", ")
    ))
}

/// Log the chosen JDK, warning if its major is outside the compatible set (we
/// still honor an explicit `--java`, but the user should know).
fn announce_selection(path: &Path, major: u32, compatible_majors: &[u32]) {
    if !compatible_majors.is_empty() && !compatible_majors.contains(&major) {
        warn!(
            " - selected java {} is major {major}, outside compatibleJavaMajors {compatible_majors:?}",
            path.display()
        );
    }
    info!(" - using java {} (major {major})", path.display());
}

/// Candidate `java` executables on `PATH`, in `PATH` order. `JAVA_HOME/bin/java`
/// is tried first when set, since it is the user's chosen default.
fn discover_path_javas() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(java_home) = std::env::var_os("JAVA_HOME") {
        candidates.push(PathBuf::from(java_home).join("bin").join("java"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("java"));
        }
    }
    // Keep only entries that exist, deduping repeated PATH dirs.
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|candidate| candidate.is_file() && seen.insert(candidate.clone()));
    candidates
}

/// Run `<java> -version` and parse the major version from its output.
///
/// # Errors
/// Returns an error if the process can't be spawned or its version line can't
/// be parsed.
fn probe_java_major(java: &Path) -> Result<u32> {
    // `java -version` prints to stderr by convention.
    let output = Command::new(java)
        .arg("-version")
        .output()
        .with_context(|| format!("running {} -version", java.display()))?;
    let text = String::from_utf8_lossy(&output.stderr);
    parse_java_major(&text)
        .ok_or_else(|| anyhow!("could not parse a java major version from: {}", text.trim()))
}

/// Extract the major version from `java -version` output.
///
/// Handles both the modern scheme (`"21.0.1"` -> 21, `"25"` -> 25) and the legacy
/// `1.x` scheme (`"1.8.0_392"` -> 8), reading the quoted version token from the
/// `... version "..."` line.
#[must_use]
pub fn parse_java_major(version_output: &str) -> Option<u32> {
    // The version sits in quotes on the first `... version "..."` line.
    let quoted = version_output
        .lines()
        .find_map(|line| line.split_once("version \"").map(|(_, rest)| rest))?;
    let version = quoted.split('"').next()?;

    let mut parts = version.split(['.', '_', '-']);
    let first = parts.next()?;
    if first == "1" {
        // Legacy `1.8.0_392` -> the major is the second component.
        parts.next()?.parse().ok()
    } else {
        first.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modern_dotted_version() {
        let out = "openjdk version \"21.0.1\" 2023-10-17\nOpenJDK Runtime Environment";
        assert_eq!(parse_java_major(out), Some(21));
    }

    #[test]
    fn parses_modern_single_component_version() {
        let out = "openjdk version \"25\" 2025-09-16";
        assert_eq!(parse_java_major(out), Some(25));
    }

    #[test]
    fn parses_legacy_one_dot_eight() {
        let out = "java version \"1.8.0_392\"\nJava(TM) SE Runtime Environment";
        assert_eq!(parse_java_major(out), Some(8));
    }

    #[test]
    fn parses_early_access_suffix() {
        let out = "openjdk version \"26-ea\" 2026-03-17";
        assert_eq!(parse_java_major(out), Some(26));
    }

    #[test]
    fn rejects_output_without_a_version_line() {
        assert_eq!(parse_java_major("no version here"), None);
    }
}
