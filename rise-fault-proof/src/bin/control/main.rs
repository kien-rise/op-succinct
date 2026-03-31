use anyhow::Result;
use clap::{Parser, Subcommand};
use commands::propose::ProposeCmd;
use tracing_subscriber::EnvFilter;

mod commands {
    pub mod propose;
}

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Propose(ProposeCmd),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(match std::env::var(EnvFilter::DEFAULT_ENV) {
            Ok(var) => EnvFilter::builder().parse(var)?,
            Err(_) => EnvFilter::new("info"),
        })
        .init();

    let args = Args::parse();
    let result = match args.command {
        Commands::Propose(cmd) => cmd.run().await,
    };
    if let Err(err) = result {
        tracing::error!(?err, "Command failed");
        std::process::exit(1);
    }
    tracing::info!("Command executed successfully");
    Ok(())
}
