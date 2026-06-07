//! Fold the sorted component patches into one [`Profile`].
//!
//! The merge is pure and order-sensitive: callers pass patches in mmc-pack.json
//! declaration (array) order - that's what `load` returns, and what the launcher
//! actually folds in (the patch `order` field is informational; see `load`).
//! Accumulating fields preserve that order; "last-wins" fields take the value
//! from the last component in the sequence that sets them.

use log::{info, warn};

use crate::model::patch::{Arguments, Library, Patch};
use crate::model::profile::{GameArgs, Profile};
use crate::resolve::artifact_key;

/// Fold sorted `patches` into a merged [`Profile`].
#[must_use]
pub fn merge(patches: &[Patch]) -> Profile {
    let mut profile = Profile::default();
    for patch in patches {
        merge_patch(&mut profile, patch);
    }

    info!(
        "Merged profile: {} libraries, main class {}",
        profile.libraries.len(),
        profile.main_class.as_deref().unwrap_or("<unset>"),
    );
    profile
}

/// Fold a single component patch into the running profile.
fn merge_patch(profile: &mut Profile, patch: &Patch) {
    // Libraries accumulate: declared `libraries` first, then `+libraries`.
    profile.libraries.extend(patch.libraries.iter().cloned());
    profile.libraries.extend(patch.plus_libraries.iter().cloned());

    // `-libraries` removes previously-added libs by group:artifact (ignoring
    // version/classifier), applied after this patch's own additions.
    for removal in &patch.minus_libraries {
        remove_library(&mut profile.libraries, removal.name());
    }

    // mavenFiles accumulate, kept separate from the classpath libraries.
    profile.maven_files.extend(patch.maven_files.iter().cloned());

    // Last-wins fields: a later component in array order overrides.
    if let Some(main_class) = &patch.main_class {
        profile.main_class = Some(main_class.clone());
    }
    if let Some(main_jar) = &patch.main_jar {
        profile.main_jar = Some(main_jar.clone());
    }
    if let Some(asset_index) = &patch.asset_index {
        profile.asset_index = Some(asset_index.clone());
    }

    apply_game_args(profile, patch);

    // Pass-through accumulators (order preserved). +jvmArgs stay tokenized.
    profile.jvm_args.extend(patch.plus_jvm_args.iter().cloned());
    profile.tweakers.extend(patch.plus_tweakers.iter().cloned());
    profile.traits.extend(patch.plus_traits.iter().cloned());
    profile.agents.extend(patch.plus_agents.iter().cloned());

    // Collect Java-major hints, order-preserving and deduped.
    for major in &patch.compatible_java_majors {
        if !profile.compatible_java_majors.contains(major) {
            profile.compatible_java_majors.push(*major);
        }
    }

    // Dependency metadata: collected now, asserted by preflight.
    profile.requires.extend(patch.requires.iter().cloned());
    profile.conflicts.extend(patch.conflicts.iter().cloned());
}

/// Apply a patch's game arguments, tracking the active form. The legacy and
/// modern forms are never merged together: legacy is last-wins (replace), while
/// modern entries accumulate across components.
fn apply_game_args(profile: &mut Profile, patch: &Patch) {
    if let Some(legacy) = &patch.minecraft_arguments {
        if matches!(profile.game_args, GameArgs::Modern { .. }) {
            warn!(
                " - component {} sets legacy minecraftArguments, replacing the \
                 modern arguments form",
                patch.uid
            );
        }
        profile.game_args = GameArgs::Legacy(legacy.clone());
    }

    if let Some(Arguments { game, jvm }) = &patch.arguments {
        match &mut profile.game_args {
            GameArgs::Modern { game: into_game, jvm: into_jvm } => {
                into_game.extend(game.iter().cloned());
                into_jvm.extend(jvm.iter().cloned());
            }
            slot => {
                if matches!(slot, GameArgs::Legacy(_)) {
                    warn!(
                        " - component {} sets modern arguments, replacing the \
                         legacy minecraftArguments form",
                        patch.uid
                    );
                }
                *slot = GameArgs::Modern { game: game.clone(), jvm: jvm.clone() };
            }
        }
    }
}

/// Remove every library whose `group:artifact` matches `name`'s, preserving the
/// order of the survivors. (`artifact_key` is shared with `resolve`'s dedup.)
fn remove_library(libraries: &mut Vec<Library>, name: &str) {
    let target = artifact_key(name);
    libraries.retain(|library| artifact_key(&library.name) != target);
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::load::load_instance;
    use crate::model::patch::ArgEntry;

    /// Parse one patch from inline JSON (test convenience).
    fn patch(json: &str) -> Patch {
        serde_json::from_str(json).expect("inline patch fixture should parse")
    }

    #[test]
    fn main_class_is_last_in_array_order_not_by_order_field() {
        // merge folds in the given (mmc-pack array) order; the LAST component
        // that sets mainClass wins. The `order` field must NOT influence this -
        // "First" has the highest order yet loses; "Last" has a lower order yet
        // wins because it is last in the sequence.
        let patches = vec![
            patch(r#"{ "uid": "a", "order": 100, "mainClass": "First" }"#),
            patch(r#"{ "uid": "b", "order": -2, "mainClass": "Middle" }"#),
            patch(r#"{ "uid": "c", "order": 5, "mainClass": "Last" }"#),
        ];

        let profile = merge(&patches);
        assert_eq!(profile.main_class.as_deref(), Some("Last"));
    }

    #[test]
    fn libraries_accumulate_in_component_then_declared_order() {
        let patches = vec![
            patch(
                r#"{ "uid": "a", "order": 1,
                     "libraries": [ { "name": "g:one:1" }, { "name": "g:two:1" } ] }"#,
            ),
            patch(
                r#"{ "uid": "b", "order": 2,
                     "+libraries": [ { "name": "g:three:1" } ] }"#,
            ),
        ];

        let profile = merge(&patches);
        let names: Vec<&str> = profile.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["g:one:1", "g:two:1", "g:three:1"]);
    }

    #[test]
    fn minus_libraries_removes_by_group_artifact_ignoring_version() {
        let patches = vec![
            patch(
                r#"{ "uid": "a", "order": 1,
                     "libraries": [ { "name": "g:a:1.0" }, { "name": "g:b:2.0" } ] }"#,
            ),
            // Reference a different version to confirm version is ignored.
            patch(r#"{ "uid": "b", "order": 2, "-libraries": [ "g:a:9.9" ] }"#),
        ];

        let profile = merge(&patches);
        let names: Vec<&str> = profile.libraries.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["g:b:2.0"]);
    }

    #[test]
    fn minus_libraries_accepts_object_reference_form() {
        let patches = vec![
            patch(r#"{ "uid": "a", "order": 1, "libraries": [ { "name": "g:a:1.0" } ] }"#),
            patch(r#"{ "uid": "b", "order": 2, "-libraries": [ { "name": "g:a" } ] }"#),
        ];

        let profile = merge(&patches);
        assert!(profile.libraries.is_empty());
    }

    #[test]
    fn legacy_argument_form_parses_and_is_recorded() {
        let patches =
            vec![patch(r#"{ "uid": "a", "order": 1, "minecraftArguments": "--foo ${bar}" }"#)];

        let profile = merge(&patches);
        match profile.game_args {
            GameArgs::Legacy(args) => assert_eq!(args, "--foo ${bar}"),
            other => panic!("expected legacy form, got {other:?}"),
        }
    }

    #[test]
    fn modern_argument_form_parses_with_conditional_entries() {
        let patches = vec![patch(
            r#"{ "uid": "a", "order": 1, "arguments": {
                   "game": [ "--demo", { "rules": [ { "action": "allow" } ], "value": "x" } ],
                   "jvm":  [ "-cp", "${classpath}",
                             { "rules": [], "value": [ "-Da=b", "-Dc=d" ] } ]
                } }"#,
        )];

        let profile = merge(&patches);
        match profile.game_args {
            GameArgs::Modern { game, jvm } => {
                assert_eq!(game.len(), 2);
                assert!(matches!(&game[0], ArgEntry::Plain(s) if s == "--demo"));
                assert!(matches!(&game[1], ArgEntry::Conditional { .. }));
                // jvm tokens stay as authored, including the placeholder pair.
                assert!(matches!(&jvm[0], ArgEntry::Plain(s) if s == "-cp"));
                assert!(matches!(&jvm[1], ArgEntry::Plain(s) if s == "${classpath}"));
            }
            other => panic!("expected modern form, got {other:?}"),
        }
    }

    #[test]
    fn plus_jvm_args_stay_tokenized_verbatim() {
        let patches = vec![patch(
            r#"{ "uid": "a", "order": 1,
                 "+jvmArgs": [ "--add-opens", "java.base/java.io=ALL-UNNAMED" ] }"#,
        )];

        let profile = merge(&patches);
        // Two separate elements - never re-split or re-joined.
        assert_eq!(
            profile.jvm_args,
            ["--add-opens", "java.base/java.io=ALL-UNNAMED"]
        );
    }

    /// End-to-end fold of the bundled lwjgl3ify 1.7.10 instance variant
    /// (`example-files/lwjgl3ify-variant`, the one with full patches).
    #[test]
    fn example_files_fold_matches_expected_profile() {
        let patches = load_instance(Path::new("example-files/lwjgl3ify-variant"))
            .expect("bundled lwjgl3ify-variant should load");

        // Loaded in mmc-pack.json array order (NOT sorted by the `order` field):
        // org.lwjgl3 before net.minecraft, matching real Prism output.
        let order: Vec<&str> = patches.iter().map(|p| p.uid.as_str()).collect();
        assert_eq!(
            order,
            [
                "org.lwjgl3",
                "net.minecraft",
                "me.eigenraven.lwjgl3ify.forgepatches",
                "net.minecraftforge",
                "me.eigenraven.lwjgl3ify.launchargs",
            ]
        );

        let profile = merge(&patches);

        // main class: last-wins from launchargs (order 100).
        assert_eq!(
            profile.main_class.as_deref(),
            Some("com.gtnewhorizons.retrofuturabootstrap.MainStartOnFirstThread")
        );

        // Library count == sum of every component's libraries + +libraries,
        // and ordering: first is net.minecraft's first, last is forge's last.
        let expected_len: usize = patches
            .iter()
            .map(|p| p.libraries.len() + p.plus_libraries.len())
            .sum();
        assert_eq!(profile.libraries.len(), expected_len);
        // First lib is org.lwjgl3's first (array order); last is forge's guava 17.0.
        assert_eq!(
            profile.libraries.first().unwrap().name,
            "org.lwjgl:lwjgl-freetype-natives-freebsd:3.4.2-20260602.093430-9"
        );
        assert_eq!(profile.libraries.last().unwrap().name, "com.google.guava:guava:17.0");

        // Single tweaker from forge; trait from launchargs.
        assert_eq!(profile.tweakers, ["cpw.mods.fml.common.launcher.FMLTweaker"]);
        assert_eq!(profile.traits, ["FirstThreadOnMacOS"]);

        // Legacy arg form, from net.minecraft.
        assert!(matches!(profile.game_args, GameArgs::Legacy(_)));

        // +jvmArgs block from forgepatches, pre-tokenized.
        assert_eq!(profile.jvm_args.first().map(String::as_str), Some("-Dfile.encoding=UTF-8"));
        assert!(profile.jvm_args.iter().any(|a| a == "--add-opens"));

        // Java-major hints collected from net.minecraft.
        assert_eq!(profile.compatible_java_majors, [17, 21, 25, 26]);

        // asset index set by the Minecraft component.
        assert_eq!(profile.asset_index.as_ref().map(|a| a.id.as_str()), Some("1.7.10"));
    }
}
