//! The asset pipeline, reusing the one [`Downloader`] fetch path.
//!
//! Download the `assetIndex` -> `assets/indexes/<id>.json`, parse it, fetch each
//! object to `assets/objects/<ab>/<hash>`, and - for a legacy `virtual` /
//! `map_to_resources` index - materialize a readable `assets/virtual/<id>/<name>`
//! tree. Modern indexes use the plain object store only.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use log::{info, warn};
use serde::Deserialize;

use crate::download::Downloader;
use crate::model::artifact::{ArtifactRecord, Role};
use crate::model::patch::AssetIndexRef;

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
        sha1: asset_index.sha1.clone().filter(|hash| !hash.is_empty()),
        size: asset_index.size,
        local_path: index_path.clone(),
        role: Role::Asset,
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
                sha1: Some(object.hash.clone()),
                size: Some(object.size),
                local_path: objects_dir.join(prefix).join(&object.hash),
                role: Role::Asset,
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
}
