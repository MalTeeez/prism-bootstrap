//! Command-line arguments.
//!
//! The full option set: the instance dir + the validated
//! `--platform` value-enum, plus heap, dummy-auth, directory, and output flags
//! the assembler consumes.

use std::path::PathBuf;

use clap::Parser;

use crate::platform::Platform;

/// Resolve a MultiMC/Prism instance into a runnable `java` command.
#[derive(Debug, Parser)]
#[command(name = "prism-bootstrap")]
pub struct Args {
    /// Instance directory containing `mmc-pack.json` and `patches/`.
    pub instance_dir: PathBuf,

    /// Target platform token, validated against the fixed list. Optional: when
    /// omitted, the tool stops after printing the merged-profile summary.
    #[arg(long, value_enum)]
    pub platform: Option<Platform>,

    /// Maximum parallel downloads.
    #[arg(long, default_value_t = 16)]
    pub jobs: usize,

    /// Skip SHA-1/size verification (faster re-runs).
    #[arg(long)]
    pub no_verify: bool,

    /// Resolve + preflight only; perform no downloads.
    #[arg(long)]
    pub dry_run: bool,

    /// Adjust internals for a no-GPU/virtual-display context. Among other
    /// things, skips extracting input-device natives.
    #[arg(long)]
    pub headless: bool,

    /// Force-extract a native whose coordinate contains this pattern, overriding
    /// the headless skip-list. Repeatable.
    #[arg(long, value_name = "PATTERN")]
    pub keep_natives: Vec<String>,

    /// Force-skip extracting a native whose coordinate contains this pattern.
    /// Repeatable.
    #[arg(long, value_name = "PATTERN")]
    pub skip_natives: Vec<String>,

    /// Initial heap size (`-Xms`); the tool injects it, patches never do.
    #[arg(long, default_value = "512m")]
    pub xms: String,

    /// Max heap size (`-Xmx`); mirrors the original instance dump, override
    /// freely.
    #[arg(long, default_value = "6144m")]
    pub xmx: String,

    /// JDK to put in the command. Default: auto-select by `compatibleJavaMajors`
    /// from `PATH`.
    #[arg(long, value_name = "PATH")]
    pub java: Option<PathBuf>,

    /// Dummy account name -> `${auth_player_name}`.
    #[arg(long, default_value = "CI")]
    pub username: String,

    /// Dummy account uuid -> `${auth_uuid}`.
    #[arg(long, default_value = "00000000-0000-0000-0000-000000000000")]
    pub uuid: String,

    /// Dummy access token -> `${auth_access_token}`.
    #[arg(long, default_value = "0")]
    pub access_token: String,

    /// Account type -> `${user_type}`; `legacy` or `msa`.
    #[arg(long, default_value = "legacy")]
    pub user_type: String,

    /// Game working directory -> `${game_directory}`. Default `<instance>/.minecraft`.
    #[arg(long, value_name = "PATH")]
    pub game_dir: Option<PathBuf>,

    /// Where to write `launch.argv`. Default `<instance>/launch.argv`.
    #[arg(long, value_name = "PATH")]
    pub emit: Option<PathBuf>,

    /// Meta base URL to resolve components that have no local
    /// `patches/<uid>.json` (e.g. `https://meta.prismlauncher.org/v1/`). When
    /// omitted, a component with no local patch is a hard error - the tool never
    /// reaches the network for a patch you didn't point it at. Also accepts a
    /// mirror / air-gap base. Opt-in by design - there is no default.
    #[arg(long, value_name = "URL")]
    pub meta_url: Option<String>,
}
