#![allow(clippy::collapsible_if)]

pub mod app;
pub mod markdown;
pub mod render;
pub mod theme;

use std::io::Stdout;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use kudelma_config::Config;
use kudelma_core::{AgentMode, SessionHandle, SessionParams, spawn_session};
use kudelma_providers::{all_models, resolve_model};
use kudelma_storage::{AuthStore, StoredSession, canonicalize_lenient, plans_dir, state_dir};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::time::interval;

use crate::app::{App, Effect};
use crate::theme::Theme;

#[derive(Clone)]
pub struct TuiParams {
    pub cwd: PathBuf,
    pub config: Arc<Config>,
    pub model: Option<String>,
    pub auth: Option<AuthStore>,
    pub yolo: bool,
    pub trusted: bool,
    pub plan: bool,
    pub mode: Option<String>,
    pub workflow: Option<kudelma_config::WorkflowPreset>,
    pub direct_lead: bool,
    pub max_turns: Option<u32>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub allowed_tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub resume: Option<StoredSession>,
    pub initial_prompt: Option<String>,
}

pub async fn run(params: TuiParams) -> Result<()> {
    let cwd = canonicalize_lenient(&params.cwd);
    let mut session = spawn_session(build_params(
        &params,
        cwd.clone(),
        params.resume.clone(),
        params.plan,
    ))
    .await?;
    let mut control = session.control.clone();
    let mut files = list_files(&cwd);
    let models = all_models(&params.config, params.auth.as_ref());
    let theme = Theme::for_name(&params.config.ui.theme);
    let mut app = App::new(
        session.meta.clone(),
        session.timeline.clone(),
        models,
        files.clone(),
        theme,
        cwd.clone(),
    );
    if let Ok(history) = kudelma_storage::InputHistory::load() {
        app.history = history.lines().to_vec();
    }
    app.role = session.role.clone();
    app.contract = session.contract.clone();
    app.workflow = Some(session.workflow.clone());
    app.skills = Some(session.skills.clone());
    app.set_mode_name(session.meta.mode.clone());

    let (_guard, mut terminal) = TerminalGuard::init(params.config.ui.mouse)?;

    if let Some(prompt) = params.initial_prompt.clone() {
        let effects = app.submit_text(prompt);
        process_effects(
            effects,
            &mut app,
            &mut session,
            &mut control,
            &params,
            &mut files,
        )
        .await;
    }

    let mut events = EventStream::new();
    let mut ticker = interval(Duration::from_millis(80));
    terminal.draw(|frame| render::draw(frame, &mut app))?;

    loop {
        if app.should_quit {
            break;
        }
        let mut effects = Vec::new();
        tokio::select! {
            maybe = events.next() => {
                if let Some(Ok(event)) = maybe {
                    match event {
                        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                            effects = app.handle_key(key);
                        }
                        Event::Paste(text) => app.on_paste(&text),
                        Event::Mouse(mouse) => match mouse.kind {
                            MouseEventKind::ScrollUp => app.on_mouse_scroll(true),
                            MouseEventKind::ScrollDown => app.on_mouse_scroll(false),
                            _ => {}
                        },
                        Event::Resize(_, _) => {}
                        _ => {}
                    }
                }
            }
            event = session.events.recv() => {
                if let Some(event) = event {
                    app.on_event(event);
                } else {
                    app.push_error("agent session ended unexpectedly");
                    app.should_quit = true;
                }
            }
            _ = ticker.tick() => {
                app.tick();
            }
        }
        process_effects(
            effects,
            &mut app,
            &mut session,
            &mut control,
            &params,
            &mut files,
        )
        .await;
        terminal.draw(|frame| render::draw(frame, &mut app))?;
    }

    session.close().await;
    Ok(())
}

async fn process_effects(
    effects: Vec<Effect>,
    app: &mut App,
    session: &mut SessionHandle,
    control: &mut kudelma_core::session::SessionControl,
    params: &TuiParams,
    files: &mut Vec<String>,
) {
    for effect in effects {
        match effect {
            Effect::Prompt(text) => control.prompt(text),
            Effect::Cancel => control.cancel(),
            Effect::Background => control.background(),
            Effect::Quit => app.should_quit = true,
            Effect::Answer(request_id, answer) => {
                if !control.respond_permission(&request_id, answer) {
                    app.push_info("Permission request expired.");
                }
            }
            Effect::Compact => control.compact(None),
            Effect::SetModel(spec) => {
                app.model_spec = spec.clone();
                control.set_model(spec);
            }
            Effect::Rewind(index) => control.rewind(index),
            Effect::SetMode(name) => {
                let path = plans_dir()
                    .unwrap_or_else(|_| state_dir().unwrap_or_else(|_| PathBuf::from(".")))
                    .join(format!("{}.md", session.id));
                if let Some(mode) = AgentMode::from_name(&name, path) {
                    app.set_mode_name(mode.name());
                    control.set_mode(mode);
                } else {
                    app.push_error(format!("unknown mode `{name}`"));
                }
            }
            Effect::ApproveContract(contract_id) => {
                control.approve_contract(contract_id);
                app.push_info("Contract approval sent to Lead.");
            }
            Effect::Verify(command) => {
                app.push_info(format!("Running verification: {command}"));
                control.verify(command);
            }
            Effect::SwitchToAgency => {
                control.switch_to_agency();
                app.role = "agency".into();
            }
            Effect::Shell(command) => match run_shell(app.cwd(), &command).await {
                Ok(output) => app.push_info(output.trim_end().to_string()),
                Err(error) => app.push_error(error),
            },
            Effect::NewSession { resume, cwd } => {
                if resume.as_deref() == Some(session.id.as_str()) {
                    app.push_info("That session is already open.");
                    continue;
                }
                let loaded = match &resume {
                    Some(id) => match kudelma_storage::SessionStore::new()
                        .and_then(|store| store.load(id))
                    {
                        Ok(session) => Some(session),
                        Err(error) => {
                            app.push_error(format!("cannot load session {id}: {error}"));
                            continue;
                        }
                    },
                    None => None,
                };
                let target_cwd = loaded
                    .as_ref()
                    .map(|stored| PathBuf::from(&stored.meta.cwd))
                    .or(cwd)
                    .unwrap_or_else(|| app.cwd().to_path_buf());
                let target_cwd = canonicalize_lenient(&target_cwd);
                if !target_cwd.is_dir() {
                    app.push_error(format!("not a directory: {}", target_cwd.display()));
                    continue;
                }
                let plan = params.plan && loaded.is_none();
                match spawn_session(build_params(params, target_cwd.clone(), loaded, plan)).await {
                    Ok(new_session) => {
                        let old = std::mem::replace(session, new_session);
                        old.close().await;
                        *control = session.control.clone();
                        *files = list_files(&target_cwd);
                        let models = all_models(&params.config, params.auth.as_ref());
                        app.reset(session.meta.clone(), session.timeline.clone(), models);
                        app.files = files.clone();
                        app.set_cwd(target_cwd);
                        app.role = session.role.clone();
                        app.contract = session.contract.clone();
                        app.workflow = Some(session.workflow.clone());
                        app.skills = Some(session.skills.clone());
                        app.set_mode_name(session.meta.mode.clone());
                    }
                    Err(error) => {
                        app.push_error(format!("cannot start session: {error}"));
                    }
                }
            }
        }
    }
}

fn build_params(
    params: &TuiParams,
    cwd: PathBuf,
    resume: Option<StoredSession>,
    plan: bool,
) -> SessionParams {
    let model = params
        .model
        .clone()
        .or_else(|| resume.as_ref().map(|session| session.meta.model.clone()));
    SessionParams {
        cwd,
        config: params.config.clone(),
        model,
        auth: params.auth.clone(),
        yolo: params.yolo,
        trusted: params.trusted,
        resume,
        session_id: None,
        plan,
        mode: params.mode.clone(),
        workflow: params.workflow,
        direct_lead: params.direct_lead,
        auto_approve_contracts: false,
        max_turns: params.max_turns,
        system_prompt: params.system_prompt.clone(),
        append_system_prompt: params.append_system_prompt.clone(),
        allowed_tools: params.allowed_tools.clone(),
        disallowed_tools: params.disallowed_tools.clone(),
        approver: None,
    }
}

fn list_files(cwd: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(cwd)
        .standard_filters(true)
        .hidden(false)
        .follow_links(false)
        .build();
    for entry in walker.flatten() {
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let Some(relative) = path.strip_prefix(cwd).ok() else {
            continue;
        };
        files.push(relative.display().to_string());
        if files.len() >= 5000 {
            break;
        }
    }
    files.sort();
    files
}

async fn run_shell(cwd: &Path, command: &str) -> Result<String, String> {
    let output = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .output()
        .await
        .map_err(|error| format!("shell failed: {error}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if !output.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    if text.trim().is_empty() {
        text = format!("(no output; exit {})", output.status);
    }
    if output.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

struct TerminalGuard {
    mouse: bool,
}

impl TerminalGuard {
    fn init(mouse: bool) -> Result<(Self, Terminal<CrosstermBackend<Stdout>>)> {
        enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
        if mouse {
            execute!(stdout, EnableMouseCapture)?;
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok((Self { mouse }, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = std::io::stdout();
        if self.mouse {
            let _ = execute!(stdout, DisableMouseCapture);
        }
        let _ = execute!(stdout, LeaveAlternateScreen, DisableBracketedPaste);
        let _ = execute!(stdout, crossterm::cursor::Show);
    }
}

#[allow(dead_code)]
fn model_exists(params: &TuiParams) -> bool {
    params
        .model
        .as_deref()
        .and_then(|spec| resolve_model(&params.config, params.auth.as_ref(), spec).ok())
        .is_some()
}
