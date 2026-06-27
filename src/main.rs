mod app;
mod browser;
mod cli;
mod config;
mod error;
mod geometry;
mod input;
mod mcp;
mod observability;
mod renderer;
mod terminal;
mod ui;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    if let Some(cli::Command::Mcp(mcp)) = &cli.command {
        return match &mcp.action {
            cli::McpAction::Install(a) => mcp::install::run_install(a.clone()).map_err(Into::into),
            cli::McpAction::Status => mcp::install::run_status().map_err(Into::into),
            cli::McpAction::Uninstall(a) => {
                mcp::install::run_uninstall(a.clone()).map_err(Into::into)
            }
        };
    }
    let file = config::load_file_config().unwrap_or_else(|e| {
        eprintln!("config error: {e}");
        std::process::exit(2);
    });
    let cfg = config::Config::resolve(cli, file)?;

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
                .unwrap_or_else(|_| {
                    "info,chromiumoxide::conn=off,chromiumoxide::handler=off".into()
                }),
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
    // The stdin reader runs in a blocking task and can be parked in read().
    // Do not make process exit wait for another keypress just to wake it.
    rt.shutdown_timeout(std::time::Duration::from_millis(100));
    Ok(())
}
