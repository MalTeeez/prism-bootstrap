//! Assemble the final `java ...` argv from a resolved instance.
//!
//! This is where everything resolved so far should become the command: build the
//! classpath (target separator, not the host's), substitute both arg
//! forms against a dummy-auth + directories map, inject the heap, and append the
//! tweakers. The result is one `Vec<String>` with the java path at index 0,
//! ready for the emitter to write a token per line.
//!
//! Two correctness rules drive the shape: never double-add `-cp` or
//! `-Djava.library.path` - only add what the modern `arguments.jvm` didn't
//! already template. And we deliberately omit Prism-only extras.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use log::warn;

use crate::exit::FatalError;
use crate::model::artifact::{ArtifactRecord, Role};
use crate::model::patch::{ArgEntry, ArgValue, Patch};
use crate::model::profile::{GameArgs, Profile};
use crate::platform::Ctx;
use crate::resolve::maven_coordinate_to_path;
use crate::rules::allowed;

/// CLI-supplied knobs the assembly needs (heap, dummy auth, directories).
pub struct Config {
    /// The selected JDK (becomes argv[0]).
    pub java: PathBuf,
    /// `-Xms` value (e.g. `512m`).
    pub xms: String,
    /// `-Xmx` value (e.g. `6144m`).
    pub xmx: String,
    /// Extra JVM args injected verbatim right after `-Xmx`.
    pub jvm_args: Vec<String>,
    /// `${auth_player_name}`.
    pub username: String,
    /// `${auth_uuid}`.
    pub uuid: String,
    /// `${auth_access_token}`.
    pub access_token: String,
    /// `${user_type}`.
    pub user_type: String,
    /// `${game_directory}` (working dir for the launched game).
    pub game_dir: PathBuf,
    /// Adjust internals for a no-display context; never wraps in xvfb.
    pub headless: bool,
}

/// Preflight checks that can run before downloads.
///
/// Validates that there is a `mainClass` to launch and that every `requires` is
/// present (with a matching version when `equals` is set) and no `conflicts`
/// component is present. `components` is the loaded component list (uid+version).
///
/// # Errors
/// Returns a typed [`FatalError`] (-> a distinct exit code) when a constraint
/// fails.
pub fn preflight(profile: &Profile, components: &[Patch]) -> Result<()> {
    if profile.main_class.as_deref().unwrap_or("").is_empty() {
        return Err(FatalError::NoMainClass.into());
    }

    for required in &profile.requires {
        let present = components.iter().find(|component| component.uid == required.uid);
        match (present, &required.equals) {
            (None, _) => {
                return Err(FatalError::UnsatisfiedDeps {
                    reason: format!("required component {} is not present", required.uid),
                }
                .into());
            }
            // A pinned `equals` must match the present component's version.
            (Some(component), Some(wanted)) if component.version.as_deref() != Some(wanted) => {
                return Err(FatalError::UnsatisfiedDeps {
                    reason: format!(
                        "component {} is version {}, but {wanted} is required",
                        required.uid,
                        component.version.as_deref().unwrap_or("<unset>"),
                    ),
                }
                .into());
            }
            _ => {}
        }
    }

    for conflict in &profile.conflicts {
        if components.iter().any(|component| component.uid == conflict.uid) {
            return Err(FatalError::UnsatisfiedDeps {
                reason: format!("conflicting component {} is present", conflict.uid),
            }
            .into());
        }
    }

    Ok(())
}

/// Build the full launch argv (`[java, ...jvm, -cp, cp, mainClass, ...game]`).
///
/// # Errors
/// Returns [`FatalError::NoMainClass`] if no component set a main class, or an
/// error if an agent coordinate is malformed.
pub fn assemble(
    profile: &Profile,
    ctx: &Ctx,
    records: &[ArtifactRecord],
    instance: &Path,
    config: &Config,
) -> Result<Vec<String>> {
    let main_class = profile
        .main_class
        .as_deref()
        .filter(|class| !class.is_empty())
        .ok_or(FatalError::NoMainClass)?;

    let classpath = build_classpath(records, ctx);
    let has_native_extracts = records.iter().any(|record| record.role == Role::NativeExtract);
    let subs = build_subs(profile, ctx, instance, config, &classpath);

    let mut jvm = JvmAssembly::default();
    jvm.append_profile_args(profile, ctx, &subs);
    jvm.append_traits(profile, ctx);
    jvm.append_agents(profile, instance)?;
    jvm.inject_heap(config);
    jvm.inject_library_path(instance, has_native_extracts);
    jvm.inject_logging(profile, instance);
    jvm.inject_headless(instance, config);

    let game = build_game_args(profile, ctx, &subs);

    // [java] + jvm + (legacy -cp if the jvm args didn't template ${classpath})
    //        + [mainClass] + game .
    let mut argv = Vec::with_capacity(jvm.args.len() + game.len() + 4);
    argv.push(config.java.to_string_lossy().into_owned());
    argv.extend(jvm.args);
    if !jvm.classpath_templated {
        argv.push("-cp".to_owned());
        argv.push(classpath);
    }
    argv.push(main_class.to_owned());
    argv.extend(game);
    Ok(argv)
}

/// Join every classpath record's local path with the *target* separator (`;` on
/// windows, `:` otherwise), in resolved order. `MavenFile` and
/// `NativeExtract` records are excluded by role.
fn build_classpath(records: &[ArtifactRecord], ctx: &Ctx) -> String {
    let entries: Vec<String> = records
        .iter()
        .filter(|record| record.role == Role::Classpath)
        .map(|record| record.local_path.to_string_lossy().into_owned())
        .collect();
    entries.join(&ctx.path_sep.to_string())
}

/// Build the substitution map: dummy auth + computed directories + classpath.
/// Substitution is pure.
fn build_subs(
    profile: &Profile,
    ctx: &Ctx,
    instance: &Path,
    config: &Config,
    classpath: &str,
) -> HashMap<&'static str, String> {
    let version_name = version_name(profile);
    let assets_index_name = profile
        .asset_index
        .as_ref()
        .map_or_else(|| version_name.clone(), |index| index.id.clone());
    let assets_root = instance.join("assets");

    let mut subs = HashMap::new();
    subs.insert("auth_player_name", config.username.clone());
    subs.insert("version_name", version_name);
    subs.insert("game_directory", config.game_dir.to_string_lossy().into_owned());
    subs.insert("assets_root", assets_root.to_string_lossy().into_owned());
    // Legacy `${game_assets}` points at the virtual asset tree for this index.
    subs.insert(
        "game_assets",
        assets_root.join("virtual").join(&assets_index_name).to_string_lossy().into_owned(),
    );
    subs.insert("assets_index_name", assets_index_name);
    subs.insert("auth_uuid", config.uuid.clone());
    subs.insert("auth_access_token", config.access_token.clone());
    subs.insert("user_properties", "{}".to_owned());
    subs.insert("user_type", config.user_type.clone());
    subs.insert("classpath", classpath.to_owned());
    subs.insert("classpath_separator", ctx.path_sep.to_string());
    subs.insert("natives_directory", instance.join("natives").to_string_lossy().into_owned());
    subs.insert("library_directory", instance.join("libraries").to_string_lossy().into_owned());
    subs.insert("launcher_name", "prism-bootstrap".to_owned());
    subs.insert("launcher_version", env!("CARGO_PKG_VERSION").to_owned());
    subs
}

/// The Minecraft version for `${version_name}`: the main jar's coordinate
/// version, else the asset index id, else a placeholder.
fn version_name(profile: &Profile) -> String {
    if let Some(version) = profile.main_jar.as_ref().and_then(|jar| coordinate_version(&jar.name)) {
        return version.to_owned();
    }
    profile
        .asset_index
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |index| index.id.clone())
}

/// The version component (third segment) of a maven coordinate, if present.
fn coordinate_version(coordinate: &str) -> Option<&str> {
    coordinate.split(':').nth(2)
}

/// The accumulating JVM-argument vector plus the flags that gate the
/// no-double-add rules.
#[derive(Default)]
struct JvmAssembly {
    args: Vec<String>,
    /// A jvm arg already supplied `${classpath}` -> don't add a legacy `-cp`.
    classpath_templated: bool,
    /// A jvm arg already set `-Djava.library.path` -> don't add ours.
    library_path_set: bool,
}

impl JvmAssembly {
    /// `+jvmArgs` verbatim (pre-tokenized), then the modern
    /// `arguments.jvm` entries - rule-filtered and substituted.
    fn append_profile_args(&mut self, profile: &Profile, ctx: &Ctx, subs: &Subs) {
        // `+jvmArgs` pass through unchanged - never re-split or substituted.
        for arg in &profile.jvm_args {
            self.note_flags(arg);
            self.args.push(arg.clone());
        }
        if let GameArgs::Modern { jvm, .. } = &profile.game_args {
            for token in resolve_arg_entries(jvm, ctx, subs) {
                self.note_flags(&token);
                self.args.push(token);
            }
        }
    }

    /// Record whether an emitted token already templated the classpath or set
    /// the native library path, so we don't add them again.
    fn note_flags(&mut self, token: &str) {
        if token == "-cp" || token == "-classpath" || token.contains("${classpath}") {
            self.classpath_templated = true;
        }
        if token.starts_with("-Djava.library.path=") {
            self.library_path_set = true;
        }
    }

    /// Translate `+traits`. Only `FirstThreadOnMacOS` maps to a JVM flag,
    /// and only on osx; other known launcher traits are JVM no-ops; an unknown
    /// trait warns but never fails.
    fn append_traits(&mut self, profile: &Profile, ctx: &Ctx) {
        for trait_name in &profile.traits {
            match trait_name.as_str() {
                "FirstThreadOnMacOS" => {
                    if ctx.os_name == "osx" {
                        self.args.push("-XstartOnFirstThread".to_owned());
                    }
                }
                // Known launcher-internal traits with no JVM-arg effect.
                "noapplet" | "legacyServices" | "legacyLaunch" | "texturepacks"
                | "no-texturepacks" => {}
                other => warn!(" - ignoring unknown trait {other:?}"),
            }
        }
    }

    /// Translate `+agents`: each -> `-javaagent:<resolved local jar>[=opts]`.
    fn append_agents(&mut self, profile: &Profile, instance: &Path) -> Result<()> {
        let libraries_dir = instance.join("libraries");
        for agent in &profile.agents {
            // An agent coordinate may carry `=options` after the maven name.
            let (coordinate, options) = match agent.name.split_once('=') {
                Some((coordinate, options)) => (coordinate, Some(options)),
                None => (agent.name.as_str(), None),
            };
            let path = libraries_dir.join(maven_coordinate_to_path(coordinate)?);
            let mut arg = format!("-javaagent:{}", path.to_string_lossy());
            if let Some(options) = options {
                arg.push('=');
                arg.push_str(options);
            }
            self.args.push(arg);
        }
        Ok(())
    }

    /// Inject the tool-supplied heap (`-Xms`/`-Xmx`) - never from patches -
    /// followed by any user-supplied extra JVM args, kept in order.
    fn inject_heap(&mut self, config: &Config) {
        self.args.push(format!("-Xms{}", config.xms));
        self.args.push(format!("-Xmx{}", config.xmx));
        self.args.extend(config.jvm_args.iter().cloned());
    }

    /// Add `-Djava.library.path=<instance>/natives` unless the jvm args already
    /// set it, and only when there are extracted natives to point at.
    fn inject_library_path(&mut self, instance: &Path, has_native_extracts: bool) {
        if self.library_path_set || !has_native_extracts {
            return;
        }
        let natives = instance.join("natives");
        self.args.push(format!("-Djava.library.path={}", natives.to_string_lossy()));
    }

    /// Add the component's `logging.argument` as the `Log4Shell` mitigation arg,
    /// with `${path}` replaced by the downloaded (and patched) config's local path.
    /// A no-op unless the profile has a logging config with an `argument` and `file`.
    fn inject_logging(&mut self, profile: &Profile, instance: &Path) {
        let Some(logging) = &profile.logging else { return };
        let (Some(argument), Some(file)) = (&logging.argument, &logging.file) else {
            return;
        };
        let path = crate::assets::log_config_path(instance, &file.id);
        self.args.push(argument.replace("${path}", &path.to_string_lossy()));
    }

    /// `--headless` internals: pin LWJGL's native-extract dir to a
    /// writable instance path. The GL env hints go in `launch.env`, not here.
    fn inject_headless(&mut self, instance: &Path, config: &Config) {
        if !config.headless {
            return;
        }
        let lwjgl_natives = instance.join("lwjgl-natives");
        self.args.push(format!(
            "-Dorg.lwjgl.system.SharedLibraryExtractDirectory={}",
            lwjgl_natives.to_string_lossy()
        ));
    }
}

/// Build the game-argument vector: the active arg form substituted, then a
/// `--tweakClass <name>` pair per accumulated tweaker.
fn build_game_args(profile: &Profile, ctx: &Ctx, subs: &Subs) -> Vec<String> {
    let mut game = match &profile.game_args {
        // Legacy: split on whitespace, substitute each token.
        GameArgs::Legacy(arguments) => {
            arguments.split_whitespace().map(|token| substitute(token, subs)).collect()
        }
        // Modern: resolve the rule-gated entries.
        GameArgs::Modern { game, .. } => resolve_arg_entries(game, ctx, subs),
        GameArgs::None => Vec::new(),
    };
    for tweaker in &profile.tweakers {
        game.push("--tweakClass".to_owned());
        game.push(tweaker.clone());
    }
    game
}

/// Resolve a modern `arguments.{game,jvm}` list: include each entry only if its
/// rules allow `ctx`, substituting placeholders and flattening list values.
fn resolve_arg_entries(entries: &[ArgEntry], ctx: &Ctx, subs: &Subs) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        match entry {
            ArgEntry::Plain(token) => out.push(substitute(token, subs)),
            ArgEntry::Conditional { rules, value } => {
                if !allowed(rules, ctx) {
                    continue;
                }
                match value {
                    ArgValue::Single(token) => out.push(substitute(token, subs)),
                    ArgValue::Many(tokens) => {
                        out.extend(tokens.iter().map(|token| substitute(token, subs)));
                    }
                }
            }
        }
    }
    out
}

/// The substitution map alias (`${name}` -> value).
type Subs = HashMap<&'static str, String>;

/// Replace every `${name}` in `token` with its value from `subs`. Pure:
/// an unknown placeholder is left intact, and an unterminated `${` is emitted
/// verbatim.
fn substitute(token: &str, subs: &Subs) -> String {
    let mut out = String::with_capacity(token.len());
    let mut rest = token;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // No closing brace - emit the opener literally and stop scanning.
            out.push_str("${");
            rest = after;
            break;
        };
        let key = &after[..end];
        if let Some(value) = subs.get(key) {
            out.push_str(value);
        } else {
            // Leave an unrecognised placeholder untouched.
            out.push_str("${");
            out.push_str(key);
            out.push('}');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::load::load_instance;
    use crate::merge::merge;
    use crate::platform::{Platform, expand_platform};
    use crate::resolve::resolve;

    /// A minimal config pointing at a fake JDK, for assembly tests.
    fn test_config() -> Config {
        Config {
            java: PathBuf::from("/jdk/bin/java"),
            xms: "512m".to_owned(),
            xmx: "6144m".to_owned(),
            jvm_args: Vec::new(),
            username: "CI".to_owned(),
            uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
            access_token: "0".to_owned(),
            user_type: "legacy".to_owned(),
            game_dir: PathBuf::from("/inst/.minecraft"),
            headless: false,
        }
    }

    fn subs_with(pairs: &[(&'static str, &str)]) -> Subs {
        pairs.iter().map(|(key, value)| (*key, (*value).to_owned())).collect()
    }

    #[test]
    fn substitute_replaces_known_and_keeps_unknown() {
        let subs = subs_with(&[("classpath", "a:b"), ("version_name", "1.7.10")]);
        assert_eq!(substitute("${classpath}", &subs), "a:b");
        assert_eq!(substitute("-cp=${classpath}", &subs), "-cp=a:b");
        assert_eq!(substitute("--version ${version_name}", &subs), "--version 1.7.10");
        // Unknown placeholder is left intact.
        assert_eq!(substitute("${unknown}", &subs), "${unknown}");
        // Unterminated opener is emitted verbatim.
        assert_eq!(substitute("a ${oops", &subs), "a ${oops");
    }

    #[test]
    fn modern_jvm_arg_value_list_is_flattened_and_rule_gated() {
        let subs = subs_with(&[("classpath", "cp")]);
        let ctx = expand_platform(Platform::Linux);
        // A plain `-cp ${classpath}` pair, a list value, and an osx-only entry
        // that the linux ctx must drop.
        let entries: Vec<ArgEntry> = serde_json::from_str(
            r#"[ "-cp", "${classpath}",
                 { "rules": [ { "action": "allow" } ], "value": [ "-Da=b", "-Dc=d" ] },
                 { "rules": [ { "action": "allow", "os": { "name": "osx" } } ],
                   "value": "-XstartOnFirstThread" } ]"#,
        )
        .unwrap();
        let resolved = resolve_arg_entries(&entries, &ctx, &subs);
        assert_eq!(resolved, ["-cp", "cp", "-Da=b", "-Dc=d"]);
    }

    #[test]
    fn legacy_assembly_supplies_cp_and_appends_tweakclass() {
        // The lwjgl3ify variant: legacy args, launcher supplies the classpath,
        // FMLTweaker appended, dummy auth substituted, heap injected.
        let patches = load_instance(Path::new("example-files/lwjgl3ify-variant")).unwrap();
        let profile = merge(&patches);
        let ctx = expand_platform(Platform::Linux);
        let instance = Path::new("/inst");
        let records = resolve(&profile, &ctx, instance).unwrap();
        let argv = assemble(&profile, &ctx, &records, instance, &test_config()).unwrap();

        assert_eq!(argv[0], "/jdk/bin/java");
        // Heap injected by the tool.
        assert!(argv.iter().any(|arg| arg == "-Xms512m"));
        assert!(argv.iter().any(|arg| arg == "-Xmx6144m"));
        // Legacy path: we supplied `-cp` (it was not templated by jvm args).
        let cp_index = argv.iter().position(|arg| arg == "-cp").expect("-cp present");
        let classpath = &argv[cp_index + 1];
        // Main jar last on the classpath, joined with ':' for a linux target.
        assert!(classpath.contains(':'));
        assert!(classpath.ends_with("versions/1.7.10/1.7.10.jar"));
        // Main class follows the classpath.
        assert_eq!(argv[cp_index + 2], "com.gtnewhorizons.retrofuturabootstrap.MainStartOnFirstThread");
        // +jvmArgs block present, verbatim and before the heap.
        assert!(argv.iter().any(|arg| arg == "-Dfile.encoding=UTF-8"));
        // Dummy auth + tweaker in the game args.
        assert!(argv.windows(2).any(|pair| pair == ["--username", "CI"]));
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["--tweakClass", "cpw.mods.fml.common.launcher.FMLTweaker"])
        );
        // No native library path on linux only if there were extracts - there is
        // one (jinput), so it must be present.
        assert!(argv.iter().any(|arg| arg.starts_with("-Djava.library.path=")));

        // The MMC-hint:local forgePatches jar is assume-local (no url) but still
        // belongs on the classpath since it must appear on -cp,
        // flat under libraries/.
        assert!(
            classpath.contains("libraries/lwjgl3ify-3.0.23-forgePatches.jar"),
            "forgePatches (no-url local) must still be on the classpath: {classpath}"
        );
    }

    #[test]
    fn extra_jvm_args_follow_xmx_in_order() {
        let patches = load_instance(Path::new("example-files/lwjgl3ify-variant")).unwrap();
        let profile = merge(&patches);
        let ctx = expand_platform(Platform::Linux);
        let instance = Path::new("/inst");
        let records = resolve(&profile, &ctx, instance).unwrap();

        let mut config = test_config();
        config.jvm_args = vec!["-XX:+UseG1GC".to_owned(), "-Dfoo=bar".to_owned()];
        let argv = assemble(&profile, &ctx, &records, instance, &config).unwrap();

        // The two extra args sit directly after `-Xmx`, preserving CLI order.
        let xmx_index = argv.iter().position(|arg| arg == "-Xmx6144m").unwrap();
        assert_eq!(&argv[xmx_index + 1..xmx_index + 3], ["-XX:+UseG1GC", "-Dfoo=bar"]);
    }

    #[test]
    fn windows_target_joins_classpath_with_semicolon() {
        let patches = load_instance(Path::new("example-files/lwjgl3ify-variant")).unwrap();
        let profile = merge(&patches);
        let ctx = expand_platform(Platform::Windows);
        let instance = Path::new("/inst");
        let records = resolve(&profile, &ctx, instance).unwrap();
        let argv = assemble(&profile, &ctx, &records, instance, &test_config()).unwrap();

        let cp_index = argv.iter().position(|arg| arg == "-cp").unwrap();
        // `;`-joined classpath even on a (linux) host - target separator.
        assert!(argv[cp_index + 1].contains(';'));
    }

    #[test]
    fn first_thread_trait_adds_flag_only_on_osx() {
        let patches = load_instance(Path::new("example-files/lwjgl3ify-variant")).unwrap();
        let profile = merge(&patches);
        let instance = Path::new("/inst");

        // The variant carries the FirstThreadOnMacOS trait.
        let osx = expand_platform(Platform::Osx);
        let osx_records = resolve(&profile, &osx, instance).unwrap();
        let osx_argv = assemble(&profile, &osx, &osx_records, instance, &test_config()).unwrap();
        assert!(osx_argv.iter().any(|arg| arg == "-XstartOnFirstThread"));

        // Not on linux.
        let linux = expand_platform(Platform::Linux);
        let linux_records = resolve(&profile, &linux, instance).unwrap();
        let linux_argv =
            assemble(&profile, &linux, &linux_records, instance, &test_config()).unwrap();
        assert!(!linux_argv.iter().any(|arg| arg == "-XstartOnFirstThread"));
    }

    #[test]
    fn headless_pins_lwjgl_extract_dir() {
        let patches = load_instance(Path::new("example-files/lwjgl3ify-variant")).unwrap();
        let profile = merge(&patches);
        let ctx = expand_platform(Platform::Linux);
        let instance = Path::new("/inst");
        let records = resolve(&profile, &ctx, instance).unwrap();
        let mut config = test_config();
        config.headless = true;
        let argv = assemble(&profile, &ctx, &records, instance, &config).unwrap();
        assert!(
            argv.iter()
                .any(|arg| arg.starts_with("-Dorg.lwjgl.system.SharedLibraryExtractDirectory="))
        );
    }

    #[test]
    fn logging_config_arg_points_at_the_asset_store_path() {
        use crate::model::patch::{Logging, LoggingFile};

        // A profile carrying a log4j config: assembly must emit the argument with
        // `${path}` -> the on-disk config path the downloader uses.
        let profile = Profile {
            main_class: Some("Main".to_owned()),
            logging: Some(Logging {
                argument: Some("-Dlog4j.configurationFile=${path}".to_owned()),
                file: Some(LoggingFile {
                    id: "client-1.7.xml".to_owned(),
                    sha1: None,
                    size: None,
                    url: None,
                }),
                config_type: Some("log4j2-xml".to_owned()),
            }),
            ..Default::default()
        };
        let ctx = expand_platform(Platform::Linux);
        let argv = assemble(&profile, &ctx, &[], Path::new("/inst"), &test_config()).unwrap();

        assert!(
            argv.iter()
                .any(|arg| arg == "-Dlog4j.configurationFile=/inst/assets/log_configs/client-1.7.xml"),
            "expected the substituted log4j arg, got: {argv:?}"
        );
    }

    #[test]
    fn no_logging_config_emits_no_log4j_arg() {
        // A profile without a logging block must not invent a log4j arg.
        let profile = Profile {
            main_class: Some("Main".to_owned()),
            ..Default::default()
        };
        let ctx = expand_platform(Platform::Linux);
        let argv = assemble(&profile, &ctx, &[], Path::new("/inst"), &test_config()).unwrap();
        assert!(!argv.iter().any(|arg| arg.contains("log4j.configurationFile")));
    }

    #[test]
    fn preflight_rejects_missing_main_class() {
        let mut profile = Profile::default();
        assert!(preflight(&profile, &[]).is_err());
        profile.main_class = Some("Main".to_owned());
        assert!(preflight(&profile, &[]).is_ok());
    }
}
