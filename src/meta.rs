//! Resolve pack-only components from `meta.prismlauncher.org`.
//! 4.5).
//!
//! A "pack-only" instance lists components by `uid`+`version` but ships no
//! `patches/`. For each such gap [`resolve_components`] fetches the component's
//! version file - `<base>/<uid>/<version>.json`, which *is* our [`Patch`] schema
//! - and slots it at the component's array position so `merge` is untouched.
//!
//! Meta resolution is opt-in: without a `--meta-url` base, a gap is a
//! fail-fast with a provisioning hint (mirrors the no-url-library rule).
//! It is input acquisition, not artifact download, so it runs regardless of
//! `--dry-run`/`--no-verify`, and it never resolves "latest" - only the version
//! the pack pins.

use std::path::PathBuf;

use anyhow::Result;
use log::info;

use crate::download::Fetcher;
use crate::exit::FatalError;
use crate::load::{Component, warn_unknown_patch_fields};
use crate::model::patch::Patch;

/// Resolve every [`Component::Missing`] gap into a [`Patch`], returning all
/// components as patches in their original (array) order, ready for `merge`.
///
/// With no gaps this is a pure pass-through (no network; `meta_url` unused). With
/// gaps and `meta_url == None` it fails with [`FatalError::MissingComponent`].
///
/// # Errors
/// [`FatalError::MissingComponent`] when a gap can't be resolved (no `--meta-url`
/// given, or the pack pinned no version), or [`FatalError::MetaResolveFailed`]
/// when a version file fails to fetch or parse.
pub async fn resolve_components(
    components: Vec<Component>,
    fetcher: &impl Fetcher,
    meta_url: Option<&str>,
) -> Result<Vec<Patch>> {
    let gaps = components.iter().filter(|component| matches!(component, Component::Missing { .. })).count();
    if gaps == 0 {
        // No pack-only components: return the local patches untouched, no network.
        return Ok(local_patches(components));
    }

    let Some(base) = meta_url else {
        // Gaps exist but meta wasn't requested: fail on the first one with a hint.
        return Err(missing_component_error(components));
    };

    info!("Resolving {gaps} missing component(s) from {base}");
    let mut patches = Vec::with_capacity(components.len());
    for component in components {
        match component {
            Component::Local(patch) => patches.push(*patch),
            Component::Missing { uid, version, patch_path } => {
                patches.push(resolve_one(fetcher, base, uid, version, patch_path).await?);
            }
        }
    }
    info!("Resolved {gaps} component(s) from meta");
    Ok(patches)
}

/// Fetch + parse one missing component's version file from the meta server.
async fn resolve_one(
    fetcher: &impl Fetcher,
    base: &str,
    uid: String,
    version: Option<String>,
    patch_path: PathBuf,
) -> Result<Patch> {
    // Without a pinned version we can't fetch a specific file, and we never
    // resolve "latest"/recommended - treat it as still-missing.
    let Some(version) = version else {
        return Err(FatalError::MissingComponent { uid, version: None, patch_path }.into());
    };

    info!(" - resolving {uid} {version} from meta");
    let url = meta_url_for(base, &uid, &version);
    let label = format!("{uid} {version}");
    let bytes = fetcher.fetch_bytes(&url, &label).await.map_err(|error| {
        // The fetch message records the last failure, so a 404 reads as
        // "HTTP 404 ..." and a transport error as "request error: ...".
        FatalError::MetaResolveFailed {
            uid: uid.clone(),
            version: version.clone(),
            reason: format!("{error:#}"),
        }
    })?;
    patch_from_bytes(&bytes, &uid, &version)
}

/// Build the version-file URL `<base>/<uid>/<version>.json`, tolerating a
/// trailing slash on `base`.
#[must_use]
pub fn meta_url_for(base: &str, uid: &str, version: &str) -> String {
    format!("{}/{uid}/{version}.json", base.trim_end_matches('/'))
}

/// Parse meta version-file bytes into a [`Patch`], warning on unmodeled fields
/// exactly as the local reader does.
///
/// # Errors
/// [`FatalError::MetaResolveFailed`] if the bytes are not valid component JSON.
pub fn patch_from_bytes(bytes: &[u8], uid: &str, version: &str) -> Result<Patch> {
    let patch: Patch = serde_json::from_slice(bytes).map_err(|error| FatalError::MetaResolveFailed {
        uid: uid.to_owned(),
        version: version.to_owned(),
        reason: format!("the meta version file is not valid component JSON: {error}"),
    })?;
    warn_unknown_patch_fields(&patch, &format!("meta {uid} {version}"));
    Ok(patch)
}

/// Unwrap an all-local component list into patches (the no-gap fast path).
fn local_patches(components: Vec<Component>) -> Vec<Patch> {
    components
        .into_iter()
        .filter_map(|component| match component {
            Component::Local(patch) => Some(*patch),
            // Unreachable: this is only called when there are no gaps.
            Component::Missing { .. } => None,
        })
        .collect()
}

/// Build the [`FatalError::MissingComponent`] for the first gap (the no-meta-url
/// path). The hint - provide the file, or pass `--meta-url` - lives in its
/// `Display`.
fn missing_component_error(components: Vec<Component>) -> anyhow::Error {
    for component in components {
        if let Component::Missing { uid, version, patch_path } = component {
            return FatalError::MissingComponent { uid, version, patch_path }.into();
        }
    }
    // Only called when at least one gap exists.
    anyhow::anyhow!("internal error: expected a missing component but found none")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::exit::{ExitCode, exit_code_for};

    /// A `Fetcher` backed by a fixed URL->bytes map; never touches the network so
    /// `cargo test` stays offline. An unknown URL yields a 404-shaped error.
    struct RecordedFetcher {
        responses: HashMap<String, Vec<u8>>,
    }

    impl Fetcher for RecordedFetcher {
        async fn fetch_bytes(&self, url: &str, _label: &str) -> Result<Vec<u8>> {
            self.responses
                .get(url)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("HTTP 404 (no recorded response for {url})"))
        }
    }

    fn local(uid: &str, main_class: &str) -> Component {
        Component::Local(Box::new(Patch {
            uid: uid.to_owned(),
            main_class: Some(main_class.to_owned()),
            ..Patch::default()
        }))
    }

    fn missing(uid: &str, version: Option<&str>) -> Component {
        Component::Missing {
            uid: uid.to_owned(),
            version: version.map(str::to_owned),
            patch_path: PathBuf::from(format!("/inst/patches/{uid}.json")),
        }
    }

    #[test]
    fn meta_url_for_joins_with_and_without_trailing_slash() {
        let expected = "https://meta/v1/net.minecraft/1.7.10.json";
        assert_eq!(meta_url_for("https://meta/v1/", "net.minecraft", "1.7.10"), expected);
        assert_eq!(meta_url_for("https://meta/v1", "net.minecraft", "1.7.10"), expected);
    }

    #[test]
    fn patch_from_bytes_parses_a_version_file() {
        let json = br#"{"uid":"net.fabricmc.fabric-loader","version":"0.19.3",
            "order":10,"mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient"}"#;
        let patch = patch_from_bytes(json, "net.fabricmc.fabric-loader", "0.19.3").unwrap();
        assert_eq!(patch.uid, "net.fabricmc.fabric-loader");
        assert_eq!(patch.main_class.as_deref(), Some("net.fabricmc.loader.impl.launch.knot.KnotClient"));
    }

    #[test]
    fn patch_from_bytes_rejects_bad_json() {
        let error = patch_from_bytes(b"not json", "x", "1").unwrap_err();
        assert_eq!(exit_code_for(&error), ExitCode::MetaResolveFailed);
    }

    #[tokio::test]
    async fn passes_through_when_all_local_without_meta_url() {
        let components = vec![local("a", "A.Main"), local("b", "B.Main")];
        let fetcher = RecordedFetcher { responses: HashMap::new() };
        let patches = resolve_components(components, &fetcher, None).await.unwrap();
        let mains: Vec<_> = patches.iter().map(|p| p.main_class.as_deref().unwrap()).collect();
        assert_eq!(mains, ["A.Main", "B.Main"]);
    }

    #[tokio::test]
    async fn gap_without_meta_url_is_missing_component() {
        let components = vec![local("a", "A.Main"), missing("b", Some("1.0"))];
        let fetcher = RecordedFetcher { responses: HashMap::new() };
        let error = resolve_components(components, &fetcher, None).await.unwrap_err();
        assert_eq!(exit_code_for(&error), ExitCode::MissingComponent);
    }

    #[tokio::test]
    async fn gap_with_no_pinned_version_is_missing_component() {
        let components = vec![missing("b", None)];
        let fetcher = RecordedFetcher { responses: HashMap::new() };
        let error = resolve_components(components, &fetcher, Some("https://meta/v1"))
            .await
            .unwrap_err();
        assert_eq!(exit_code_for(&error), ExitCode::MissingComponent);
    }

    #[tokio::test]
    async fn unreachable_meta_url_is_meta_resolve_failed() {
        let components = vec![missing("b", Some("1.0"))];
        let fetcher = RecordedFetcher { responses: HashMap::new() }; // no recorded URL -> 404
        let error = resolve_components(components, &fetcher, Some("https://meta/v1"))
            .await
            .unwrap_err();
        assert_eq!(exit_code_for(&error), ExitCode::MetaResolveFailed);
    }

    #[tokio::test]
    async fn resolves_gap_and_slots_it_in_array_order() {
        // [Local(a), Missing(b), Local(c)] -> b fetched from meta, slotted between.
        let url = meta_url_for("https://meta/v1", "b", "1.0");
        let body = br#"{"uid":"b","version":"1.0","mainClass":"B.Main"}"#.to_vec();
        let fetcher = RecordedFetcher { responses: HashMap::from([(url, body)]) };

        let components = vec![local("a", "A.Main"), missing("b", Some("1.0")), local("c", "C.Main")];
        let patches = resolve_components(components, &fetcher, Some("https://meta/v1"))
            .await
            .unwrap();

        let uids: Vec<_> = patches.iter().map(|p| p.uid.as_str()).collect();
        assert_eq!(uids, ["a", "b", "c"], "order is preserved by array index");
        assert_eq!(patches[1].main_class.as_deref(), Some("B.Main"), "the gap was filled from meta");
    }

    // ---- Wet (real-network) tests --------------------------------------
    // These resolve a pack-only fixture against the LIVE Prism meta server
    // (every pinned version verified to exist). Excluded from the default run;
    // execute with:  cargo test -- --ignored
    const LIVE_META: &str = "https://meta.prismlauncher.org/v1/";

    /// A real `Downloader` (no verify/dry-run) used as the live `Fetcher`.
    fn live_fetcher() -> crate::download::Downloader {
        crate::download::Downloader::new(crate::download::DownloadOptions {
            jobs: 4,
            verify: false,
            dry_run: false,
        })
        .expect("HTTP client builds")
    }

    /// Resolve a fixture's components live and assert it merges to a launchable
    /// profile (a main class) with one patch per pack component.
    async fn assert_variant_resolves(variant: &str, components: usize) {
        let loaded = crate::load::load_components(std::path::Path::new(variant)).unwrap();
        let patches = resolve_components(loaded, &live_fetcher(), Some(LIVE_META))
            .await
            .unwrap_or_else(|error| panic!("{variant} should resolve from live meta: {error:#}"));
        assert_eq!(patches.len(), components, "one patch per pack component");
        let profile = crate::merge::merge(&patches);
        assert!(profile.main_class.is_some(), "merged profile has a main class");
    }

    #[tokio::test]
    #[ignore = "wet: needs network access to meta.prismlauncher.org"]
    async fn wet_resolves_old_mc_variant_legacy() {
        // 1.7.10 + Forge on LWJGL2 - three pack-only components.
        assert_variant_resolves("example-files/old-mc-variant", 3).await;
    }

    #[tokio::test]
    #[ignore = "wet: needs network access to meta.prismlauncher.org"]
    async fn wet_resolves_modern_fabric_variant() {
        // Modern vanilla + Fabric loader stack - four pack-only components.
        assert_variant_resolves("example-files/modern-fabric-variant", 4).await;
    }
}
