use std::process::Command as Proc;

use crate::cli::{AgentKind, InstallArgs, UninstallArgs};
use crate::error::{Error, Result};

const DEFAULT_PORT: u16 = 4470;

pub fn mcp_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

fn claude_add_args(port: u16) -> Vec<String> {
    vec![
        "mcp".into(),
        "add".into(),
        "--transport".into(),
        "http".into(),
        "webcat".into(),
        mcp_url(port),
    ]
}

fn print_snippet(port: u16) -> String {
    format!(
        "webcat MCP server\n  URL: {url}\n  transport: streamable-http\n\nClaude Code:\n  claude mcp add --transport http webcat {url}\n",
        url = mcp_url(port)
    )
}

fn resolve_and_persist_port(cli_port: Option<u16>, allow_control: bool) -> Result<u16> {
    let file = crate::config::load_file_config()?.unwrap_or_default();
    let existing = file.mcp.as_ref().and_then(|m| m.port);
    let port = cli_port.or(existing).unwrap_or(DEFAULT_PORT);
    write_config_mcp(port, allow_control)?;
    Ok(port)
}

fn write_config_mcp(port: u16, allow_control: bool) -> Result<()> {
    use crate::config::{FileConfig, FileMcpConfig};

    let home = dirs::home_dir().ok_or_else(|| Error::Other(anyhow::anyhow!("no home dir")))?;
    let dir = home.join(".webcat");
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Other(anyhow::anyhow!("create {}: {e}", dir.display())))?;
    let path = dir.join("config.yaml");
    let mut cfg: FileConfig = crate::config::load_file_config()?.unwrap_or_default();
    cfg.mcp = Some(FileMcpConfig {
        enabled: Some(true),
        port: Some(port),
        allow_control: Some(allow_control),
    });
    let text = serde_yml::to_string(&cfg).map_err(|e| Error::Other(anyhow::anyhow!(e)))?;
    std::fs::write(&path, text)
        .map_err(|e| Error::Other(anyhow::anyhow!("write {}: {e}", path.display())))?;
    Ok(())
}

pub fn run_install(args: InstallArgs) -> Result<()> {
    let port = resolve_and_persist_port(args.port, args.allow_control)?;
    match args.agent {
        AgentKind::Print => {
            print!("{}", print_snippet(port));
        }
        AgentKind::Claude => {
            let status = Proc::new("claude").args(claude_add_args(port)).status();
            match status {
                Ok(s) if s.success() => {
                    println!(
                        "Registered webcat MCP server with Claude Code at {}",
                        mcp_url(port)
                    );
                }
                _ => {
                    println!(
                        "Could not run `claude mcp add` automatically. Add it manually:\n\n{}",
                        print_snippet(port)
                    );
                }
            }
        }
    }
    println!(
        "\nNote: webcat must be running (with mcp enabled) for the agent to connect.\n\
         Config written to ~/.webcat/config.yaml (mcp.enabled=true, port={port}, allow_control={}).",
        args.allow_control
    );
    Ok(())
}

pub fn run_status() -> Result<()> {
    let file = crate::config::load_file_config()?.unwrap_or_default();
    match file.mcp {
        Some(m) => {
            let port = m.port.unwrap_or(DEFAULT_PORT);
            println!(
                "MCP config (~/.webcat/config.yaml):\n  enabled: {}\n  allow_control: {}\n  url: {}",
                m.enabled.unwrap_or(false),
                m.allow_control.unwrap_or(false),
                mcp_url(port),
            );
        }
        None => println!("No MCP config found in ~/.webcat/config.yaml."),
    }
    Ok(())
}

pub fn run_uninstall(args: UninstallArgs) -> Result<()> {
    match args.agent {
        AgentKind::Claude => {
            let status = Proc::new("claude")
                .args(["mcp", "remove", "webcat"])
                .status();
            match status {
                Ok(s) if s.success() => println!("Removed webcat MCP server from Claude Code."),
                _ => println!("Could not run `claude mcp remove webcat`. Remove it manually."),
            }
        }
        AgentKind::Print => println!("Nothing to remove for --agent print."),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_format() {
        assert_eq!(mcp_url(4470), "http://127.0.0.1:4470/mcp");
    }

    #[test]
    fn claude_args_shape() {
        let a = claude_add_args(4470);
        assert_eq!(
            a,
            vec![
                "mcp",
                "add",
                "--transport",
                "http",
                "webcat",
                "http://127.0.0.1:4470/mcp",
            ]
        );
    }

    #[test]
    fn snippet_mentions_url_and_claude() {
        let s = print_snippet(5000);
        assert!(s.contains("http://127.0.0.1:5000/mcp"));
        assert!(s.contains("claude mcp add"));
    }
}
