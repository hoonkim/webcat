use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// webcat — a terminal browser rendering headless Chromium via the Kitty graphics protocol.
#[derive(Parser, Debug, Clone)]
#[command(name = "webcat", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// URL to open on startup (defaults to about:blank).
    pub url: Option<String>,

    /// Dedicated profile directory (overrides $WEBCAT_PROFILE_DIR).
    #[arg(long, env = "WEBCAT_PROFILE_DIR")]
    pub profile_dir: Option<PathBuf>,

    /// Path to the Chrome/Chromium binary (overrides $WEBCAT_CHROME).
    #[arg(long, env = "WEBCAT_CHROME")]
    pub chrome: Option<PathBuf>,

    /// JPEG screencast quality 1-100. Higher is sharper (less compression blur)
    /// at the cost of bigger frames; 92 is a good default for crisp text.
    #[arg(long)]
    pub quality: Option<u8>,

    /// Page zoom factor (clamped 0.5–4.0). Defaults to the display's scale factor
    /// (2.0 on Retina) so sites open at their natural size — like a Chrome window
    /// of the terminal's dimensions. Raise for bigger text, or pass --zoom 1 on a
    /// non-HiDPI external monitor.
    #[arg(long)]
    pub zoom: Option<f64>,

    /// Enable the embedded MCP server with read tools. Overrides config `mcp.enabled`.
    #[arg(long = "mcp", conflicts_with = "no_mcp", action = clap::ArgAction::SetTrue)]
    pub mcp_on: bool,

    /// Disable the embedded MCP server even when config enables it.
    #[arg(long = "no-mcp", id = "no_mcp", action = clap::ArgAction::SetTrue)]
    pub mcp_off: bool,

    /// Port for the MCP server. Overrides config `mcp.port`; if neither is set,
    /// webcat asks the OS for an ephemeral port.
    #[arg(long)]
    pub mcp_port: Option<u16>,

    /// Allow MCP control tools: navigate/click/type/etc. Overrides config
    /// `mcp.allow_control`.
    #[arg(long = "mcp-allow-control", conflicts_with = "no_mcp_allow_control", action = clap::ArgAction::SetTrue)]
    pub mcp_allow_control_on: bool,

    /// Disable MCP control tools even when config enables them.
    #[arg(long = "no-mcp-allow-control", id = "no_mcp_allow_control", action = clap::ArgAction::SetTrue)]
    pub mcp_allow_control_off: bool,
}

impl Cli {
    pub fn mcp(&self) -> Option<bool> {
        match (self.mcp_on, self.mcp_off) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        }
    }

    pub fn mcp_allow_control(&self) -> Option<bool> {
        match (self.mcp_allow_control_on, self.mcp_allow_control_off) {
            (true, false) => Some(true),
            (false, true) => Some(false),
            _ => None,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Manage the MCP integration and agent registration.
    Mcp(McpCmd),
}

#[derive(Args, Debug, Clone)]
pub struct McpCmd {
    #[command(subcommand)]
    pub action: McpAction,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpAction {
    /// Register the webcat MCP server with an agent and persist MCP config.
    Install(InstallArgs),
    /// Show MCP config and registration status.
    Status,
    /// Remove the webcat MCP server registration from an agent.
    Uninstall(UninstallArgs),
}

#[derive(Args, Debug, Clone)]
pub struct InstallArgs {
    /// Target agent.
    #[arg(long, value_enum, default_value_t = AgentKind::Claude)]
    pub agent: AgentKind,
    /// Stable port to write to config. Defaults to existing config `mcp.port`,
    /// otherwise 4470.
    #[arg(long)]
    pub port: Option<u16>,
    /// Persist `mcp.allow_control: true` so agents can mutate the live page.
    #[arg(long)]
    pub allow_control: bool,
}

#[derive(Args, Debug, Clone)]
pub struct UninstallArgs {
    #[arg(long, value_enum, default_value_t = AgentKind::Claude)]
    pub agent: AgentKind,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Register via `claude mcp add`.
    Claude,
    /// Print a config snippet only; no side effects.
    Print,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    #[test]
    fn no_subcommand_parses_url() {
        let cli = Cli::parse_from(["webcat", "https://example.com"]);
        assert!(cli.command.is_none());
        assert_eq!(cli.url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn mcp_flags_parse() {
        let cli = Cli::parse_from([
            "webcat",
            "--mcp",
            "--mcp-port",
            "4470",
            "--mcp-allow-control",
        ]);
        assert_eq!(cli.mcp(), Some(true));
        assert_eq!(cli.mcp_port, Some(4470));
        assert_eq!(cli.mcp_allow_control(), Some(true));
    }

    #[test]
    fn mcp_negative_flags_parse() {
        let cli = Cli::parse_from(["webcat", "--no-mcp", "--no-mcp-allow-control"]);
        assert_eq!(cli.mcp(), Some(false));
        assert_eq!(cli.mcp_allow_control(), Some(false));
    }

    #[test]
    fn unset_quality_remains_none_for_config_precedence() {
        let cli = Cli::parse_from(["webcat"]);
        assert_eq!(cli.quality, None);
        let cli = Cli::parse_from(["webcat", "--quality", "92"]);
        assert_eq!(cli.quality, Some(92));
    }

    #[test]
    fn mcp_install_subcommand_parses() {
        let cli = Cli::parse_from([
            "webcat",
            "mcp",
            "install",
            "--agent",
            "print",
            "--port",
            "5000",
            "--allow-control",
        ]);
        match cli.command {
            Some(Command::Mcp(McpCmd {
                action: McpAction::Install(a),
            })) => {
                assert!(matches!(a.agent, AgentKind::Print));
                assert_eq!(a.port, Some(5000));
                assert!(a.allow_control);
            }
            _ => panic!("expected mcp install"),
        }
    }

    #[test]
    fn mcp_install_defaults_agent_to_claude() {
        let cli = Cli::parse_from(["webcat", "mcp", "install"]);
        match cli.command {
            Some(Command::Mcp(McpCmd {
                action: McpAction::Install(a),
            })) => {
                assert!(matches!(a.agent, AgentKind::Claude));
            }
            _ => panic!("expected mcp install"),
        }
    }

    #[test]
    fn help_documents_mcp_port_resolution() {
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("if neither is set"));
        assert!(help.contains("ephemeral port"));

        let install_help = Cli::command()
            .find_subcommand_mut("mcp")
            .unwrap()
            .find_subcommand_mut("install")
            .unwrap()
            .render_help()
            .to_string();
        assert!(install_help.contains("existing config"));
        assert!(install_help.contains("otherwise 4470"));
    }
}
