//! Command-line arguments.
//!
//! A minimal stub for phase 1: enough to point the loader at an instance. The
//! full option set and the validated `--platform` value-enum are
//! wired in phase 6; this struct grows there.

use std::path::PathBuf;

use clap::Parser;

use crate::platform::Platform;

/// Resolve a MultiMC/Prism instance into a runnable `java` command.
#[derive(Debug, Parser)]
#[command(name = "mc-headless-launcher")]
pub struct Args {
    /// Instance directory containing `mmc-pack.json` and `patches/`.
    pub instance_dir: PathBuf,

    /// Target platform token, validated against the fixed list. Optional: when
    /// Required once the resolve/emit phases land; optional for now.
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
}
