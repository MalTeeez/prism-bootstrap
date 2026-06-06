//! `mmc-pack.json` - the ordered list of components in an instance.
//!
//! We only read `components[].uid` here to know which `patches/<uid>.json`
//! files to load; the authoritative merge order is each patch's own `order`.

use serde::Deserialize;

/// The whole `mmc-pack.json` manifest.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Pack {
    /// Manifest format version (currently `1`); kept for completeness.
    #[serde(default)]
    pub format_version: Option<u32>,
    /// The components making up this instance, in declaration order.
    #[serde(default)]
    pub components: Vec<PackComponent>,
}

/// One `components[]` entry: a reference to a patch file plus UI/cache hints.
///
/// Only `uid` is required; all `cached*` and flag fields are optional and
/// tolerated (unknown fields are ignored, never fatal).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PackComponent {
    /// Component id -> `patches/<uid>.json`.
    pub uid: String,
    /// Pinned version of the component (display/diagnostics only).
    #[serde(default)]
    pub version: Option<String>,
    /// Optional UI flag marking the "important" component.
    #[serde(default)]
    pub important: Option<bool>,
    /// Optional flag: pulled in only as a dependency of another component.
    #[serde(default)]
    pub dependency_only: Option<bool>,
}
