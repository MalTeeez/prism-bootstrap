//! `patches/<uid>.json` - one component definition.
//!
//! This module is pure data: every type here is serde-derived and models the
//! component schema. Deserialization is deliberately permissive - we
//! never `deny_unknown_fields`, so an unfamiliar key warns (via the `extra`
//! catch-all on [`Patch`]) rather than failing the parse.
//!
//! Classifying libraries into their roles (plain-cp, native, maven-file, ...)
//! is `resolve`'s job; here we only need every shape to deserialize.

use std::collections::HashMap;

use serde::Deserialize;

/// One parsed `patches/<uid>.json`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Patch {
    /// Component id (matches the pack component and the file name).
    #[serde(default)]
    pub uid: String,
    /// Display name (diagnostics only).
    #[serde(default)]
    pub name: Option<String>,
    /// Pinned component version.
    #[serde(default)]
    pub version: Option<String>,
    /// Merge order; components are folded ascending by this value.
    #[serde(default)]
    pub order: i64,

    /// Entrypoint; last-wins across components by `order`.
    #[serde(default)]
    pub main_class: Option<String>,
    /// The client jar; last-wins, goes on the classpath.
    #[serde(default)]
    pub main_jar: Option<MainJar>,
    /// Asset index reference; set by the Minecraft component.
    #[serde(default)]
    pub asset_index: Option<AssetIndexRef>,

    /// Legacy game-arg string (=<1.12). Mutually exclusive in practice with
    /// [`Patch::arguments`]; the two forms are never merged.
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    /// Modern structured args (1.13+): `{ game, jvm }`.
    #[serde(default)]
    pub arguments: Option<Arguments>,

    /// Hint(s) for which JDK majors this component runs on.
    #[serde(default)]
    pub compatible_java_majors: Vec<u32>,
    /// Mojang's structured Java requirement (`{ majorVersion }`); the modern
    /// equivalent of `compatibleJavaMajors`. Folded into the same hint list.
    #[serde(default)]
    pub java_version: Option<JavaVersion>,

    /// Libraries declared by this component -> classpath (accumulate).
    #[serde(default)]
    pub libraries: Vec<Library>,
    /// Additive-operator form of `libraries` (accumulate).
    #[serde(default, rename = "+libraries")]
    pub plus_libraries: Vec<Library>,
    /// Removal operator: drop previously-added libs by `group:artifact`.
    #[serde(default, rename = "-libraries")]
    pub minus_libraries: Vec<LibraryRef>,
    /// Artifacts downloaded into `libraries/` but kept off the classpath.
    #[serde(default)]
    pub maven_files: Vec<Library>,

    /// Pre-tokenized JVM args, passed through verbatim. 
    /// re-split on spaces.
    #[serde(default, rename = "+jvmArgs")]
    pub plus_jvm_args: Vec<String>,
    /// `LaunchWrapper` tweaker classes -> emitted as `--tweakClass`.
    #[serde(default, rename = "+tweakers")]
    pub plus_tweakers: Vec<String>,
    /// Launcher behavior flags (e.g. `FirstThreadOnMacOS`); table-driven later.
    #[serde(default, rename = "+traits")]
    pub plus_traits: Vec<String>,
    /// Java agents -> `-javaagent` pass-through.
    #[serde(default, rename = "+agents")]
    pub plus_agents: Vec<Agent>,

    /// Dependency metadata (pre-resolved by the launcher; we assert only).
    #[serde(default)]
    pub requires: Vec<Dependency>,
    #[serde(default)]
    pub suggests: Vec<Dependency>,
    #[serde(default)]
    pub conflicts: Vec<Dependency>,

    // Known-but-unused fields, captured so they don't surface as "unknown".
    #[serde(default)]
    pub format_version: Option<u32>,
    #[serde(default, rename = "type")]
    pub release_type: Option<String>,
    #[serde(default)]
    pub release_time: Option<String>,
    #[serde(default)]
    pub volatile: Option<bool>,
    #[serde(default)]
    pub compatible_java_name: Option<String>,

    /// Anything we didn't model. Non-empty -> `load` warns.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// A library entry, covering every variant in one permissive shape:
/// (a) plain `downloads.artifact`, (b) bare `name` + `url` base,
/// (c) empty/missing sha1, (d) no-url / `MMC-hint: "local"`, and
/// (e) natives (modern classifier-lib or legacy `classifiers` + `natives`).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Library {
    /// Maven coordinate `group:artifact:version[:classifier]`.
    pub name: String,
    /// Explicit download(s); absent for the bare-name and no-url variants.
    #[serde(default)]
    pub downloads: Option<Downloads>,
    /// Maven repository base URL for the bare `name` + `url` variant (6.3b).
    #[serde(default)]
    pub url: Option<String>,
    /// `MultiMC` hint; `"local"` marks a no-url, must-be-present jar (6.3d).
    #[serde(default, rename = "MMC-hint")]
    pub mmc_hint: Option<String>,
    /// Legacy natives os->classifier map (6.3e); selects from `classifiers`.
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
    /// Legacy natives extraction directives (`exclude` globs).
    #[serde(default)]
    pub extract: Option<Extract>,
    /// Platform gating rules; evaluated in phase 2.
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,
}

/// A reference to a library by name, used by the `-libraries` operator. Accepts
/// either a bare `"group:artifact:version"` string or a `{ "name": ... }` object.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LibraryRef {
    Bare(String),
    Entry { name: String },
}

impl LibraryRef {
    /// The maven coordinate this reference names.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            LibraryRef::Bare(name) | LibraryRef::Entry { name } => name,
        }
    }
}

/// The `downloads` block of a library or main jar.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Downloads {
    /// The single primary artifact (plain libs, modern natives, main jar).
    #[serde(default)]
    pub artifact: Option<Artifact>,
    /// Per-classifier artifacts (legacy natives -> see [`Library::natives`]).
    #[serde(default)]
    pub classifiers: Option<HashMap<String, Artifact>>,
}

/// A single downloadable artifact. `sha1` may be empty (verification skipped,
/// 6.3c) and `url` may be absent for must-be-local entries.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Artifact {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

/// Legacy-native extraction directives.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Extract {
    /// Path prefixes to exclude when unzipping into `natives/`.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One Mojang/MMC rule entry; evaluated with allow/disallow semantics in
/// phase 2.
#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    /// `"allow"` or `"disallow"`.
    pub action: String,
    /// OS predicate (token form or classic name/arch/version).
    #[serde(default)]
    pub os: Option<Os>,
    /// Feature-flag predicate (e.g. `is_demo_user`); defaults all-false.
    #[serde(default)]
    pub features: Option<HashMap<String, bool>>,
}

/// The `os` predicate of a [`Rule`], accepting both rule dialects.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Os {
    /// MMC token (`linux`, `osx-arm64`, ...) or classic name (`linux`/`osx`/...).
    #[serde(default)]
    pub name: Option<String>,
    /// Classic arch (`x86`/`x86_64`/`arm64`).
    #[serde(default)]
    pub arch: Option<String>,
    /// Classic OS version regex.
    #[serde(default)]
    pub version: Option<String>,
}

/// Modern argument block.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<ArgEntry>,
    #[serde(default)]
    pub jvm: Vec<ArgEntry>,
}

/// One modern-arg element: either a plain string or a rule-gated value.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgEntry {
    /// An unconditional argument token.
    Plain(String),
    /// A conditional argument: included only if `rules` allow the ctx.
    Conditional { rules: Vec<Rule>, value: ArgValue },
}

/// The `value` of a conditional [`ArgEntry`]: one token or several.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ArgValue {
    Single(String),
    Many(Vec<String>),
}

/// The client jar (`mainJar`); shares the `downloads` shape with libraries.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MainJar {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<Downloads>,
}

/// Mojang's structured Java requirement (1.17+ patches). Only `majorVersion`
/// matters for JDK selection; `component` (the Mojang JRE name) is advisory.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    #[serde(default)]
    pub major_version: Option<u32>,
    #[serde(default)]
    pub component: Option<String>,
}

/// The asset index reference declared by the Minecraft component.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub total_size: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
}

/// A Java-agent entry from `+agents`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Agent {
    /// Maven coordinate of the agent jar.
    pub name: String,
}

/// A `requires`/`suggests`/`conflicts` dependency record (assert-only).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Dependency {
    pub uid: String,
    /// Exact version constraint.
    #[serde(default)]
    pub equals: Option<String>,
    /// Suggested version (advisory).
    #[serde(default)]
    pub suggests: Option<String>,
}
