//! Read an instance's `mmc-pack.json` and every `patches/<uid>.json`.
//!
//! Pure input-reading: no network, no writes. [`load_components`] returns one
//! [`Component`] per pack entry, in `mmc-pack.json` array order, marking any
//! component whose `patches/<uid>.json` is absent as [`Component::Missing`] so a
//! later step (phase 4.5 `meta`) can resolve it from `meta.prismlauncher.org`.
//! [`load_instance`] is the all-local convenience wrapper: it errors on the
//! first missing patch.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use log::{info, warn};

use crate::model::pack::Pack;
use crate::model::patch::Patch;

/// One pack component after the local read.
///
/// Either its `patches/<uid>.json` was on disk and parsed, or it is missing and
/// must be resolved elsewhere - in which case we carry what the resolver needs
/// (`uid` + pinned `version`) and the path we expected the patch at, so the
/// resolver or the error can name it.
pub enum Component {
    /// `patches/<uid>.json` existed and parsed. Boxed: a `Patch` is far larger
    /// than the `Missing` variant, so boxing keeps the enum (and `Vec`) compact.
    Local(Box<Patch>),
    /// No local patch; defer to the meta resolver (phase 4.5) or fail.
    Missing { uid: String, version: Option<String>, patch_path: PathBuf },
}

/// Read an instance directory into one [`Component`] per `mmc-pack.json` entry,
/// preserving array order. A missing `patches/<uid>.json` becomes
/// [`Component::Missing`] rather than an error - resolving it (or failing) is the
/// caller's decision.
///
/// # Errors
/// Returns an error if `mmc-pack.json` is missing/unparseable, or if a *present*
/// `patches/<uid>.json` cannot be read or parsed.
pub fn load_components(instance_dir: &Path) -> Result<Vec<Component>> {
    info!("Loading instance from {}", instance_dir.display());

    let pack = read_pack(instance_dir.join("mmc-pack.json").as_path())?;
    info!(" - manifest lists {} component(s)", pack.components.len());

    let mut components = Vec::with_capacity(pack.components.len());
    for component in &pack.components {
        let patch_path = instance_dir
            .join("patches")
            .join(format!("{}.json", component.uid));
        if patch_path.exists() {
            info!(" - reading component {}", component.uid);
            components.push(Component::Local(Box::new(read_patch_file(&patch_path)?)));
        } else {
            // No local patch: defer to the meta resolver (phase 4.5). We keep
            // mmc-pack array order - that is what merge folds in, NOT the `order`
            // field (see the phase-1 decisions log).
            info!(" - component {} has no local patch", component.uid);
            components.push(Component::Missing {
                uid: component.uid.clone(),
                version: component.version.clone(),
                patch_path,
            });
        }
    }

    info!("Loaded {} component(s)", components.len());
    Ok(components)
}

/// Load an instance whose patches are all local, returning them in array
/// order. A missing patch is an error here; callers that support meta resolution
/// use [`load_components`] plus the `meta` resolver instead.
///
/// # Errors
/// As [`load_components`], plus an error naming the first component that has no
/// local `patches/<uid>.json`.
// Only the test suites load a guaranteed-all-local instance directly; the binary
// always goes through `load_components` + the `meta` resolver.
#[cfg(test)]
pub fn load_instance(instance_dir: &Path) -> Result<Vec<Patch>> {
    load_components(instance_dir)?
        .into_iter()
        .map(|component| match component {
            Component::Local(patch) => Ok(*patch),
            Component::Missing { uid, patch_path, .. } => Err(anyhow::anyhow!(
                "component '{uid}' has no local patch at {} \
                 (pass --meta-url to resolve it from the meta server)",
                patch_path.display()
            )),
        })
        .collect()
}

/// Warn (don't fail) about any patch fields we don't model. Shared
/// by the local reader here and the meta resolver so both report identically.
pub(crate) fn warn_unknown_patch_fields(patch: &Patch, source: &str) {
    if patch.extra.is_empty() {
        return;
    }
    let mut keys: Vec<&str> = patch.extra.keys().map(String::as_str).collect();
    keys.sort_unstable();
    warn!(" - unknown field(s) in {source}: {}", keys.join(", "));
}

/// Read and parse `mmc-pack.json`.
fn read_pack(path: &Path) -> Result<Pack> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("reading pack manifest {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("parsing pack manifest {}", path.display()))
}

/// Read and parse one existing `patches/<uid>.json`, warning on unmodeled fields.
fn read_patch_file(path: &Path) -> Result<Patch> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("reading patch file {}", path.display()))?;
    let parsed: Patch = serde_json::from_str(&content)
        .with_context(|| format!("parsing patch file {}", path.display()))?;

    warn_unknown_patch_fields(&parsed, &path.display().to_string());
    Ok(parsed)
}
