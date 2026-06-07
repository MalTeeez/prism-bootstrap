//! Emit the deliverable.
//!
//! Write `launch.argv` (one token per line) and print the same command to
//! stdout. With `--headless` also write a companion `launch.env` carrying the
//! software-GL hints for the caller to source - never an xvfb-wrapped command
//!. Optionally write a `resolution.lock` audit manifest.

use std::path::Path;

use anyhow::{Context, Result};
use log::info;
use serde::Serialize;

use crate::model::artifact::{ArtifactRecord, Role};

/// Software-GL environment hints written to `launch.env` under `--headless`
///. The caller sources these; they are not part of the argv.
const HEADLESS_ENV: &[(&str, &str)] =
    &[("LIBGL_ALWAYS_SOFTWARE", "1"), ("GALLIUM_DRIVER", "llvmpipe")];

/// Write `launch.argv` to `argv_path`, print the command to stdout, and (when
/// `headless`) write `launch.env` beside it. Always writes `resolution.lock`
/// next to the argv as a cheap audit manifest.
///
/// # Errors
/// Returns an error if any output file cannot be written.
pub fn emit(
    argv: &[String],
    argv_path: &Path,
    records: &[ArtifactRecord],
    headless: bool,
) -> Result<()> {
    // One token per line, trailing newline.
    let argv_text = format!("{}\n", argv.join("\n"));
    std::fs::write(argv_path, argv_text)
        .with_context(|| format!("writing launch.argv to {}", argv_path.display()))?;
    info!("Wrote launch command ({} tokens) to {}", argv.len(), argv_path.display());

    // The deliverable also goes to stdout as a single runnable line.
    println!("{}", argv.join(" "));

    let output_dir = argv_path.parent().unwrap_or_else(|| Path::new("."));

    if headless {
        write_env(&output_dir.join("launch.env"))?;
    }
    write_resolution_lock(&output_dir.join("resolution.lock"), records)?;
    Ok(())
}

/// Write the headless `launch.env` (one `KEY=value` per line).
fn write_env(env_path: &Path) -> Result<()> {
    let mut text = String::new();
    for (key, value) in HEADLESS_ENV {
        text.push_str(key);
        text.push('=');
        text.push_str(value);
        text.push('\n');
    }
    std::fs::write(env_path, text)
        .with_context(|| format!("writing launch.env to {}", env_path.display()))?;
    info!(" - wrote headless env hints to {}", env_path.display());
    Ok(())
}

/// A single artifact row in `resolution.lock`.
#[derive(Serialize)]
struct LockEntry<'a> {
    coordinate: &'a str,
    url: Option<&'a str>,
    sha1: Option<&'a str>,
    local_path: String,
    role: &'static str,
}

/// Write the `resolution.lock` audit manifest: every resolved artifact's
/// coordinate, url, sha1, local path, and role.
fn write_resolution_lock(lock_path: &Path, records: &[ArtifactRecord]) -> Result<()> {
    let entries: Vec<LockEntry> = records
        .iter()
        .map(|record| LockEntry {
            coordinate: &record.coordinate,
            url: record.url.as_deref(),
            sha1: record.sha1.as_deref(),
            local_path: record.local_path.to_string_lossy().into_owned(),
            role: role_name(record.role),
        })
        .collect();
    let json = serde_json::to_string_pretty(&entries).context("serialising resolution.lock")?;
    std::fs::write(lock_path, json)
        .with_context(|| format!("writing resolution.lock to {}", lock_path.display()))?;
    Ok(())
}

/// The stable string name of a role for the manifest.
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Classpath => "classpath",
        Role::MavenFile => "maven-file",
        Role::NativeExtract => "native-extract",
        Role::Asset => "asset",
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn record(coordinate: &str) -> ArtifactRecord {
        ArtifactRecord {
            coordinate: coordinate.to_owned(),
            url: Some("https://example/lib.jar".to_owned()),
            sha1: Some("abc".to_owned()),
            size: Some(1),
            local_path: PathBuf::from("/inst/libraries/lib.jar"),
            role: Role::Classpath,
            extract_exclude: Vec::new(),
        }
    }

    #[test]
    fn emit_writes_argv_one_token_per_line() {
        let dir = std::env::temp_dir().join(format!("mc-emit-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let argv_path = dir.join("launch.argv");
        let argv = vec!["/jdk/bin/java".to_owned(), "-Xmx1g".to_owned(), "Main".to_owned()];

        emit(&argv, &argv_path, &[record("g:a:1")], false).unwrap();

        let written = std::fs::read_to_string(&argv_path).unwrap();
        assert_eq!(written, "/jdk/bin/java\n-Xmx1g\nMain\n");
        // resolution.lock written beside it; launch.env not (not headless).
        assert!(dir.join("resolution.lock").exists());
        assert!(!dir.join("launch.env").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn headless_writes_env_file() {
        let dir = std::env::temp_dir().join(format!("mc-emit-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let argv_path = dir.join("launch.argv");

        emit(&["java".to_owned()], &argv_path, &[], true).unwrap();

        let env = std::fs::read_to_string(dir.join("launch.env")).unwrap();
        assert!(env.contains("LIBGL_ALWAYS_SOFTWARE=1"));
        assert!(env.contains("GALLIUM_DRIVER=llvmpipe"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
