//! Entrypoint: init logging, parse CLI, run the (phase-1) pipeline, and map any
//! top-level error to a distinct exit code.
//!
//! Today the pipeline only loads + merges an instance and logs a summary of the
//! resulting profile; the resolve/download/assemble stages land in later phases.

mod assemble;
mod assets;
mod cli;
mod download;
mod emit;
mod exit;
mod java;
mod load;
mod merge;
mod meta;
mod model;
mod natives;
mod platform;
mod resolve;
mod rules;

use anyhow::{Context, Result};
use clap::Parser;
use log::{LevelFilter, info};

use crate::download::{DownloadOptions, Downloader};
use crate::model::artifact::{ArtifactRecord, Role};
use crate::model::profile::{GameArgs, Profile};
use crate::platform::Ctx;

#[tokio::main]
async fn main() {
    if let Err(error) = init_logging() {
        eprintln!("failed to initialise logging: {error:?}");
        std::process::exit(exit::ExitCode::IoError.code());
    }

    let args = cli::Args::parse();
    match run(&args).await {
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

/// The full pipeline: load, resolve any pack-only components from the meta
/// server, merge, preflight, resolve artifacts, download (libraries and assets),
/// extract natives, select java, assemble, and emit. Without a `--platform` we
/// stop after the merge summary (everything downstream needs a target).
async fn run(args: &cli::Args) -> Result<()> {
    let components = load::load_components(args.instance_dir.as_path())?;

    // Build the HTTP client once, up front: meta resolution (next) and the
    // artifact downloads (later) share this one downloader.
    let downloader = Downloader::new(DownloadOptions {
        jobs: args.jobs,
        verify: !args.no_verify,
        dry_run: args.dry_run,
    })
    .context("initialising the downloader")?;

    // Fill in any pack-only components from the meta server (opt-in via
    // --meta-url); a gap without --meta-url fails with a provisioning hint.
    let patches = meta::resolve_components(components, &downloader, args.meta_url.as_deref())
        .await
        .context("resolving components")?;
    let profile = merge::merge(&patches);
    log_summary(&profile);

    let Some(platform) = args.platform else {
        return Ok(());
    };
    let ctx = platform::expand_platform(platform);
    let instance = resolve::absolute_instance_dir(args.instance_dir.as_path())?;

    // Fail-fast before any downloads: a missing main class or unsatisfied
    // dependency is cheaper to report now than after fetching everything.
    assemble::preflight(&profile, &patches).context("preflight checks")?;

    let records = resolve::resolve(&profile, &ctx, &instance)
        .context("resolving artifacts for the target platform")?;
    report_resolution(&ctx, &records);

    downloader.download_all("libraries", &records).await?;
    if let Some(asset_index) = &profile.asset_index {
        assets::download_assets(&downloader, asset_index, &instance).await?;
    }

    // Extract legacy natives now that their jars are on disk (skipped on a
    // dry run, where nothing was downloaded).
    if !args.dry_run {
        extract_natives(args, &records, &instance).await?;
    }
    info!("All artifacts present for {}.", ctx.os_token);

    // Assemble and emit the launch command.
    info!("Selecting a JDK:");
    let java = java::select_java(args.java.as_deref(), &profile.compatible_java_majors)
        .context("selecting a JDK")?;
    let config = assemble_config(args, &instance, java);
    let command = assemble::assemble(&profile, &ctx, &records, &instance, &config)
        .context("assembling the launch command")?;
    let argv_path = args.emit.clone().unwrap_or_else(|| instance.join("launch.argv"));
    emit::emit(&command, &argv_path, &records, args.headless)
        .context("emitting the launch command")?;
    Ok(())
}

/// Build the assembly [`Config`](assemble::Config) from the CLI args, applying
/// the `--game-dir` default of `<instance>/.minecraft`.
fn assemble_config(args: &cli::Args, instance: &std::path::Path, java: std::path::PathBuf) -> assemble::Config {
    assemble::Config {
        java,
        xms: args.xms.clone(),
        xmx: args.xmx.clone(),
        username: args.username.clone(),
        uuid: args.uuid.clone(),
        access_token: args.access_token.clone(),
        user_type: args.user_type.clone(),
        game_dir: args.game_dir.clone().unwrap_or_else(|| instance.join(".minecraft")),
        headless: args.headless,
    }
}

/// Extract legacy natives off the async runtime (the `zip` crate is blocking).
async fn extract_natives(
    args: &cli::Args,
    records: &[ArtifactRecord],
    instance: &std::path::Path,
) -> Result<()> {
    let native_records: Vec<ArtifactRecord> =
        records.iter().filter(|record| record.role == Role::NativeExtract).cloned().collect();
    let natives_dir = instance.join("natives");
    let options = natives::NativeOptions {
        headless: args.headless,
        keep: args.keep_natives.clone(),
        skip: args.skip_natives.clone(),
    };
    tokio::task::spawn_blocking(move || {
        natives::extract_natives(&native_records, &natives_dir, &options)
    })
    .await
    .context("native-extraction task")??;
    Ok(())
}

/// Report the resolved target and a per-role breakdown of its artifacts. The
/// downloads themselves land in phase 4.
fn report_resolution(ctx: &Ctx, records: &[ArtifactRecord]) {
    info!("Target platform: {} (os {}, arch {})", ctx.os_token, ctx.os_name, ctx.arch);
    let (mut classpath, mut natives, mut maven) = (0, 0, 0);
    for record in records {
        match record.role {
            Role::Classpath => classpath += 1,
            Role::NativeExtract => natives += 1,
            Role::MavenFile => maven += 1,
            Role::Asset => {}
        }
    }
    // Assume-local count is orthogonal to role: any record without a url.
    let assume_local = records.iter().filter(|record| record.url.is_none()).count();
    info!(
        " - {} artifacts: {classpath} classpath, {natives} native-extract, \
         {maven} maven-file ({assume_local} assume-local, no url)",
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
