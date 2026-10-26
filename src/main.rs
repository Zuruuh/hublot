use std::{path::PathBuf, process::ExitCode};

use anyhow::Result;
use clap::{Parser, Subcommand};
use hublot::{check, config::Config, site};
use tracing::error;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "hublot", version, about = "Docker image update checker")]
struct Cli {
    #[arg(
        long,
        global = true,
        env = "HUBLOT_CONFIG",
        default_value = "/etc/hublot/config.toml"
    )]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check registries, update state, and rebuild the static site.
    Check,
    /// Rebuild the static site from the existing state without checking registries.
    Render,
}

#[tokio::main]
async fn main() -> ExitCode {
    init_tracing();
    let cli = Cli::parse();

    match run(cli).await {
        Ok(code) => code,
        Err(error) => {
            error!(error = %format!("{error:#}"), "hublot failed");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    let config = Config::load(&cli.config)?;

    match cli.command {
        Command::Check => {
            let report = check::run(&config).await?;
            println!(
                "checked {} images, {} errors; site written to {}",
                report.image_count,
                report.error_count,
                config.output_dir.display()
            );
            if report.error_count == 0 {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            }
        }
        Command::Render => {
            site::write_site(&config)?;
            println!("site written to {}", config.output_dir.display());
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("hublot=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
