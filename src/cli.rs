use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum InputFormat {
    #[default]
    Text,
    StreamJson,
}

#[derive(Parser, Debug)]
#[command(
    name = "kudelma",
    version,
    about = "A fast Rust coding agent for the terminal",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Run one prompt non-interactively and exit
    #[arg(short = 'p', long = "print")]
    pub print: bool,

    /// Model spec, e.g. anthropic/claude-sonnet-4-5 or moonshot/kimi-k2.5
    #[arg(short, long)]
    pub model: Option<String>,

    /// Resume the most recent session in this directory
    #[arg(short = 'c', long = "continue")]
    pub continue_session: bool,

    /// Resume a specific session id
    #[arg(short = 'r', long = "resume", visible_alias = "session")]
    pub resume: Option<String>,

    /// Skip permission prompts (explicit deny rules still apply)
    #[arg(long, alias = "dangerously-skip-permissions")]
    pub yolo: bool,

    /// Trust this project for this run (loads project config, MCP and hooks)
    #[arg(long)]
    pub trust: bool,

    /// Start in plan mode (read-only exploration and a plan file)
    #[arg(long)]
    pub plan: bool,

    /// Output format for --print mode
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output_format: OutputFormat,

    /// Input format for --print mode (stream-json reads NDJSON user messages from stdin)
    #[arg(long, value_enum, default_value_t = InputFormat::Text)]
    pub input_format: InputFormat,

    /// Maximum agent turns for this run
    #[arg(long)]
    pub max_turns: Option<u32>,

    /// Replace the system prompt
    #[arg(long)]
    pub system_prompt: Option<String>,

    /// Append to the system prompt
    #[arg(long)]
    pub append_system_prompt: Option<String>,

    /// Comma-separated allowlist of tools
    #[arg(long, value_delimiter = ',')]
    pub allowed_tools: Vec<String>,

    /// Comma-separated tools to disable
    #[arg(long, value_delimiter = ',')]
    pub disallowed_tools: Vec<String>,

    /// Use this session id for a new session
    #[arg(long)]
    pub session_id: Option<String>,

    /// Show tool activity on stderr in --print mode
    #[arg(long)]
    pub verbose: bool,

    /// Initial prompt; reads stdin when piped
    #[arg(value_name = "PROMPT")]
    pub prompt: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage provider credentials
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// List available models and prices
    Models {
        /// Accepted for compatibility; the catalog is built in
        #[arg(long)]
        refresh: bool,
    },
    /// Manage saved sessions
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
    /// Print the tree-sitter symbol outline of a file
    Index {
        #[arg(value_name = "FILE")]
        path: PathBuf,
    },
    /// Manage folders trusted to load project config and MCP servers
    Trust {
        #[command(subcommand)]
        action: TrustAction,
    },
    /// Print the rendered system prompt or tool definitions
    Prompt(PromptArgs),
    /// Run as an Agent Client Protocol server over stdio
    Acp(AcpArgs),
    /// Inspect the configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// List configured MCP servers
    Mcp,
    /// Check the installation and configuration
    Doctor,
}

#[derive(Subcommand, Debug)]
pub enum AuthAction {
    /// Store an API key for a provider
    Login {
        /// Provider slug, e.g. anthropic, openai, moonshot
        provider: Option<String>,
    },
    /// Remove a stored API key
    Logout { provider: String },
    /// Show which providers have credentials
    Status,
}

#[derive(Subcommand, Debug)]
pub enum SessionAction {
    /// List saved sessions
    List {
        /// Only sessions for the current directory
        #[arg(long)]
        local: bool,
    },
    /// Delete a session
    Delete {
        id: String,
        /// Do not ask for confirmation
        #[arg(short, long)]
        force: bool,
    },
    /// Write a session as JSON to stdout
    Export { id: String },
    /// Import a session JSON file (or stdin) as a new session
    Import {
        /// File path; stdin when omitted or `-`
        file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum TrustAction {
    /// Trust a folder (defaults to the current directory)
    Add {
        path: Option<PathBuf>,
        /// Do not ask for confirmation
        #[arg(short, long)]
        yes: bool,
    },
    /// Remove a trust decision
    Remove { path: Option<PathBuf> },
    /// List trusted folders
    List,
}

#[derive(Args, Debug)]
pub struct PromptArgs {
    /// Print tool definitions as JSON instead of the system prompt
    #[arg(long)]
    pub tools: bool,
    /// Render the plan-mode prompt
    #[arg(long)]
    pub plan: bool,
}

#[derive(Args, Debug)]
pub struct AcpArgs {
    #[arg(short, long)]
    pub model: Option<String>,
    #[arg(long)]
    pub yolo: bool,
    #[arg(long)]
    pub trust: bool,
}

#[derive(Subcommand, Debug)]
pub enum ConfigAction {
    /// Print the global config path
    Path,
    /// Print the merged effective config
    Show,
    /// Create a commented default config if none exists
    Init {
        #[arg(short, long)]
        force: bool,
    },
}
