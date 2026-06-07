//! Artifact resolution - turn a merged `Profile` into `ArtifactRecord`s.
//!
//! For each kept library/mavenFile/mainJar this decides the [`Role`], computes
//! the maven `local_path`, and attaches the download `url`. Crucially the path
//! and url come from the same function so they can never drift: only the
//! bare-`name`+`url` variant derives a url, and everything carrying an explicit
//! `downloads.*.url` uses it verbatim - deriving a SNAPSHOT url from the
//! (resolved-version) `name` would be wrong.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use log::info;

use crate::model::artifact::{ArtifactRecord, Role};
use crate::model::patch::{Library, MainJar};
use crate::model::profile::Profile;
use crate::platform::Ctx;
use crate::rules::allowed;

/// Resolve a merged profile into the artifact records the IO phases consume.
///
/// Applies rule filtering and order-preserving last-wins dedup, then classifies
/// each survivor. Output order is: classpath/native libraries (component then
/// declared order), the main jar, then maven files.
///
/// # Errors
/// Returns an error if any coordinate is malformed (not
/// `group:artifact:version[:classifier]`).
pub fn resolve(profile: &Profile, ctx: &Ctx, instance_dir: &Path) -> Result<Vec<ArtifactRecord>> {
    let instance_abs = absolute_instance_dir(instance_dir)?;
    let libraries_dir = instance_abs.join("libraries");
    let mut records = Vec::new();

    // Keep only libs the target platform allows, then drop duplicate
    // group:artifact keeping the highest-order (last) declaration.
    let allowed_libraries: Vec<&Library> = profile
        .libraries
        .iter()
        .filter(|library| allowed(library.rules.as_deref().unwrap_or(&[]), ctx))
        .collect();
    for library in dedup_last_wins(allowed_libraries) {
        if let Some(record) = classify_library(library, ctx, &libraries_dir)
            .with_context(|| format!("resolving library {}", library.name))?
        {
            records.push(record);
        }
    }

    // The main jar goes on the classpath at the conventional versions/ path.
    if let Some(main_jar) = &profile.main_jar {
        records.push(
            classify_main_jar(main_jar, &instance_abs)
                .with_context(|| format!("resolving main jar {}", main_jar.name))?,
        );
    }

    // mavenFiles are downloaded like libraries but never on the classpath.
    for maven_file in &profile.maven_files {
        if !allowed(maven_file.rules.as_deref().unwrap_or(&[]), ctx) {
            continue;
        }
        if let Some(mut record) = classify_library(maven_file, ctx, &libraries_dir)
            .with_context(|| format!("resolving maven file {}", maven_file.name))?
        {
            // A downloadable maven file is off-classpath; keep NoUrl as-is so
            // its assume-local semantics survive.
            if record.role == Role::Classpath {
                record.role = Role::MavenFile;
            }
            records.push(record);
        }
    }

    info!("Resolved {} artifact(s) for {}", records.len(), ctx.os_token);
    Ok(records)
}

/// Classify one library into a record, or `None` if it has no artifact for the
/// target platform (a legacy native lacking a classifier for this OS).
fn classify_library(
    library: &Library,
    ctx: &Ctx,
    libraries_dir: &Path,
) -> Result<Option<ArtifactRecord>> {
    // (e-legacy) a `natives` os->classifier map means extract, not classpath.
    // Modern natives carry the classifier in their `name` and have no map, so
    // they fall through to the plain-artifact path below as Classpath.
    if library.natives.is_some() {
        return classify_legacy_native(library, ctx, libraries_dir);
    }

    let local_path = libraries_dir.join(maven_coordinate_to_path(&library.name)?);

    // (a)/(c) explicit artifact with a url -> classpath, url verbatim.
    if let Some(artifact) = library.downloads.as_ref().and_then(|d| d.artifact.as_ref())
        && let Some(url) = artifact.url.as_ref().filter(|url| !url.is_empty())
    {
        return Ok(Some(ArtifactRecord {
            coordinate: library.name.clone(),
            url: Some(url.clone()),
            sha1: normalize_sha1(artifact.sha1.as_deref()),
            size: artifact.size,
            local_path,
            role: Role::Classpath,
            extract_exclude: Vec::new(),
        }));
    }

    // (b) bare `name` + `url` base -> derive the url from the SAME relative path
    // so the classpath entry and download source cannot disagree.
    if let Some(base) = library.url.as_ref().filter(|url| !url.is_empty()) {
        let url = format!("{}/{}", base.trim_end_matches('/'), maven_relative_path(&library.name)?);
        return Ok(Some(ArtifactRecord {
            coordinate: library.name.clone(),
            url: Some(url),
            sha1: None,
            size: None,
            local_path,
            role: Role::Classpath,
            extract_exclude: Vec::new(),
        }));
    }

    // (d) no resolvable url -> assume-local; phase 4 asserts it (never fetch).
    // `MMC-hint: "local"` jars live flat in the instance's libraries dir
    // (matching MultiMC/Prism - confirmed against a real launch log), not under
    // the maven layout the downloaded libs use.
    let local_path = if library.mmc_hint.as_deref() == Some("local") {
        libraries_dir.join(maven_filename(&library.name)?)
    } else {
        local_path
    };
    Ok(Some(ArtifactRecord {
        coordinate: library.name.clone(),
        url: None,
        sha1: None,
        size: None,
        local_path,
        role: Role::NoUrl,
        extract_exclude: Vec::new(),
    }))
}

/// Resolve a legacy native: pick the classifier for the target OS (token first,
/// then classic name), substitute `${arch}`, and look up its download.
fn classify_legacy_native(
    library: &Library,
    ctx: &Ctx,
    libraries_dir: &Path,
) -> Result<Option<ArtifactRecord>> {
    let natives = library.natives.as_ref().expect("caller checked natives.is_some()");

    // The os->classifier map keys are MMC tokens and/or classic names.
    let Some(template) = natives.get(&ctx.os_token).or_else(|| natives.get(&ctx.os_name)) else {
        // No native for this platform - nothing to extract.
        return Ok(None);
    };
    // e.g. "natives-windows-${arch}" -> "natives-windows-64".
    let classifier = template.replace("${arch}", ctx.arch_number());

    let Some(artifact) = library
        .downloads
        .as_ref()
        .and_then(|downloads| downloads.classifiers.as_ref())
        .and_then(|classifiers| classifiers.get(&classifier))
    else {
        bail!("legacy native has no download for classifier {classifier:?}");
    };

    // Local path = the coordinate with the picked classifier appended, run
    // through the same path function as everything else.
    let coordinate = format!("{}:{classifier}", library.name);
    let local_path = libraries_dir.join(maven_coordinate_to_path(&coordinate)?);
    // Carry the library's extract.exclude onto the record for phase 5.
    let extract_exclude =
        library.extract.as_ref().map_or_else(Vec::new, |extract| extract.exclude.clone());
    Ok(Some(ArtifactRecord {
        url: artifact.url.as_ref().filter(|url| !url.is_empty()).cloned(),
        sha1: normalize_sha1(artifact.sha1.as_deref()),
        size: artifact.size,
        local_path,
        role: Role::NativeExtract,
        coordinate,
        extract_exclude,
    }))
}

/// Classify the main jar to the conventional `versions/<ver>/<ver>.jar` layout
/// with its url verbatim.
fn classify_main_jar(main_jar: &MainJar, instance_dir: &Path) -> Result<ArtifactRecord> {
    let coordinate = parse_coordinate(&main_jar.name)?;
    let version = coordinate.version;
    let local_path = instance_dir
        .join("versions")
        .join(version)
        .join(format!("{version}.jar"));

    let artifact = main_jar.downloads.as_ref().and_then(|downloads| downloads.artifact.as_ref());
    Ok(ArtifactRecord {
        coordinate: main_jar.name.clone(),
        url: artifact.and_then(|a| a.url.clone()).filter(|url| !url.is_empty()),
        sha1: normalize_sha1(artifact.and_then(|a| a.sha1.as_deref())),
        size: artifact.and_then(|a| a.size),
        local_path,
        role: Role::Classpath,
        extract_exclude: Vec::new(),
    })
}

/// Drop duplicate `group:artifact`, keeping the last (highest-order)
/// declaration, while preserving the relative order of the survivors.
fn dedup_last_wins(libraries: Vec<&Library>) -> Vec<&Library> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(libraries.len());
    let mut kept: Vec<&Library> = Vec::with_capacity(libraries.len());
    // Walk from the end so the last occurrence of each key is the one kept.
    for library in libraries.into_iter().rev() {
        if seen.insert(artifact_key(&library.name)) {
            kept.push(library);
        }
    }
    kept.reverse();
    kept
}

/// The `group:artifact` prefix of a maven coordinate - the first two
/// colon-separated segments, ignoring any version/classifier that follow. Also
/// used by `merge` to match `-libraries` removals.
#[must_use]
pub fn artifact_key(name: &str) -> &str {
    match name.match_indices(':').nth(1) {
        Some((index, _)) => &name[..index],
        None => name,
    }
}

/// Compute the relative maven path (`group/artifact/version/file.jar`,
/// `/`-joined) for a coordinate.
///
/// # Errors
/// Returns an error if `name` is not `group:artifact:version[:classifier]`.
pub fn maven_coordinate_to_path(name: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(maven_relative_path(name)?))
}

/// The maven layout path as a `/`-joined string (the single source both the
/// on-disk path and the variant-(b) url are built from, so they can't drift).
fn maven_relative_path(name: &str) -> Result<String> {
    let coordinate = parse_coordinate(name)?;
    let group_path = coordinate.group.replace('.', "/");
    Ok(format!(
        "{group_path}/{}/{}/{}",
        coordinate.artifact,
        coordinate.version,
        coordinate.filename()
    ))
}

/// The bare jar filename (`artifact-version[-classifier].jar`) - used for
/// `MMC-hint: local` jars, which live flat in the instance's libraries dir.
fn maven_filename(name: &str) -> Result<String> {
    Ok(parse_coordinate(name)?.filename())
}

/// A parsed maven coordinate (borrows from the input name).
struct Coordinate<'a> {
    group: &'a str,
    artifact: &'a str,
    version: &'a str,
    classifier: Option<&'a str>,
}

impl Coordinate<'_> {
    /// The jar filename: `artifact-version[-classifier].jar`.
    fn filename(&self) -> String {
        match self.classifier {
            Some(classifier) => format!("{}-{}-{classifier}.jar", self.artifact, self.version),
            None => format!("{}-{}.jar", self.artifact, self.version),
        }
    }
}

/// Split `group:artifact:version[:classifier]` into its parts.
fn parse_coordinate(name: &str) -> Result<Coordinate<'_>> {
    let parts: Vec<&str> = name.split(':').collect();
    match parts.as_slice() {
        [group, artifact, version] => {
            Ok(Coordinate { group, artifact, version, classifier: None })
        }
        [group, artifact, version, classifier] => {
            Ok(Coordinate { group, artifact, version, classifier: Some(classifier) })
        }
        _ => bail!(
            "invalid maven coordinate {name:?}: expected group:artifact:version[:classifier]"
        ),
    }
}

/// Treat an empty SHA-1 as "no hash" so download verification is uniformly
/// skipped on both empty-string and missing hashes.
fn normalize_sha1(sha1: Option<&str>) -> Option<String> {
    sha1.filter(|hash| !hash.is_empty()).map(str::to_owned)
}

/// Make the instance dir absolute (without requiring it to exist yet) so the
/// resolved `local_path`s are absolute for phase 6 to join directly. Idempotent
/// on an already-absolute path, so callers may pre-absolutize and share it.
///
/// # Errors
/// Returns an error if the current directory cannot be read.
pub fn absolute_instance_dir(instance_dir: &Path) -> Result<PathBuf> {
    if instance_dir.is_absolute() {
        Ok(instance_dir.to_path_buf())
    } else {
        let cwd = std::env::current_dir().context("determining the current directory")?;
        Ok(cwd.join(instance_dir))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::load::load_instance;
    use crate::merge::merge;
    use crate::platform::{Platform, expand_platform};

    /// Parse one library from inline JSON (test convenience).
    fn library(json: &str) -> Library {
        serde_json::from_str(json).expect("inline library fixture should parse")
    }

    #[test]
    fn coordinate_to_path_plain_and_classified() {
        assert_eq!(
            maven_coordinate_to_path("com.google.guava:guava:17.0").unwrap(),
            Path::new("com/google/guava/guava/17.0/guava-17.0.jar")
        );
        // The 4-segment classifier form (e.g. the client main jar coordinate).
        assert_eq!(
            maven_coordinate_to_path("com.mojang:minecraft:1.7.10:client").unwrap(),
            Path::new("com/mojang/minecraft/1.7.10/minecraft-1.7.10-client.jar")
        );
    }

    #[test]
    fn coordinate_to_path_rejects_malformed() {
        assert!(maven_coordinate_to_path("group:artifact").is_err());
    }

    #[test]
    fn snapshot_url_is_kept_verbatim_not_derived_from_name() {
        // The name carries the resolved version; the url path uses the base
        // SNAPSHOT dir + timestamped file. The record must keep the url as-is.
        let snapshot = library(
            r#"{ "name": "org.lwjgl:lwjgl:3.4.2-20260602.093430-9",
                 "downloads": { "artifact": {
                   "sha1": "abc", "size": 10,
                   "url": "https://nexus/org/lwjgl/lwjgl/3.4.2-SNAPSHOT/lwjgl-3.4.2-20260602.093430-9.jar"
                 } } }"#,
        );
        let ctx = expand_platform(Platform::Linux);
        let record = classify_library(&snapshot, &ctx, Path::new("/inst/libraries"))
            .unwrap()
            .unwrap();

        assert_eq!(record.role, Role::Classpath);
        assert_eq!(
            record.url.as_deref(),
            Some("https://nexus/org/lwjgl/lwjgl/3.4.2-SNAPSHOT/lwjgl-3.4.2-20260602.093430-9.jar")
        );
        // local path derives from the (resolved-version) name, independently.
        assert!(record.local_path.ends_with(
            "org/lwjgl/lwjgl/3.4.2-20260602.093430-9/lwjgl-3.4.2-20260602.093430-9.jar"
        ));
    }

    #[test]
    fn bare_name_plus_url_base_derives_matching_url_and_path() {
        let bare = library(r#"{ "name": "org.example:lib:1.0", "url": "https://repo.example/maven/" }"#);
        let ctx = expand_platform(Platform::Linux);
        let record = classify_library(&bare, &ctx, Path::new("/inst/libraries"))
            .unwrap()
            .unwrap();

        assert_eq!(record.role, Role::Classpath);
        assert_eq!(
            record.url.as_deref(),
            Some("https://repo.example/maven/org/example/lib/1.0/lib-1.0.jar")
        );
        assert!(record.local_path.ends_with("org/example/lib/1.0/lib-1.0.jar"));
    }

    #[test]
    fn empty_sha1_becomes_none() {
        let lib = library(
            r#"{ "name": "lzma:lzma:0.0.1",
                 "downloads": { "artifact": { "sha1": "", "size": 0,
                   "url": "https://libraries.minecraft.net/lzma/lzma/0.0.1/lzma-0.0.1.jar" } } }"#,
        );
        let ctx = expand_platform(Platform::Linux);
        let record = classify_library(&lib, &ctx, Path::new("/inst/libraries")).unwrap().unwrap();
        assert_eq!(record.role, Role::Classpath);
        assert_eq!(record.sha1, None);
    }

    #[test]
    fn no_url_local_hint_is_nourl_with_no_url() {
        let local = library(
            r#"{ "name": "com.github.GTNewHorizons:lwjgl3ify:3.0.23:forgePatches",
                 "MMC-hint": "local" }"#,
        );
        let ctx = expand_platform(Platform::Linux);
        let record = classify_library(&local, &ctx, Path::new("/inst/libraries"))
            .unwrap()
            .unwrap();

        assert_eq!(record.role, Role::NoUrl);
        assert_eq!(record.url, None);
        // `MMC-hint: local` jars live flat in <instance>/libraries/, matching
        // MultiMC/Prism - not under the maven layout.
        assert!(record.local_path.ends_with("libraries/lwjgl3ify-3.0.23-forgePatches.jar"));
    }

    #[test]
    fn no_url_without_local_hint_keeps_maven_path() {
        // A url-less entry that is *not* MMC-hint:local stays on the maven layout
        // (the flat placement is specific to the local hint).
        let bare = library(r#"{ "name": "org.example:thing:1.0" }"#);
        let ctx = expand_platform(Platform::Linux);
        let record = classify_library(&bare, &ctx, Path::new("/inst/libraries"))
            .unwrap()
            .unwrap();
        assert_eq!(record.role, Role::NoUrl);
        assert!(record.local_path.ends_with("org/example/thing/1.0/thing-1.0.jar"));
    }

    #[test]
    fn legacy_native_picks_classifier_with_arch_substituted() {
        // twitch-platform shape: windows classifier carries ${arch}.
        let native = library(
            r#"{ "name": "tv.twitch:twitch-platform:5.16",
                 "natives": { "windows": "natives-windows-${arch}" },
                 "downloads": { "classifiers": {
                   "natives-windows-32": { "sha1": "aa", "size": 1,
                     "url": "https://example/twitch-platform-5.16-natives-windows-32.jar" },
                   "natives-windows-64": { "sha1": "bb", "size": 2,
                     "url": "https://example/twitch-platform-5.16-natives-windows-64.jar" }
                 } } }"#,
        );

        // windows-x86 -> arch_number 32 -> picks the -32 classifier.
        let ctx = expand_platform(Platform::WindowsX86);
        let record = classify_library(&native, &ctx, Path::new("/inst/libraries"))
            .unwrap()
            .unwrap();
        assert_eq!(record.role, Role::NativeExtract);
        assert_eq!(record.url.as_deref(), Some("https://example/twitch-platform-5.16-natives-windows-32.jar"));
        assert!(record.local_path.ends_with(
            "tv/twitch/twitch-platform/5.16/twitch-platform-5.16-natives-windows-32.jar"
        ));

        // 64-bit windows -> the -64 classifier.
        let ctx64 = expand_platform(Platform::Windows);
        let record64 = classify_library(&native, &ctx64, Path::new("/inst/libraries"))
            .unwrap()
            .unwrap();
        assert_eq!(record64.sha1.as_deref(), Some("bb"));

        // linux has no entry in the natives map -> skipped (None).
        let linux = expand_platform(Platform::Linux);
        assert!(classify_library(&native, &linux, Path::new("/inst/libraries")).unwrap().is_none());
    }

    #[test]
    fn dedup_keeps_last_declared_and_preserves_order() {
        let libs = [
            library(r#"{ "name": "g:a:1.0" }"#),
            library(r#"{ "name": "g:b:1.0" }"#),
            library(r#"{ "name": "g:a:2.0" }"#),
        ];
        let refs: Vec<&Library> = libs.iter().collect();
        let kept = dedup_last_wins(refs);
        let names: Vec<&str> = kept.iter().map(|l| l.name.as_str()).collect();
        // g:a deduped to its 2.0 (last) occurrence; b preserved before it.
        assert_eq!(names, ["g:b:1.0", "g:a:2.0"]);
    }

    /// End-to-end resolve of the lwjgl3ify variant for a linux target.
    #[test]
    fn example_resolve_linux_roles_and_dedup() {
        let patches = load_instance(Path::new("example-files/lwjgl3ify-variant")).unwrap();
        let profile = merge(&patches);
        let ctx = expand_platform(Platform::Linux);
        let records = resolve(&profile, &ctx, Path::new("example-files/lwjgl3ify-variant")).unwrap();

        // guava appears in two components (15.0, 17.0) -> deduped to one, the
        // higher-order forge 17.0.
        let guavas: Vec<&str> = records
            .iter()
            .filter(|r| r.coordinate.starts_with("com.google.guava:guava:"))
            .map(|r| r.coordinate.as_str())
            .collect();
        assert_eq!(guavas, ["com.google.guava:guava:17.0"]);

        // On linux exactly one legacy native extracts (jinput-platform); the
        // twitch natives are rule-excluded.
        let natives: Vec<&str> = records
            .iter()
            .filter(|r| r.role == Role::NativeExtract)
            .map(|r| r.coordinate.as_str())
            .collect();
        assert_eq!(natives.len(), 1);
        assert!(natives[0].starts_with("net.java.jinput:jinput-platform:2.0.5:natives-linux"));

        // The MMC-hint local forgePatches jar is the lone no-url entry.
        let no_url: Vec<&str> = records
            .iter()
            .filter(|r| r.role == Role::NoUrl)
            .map(|r| r.coordinate.as_str())
            .collect();
        assert_eq!(no_url, ["com.github.GTNewHorizons:lwjgl3ify:3.0.23:forgePatches"]);

        // The main jar resolves to the conventional versions/ layout.
        let main_jar = records
            .iter()
            .find(|r| r.coordinate == "com.mojang:minecraft:1.7.10:client")
            .expect("main jar record present");
        assert_eq!(main_jar.role, Role::Classpath);
        assert!(main_jar.local_path.ends_with("versions/1.7.10/1.7.10.jar"));
    }
}
