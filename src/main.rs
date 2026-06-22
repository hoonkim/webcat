mod cli;
mod config;
mod error;
mod geometry;
mod renderer;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let cfg = config::Config::resolve(cli)?;
    // Wired up incrementally in later tasks.
    eprintln!("resolved config: {:?}", cfg);
    Ok(())
}
