//! The asset pipeline, reusing the one [`Downloader`] fetch path.
//!
//! Download the `assetIndex` -> `assets/indexes/<id>.json`, parse it, fetch each
//! object to `assets/objects/<ab>/<hash>`, and - for a legacy `virtual` /
//! `map_to_resources` index - materialize a readable `assets/virtual/<id>/<name>`
//! tree. Modern indexes use the plain object store only.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use log::{info, warn};
use regex::Regex;
use serde::Deserialize;

use crate::download::Downloader;
use crate::model::artifact::{ArtifactRecord, Role};
use crate::model::patch::{AssetIndexRef, Logging};

/// Mojang's asset host for the hashed object store.
const RESOURCES_BASE: &str = "https://resources.download.minecraft.net";

/// A parsed `assets/indexes/<id>.json`.
#[derive(Debug, Deserialize)]
struct AssetIndex {
    #[serde(default)]
    objects: HashMap<String, AssetObject>,
    /// Legacy pre-1.7 layout: a readable name->file tree is required.
    #[serde(default, rename = "virtual")]
    is_virtual: bool,
    /// 1.7-era flag: assets are mapped into a resources tree.
    #[serde(default)]
    map_to_resources: bool,
}

/// One asset object: content-addressed by its sha1 `hash`.
#[derive(Debug, Deserialize)]
struct AssetObject {
    hash: String,
    size: u64,
}

impl AssetIndex {
    /// Whether a readable `virtual/<id>/<name>` tree must be materialized.
    fn needs_virtual_tree(&self) -> bool {
        self.is_virtual || self.map_to_resources
    }
}

/// Run the asset pipeline for one instance's asset index.
///
/// # Errors
/// Missing index url, a failed index/object download, or a parse/IO failure.
pub async fn download_assets(
    downloader: &Downloader,
    asset_index: &AssetIndexRef,
    instance_dir: &Path,
) -> Result<()> {
    let assets_dir = instance_dir.join("assets");
    let Some(url) = asset_index.url.as_deref() else {
        bail!("asset index {} has no download url", asset_index.id);
    };

    if downloader.dry_run() {
        info!("Dry run: skipping asset index + objects for {}", asset_index.id);
        return Ok(());
    }

    // 1. The index file itself (verified by its own sha1).
    let index_path = assets_dir.join("indexes").join(format!("{}.json", asset_index.id));
    let index_record = ArtifactRecord {
        coordinate: format!("assetIndex:{}", asset_index.id),
        url: Some(url.to_owned()),
        url_is_fallback: false,
        sha1: asset_index.sha1.clone().filter(|hash| !hash.is_empty()),
        size: asset_index.size,
        local_path: index_path.clone(),
        role: Role::Asset,
        extract_exclude: Vec::new(),
    };
    downloader.download_all("asset index", std::slice::from_ref(&index_record)).await?;

    // 2. Parse it and fetch every object through the same path.
    let content = tokio::fs::read(&index_path)
        .await
        .with_context(|| format!("reading asset index {}", index_path.display()))?;
    let index: AssetIndex = serde_json::from_slice(&content)
        .with_context(|| format!("parsing asset index {}", index_path.display()))?;

    let objects = build_object_records(&index, &assets_dir);
    downloader.download_all("asset objects", &objects).await?;

    // 3. Legacy indexes also need the readable tree.
    if index.needs_virtual_tree() {
        materialize_virtual_tree(&index, &assets_dir, &asset_index.id).await?;
    }
    Ok(())
}

/// On-disk path for a log4j config: `<instance>/assets/log_configs/<id>`, matching
/// the launcher layout. Shared by [`ensure_log_config`] and the assembler so the
/// downloaded file and the `${path}` arg cannot drift.
#[must_use]
pub fn log_config_path(instance_dir: &Path, file_id: &str) -> PathBuf {
    instance_dir.join("assets").join("log_configs").join(file_id)
}

/// Download the component's log4j config (the `Log4Shell` mitigation) and patch its
/// console layout for headless use. Mojang's config logs `XMLLayout` to the console
/// for the GUI launcher to parse; piped headless that XML spews to stdout, so we
/// swap it for a plain `PatternLayout` while leaving the JNDI `RegexFilter` (the
/// actual mitigation) untouched. A no-op when no file/url is declared, or on a dry
/// run (the assembler still emits the arg).
///
/// # Errors
/// A failed download or an IO failure reading/writing the config.
pub async fn ensure_log_config(
    downloader: &Downloader,
    logging: &Logging,
    instance_dir: &Path,
) -> Result<()> {
    // `file` carries the download; the assembler emits `argument` regardless.
    let Some(file) = &logging.file else { return Ok(()) };
    let Some(url) = file.url.as_deref().filter(|url| !url.is_empty()) else {
        return Ok(());
    };
    if downloader.dry_run() {
        info!("Dry run: skipping log4j config {}", file.id);
        return Ok(());
    }

    let path = log_config_path(instance_dir, &file.id);
    // Idempotency guard: a config already on disk without `XMLLayout` is either
    // patched or natively plain. Skip before the network, since patching below
    // invalidates the sha1 and would otherwise force a re-download every run.
    if let Ok(existing) = tokio::fs::read_to_string(&path).await
        && !existing.contains("XMLLayout")
    {
        return Ok(());
    }

    // Fetch the official file (verified against Mojang's published sha1).
    let record = ArtifactRecord {
        coordinate: format!("logConfig:{}", file.id),
        url: Some(url.to_owned()),
        url_is_fallback: false,
        sha1: file.sha1.clone().filter(|hash| !hash.is_empty()),
        size: file.size,
        local_path: path.clone(),
        role: Role::Asset,
        extract_exclude: Vec::new(),
    };
    downloader.download_all("log config", std::slice::from_ref(&record)).await?;

    // Patch the console layout only when the download actually used `XMLLayout`.
    let contents = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading log4j config {}", path.display()))?;
    if let Some(patched) = patch_console_layout(&contents) {
        tokio::fs::write(&path, patched)
            .await
            .with_context(|| format!("writing patched log4j config {}", path.display()))?;
        info!(" - patched log4j console layout in {} for headless output", file.id);
    }
    Ok(())
}

/// Swap a self-closing console `<XMLLayout />` for a plain `PatternLayout` (Mojang's
/// own file-appender pattern), so headless stdout is readable. Returns the patched
/// content, or `None` when there is no such layout to swap.
///
/// Only the self-closing form is handled (what every known Mojang `client-1.x.xml`
/// uses); a paired `<XMLLayout>...</XMLLayout>` would be left as-is.
fn patch_console_layout(content: &str) -> Option<String> {
    let xml_layout = Regex::new(r"<XMLLayout\s*/>").expect("static regex compiles");
    if !xml_layout.is_match(content) {
        return None;
    }
    let pattern_layout = r#"<PatternLayout pattern="[%d{HH:mm:ss}] [%t/%level]: %msg%n" />"#;
    Some(xml_layout.replace_all(content, pattern_layout).into_owned())
}

/// Turn each `objects[name] = {hash, size}` into a download record at
/// `assets/objects/<hash[:2]>/<hash>`.
fn build_object_records(index: &AssetIndex, assets_dir: &Path) -> Vec<ArtifactRecord> {
    let objects_dir = assets_dir.join("objects");
    index
        .objects
        .values()
        .filter_map(|object| {
            if object.hash.len() < 2 {
                warn!("skipping asset with malformed hash {:?}", object.hash);
                return None;
            }
            let prefix = &object.hash[..2];
            Some(ArtifactRecord {
                coordinate: format!("asset:{}", object.hash),
                url: Some(format!("{RESOURCES_BASE}/{prefix}/{}", object.hash)),
                url_is_fallback: false,
                sha1: Some(object.hash.clone()),
                size: Some(object.size),
                local_path: objects_dir.join(prefix).join(&object.hash),
                role: Role::Asset,
                extract_exclude: Vec::new(),
            })
        })
        .collect()
}

/// Materialize `assets/virtual/<id>/<name>` from the object store, hardlinking
/// where possible and copying otherwise.
async fn materialize_virtual_tree(index: &AssetIndex, assets_dir: &Path, id: &str) -> Result<()> {
    let objects_dir = assets_dir.join("objects");
    let virtual_root = assets_dir.join("virtual").join(id);
    info!("Materializing virtual asset tree for {id} ({} entries)", index.objects.len());

    for (name, object) in &index.objects {
        if object.hash.len() < 2 {
            continue;
        }
        let source = objects_dir.join(&object.hash[..2]).join(&object.hash);
        let destination = virtual_root.join(name);
        if destination.exists() {
            continue;
        }
        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        // Hardlink to avoid duplicating the store; fall back to a copy if the
        // filesystem refuses (e.g. cross-device).
        if tokio::fs::hard_link(&source, &destination).await.is_err() {
            tokio::fs::copy(&source, &destination).await.with_context(|| {
                format!("copying {} -> {}", source.display(), destination.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn parse(json: &str) -> AssetIndex {
        serde_json::from_str(json).expect("inline asset index should parse")
    }

    #[test]
    fn detects_virtual_and_map_to_resources() {
        assert!(parse(r#"{ "virtual": true, "objects": {} }"#).needs_virtual_tree());
        assert!(parse(r#"{ "map_to_resources": true, "objects": {} }"#).needs_virtual_tree());
        assert!(!parse(r#"{ "objects": {} }"#).needs_virtual_tree());
    }

    #[test]
    fn object_record_url_and_path_use_hash_prefix() {
        let index = parse(
            r#"{ "objects": { "minecraft/lang/en_US.lang":
                 { "hash": "abcd1234ef567890abcd1234ef567890abcd1234", "size": 42 } } }"#,
        );
        let records = build_object_records(&index, Path::new("/inst/assets"));
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.role, Role::Asset);
        assert_eq!(record.size, Some(42));
        assert_eq!(
            record.url.as_deref(),
            Some("https://resources.download.minecraft.net/ab/abcd1234ef567890abcd1234ef567890abcd1234")
        );
        assert!(record.local_path.ends_with(
            "objects/ab/abcd1234ef567890abcd1234ef567890abcd1234"
        ));
    }

    #[test]
    fn patch_console_layout_swaps_xmllayout_keeping_the_mitigation() {
        // A client-1.7.xml-shaped config: the console XMLLayout becomes a
        // PatternLayout, while the JNDI RegexFilter survives verbatim.
        let original = r#"<Configuration status="WARN">
    <Appenders>
        <Console name="SysOut" target="SYSTEM_OUT">
            <XMLLayout />
        </Console>
    </Appenders>
    <Loggers><Root level="info"><filters>
        <RegexFilter regex="(?s).*\$\{[^}]*\}.*" onMatch="DENY" onMismatch="NEUTRAL" />
    </filters></Root></Loggers>
</Configuration>"#;

        let patched = patch_console_layout(original).expect("XMLLayout present -> patched");
        assert!(patched.contains("<PatternLayout"));
        assert!(!patched.contains("XMLLayout"));
        // The mitigation filter is left untouched.
        assert!(patched.contains(r"(?s).*\$\{[^}]*\}.*"));
    }

    #[test]
    fn patch_console_layout_is_a_noop_without_xmllayout() {
        // A config that already logs plain text needs no patch.
        let plain = r#"<Console name="SysOut"><PatternLayout pattern="%msg%n" /></Console>"#;
        assert!(patch_console_layout(plain).is_none());
    }

    #[tokio::test]
    async fn virtual_tree_links_objects_to_readable_names() {
        let dir = std::env::temp_dir().join(format!("mc-assets-{}", std::process::id()));
        let assets_dir = dir.join("assets");
        let hash = "abcd1234ef567890abcd1234ef567890abcd1234";
        let object_path = assets_dir.join("objects").join("ab").join(hash);
        tokio::fs::create_dir_all(object_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&object_path, b"hello").await.unwrap();

        let index = parse(&format!(
            r#"{{ "virtual": true, "objects": {{ "lang/en.txt": {{ "hash": "{hash}", "size": 5 }} }} }}"#
        ));
        materialize_virtual_tree(&index, &assets_dir, "legacy").await.unwrap();

        let materialized: PathBuf = assets_dir.join("virtual").join("legacy").join("lang/en.txt");
        assert_eq!(tokio::fs::read(&materialized).await.unwrap(), b"hello");

        std::fs::remove_dir_all(&dir).ok();
    }

    // Wet: resolves the 1.7.10 log config against the LIVE Mojang server (verified
    // against its published sha1), then asserts the patch landed. Run with:
    //   cargo test -- --ignored
    #[tokio::test]
    #[ignore = "wet: needs network access to launcher.mojang.com"]
    async fn wet_ensure_log_config_downloads_and_patches_client_1_7() {
        use crate::download::DownloadOptions;
        use crate::model::patch::{Logging, LoggingFile};

        let dir = std::env::temp_dir().join(format!("mc-log4j-wet-{}", std::process::id()));
        let downloader = Downloader::new(DownloadOptions { jobs: 2, verify: true, dry_run: false })
            .expect("HTTP client builds");
        let logging = Logging {
            argument: Some("-Dlog4j.configurationFile=${path}".to_owned()),
            file: Some(LoggingFile {
                id: "client-1.7.xml".to_owned(),
                sha1: Some("50c9cc4af6d853d9fc137c84bcd153e2bd3a9a82".to_owned()),
                size: Some(966),
                url: Some(
                    "https://launcher.mojang.com/v1/objects/\
                     50c9cc4af6d853d9fc137c84bcd153e2bd3a9a82/client-1.7.xml"
                        .to_owned(),
                ),
            }),
            config_type: Some("log4j2-xml".to_owned()),
        };

        ensure_log_config(&downloader, &logging, &dir).await.unwrap();

        let written = tokio::fs::read_to_string(log_config_path(&dir, "client-1.7.xml"))
            .await
            .unwrap();
        assert!(written.contains("<PatternLayout"), "console swapped to PatternLayout");
        assert!(!written.contains("XMLLayout"), "no XMLLayout left for headless");
        assert!(written.contains(r"(?s).*\$\{[^}]*\}.*"), "JNDI mitigation kept");

        std::fs::remove_dir_all(&dir).ok();
    }
}
