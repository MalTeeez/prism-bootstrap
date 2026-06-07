//! `mmc-pack.json` - the ordered list of components in an instance.
//!
//! We only read `components[].uid` here to know which `patches/<uid>.json`
//! files to load; the merge order is the mmc-pack array order, not the patch
//! `order` field.

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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// Parse the `mmc-pack.json` of one `example-files/<variant>`.
    fn parse_variant(variant: &str) -> Pack {
        let path = Path::new("example-files").join(variant).join("mmc-pack.json");
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }

    /// The component uids of a pack, in declaration order.
    fn uids(pack: &Pack) -> Vec<&str> {
        pack.components.iter().map(|c| c.uid.as_str()).collect()
    }

    /// Find a component by uid (every fixture uid is unique).
    fn component<'a>(pack: &'a Pack, uid: &str) -> &'a PackComponent {
        pack.components
            .iter()
            .find(|c| c.uid == uid)
            .unwrap_or_else(|| panic!("component {uid} missing"))
    }

    #[test]
    fn lwjgl3ify_variant_pack_parses() {
        // The legacy-args GTNH example (LWJGL3 swap), the only variant with
        // full patches; the end-to-end fold lives in `merge`.
        let pack = parse_variant("lwjgl3ify-variant");
        assert_eq!(pack.format_version, Some(1));
        assert_eq!(
            uids(&pack),
            [
                "org.lwjgl3",
                "net.minecraft",
                "me.eigenraven.lwjgl3ify.forgepatches",
                "net.minecraftforge",
                "me.eigenraven.lwjgl3ify.launchargs",
            ]
        );
    }

    #[test]
    fn modern_mc_variant_pack_parses() {
        // Plain modern vanilla: just LWJGL3 + Minecraft.
        let pack = parse_variant("modern-mc-variant");
        assert_eq!(uids(&pack), ["org.lwjgl3", "net.minecraft"]);
        assert_eq!(component(&pack, "org.lwjgl3").dependency_only, Some(true));
        assert_eq!(component(&pack, "net.minecraft").important, Some(true));
    }

    #[test]
    fn modern_fabric_variant_pack_parses() {
        // Modern vanilla + a Fabric loader stack (intermediary mappings +
        // loader). Exercises dependency-only loader components.
        let pack = parse_variant("modern-fabric-variant");
        assert_eq!(
            uids(&pack),
            [
                "org.lwjgl3",
                "net.minecraft",
                "net.fabricmc.intermediary",
                "net.fabricmc.fabric-loader",
            ]
        );
        assert_eq!(
            component(&pack, "net.fabricmc.intermediary").dependency_only,
            Some(true)
        );
        // The loader itself is a real (non-dependency-only) component.
        assert_eq!(
            component(&pack, "net.fabricmc.fabric-loader").dependency_only,
            None
        );
    }

    #[test]
    fn old_mc_variant_pack_parses_with_lwjgl2() {
        // Legacy 1.7.10 + Forge on LWJGL2 (`org.lwjgl`, not `org.lwjgl3`) -
        // the classic-natives-extract instance type.
        let pack = parse_variant("old-mc-variant");
        assert_eq!(uids(&pack), ["org.lwjgl", "net.minecraft", "net.minecraftforge"]);
        assert_eq!(component(&pack, "org.lwjgl").dependency_only, Some(true));
    }
}
