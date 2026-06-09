//! Legacy native extraction.
//!
//! Two native mechanisms exist and must not be conflated:
//! - Modern (LWJGL3 / 1.13+): the per-OS classifier jar is its own library,
//!   classified `Classpath` by `resolve` - it rides the classpath and the loader
//!   self-extracts. There is deliberately nothing to do here for those.
//! - Legacy (LWJGL2 / =<1.12): a `NativeExtract` jar whose contents we unzip
//!   into `<instance>/natives/`, honoring `extract.exclude`, so assemble can point
//!   `-Djava.library.path` at it.
//!
//! With `--headless`, input-device natives (the jinput family) are skipped -
//! there's no input device - while their jar stays on the classpath; controller
//! support is simply unavailable. `--keep-natives`/`--skip-natives` override the
//! built-in list.

use std::io;
use std::path::Path;

use anyhow::{Context, Result};
use log::{info, warn};

use crate::model::artifact::{ArtifactRecord, Role};

/// Coordinate substrings for input-device native families skipped under
/// `--headless` (no input device exists in a headless context).
const INPUT_DEVICE_NATIVES: &[&str] = &["jinput", "ois"];

/// Options controlling which natives get extracted.
#[derive(Debug, Clone)]
pub struct NativeOptions {
    /// Skip input-device natives (the built-in list) unless `--keep` overrides.
    pub headless: bool,
    /// Coordinate patterns to force-extract (highest precedence).
    pub keep: Vec<String>,
    /// Coordinate patterns to force-skip.
    pub skip: Vec<String>,
}

/// Extract every `NativeExtract` record into `natives_dir`, returning how many
/// jars were extracted. Modern (classpath) natives are ignored here by design.
///
/// This is synchronous (the `zip` crate is blocking); callers on an async
/// runtime should run it via `spawn_blocking`.
///
/// # Errors
/// Returns an error if a native jar cannot be opened or an entry cannot be
/// written.
pub fn extract_natives(
    records: &[ArtifactRecord],
    natives_dir: &Path,
    options: &NativeOptions,
) -> Result<usize> {
    let legacy: Vec<&ArtifactRecord> =
        records.iter().filter(|record| record.role == Role::NativeExtract).collect();

    if legacy.is_empty() {
        // Common for LWJGL3/3ify instances - modern natives ride the classpath
        // and need no extraction. natives/ need not exist; assemble then
        // omits -Djava.library.path.
        info!("No legacy natives to extract (modern instance - natives ride the classpath)");
        return Ok(0);
    }

    let mut extracted = 0;
    for record in legacy {
        if should_extract(&record.coordinate, options) {
            info!(" - extracting native {}", record.coordinate);
            extract_jar(&record.local_path, natives_dir, &record.extract_exclude)
                .with_context(|| format!("extracting native {}", record.coordinate))?;
            extracted += 1;
        } else {
            info!(" - skipping native {} (headless input device)", record.coordinate);
        }
    }
    info!("Extracted {extracted} native jar(s) into {}", natives_dir.display());
    Ok(extracted)
}

/// Decide whether one native should be extracted. Precedence: `--keep` wins over
/// everything, then `--skip`, then the headless built-in input-device list.
fn should_extract(coordinate: &str, options: &NativeOptions) -> bool {
    if coordinate_matches(coordinate, &options.keep) {
        return true;
    }
    if coordinate_matches(coordinate, &options.skip) {
        return false;
    }
    if options.headless && coordinate_matches(coordinate, INPUT_DEVICE_NATIVES) {
        return false;
    }
    true
}

/// Whether `coordinate` contains any of the given substrings.
fn coordinate_matches<S: AsRef<str>>(coordinate: &str, patterns: &[S]) -> bool {
    patterns.iter().any(|pattern| coordinate.contains(pattern.as_ref()))
}

/// Unzip one native jar into `natives_dir`, preserving entry paths but skipping
/// directories and any entry under an `extract.exclude` prefix. Entry
/// names are sanitized against zip-slip via `enclosed_name`.
fn extract_jar(jar_path: &Path, natives_dir: &Path, exclude: &[String]) -> Result<()> {
    let file = std::fs::File::open(jar_path)
        .with_context(|| format!("opening native jar {}", jar_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("reading native jar {}", jar_path.display()))?;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let Some(relative) = entry.enclosed_name() else {
            warn!("skipping unsafe zip entry {:?} in {}", entry.name(), jar_path.display());
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        // Honor extract.exclude (e.g. "META-INF/") exactly - stray signatures
        // can break native loading.
        let relative_str = relative.to_string_lossy();
        if exclude.iter().any(|prefix| relative_str.starts_with(prefix.as_str())) {
            continue;
        }

        let destination = natives_dir.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let mut out = std::fs::File::create(&destination)
            .with_context(|| format!("creating {}", destination.display()))?;
        io::copy(&mut entry, &mut out)
            .with_context(|| format!("writing {}", destination.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use super::*;

    fn options(headless: bool, keep: &[&str], skip: &[&str]) -> NativeOptions {
        NativeOptions {
            headless,
            keep: keep.iter().map(|s| (*s).to_owned()).collect(),
            skip: skip.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn headless_skips_jinput_but_keep_overrides() {
        let jinput = "net.java.jinput:jinput-platform:2.0.5:natives-linux";
        let lwjgl = "org.lwjgl.lwjgl:lwjgl-platform:2.9.4:natives-linux";

        // Non-headless: everything extracts.
        assert!(should_extract(jinput, &options(false, &[], &[])));
        // Headless: jinput skipped, lwjgl (not an input device) still extracts.
        assert!(!should_extract(jinput, &options(true, &[], &[])));
        assert!(should_extract(lwjgl, &options(true, &[], &[])));
        // --keep-natives jinput re-includes it even headless.
        assert!(should_extract(jinput, &options(true, &["jinput"], &[])));
        // --skip-natives wins over default-extract.
        assert!(!should_extract(lwjgl, &options(false, &[], &["lwjgl-platform"])));
        // --keep wins over --skip.
        assert!(should_extract(jinput, &options(true, &["jinput"], &["jinput"])));
    }

    /// Build a jar at `path` with the given (name, contents) entries.
    fn make_jar(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for (name, content) in entries {
            writer.start_file(*name, opts).unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mc-natives-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extract_jar_honors_exclude_and_writes_libs() {
        let dir = scratch("extract");
        let jar = dir.join("native.jar");
        make_jar(
            &jar,
            &[
                ("liblwjgl.so", b"ELF-lwjgl"),
                ("libopenal.so", b"ELF-openal"),
                ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0"),
            ],
        );
        let natives = dir.join("natives");
        extract_jar(&jar, &natives, &["META-INF/".to_owned()]).unwrap();

        assert_eq!(std::fs::read(natives.join("liblwjgl.so")).unwrap(), b"ELF-lwjgl");
        assert!(natives.join("libopenal.so").is_file());
        // Excluded prefix must not be written.
        assert!(!natives.join("META-INF").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_natives_empty_set_is_clean() {
        let dir = scratch("empty");
        // Only a classpath record -> nothing legacy to extract.
        let record = ArtifactRecord {
            coordinate: "g:a:1".to_owned(),
            url: None,
            url_is_fallback: false,
            sha1: None,
            size: None,
            local_path: PathBuf::new(),
            role: Role::Classpath,
            extract_exclude: Vec::new(),
        };
        let count = extract_natives(&[record], &dir.join("natives"), &options(false, &[], &[]))
            .unwrap();
        assert_eq!(count, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_natives_runs_for_legacy_record() {
        let dir = scratch("legacy");
        let jar = dir.join("jinput.jar");
        make_jar(&jar, &[("libjinput-linux64.so", b"ELF"), ("META-INF/x", b"y")]);
        let natives = dir.join("natives");

        let record = ArtifactRecord {
            coordinate: "net.java.jinput:jinput-platform:2.0.5:natives-linux".to_owned(),
            url: None,
            url_is_fallback: false,
            sha1: None,
            size: None,
            local_path: jar.clone(),
            role: Role::NativeExtract,
            extract_exclude: vec!["META-INF/".to_owned()],
        };

        // Non-headless: extracted.
        assert_eq!(extract_natives(std::slice::from_ref(&record), &natives, &options(false, &[], &[])).unwrap(), 1);
        assert!(natives.join("libjinput-linux64.so").is_file());

        // Headless: the jinput native is skipped.
        let natives2 = dir.join("natives-headless");
        assert_eq!(extract_natives(std::slice::from_ref(&record), &natives2, &options(true, &[], &[])).unwrap(), 0);
        assert!(!natives2.join("libjinput-linux64.so").exists());

        std::fs::remove_dir_all(&dir).ok();
    }
}
