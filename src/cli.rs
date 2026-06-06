//! Command-line arguments.
//!
//! A minimal stub for phase 1: enough to point the loader at an instance. The
//! full option set and the validated `--platform` value-enum are
//! wired in phase 6; this struct grows there.

use std::path::PathBuf;

use clap::Parser;

/// Resolve a MultiMC/Prism instance into a runnable `java` command.
#[derive(Debug, Parser)]
#[command(name = "mc-headless-launcher")]
pub struct Args {
    /// Instance directory containing `mmc-pack.json` and `patches/`.
    pub instance_dir: PathBuf,

    /// Target platform token (validated and consumed in a later phase).
    #[arg(long)]
    pub platform: Option<String>,
}
