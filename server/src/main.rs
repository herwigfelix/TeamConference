#![allow(dead_code)]

mod config;
mod server;
mod tls;
mod control;
mod audio;
mod room;
mod user;
mod chat;
mod files;
mod admin;
mod db;

use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "teamconference-server")]
#[command(about = "TeamConference Voice Chat Server")]
struct Args {
    /// Path to configuration file
    #[arg(short, long, default_value = "config.default.toml")]
    config: PathBuf,

    /// Generate a default admin user
    #[arg(long)]
    create_admin: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let cfg = config::Config::load(&args.config)?;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.logging.level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();

    tracing::info!("Starting {} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));

    server::run(cfg, args.create_admin).await
}
