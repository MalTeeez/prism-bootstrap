//! `Profile` - the merged result of folding every patch.
//!
//! This is the single in-memory contract the later phases consume: an ordered
//! library list, the resolved last-wins fields, and the accumulated arg /
//! tweaker / trait / agent sets. It is produced by `merge` and never mutated
//! after.

use crate::model::patch::{
    AssetIndexRef, Agent, ArgEntry, Dependency, Library, MainJar,
};

/// The game-argument form in effect for a profile. The legacy and modern forms
/// are never mixed - we track which one the components actually used so phase 6
/// can branch.
#[derive(Debug, Clone, Default)]
pub enum GameArgs {
    /// No component declared game arguments.
    #[default]
    None,
    /// Legacy `minecraftArguments` string (=<1.12); last-wins.
    Legacy(String),
    /// Modern `arguments { game, jvm }` (1.13+); entries accumulate in order.
    Modern { game: Vec<ArgEntry>, jvm: Vec<ArgEntry> },
}

/// The merged launch profile - input to filter/resolve/assemble.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    /// Classpath libraries in (component order, then declared order). Dedup is
    /// `resolve`'s job; the ordering guarantee starts here.
    pub libraries: Vec<Library>,
    /// `mavenFiles`: downloaded but kept off the classpath.
    pub maven_files: Vec<Library>,
    /// Entrypoint class; highest-`order` component that set it wins.
    pub main_class: Option<String>,
    /// Client jar; last-wins.
    pub main_jar: Option<MainJar>,
    /// Asset index; set by the Minecraft component.
    pub asset_index: Option<AssetIndexRef>,
    /// Active game-arg form (see [`GameArgs`]).
    pub game_args: GameArgs,
    /// `+jvmArgs`, accumulated and kept pre-tokenized.
    pub jvm_args: Vec<String>,
    /// `+tweakers`, accumulated in order.
    pub tweakers: Vec<String>,
    /// `+traits`, accumulated in order.
    pub traits: Vec<String>,
    /// `+agents`, accumulated in order.
    pub agents: Vec<Agent>,
    /// Collected `compatibleJavaMajors`, order-preserving and deduped, for the
    /// phase 6 JDK select.
    pub compatible_java_majors: Vec<u32>,
    /// Collected `requires`/`conflicts` for the preflight assertion in a later
    /// phase.
    pub requires: Vec<Dependency>,
    pub conflicts: Vec<Dependency>,
}
