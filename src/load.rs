//! Read an instance's `mmc-pack.json` and every `patches/<uid>.json`, then
//! return the patches sorted ascending by `order` ready for `merge`.
//!
//! Pure input-reading: no network, no writes. A missing patch for a listed uid
//! is a hard error here - resolving it from `meta.prismlauncher.org` is out of
//! scope.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use log::{info, warn};

use crate::model::pack::Pack;
use crate::model::patch::Patch;

/// Load an instance directory into the sorted list of its component patches.
///
/// # Errors
/// Returns an error if `mmc-pack.json` is missing/unparseable, or if any listed
/// component has no `patches/<uid>.json` (the path is named in the message).
pub fn load_instance(instance_dir: &Path) -> Result<Vec<Patch>> {
    info!("Loading instance from {}", instance_dir.display());

    let pack = read_pack(instance_dir.join("mmc-pack.json").as_path())?;
    info!(" - manifest lists {} component(s)", pack.components.len());

    let mut patches = Vec::with_capacity(pack.components.len());
    for component in &pack.components {
        let patch_path = instance_dir
            .join("patches")
            .join(format!("{}.json", component.uid));
        info!(" - reading component {}", component.uid);
        patches.push(read_patch(&patch_path, &component.uid)?);
    }

    // Stable sort: ties keep manifest declaration order.
    patches.sort_by_key(|patch| patch.order);
    info!("Loaded {} component(s)", patches.len());
    Ok(patches)
}

/// Read and parse `mmc-pack.json`.
fn read_pack(path: &Path) -> Result<Pack> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("reading pack manifest {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("parsing pack manifest {}", path.display()))
}

/// Read and parse one `patches/<uid>.json`, warning on any unmodeled fields.
fn read_patch(path: &Path, uid: &str) -> Result<Patch> {
    if !path.exists() {
        bail!(
            "patch file for component '{uid}' not found at {} \
             (resolving it from meta.prismlauncher.org is out of scope)",
            path.display()
        );
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("reading patch file {}", path.display()))?;
    let parsed: Patch = serde_json::from_str(&content)
        .with_context(|| format!("parsing patch file {}", path.display()))?;

    // Permissive parse: unknown fields warn rather than fail.
    if !parsed.extra.is_empty() {
        let mut keys: Vec<&str> = parsed.extra.keys().map(String::as_str).collect();
        keys.sort_unstable();
        warn!(" - unknown field(s) in {}: {}", path.display(), keys.join(", "));
    }

    Ok(parsed)
}
