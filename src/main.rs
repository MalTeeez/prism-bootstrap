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
mod resolve;
mod rules;

use anyhow::{Context, Result};
use clap::Parser;
use log::{LevelFilter, info};

use crate::model::artifact::{ArtifactRecord, Role};
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
/// resolve the artifacts for that target and report their roles.
fn run(args: &cli::Args) -> Result<()> {
    let patches = load::load_instance(args.instance_dir.as_path())?;
    let profile = merge::merge(&patches);
    log_summary(&profile);

    if let Some(platform) = args.platform {
        let ctx = platform::expand_platform(platform);
        let records = resolve::resolve(&profile, &ctx, args.instance_dir.as_path())
            .context("resolving artifacts for the target platform")?;
        report_resolution(&ctx, &records);
    }
    Ok(())
}

/// Report the resolved target and a per-role breakdown of its artifacts. The
/// downloads themselves land in phase 4.
fn report_resolution(ctx: &Ctx, records: &[ArtifactRecord]) {
    info!("Target platform: {} (os {}, arch {})", ctx.os_token, ctx.os_name, ctx.arch);
    let (mut classpath, mut natives, mut maven, mut no_url) = (0, 0, 0, 0);
    for record in records {
        match record.role {
            Role::Classpath => classpath += 1,
            Role::NativeExtract => natives += 1,
            Role::MavenFile => maven += 1,
            Role::NoUrl => no_url += 1,
            Role::Asset => {}
        }
    }
    info!(
        " - {} artifacts: {classpath} classpath, {natives} native-extract, \
         {maven} maven-file, {no_url} no-url (assume-local)",
        records.len()
    );
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
