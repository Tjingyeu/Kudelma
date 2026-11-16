use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Result, bail};
use kudelma_config::{Config, global_config_path};
use kudelma_core::{AgentMode, SessionHandle};
use kudelma_providers::{ProviderPreset, all_models, builtin_providers, resolve_model};
use kudelma_storage::{
    AuthStore, SessionClaim, SessionStore, StoredSession, TrustStore, atomic_write,
    canonicalize_lenient, new_id, now_rfc3339, state_dir,
};
use kudelma_tools::{
    IndexTool, LocalEnv, Tool, ToolContext, ToolRegistry, ToolSearchTool, builtin_tools,
};

use crate::cli::{
    AcpArgs, AuthAction, Cli, Command, ConfigAction, InputFormat, OutputFormat, PromptArgs,
    SessionAction, TrustAction,
};
use crate::headless::{HeadlessOptions, check_credentials};

pub async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Auth { action }) => auth(action).await,
        Some(Command::Models { .. }) => models(),
        Some(Command::Session { action }) => sessions(action).await,
        Some(Command::Index { path }) => index(path).await,
        Some(Command::Trust { action }) => trust(action),
        Some(Command::Prompt(args)) => prompt(args),
        Some(Command::Acp(args)) => acp(args).await,
        Some(Command::Config { action }) => config(action),
        Some(Command::Mcp) => mcp(),
        Some(Command::Doctor) => doctor(),
        None => run_default(cli).await,
    }
}

async fn run_default(cli: Cli) -> Result<()> {
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = cli.trust
        || TrustStore::load()
            .map(|store| store.is_trusted(&cwd))
            .unwrap_or(false);
    let config = Arc::new(Config::load_with_trust(&cwd, trusted)?);
    let auth = AuthStore::load().ok();

    let store = SessionStore::new()?;
    let resume = if let Some(id) = &cli.resume {
        Some(store.load(id)?)
    } else if cli.continue_session {
        store.latest(&cwd)?
    } else {
        None
    };

    let model_spec = cli
        .model
        .clone()
        .or_else(|| resume.as_ref().map(|session| session.meta.model.clone()))
        .unwrap_or_else(|| config.default_model.clone());
    let model = resolve_model(&config, auth.as_ref(), &model_spec)?;
    if !matches!(cli.command, Some(Command::Models { .. })) {
        check_credentials(&model)?;
    }

    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    let interactive = !cli.print && stdout_tty && (stdin_tty || cli.prompt.is_some());

    if interactive {
        let params = kudelma_tui::TuiParams {
            cwd,
            config,
            model: cli.model,
            auth,
            yolo: cli.yolo,
            trusted,
            plan: cli.plan,
            max_turns: cli.max_turns,
            system_prompt: cli.system_prompt,
            append_system_prompt: cli.append_system_prompt,
            allowed_tools: (!cli.allowed_tools.is_empty()).then_some(cli.allowed_tools),
            disallowed_tools: cli.disallowed_tools,
            resume,
            initial_prompt: cli.prompt,
        };
        return kudelma_tui::run(params).await;
    }

    let prompt = match cli.prompt {
        Some(prompt) => Some(prompt),
        None if !stdin_tty => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            Some(text)
        }
        None => None,
    };
    let options = HeadlessOptions {
        cwd,
        config,
        auth,
        model: cli.model,
        yolo: cli.yolo,
        trusted,
        resume,
        plan: cli.plan,
        max_turns: cli.max_turns,
        system_prompt: cli.system_prompt,
        append_system_prompt: cli.append_system_prompt,
        allowed_tools: (!cli.allowed_tools.is_empty()).then_some(cli.allowed_tools),
        disallowed_tools: cli.disallowed_tools,
        output: cli.output_format,
        input: cli.input_format,
        verbose: cli.verbose,
        prompt,
        session_id: cli.session_id,
    };
    crate::headless::run(options).await
}

async fn auth(action: AuthAction) -> Result<()> {
    match action {
        AuthAction::Login { provider } => {
            let provider = match provider {
                Some(provider) => provider,
                None => {
                    println!(
                        "Providers: anthropic, openai, moonshot, deepseek, gemini, openrouter"
                    );
                    print!("Provider: ");
                    std::io::stdout().flush()?;
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim().to_string()
                }
            };
            if provider.is_empty() {
                bail!("no provider selected");
            }
            let key = rpassword::prompt_password(format!("API key for {provider}: "))?;
            if key.trim().is_empty() {
                bail!("empty API key");
            }
            let mut store = AuthStore::load()?;
            store.set(&provider, key.trim())?;
            println!(
                "Stored credentials for {provider} in {}",
                store.path().display()
            );
        }
        AuthAction::Logout { provider } => {
            let mut store = AuthStore::load()?;
            if store.remove(&provider)? {
                println!("Removed stored credentials for {provider}");
            } else {
                println!("No stored credentials for {provider}");
            }
        }
        AuthAction::Status => {
            let store = AuthStore::load()?;
            println!("{:<12} {:<24} STORED", "PROVIDER", "ENVIRONMENT");
            for preset in builtin_providers() {
                let env = std::env::var(preset.api_key_env)
                    .ok()
                    .filter(|value| !value.is_empty())
                    .is_some();
                let stored = store.get(preset.name).is_some();
                println!(
                    "{:<12} {:<24} {}",
                    preset.name,
                    preset.api_key_env,
                    if stored {
                        "yes"
                    } else if env {
                        "via env"
                    } else {
                        "-"
                    }
                );
            }
        }
    }
    Ok(())
}

fn models() -> Result<()> {
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = TrustStore::load()
        .map(|store| store.is_trusted(&cwd))
        .unwrap_or(false);
    let config = Config::load_with_trust(&cwd, trusted)?;
    let auth = AuthStore::load().ok();
    let models = all_models(&config, auth.as_ref());
    if models.is_empty() {
        bail!("no models available");
    }
    println!(
        "{:<44} {:>9} {:>9} {:>9}  AUTH",
        "MODEL", "CONTEXT", "IN $/M", "OUT $/M"
    );
    for model in models {
        let auth = if model.api_key.is_some() {
            "key"
        } else if model.base_url.contains("localhost") || model.base_url.contains("127.0.0.1") {
            "local"
        } else {
            "missing"
        };
        println!(
            "{:<44} {:>9} {:>9.2} {:>9.2}  {}",
            model.spec(),
            model.context_window,
            model.input_cost,
            model.output_cost,
            auth
        );
    }
    Ok(())
}

async fn sessions(action: SessionAction) -> Result<()> {
    let store = SessionStore::new()?;
    match action {
        SessionAction::List { local } => {
            let cwd = canonicalize_lenient(&std::env::current_dir()?);
            let mut sessions = store.list()?;
            if local {
                sessions.retain(|session| session.cwd == cwd.display().to_string());
            }
            if sessions.is_empty() {
                println!("No saved sessions.");
                return Ok(());
            }
            for session in sessions {
                println!(
                    "{}  {}  {:<40}  {}",
                    session.id,
                    session.updated_at,
                    session.title.chars().take(40).collect::<String>(),
                    session.model
                );
            }
        }
        SessionAction::Delete { id, force } => {
            let _claim = SessionClaim::acquire(&id)?;
            if !force {
                print!("Delete session {id}? [y/N] ");
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim(), "y" | "Y" | "yes") {
                    println!("Cancelled.");
                    return Ok(());
                }
            }
            store.delete(&id)?;
            println!("Deleted {id}");
        }
        SessionAction::Export { id } => {
            let session = store.load(&id)?;
            println!("{}", serde_json::to_string_pretty(&session)?);
        }
        SessionAction::Import { file } => {
            let text = match file {
                Some(path) if path.as_os_str() != std::ffi::OsStr::new("-") => {
                    std::fs::read_to_string(path)?
                }
                _ => {
                    let mut text = String::new();
                    std::io::stdin().read_to_string(&mut text)?;
                    text
                }
            };
            let mut session: StoredSession = serde_json::from_str(&text)?;
            let mut meta = session.meta;
            meta.id = new_id();
            meta.created_at = now_rfc3339();
            meta.updated_at = meta.created_at.clone();
            meta.cwd = canonicalize_lenient(&std::env::current_dir()?)
                .display()
                .to_string();
            session.meta = meta.clone();
            let claim = SessionClaim::acquire(&meta.id)?;
            store.save(&session)?;
            drop(claim);
            println!("{}", meta.id);
        }
    }
    Ok(())
}

async fn index(path: PathBuf) -> Result<()> {
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = TrustStore::load()
        .map(|store| store.is_trusted(&cwd))
        .unwrap_or(false);
    let config = Arc::new(Config::load_with_trust(&cwd, trusted)?);
    let state = state_dir()?;
    let env = Arc::new(LocalEnv::new(cwd));
    let ctx = ToolContext::new(
        env,
        state,
        "index",
        kudelma_providers::CancelToken::new(),
        config,
    );
    let tool = IndexTool;
    let output = tool
        .execute(serde_json::json!({ "path": path }), &ctx)
        .await?;
    println!("{}", output.content);
    Ok(())
}

fn trust(action: TrustAction) -> Result<()> {
    let mut store = TrustStore::load()?;
    match action {
        TrustAction::Add { path, yes } => {
            let path = canonicalize_lenient(&path.unwrap_or(std::env::current_dir()?));
            if !yes {
                print!(
                    "Trust {}? Project config, hooks and MCP servers will load. [y/N] ",
                    path.display()
                );
                std::io::stdout().flush()?;
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !matches!(line.trim(), "y" | "Y" | "yes") {
                    println!("Cancelled.");
                    return Ok(());
                }
            }
            store.add(&path)?;
            println!("Trusted {}", path.display());
        }
        TrustAction::Remove { path } => {
            let path = canonicalize_lenient(&path.unwrap_or(std::env::current_dir()?));
            if store.remove(&path)? {
                println!("Removed trust for {}", path.display());
            } else {
                println!("{} was not trusted", path.display());
            }
        }
        TrustAction::List => {
            if store.list().is_empty() {
                println!("No trusted folders.");
            }
            for folder in store.list() {
                println!("{folder}");
            }
        }
    }
    Ok(())
}

fn prompt(args: PromptArgs) -> Result<()> {
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = TrustStore::load()
        .map(|store| store.is_trusted(&cwd))
        .unwrap_or(false);
    let config = Config::load_with_trust(&cwd, trusted)?;
    if args.tools {
        let registry = ToolRegistry::new();
        for tool in builtin_tools() {
            registry.register(tool);
        }
        registry.register(Arc::new(ToolSearchTool::new(registry.clone())));
        let definitions = registry.definitions();
        println!("{}", serde_json::to_string_pretty(&definitions)?);
        return Ok(());
    }
    let mode = if args.plan {
        AgentMode::Plan(
            state_dir()
                .map(|dir| dir.join("plans").join("preview.md"))
                .unwrap_or_else(|_| PathBuf::from("plan.md")),
        )
    } else {
        AgentMode::Build
    };
    println!(
        "{}",
        kudelma_core::prompt::build_system_prompt(&config, &cwd, &mode, None, None)
    );
    Ok(())
}

async fn acp(args: AcpArgs) -> Result<()> {
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = args.trust
        || TrustStore::load()
            .map(|store| store.is_trusted(&cwd))
            .unwrap_or(false);
    let config = Arc::new(Config::load_with_trust(&cwd, trusted)?);
    kudelma_acp::run(kudelma_acp::AcpParams {
        model: args.model,
        yolo: args.yolo,
        trust: args.trust,
        config,
    })
    .await
}

fn config(action: ConfigAction) -> Result<()> {
    let path = global_config_path();
    match action {
        ConfigAction::Path => println!("{}", path.display()),
        ConfigAction::Show => {
            let cwd = canonicalize_lenient(&std::env::current_dir()?);
            let trusted = TrustStore::load()
                .map(|store| store.is_trusted(&cwd))
                .unwrap_or(false);
            let config = Config::load_with_trust(&cwd, trusted)?;
            print!("{}", toml::to_string_pretty(&config)?);
        }
        ConfigAction::Init { force } => {
            if path.exists() && !force {
                bail!("{} already exists (use --force)", path.display());
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            atomic_write(&path, DEFAULT_CONFIG.as_bytes())?;
            println!("Wrote {}", path.display());
        }
    }
    Ok(())
}

fn mcp() -> Result<()> {
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = TrustStore::load()
        .map(|store| store.is_trusted(&cwd))
        .unwrap_or(false);
    let config = Config::load_with_trust(&cwd, trusted)?;
    if config.mcp.is_empty() {
        println!("No MCP servers configured. Add [mcp.<name>] to the config.");
        return Ok(());
    }
    for (name, server) in &config.mcp {
        if let Some(url) = &server.url {
            println!("{name}: {url}");
        } else if let Some(command) = &server.command {
            println!("{name}: {} {}", command, server.args.join(" "));
        } else {
            println!("{name}: (no command or url)");
        }
    }
    Ok(())
}

fn doctor() -> Result<()> {
    println!("Kudelma {}", env!("CARGO_PKG_VERSION"));
    println!("cwd:        {}", std::env::current_dir()?.display());
    println!("config:     {}", global_config_path().display());
    println!(
        "state:      {}",
        state_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_else(|_| "unavailable".into())
    );
    println!(
        "terminal:   {}",
        if std::io::stdout().is_terminal() {
            "interactive"
        } else {
            "not a TTY"
        }
    );
    let store = AuthStore::load()?;
    println!(
        "auth:       {}",
        if store.providers().is_empty() {
            "no stored keys".to_string()
        } else {
            store.providers().join(", ")
        }
    );
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = TrustStore::load()
        .map(|store| store.is_trusted(&cwd))
        .unwrap_or(false);
    let config = Config::load_with_trust(&cwd, trusted)?;
    match resolve_model(&config, Some(&store), &config.default_model) {
        Ok(model) => {
            println!("model:      {}", model.spec());
            match check_credentials(&model) {
                Ok(()) => println!("credentials: ok"),
                Err(error) => println!("credentials: {error}"),
            }
        }
        Err(error) => println!("model:      {error}"),
    }
    let git = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    println!(
        "git:        {}",
        git.unwrap_or_else(|| "not a repository".into())
    );
    Ok(())
}

const DEFAULT_CONFIG: &str = r#"# Kudelma configuration
# Docs: https://github.com/Tjingyeu/Kudelma

default_model = "anthropic/claude-sonnet-4-5"

[ui]
theme = "system"          # system | dark | light
show_thinking_lines = 3
mouse = true

[agent]
max_turns = 50
compaction_percent = 80
bash_timeout_secs = 120
mcp_defer_threshold = 15

[permissions]
default = "prompt"        # allow | deny | prompt
# yolo = false
# [[permissions.rules]]
# tool = "bash"
# scope = "cargo *"
# effect = "allow"

# [providers.ollama]
# kind = "openai"
# base_url = "http://localhost:11434/v1"
# api_key_env = "OLLAMA_API_KEY"

# [mcp.filesystem]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "."]

[hooks]
# pre_tool_use = "path/to/hook.sh"
# post_tool_use = "path/to/hook.sh"
# session_end = "path/to/hook.sh"
"#;

#[allow(dead_code)]
fn unused(_: ProviderPreset, _: &SessionHandle, _: InputFormat, _: OutputFormat) {}
