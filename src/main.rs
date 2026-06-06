//! Entrypoint: init logging, parse CLI, run the (phase-1) pipeline, and map any
//! top-level error to a distinct exit code.
//!
//! Today the pipeline only loads + merges an instance and logs a summary of the
//! resulting profile; the resolve/download/assemble stages land in later phases.

mod cli;
mod exit;
mod load;
mod merge;
mod model;
mod platform;
mod rules;

use anyhow::{Context, Result};
use clap::Parser;
use log::{LevelFilter, info};

use crate::model::profile::{GameArgs, Profile};
use crate::platform::Ctx;

fn main() {
    if let Err(error) = init_logging() {
        eprintln!("failed to initialise logging: {error:?}");
        std::process::exit(exit::ExitCode::IoError.code());
    }

    let args = cli::Args::parse();
    match run(&args) {
        Ok(()) => std::process::exit(exit::ExitCode::Ok.code()),
        Err(error) => {
            // Log the full context chain so the cause is visible to the user.
            log::error!("{error:?}");
            std::process::exit(exit::exit_code_for(&error).code());
        }
    }
}

/// Initialise `simple_logger` without timestamps (project logging convention).
fn init_logging() -> Result<()> {
    simple_logger::SimpleLogger::new()
        .without_timestamps()
        .with_level(LevelFilter::Info)
        .init()
        .context("initialising the logger")
}

/// The pipeline so far: load -> merge -> summarise, and (if a platform is given)
/// report how the rules gate the libraries for that target.
fn run(args: &cli::Args) -> Result<()> {
    let patches = load::load_instance(args.instance_dir.as_path())?;
    let profile = merge::merge(&patches);
    log_summary(&profile);

    if let Some(platform) = args.platform {
        report_platform(&profile, &platform::expand_platform(platform));
    }
    Ok(())
}

/// Report the resolved target and how many libraries its rules admit. A preview
/// of the phase-3 filter step, exercising `expand_platform` + `allowed`.
fn report_platform(profile: &Profile, ctx: &Ctx) {
    info!("Target platform: {} (os {}, arch {})", ctx.os_token, ctx.os_name, ctx.arch);
    let admitted = profile
        .libraries
        .iter()
        .filter(|library| rules::allowed(library.rules.as_deref().unwrap_or(&[]), ctx))
        .count();
    info!(" - {admitted} of {} libraries apply to this platform", profile.libraries.len());
}

/// Log a one-glance summary of the merged profile.
fn log_summary(profile: &Profile) {
    info!("Profile summary:");
    info!(" - main class: {}", profile.main_class.as_deref().unwrap_or("<unset>"));
    info!(" - libraries: {}", profile.libraries.len());
    info!(" - maven files: {}", profile.maven_files.len());
    info!(" - jvm args: {}", profile.jvm_args.len());
    info!(" - tweakers: {}", profile.tweakers.len());
    info!(" - traits: {}", profile.traits.len());
    info!(
        " - game arg form: {}",
        match profile.game_args {
            GameArgs::None => "none",
            GameArgs::Legacy(_) => "legacy (minecraftArguments)",
            GameArgs::Modern { .. } => "modern (arguments)",
        }
    );
    if !profile.compatible_java_majors.is_empty() {
        info!(" - compatible java majors: {:?}", profile.compatible_java_majors);
    }
}
