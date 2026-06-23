mod app;
mod browser;
mod cli;
mod config;
mod error;
mod geometry;
mod input;
mod renderer;
mod terminal;
mod ui;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    let cfg = config::Config::resolve(cli)?;

    // File logging only — never touch the terminal screen.
    if let Some(parent) = cfg.log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_path)?;
    tracing_subscriber::fmt()
        .with_writer(file)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("WEBCAT_LOG_LEVEL")
                // Chrome 149 emits CDP messages chromiumoxide 0.7.0 can't
                // deserialize; its connection layer logs one error per message,
                // flooding the log. They're harmless (we ignore them), so
                // silence those two modules by default.
                .unwrap_or_else(|_| "info,chromiumoxide::conn=off,chromiumoxide::handler=off".into()),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        if let Err(e) = app::run(cfg).await {
            tracing::error!("fatal: {e}");
            eprintln!("webcat: {e}");
            std::process::exit(1);
        }
    });
    Ok(())
}
