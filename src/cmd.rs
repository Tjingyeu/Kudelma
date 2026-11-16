use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use color_eyre::eyre::{Result, bail};
use kudelma_config::{Config, WorkflowPreset, global_config_path};
use kudelma_core::skills::SkillRegistry;
use kudelma_core::workflows::WorkflowStore;
use kudelma_core::{AgentMode, SessionHandle};
use kudelma_providers::{ProviderPreset, all_models, builtin_providers, resolve_model};
use kudelma_storage::{
    ArtifactStore, AuthStore, SessionClaim, SessionStore, StoredSession, TrustStore, atomic_write,
    canonicalize_lenient, new_id, now_rfc3339, state_dir, tasks_dir,
};
use kudelma_tools::{
    IndexTool, LocalEnv, Tool, ToolContext, ToolRegistry, ToolSearchTool, builtin_tools,
};

use crate::cli::{
    AcpArgs, AuthAction, Cli, Command, ConfigAction, InputFormat, OutputFormat, PromptArgs,
    SessionAction, SkillAction, TaskAction, TrustAction, WorkflowAction,
};
use crate::headless::{HeadlessOptions, check_credentials};

pub async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Auth { action }) => auth(action).await,
        Some(Command::Models { .. }) => models(),
        Some(Command::Session { action }) => sessions(action).await,
        Some(Command::Skill { action }) => skills_command(action),
        Some(Command::Workflow { action }) => workflows_command(action).await,
        Some(Command::Task { action }) => tasks_command(action),
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
    let direct_lead = cli.direct_lead || cli.mode.is_some() || cli.plan;

    if interactive {
        let params = kudelma_tui::TuiParams {
            cwd,
            config,
            model: cli.model,
            auth,
            yolo: cli.yolo,
            trusted,
            plan: cli.plan,
            mode: cli.mode,
            workflow: cli.workflow.as_deref().map(parse_workflow),
            direct_lead,
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
        mode: cli.mode,
        workflow: cli.workflow.as_deref().map(parse_workflow),
        direct_lead,
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

fn skills_command(action: SkillAction) -> Result<()> {
    let cwd = canonicalize_lenient(&std::env::current_dir()?);
    let trusted = TrustStore::load()
        .map(|store| store.is_trusted(&cwd))
        .unwrap_or(false);
    let config = Config::load_with_trust(&cwd, trusted)?;
    let registry = SkillRegistry::discover(&config, &cwd, trusted);
    match action {
        SkillAction::List { role, mode } => {
            let mut skills: Vec<_> = registry.list().iter().collect();
            if let (Some(role), Some(mode)) = (role.as_deref(), mode.as_deref()) {
                skills.retain(|skill| skill.applies_to(role, mode));
            } else if let Some(role) = role.as_deref() {
                skills.retain(|skill| {
                    skill.metadata.agents.is_empty()
                        || skill
                            .metadata
                            .agents
                            .iter()
                            .any(|agent| agent.eq_ignore_ascii_case(role))
                });
            } else if let Some(mode) = mode.as_deref() {
                skills.retain(|skill| {
                    skill.metadata.modes.is_empty()
                        || skill
                            .metadata
                            .modes
                            .iter()
                            .any(|candidate| candidate.eq_ignore_ascii_case(mode))
                });
            }
            if skills.is_empty() {
                println!("No matching skills.");
                return Ok(());
            }
            for skill in skills {
                println!(
                    "{:<34} {:<20} {:<9} {}",
                    skill.metadata.name,
                    skill.source.label(),
                    if config.skills.enabled_for(&skill.metadata.name) {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    skill.metadata.description
                );
            }
        }
        SkillAction::Inspect { name } => {
            let Some(skill) = registry.get(&name) else {
                bail!("unknown skill `{name}`");
            };
            println!("name: {}", skill.metadata.name);
            println!("source: {}", skill.source.label());
            println!("description: {}", skill.metadata.description);
            println!("agents: {}", skill.metadata.agents.join(", "));
            println!("modes: {}", skill.metadata.modes.join(", "));
            println!("hash: {}", skill.content_hash);
            println!("\n{}", skill.body);
        }
        SkillAction::Enable { name } => set_skill_enabled(&name, true)?,
        SkillAction::Disable { name } => set_skill_enabled(&name, false)?,
    }
    Ok(())
}

fn set_skill_enabled(name: &str, enabled: bool) -> Result<()> {
    let path = global_config_path();
    let mut root = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
    let root_table = root
        .as_table_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("config is not a TOML table"))?;
    let skills = root_table
        .entry("skills")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let skills = skills
        .as_table_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("[skills] is not a table"))?;
    let disabled = skills
        .entry("disabled")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let disabled = disabled
        .as_array_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("skills.disabled is not an array"))?;
    disabled.retain(|value| value.as_str() != Some(name));
    if !enabled {
        disabled.push(toml::Value::String(name.into()));
    }
    let overrides = skills
        .entry("enabled_overrides")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let overrides = overrides
        .as_array_mut()
        .ok_or_else(|| color_eyre::eyre::eyre!("skills.enabled_overrides is not an array"))?;
    overrides.retain(|value| value.as_str() != Some(name));
    if enabled {
        overrides.push(toml::Value::String(name.into()));
    }
    let text = toml::to_string_pretty(&root)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write(&path, text.as_bytes())?;
    println!(
        "{} skill `{name}` in {}",
        if enabled { "Enabled" } else { "Disabled" },
        path.display()
    );
    Ok(())
}

async fn workflows_command(action: WorkflowAction) -> Result<()> {
    let store = WorkflowStore::new()?;
    match action {
        WorkflowAction::List => {
            println!("Presets:");
            for preset in WorkflowPreset::ALL {
                println!("  {}", preset.name());
            }
            let states = store.list()?;
            if states.is_empty() {
                println!("No saved workflow state.");
            } else {
                println!("Saved runs:");
                for state in states {
                    println!(
                        "  {}  {:<12} {:<8} {:<10} session {}",
                        state.id,
                        state.kind.name(),
                        state.stage.name(),
                        state.mode,
                        state.session_id
                    );
                }
            }
        }
        WorkflowAction::Inspect { id } => {
            if let Ok(state) = store.load(&id) {
                println!("{}", state.render());
                println!(
                    "contract: {}",
                    state.contract_id.as_deref().unwrap_or("none")
                );
                println!("evidence: {}", state.evidence_ids.join(", "));
                if let Ok(dir) = kudelma_storage::workflows_dir() {
                    let ledger_store = ArtifactStore::new(dir);
                    if let Ok(ledger) = ledger_store
                        .load::<kudelma_core::orchestration::ExecutionLedger>(&format!(
                            "ledger-{}",
                            state.id
                        ))
                    {
                        println!(
                            "cost: ${:.4} · tokens: {} · workers: {}/{} · accepted: {}",
                            ledger.total_cost(),
                            ledger.total_tokens(),
                            ledger.workers_succeeded,
                            ledger.workers_started,
                            ledger.accepted
                        );
                    }
                }
            } else if let Some(preset) = WorkflowPreset::ALL
                .iter()
                .find(|preset| preset.name() == id)
            {
                println!("{} ({:?})", preset.name(), preset);
            } else {
                bail!("unknown workflow or saved id `{id}`");
            }
        }
        WorkflowAction::Resume { id } => {
            let state = store.load(&id)?;
            let sessions = SessionStore::new()?;
            let resume = sessions.load(&state.session_id)?;
            let cwd = canonicalize_lenient(&PathBuf::from(&resume.meta.cwd));
            let trusted = TrustStore::load()
                .map(|store| store.is_trusted(&cwd))
                .unwrap_or(false);
            let config = Arc::new(Config::load_with_trust(&cwd, trusted)?);
            let auth = AuthStore::load().ok();
            let params = kudelma_tui::TuiParams {
                cwd,
                config,
                model: Some(resume.meta.model.clone()),
                auth,
                yolo: false,
                trusted,
                plan: false,
                mode: Some(state.mode.clone()),
                workflow: None,
                direct_lead: true,
                max_turns: None,
                system_prompt: None,
                append_system_prompt: None,
                allowed_tools: None,
                disallowed_tools: Vec::new(),
                resume: Some(resume),
                initial_prompt: None,
            };
            return kudelma_tui::run(params).await;
        }
    }
    Ok(())
}

fn tasks_command(action: TaskAction) -> Result<()> {
    let store = ArtifactStore::new(tasks_dir()?);
    match action {
        TaskAction::Inspect { id } => {
            let candidates = [
                id.clone(),
                format!("task-{id}"),
                format!("report-{id}"),
                format!("exploration-{id}"),
                format!("explorer-{id}"),
            ];
            for candidate in candidates {
                if let Ok(value) = store.load::<serde_json::Value>(&candidate) {
                    println!("{}", serde_json::to_string_pretty(&value)?);
                    return Ok(());
                }
            }
            bail!("no task, report or exploration artifact with id `{id}`");
        }
        TaskAction::History => {
            let ids = store.list_ids()?;
            if ids.is_empty() {
                println!("No worker tasks or reports.");
                return Ok(());
            }
            for id in ids {
                if let Ok(value) = store.load::<serde_json::Value>(&id) {
                    let summary = value["summary"]
                        .as_str()
                        .or_else(|| value["objective"].as_str())
                        .or_else(|| value["status"].as_str())
                        .unwrap_or("");
                    println!(
                        "{:<44} {}",
                        id,
                        summary.chars().take(80).collect::<String>()
                    );
                } else {
                    println!("{id}");
                }
            }
        }
    }
    Ok(())
}

fn parse_workflow(raw: &str) -> WorkflowPreset {
    match raw.to_ascii_lowercase().replace('-', "_").as_str() {
        "feature" | "feature_development" => WorkflowPreset::Feature,
        "bug" | "bug_fix" => WorkflowPreset::BugFix,
        "refactor" | "refactoring" => WorkflowPreset::Refactor,
        "simple" | "simple_task" => WorkflowPreset::Simple,
        _ => WorkflowPreset::Adaptive,
    }
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

# Independent role models. Leave unset to fall back to default_model.
[agency]
# provider = "openai"
# model = "gpt-4.1-mini"
max_turns = 10
max_clarification_rounds = 8

[lead]
# provider = "anthropic"
# model = "claude-sonnet-4-5"
max_turns = 40
max_children = 4

[agents.coder]
# provider = "openai"
# model = "gpt-4.1"
max_turns = 25

# Shared read-only exploration is deterministic and has no model of its own.
[exploration]
enabled = true
read_only = true
prefer_deterministic_retrieval = true
max_files = 30
max_results = 100
max_investigation_rounds = 5
allow_external_fetch = true

[workflow]
default = "adaptive"          # adaptive | feature | bug_fix | refactor | simple
require_review = true
require_verification = true
require_spec_approval = "risk_based"   # never | risk_based | always
require_plan_approval = "risk_based"
verification_commands = []    # e.g. ["cargo test --workspace", "cargo clippy -- -D warnings"]

[workflow.execution]
strategy = "adaptive"         # inline | subagent_driven | adaptive
max_parallel_coders = 3
fresh_coder_per_task = true
review_after_each_task = true

[skills]
enabled = true
lazy_loading = true
project_skills_require_trust = true
disabled = []

[budgets]
task_cost_usd = 0.0           # 0 disables the budget
task_wall_clock_secs = 0
# [budgets.role_cost_usd]
# lead = 5.0
# agency = 1.0

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
