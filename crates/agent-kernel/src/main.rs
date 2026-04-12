mod prompt;
mod provider;
mod tui;

use kernel_core::event_loop::{EventLoop, EventLoopConfig};
use kernel_core::proxy_frontend::{PermissionResponse, ProxyFrontend};
use kernel_core::session_events::{
    FileSink, HttpSink, NullSink, SessionEventSink, TeeSink, default_events_path,
};
use kernel_core::toolset_pool::{ToolsetPool, default_registry};
use kernel_interfaces::protocol::{KernelEvent, KernelRequest, SessionCreateConfig};
use kernel_interfaces::types::{
    CompletionConfig, Decision, ResourceBudget, SessionId, SessionMode,
};

use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Tool display formatting
// ---------------------------------------------------------------------------

fn format_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "file_read" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let mut s = path.to_string();
            if let Some(offset) = input.get("offset").and_then(|v| v.as_u64()) {
                if let Some(limit) = input.get("limit").and_then(|v| v.as_u64()) {
                    s.push_str(&format!(" (lines {}-{})", offset, offset + limit));
                } else {
                    s.push_str(&format!(" (from line {})", offset));
                }
            }
            s
        }
        "file_write" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let content_len = input
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.len())
                .unwrap_or(0);
            format!("{path} ({content_len} bytes)")
        }
        "shell" => input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),
        "grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            format!("/{pattern}/ in {path}")
        }
        "ls" => input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string(),
        _ => {
            let s = input.to_string();
            if s.len() > 120 {
                format!("{}...", &s[..120])
            } else {
                s
            }
        }
    }
}

fn format_tool_result(tool_name: &str, result: &serde_json::Value) -> String {
    if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
        return format!("[error] {err}");
    }

    match tool_name {
        "file_read" => {
            if let Some(content) = result.as_str() {
                let lines: Vec<&str> = content.lines().collect();
                let display_lines = 20;
                let total = lines.len();
                let width = total.to_string().len().max(2);
                let mut out: Vec<String> = lines
                    .iter()
                    .enumerate()
                    .take(display_lines)
                    .map(|(i, l)| format!("{:>width$} │ {l}", i + 1))
                    .collect();
                if total > display_lines {
                    out.push(format!(
                        "{:>width$}   ... ({} more lines)",
                        "",
                        total - display_lines
                    ));
                }
                out.join("\n")
            } else {
                result.to_string()
            }
        }
        "file_write" => {
            if let Some(msg) = result.as_str() {
                msg.to_string()
            } else {
                "written".to_string()
            }
        }
        "shell" => {
            let stdout = result.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
            let exit_code = result.get("exit_code").and_then(|v| v.as_i64());
            let mut out = String::new();
            if let Some(code) = exit_code
                && code != 0
            {
                out.push_str(&format!("[exit {}] ", code));
            }
            let lines: Vec<&str> = stdout.lines().collect();
            let display_lines = 20;
            for line in lines.iter().take(display_lines) {
                out.push_str(line);
                out.push('\n');
            }
            if lines.len() > display_lines {
                out.push_str(&format!("... ({} more lines)", lines.len() - display_lines));
            }
            out.trim_end().to_string()
        }
        "grep" => {
            if let Some(content) = result.as_str() {
                let lines: Vec<&str> = content.lines().collect();
                let display_lines = 20;
                let mut out: Vec<String> = lines
                    .iter()
                    .take(display_lines)
                    .map(|l| l.to_string())
                    .collect();
                if lines.len() > display_lines {
                    out.push(format!("... ({} more lines)", lines.len() - display_lines));
                }
                out.join("\n")
            } else {
                result.to_string()
            }
        }
        "ls" => {
            if let Some(arr) = result.as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join("  ")
            } else if let Some(s) = result.as_str() {
                s.to_string()
            } else {
                result.to_string()
            }
        }
        _ => {
            let s = result.to_string();
            if s.len() > 500 {
                format!("{}...", &s[..500])
            } else {
                s
            }
        }
    }
}

fn main() {
    let workspace = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let args: Vec<String> = env::args().collect();
    let repl_mode = args.iter().any(|a| a == "--repl");

    // --manifest (preferred) or --distro (deprecated alias)
    let manifest_path = args
        .iter()
        .position(|a| a == "--manifest")
        .or_else(|| args.iter().position(|a| a == "--distro"))
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    #[allow(deprecated)]
    let settings = match manifest_path.as_ref() {
        Some(path) => match load_settings(path) {
            Ok(s) => {
                eprintln!(
                    "loaded manifest: {} v{}",
                    s.manifest.distribution.name, s.manifest.distribution.version
                );
                s
            }
            Err(e) => {
                eprintln!("failed to load manifest: {e}");
                eprintln!("falling back to hard-coded defaults");
                Settings::deprecated_defaults()
            }
        },
        None => {
            eprintln!("warning: --manifest not set; using deprecated hard-coded defaults");
            Settings::deprecated_defaults()
        }
    };

    use kernel_interfaces::manifest::FrontendKind;
    let frontend_kind = if repl_mode {
        eprintln!(
            "warning: --repl CLI flag is deprecated; prefer [frontend] type = \"repl\" in the manifest"
        );
        FrontendKind::Repl
    } else {
        settings.frontend
    };

    match frontend_kind {
        FrontendKind::Repl => run_repl(&workspace, settings),
        FrontendKind::Tui => run_tui(&workspace, settings),
    }
}

// ---------------------------------------------------------------------------
// Settings (loaded from manifest or deprecated defaults)
// ---------------------------------------------------------------------------

struct Settings {
    manifest: kernel_interfaces::manifest::DistributionManifest,
    policy: kernel_interfaces::policy::Policy,
    frontend: kernel_interfaces::manifest::FrontendKind,
}

impl Settings {
    #[deprecated(note = "temporary shim while we migrate to manifest-driven config")]
    #[allow(deprecated)]
    fn deprecated_defaults() -> Self {
        Self {
            manifest: kernel_interfaces::manifest::DistributionManifest {
                distribution: kernel_interfaces::manifest::DistributionMeta {
                    name: "code-agent".into(),
                    version: "0.1.0".into(),
                },
                provider: kernel_interfaces::manifest::ProviderConfig::Echo,
                policy: None,
                toolsets: Vec::new(),
                frontend: None,
            },
            policy: default_policy(),
            frontend: kernel_interfaces::manifest::FrontendKind::Tui,
        }
    }
}

fn load_settings(manifest_path: &std::path::Path) -> Result<Settings, String> {
    let manifest = kernel_interfaces::manifest::load_manifest(manifest_path)?;
    let manifest_dir = kernel_interfaces::manifest::manifest_dir(manifest_path);

    let policy = match manifest.policy.as_ref() {
        Some(cfg) => {
            let path = cfg.resolve(&manifest_dir);
            let yaml = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read policy file {}: {e}", path.display()))?;
            serde_yaml::from_str::<kernel_interfaces::policy::Policy>(&yaml)
                .map_err(|e| format!("failed to parse policy file {}: {e}", path.display()))?
        }
        None => {
            return Err("manifest has no [policy] section".into());
        }
    };

    let frontend = manifest
        .frontend
        .as_ref()
        .map(|f| f.kind)
        .unwrap_or(kernel_interfaces::manifest::FrontendKind::Tui);

    Ok(Settings {
        manifest,
        policy,
        frontend,
    })
}

#[deprecated(note = "hard-coded policy; pass --manifest with a [policy] section instead")]
fn default_policy() -> kernel_interfaces::policy::Policy {
    kernel_interfaces::policy::Policy {
        version: 1,
        name: "default-permissive".into(),
        rules: vec![
            kernel_interfaces::policy::PolicyRule {
                match_capabilities: vec!["fs:read".into(), "fs:write".into()],
                action: kernel_interfaces::policy::PolicyAction::Allow,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            },
            kernel_interfaces::policy::PolicyRule {
                match_capabilities: vec!["shell:exec".into(), "net:*".into()],
                action: kernel_interfaces::policy::PolicyAction::Ask,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            },
        ],
        resource_budgets: None,
    }
}

fn prepend_allow_rule(policy: &mut kernel_interfaces::policy::Policy, capabilities: Vec<String>) {
    policy.rules.insert(
        0,
        kernel_interfaces::policy::PolicyRule {
            match_capabilities: capabilities,
            action: kernel_interfaces::policy::PolicyAction::Allow,
            scope_paths: Vec::new(),
            scope_commands: Vec::new(),
            except: Vec::new(),
        },
    );
}

// ---------------------------------------------------------------------------
// Session setup
// ---------------------------------------------------------------------------

struct SessionHandle {
    input_tx: crossbeam_channel::Sender<KernelRequest>,
    event_rx: crossbeam_channel::Receiver<KernelEvent>,
    permission_tx: crossbeam_channel::Sender<PermissionResponse>,
    _thread: std::thread::JoinHandle<()>,
}

fn setup_session(workspace: &std::path::Path, settings: &Settings) -> SessionHandle {
    let session_id = SessionId(0);

    // Build provider
    let provider_factory =
        provider::build_provider_factory(&settings.manifest).unwrap_or_else(|e| {
            eprintln!("failed to build provider: {e}");
            std::process::exit(1);
        });
    let provider = provider_factory();

    // Build toolset pool
    let registry = default_registry();
    let pool = ToolsetPool::build(&settings.manifest.toolsets, &registry).unwrap_or_else(|e| {
        eprintln!("failed to build toolset pool: {e}");
        std::process::exit(1);
    });
    let tools = pool.tools_for_session();
    eprintln!(
        "tools: {}",
        tools
            .iter()
            .map(|t| t.name())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Channels
    let (input_tx, input_rx) = crossbeam_channel::unbounded();
    let (event_tx, event_rx) = crossbeam_channel::unbounded();
    let (permission_tx, permission_rx) = crossbeam_channel::unbounded();

    // Build system prompt
    let tool_names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
    let system_prompt = prompt::build_system_prompt(&prompt::PromptContext {
        workspace: workspace.display().to_string(),
        tool_names,
    });

    // ProxyFrontend sends KernelEvents to event_tx
    let frontend = ProxyFrontend::new(
        session_id,
        event_tx.clone(),
        permission_rx,
        Duration::from_secs(300),
    );

    // Session event sink
    let events: Box<dyn SessionEventSink> = {
        let log_path = default_events_path(session_id);
        let local: Box<dyn SessionEventSink> = match FileSink::new(session_id, &log_path) {
            Ok(sink) => Box::new(sink),
            Err(e) => {
                eprintln!(
                    "session_events: failed to open {} ({e}); using NullSink",
                    log_path.display()
                );
                Box::new(NullSink::new(session_id))
            }
        };

        match std::env::var("AGENT_KERNEL_REMOTE_SINK_URL") {
            Ok(url) if !url.is_empty() => {
                let token = std::env::var("AGENT_KERNEL_REMOTE_SINK_TOKEN").ok();
                match HttpSink::new(session_id, &url, token) {
                    Ok(remote) => {
                        eprintln!(
                            "session_events: teeing to remote sink {}",
                            remote.endpoint()
                        );
                        Box::new(TeeSink::new(local, remote))
                    }
                    Err(e) => {
                        eprintln!("session_events: bad remote sink URL ({e}); local only");
                        local
                    }
                }
            }
            _ => local,
        }
    };

    let config = EventLoopConfig {
        session_id,
        session_create: SessionCreateConfig {
            mode: SessionMode::Interactive,
            system_prompt,
            completion_config: CompletionConfig::default(),
            policy: settings.policy.clone(),
            resource_budget: ResourceBudget::default(),
            workspace: workspace.to_string_lossy().into_owned(),
        },
        tools,
        provider,
        frontend,
        events,
    };

    let mut event_loop = EventLoop::new(config, input_rx, event_tx);
    let thread = std::thread::spawn(move || {
        event_loop.run();
    });

    SessionHandle {
        input_tx,
        event_rx,
        permission_tx,
        _thread: thread,
    }
}

// ---------------------------------------------------------------------------
// TUI mode
// ---------------------------------------------------------------------------

fn run_tui(workspace: &std::path::Path, settings: Settings) {
    let mut current_policy = settings.policy.clone();
    let session = setup_session(workspace, &settings);

    let (ui_tx, ui_rx) = mpsc::channel::<KernelEvent>();

    // Bridge crossbeam → mpsc so the TUI main loop can use try_recv
    let event_rx = session.event_rx;
    std::thread::spawn(move || {
        for event in event_rx {
            if ui_tx.send(event).is_err() {
                break;
            }
        }
    });

    let mut terminal = tui::init_terminal().unwrap_or_else(|e| {
        eprintln!("Failed to init terminal: {e}");
        std::process::exit(1);
    });

    let mut app = tui::App::new();

    std::thread::sleep(Duration::from_millis(100));
    while let Ok(ev) = ui_rx.try_recv() {
        apply_event(&mut app, &ev);
    }

    let result = run_tui_loop(
        &mut terminal,
        &mut app,
        &ui_rx,
        &session.input_tx,
        &session.permission_tx,
        &mut current_policy,
    );

    let _ = session.input_tx.send(KernelRequest::Shutdown);

    tui::restore_terminal();

    if let Err(e) = result {
        eprintln!("TUI error: {e}");
    }

    eprintln!("\nGoodbye.");
}

fn run_tui_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    app: &mut tui::App,
    event_rx: &mpsc::Receiver<KernelEvent>,
    input_tx: &crossbeam_channel::Sender<KernelRequest>,
    permission_tx: &crossbeam_channel::Sender<PermissionResponse>,
    current_policy: &mut kernel_interfaces::policy::Policy,
) -> io::Result<()> {
    loop {
        if app.dirty {
            terminal.draw(|frame| tui::draw(frame, app))?;
            app.dirty = false;
        }

        if crossterm::event::poll(Duration::from_millis(50))? {
            let action = match crossterm::event::read()? {
                crossterm::event::Event::Key(key) => tui::handle_key(app, key),
                crossterm::event::Event::Mouse(mouse) => tui::handle_mouse(app, mouse),
                crossterm::event::Event::Resize(_, _) => {
                    app.dirty = true;
                    tui::InputAction::None
                }
                _ => tui::InputAction::None,
            };
            app.dirty = true;
            match action {
                tui::InputAction::Submit(text) => {
                    app.entries
                        .push(tui::ConversationEntry::UserInput(text.clone()));
                    app.scroll_to_bottom();
                    app.turn_active = true;
                    let _ = input_tx.send(KernelRequest::AddInput {
                        session_id: SessionId(0),
                        text,
                    });
                }
                tui::InputAction::PermissionDecision(allow) => {
                    if let Some(req_id) = app.pending_permission_request_id.take() {
                        let decision = if allow {
                            Decision::Allow
                        } else {
                            Decision::Deny("user denied".into())
                        };
                        app.entries.retain(|e| {
                            !matches!(e, tui::ConversationEntry::PermissionPrompt { .. })
                        });
                        app.awaiting_permission = false;
                        app.pending_permission_capabilities = None;
                        let _ = permission_tx.send(PermissionResponse {
                            request_id: kernel_interfaces::protocol::RequestId(req_id),
                            decision,
                        });
                    }
                }
                tui::InputAction::PermissionAlwaysAllow => {
                    if let (Some(req_id), Some(capabilities)) = (
                        app.pending_permission_request_id.take(),
                        app.pending_permission_capabilities.take(),
                    ) {
                        prepend_allow_rule(current_policy, capabilities);
                        app.entries.retain(|e| {
                            !matches!(e, tui::ConversationEntry::PermissionPrompt { .. })
                        });
                        app.awaiting_permission = false;
                        let _ = input_tx.send(KernelRequest::SetPolicy {
                            session_id: SessionId(0),
                            policy: current_policy.clone(),
                        });
                        let _ = permission_tx.send(PermissionResponse {
                            request_id: kernel_interfaces::protocol::RequestId(req_id),
                            decision: Decision::Allow,
                        });
                    }
                }
                tui::InputAction::Cancel => {
                    let _ = input_tx.send(KernelRequest::CancelTurn {
                        session_id: SessionId(0),
                    });
                }
                tui::InputAction::SlashCommand(cmd) => match cmd {
                    tui::SlashCommand::Clear => {
                        app.entries.clear();
                        app.scroll_to_bottom();
                    }
                    tui::SlashCommand::Compact => {
                        let _ = input_tx.send(KernelRequest::RequestCompaction {
                            session_id: SessionId(0),
                        });
                    }
                    tui::SlashCommand::Status => {
                        let _ = input_tx.send(KernelRequest::QuerySession {
                            session_id: SessionId(0),
                        });
                    }
                    tui::SlashCommand::Quit => return Ok(()),
                    tui::SlashCommand::Unknown(name) => {
                        app.entries.push(tui::ConversationEntry::Error(format!(
                            "unknown command: {name}"
                        )));
                        app.scroll_to_bottom();
                    }
                },
                tui::InputAction::Quit => return Ok(()),
                tui::InputAction::None => {}
            }
        }

        let mut turn_ended = false;
        while let Ok(ev) = event_rx.try_recv() {
            if matches!(ev, KernelEvent::TurnEnded { .. }) {
                turn_ended = true;
            }
            apply_event(app, &ev);
            app.dirty = true;
        }

        if turn_ended {
            let _ = input_tx.send(KernelRequest::QuerySession {
                session_id: SessionId(0),
            });
        }

        if app.turn_active {
            app.spinner_tick = app.spinner_tick.wrapping_add(1);
            app.dirty = true;
        }
    }
}

fn apply_event(app: &mut tui::App, event: &KernelEvent) {
    match event {
        KernelEvent::SessionCreated { .. } => {
            app.entries
                .push(tui::ConversationEntry::Info("Session created.".into()));
        }

        KernelEvent::TextOutput { text, .. } => {
            if let Some(tui::ConversationEntry::AssistantText(existing)) = app.entries.last_mut() {
                existing.push('\n');
                existing.push_str(text);
            } else {
                app.entries
                    .push(tui::ConversationEntry::AssistantText(text.clone()));
            }
            app.scroll_to_bottom();
        }

        KernelEvent::ToolCallStarted {
            tool_name, input, ..
        } => {
            app.entries.push(tui::ConversationEntry::ToolCall {
                tool_name: tool_name.clone(),
                input_summary: format_tool_input(tool_name, input),
                status: tui::ToolCallStatus::Running(std::time::Instant::now()),
                result_summary: None,
            });
            app.scroll_to_bottom();
        }

        KernelEvent::ToolOutputChunk {
            tool_name, data, ..
        } => {
            for entry in app.entries.iter_mut().rev() {
                if let tui::ConversationEntry::ToolCall {
                    tool_name: n,
                    status,
                    result_summary,
                    ..
                } = entry
                    && n == tool_name
                    && matches!(status, tui::ToolCallStatus::Running(_))
                {
                    let existing = result_summary.take().unwrap_or_default();
                    *result_summary = Some(format!("{existing}{data}"));
                    break;
                }
            }
            app.scroll_to_bottom();
        }

        KernelEvent::ToolCompleted {
            tool_name, result, ..
        } => {
            let summary = format_tool_result(tool_name, result);
            for entry in app.entries.iter_mut().rev() {
                if let tui::ConversationEntry::ToolCall {
                    tool_name: n,
                    status,
                    result_summary,
                    ..
                } = entry
                    && n == tool_name
                    && let tui::ToolCallStatus::Running(start) = status
                {
                    let duration = start.elapsed();
                    *status = tui::ToolCallStatus::Success(duration);
                    *result_summary = Some(summary);
                    break;
                }
            }
            app.scroll_to_bottom();
        }

        KernelEvent::PermissionRequired {
            request_id,
            request,
            ..
        } => {
            app.awaiting_permission = true;
            app.pending_permission_request_id = Some(request_id.0);
            app.pending_permission_capabilities = Some(request.capabilities.clone());
            app.entries.push(tui::ConversationEntry::PermissionPrompt {
                tool_name: request.tool_name.clone(),
                capabilities: request.capabilities.clone(),
                input_summary: request.input_summary.clone(),
            });
            app.scroll_to_bottom();
        }

        KernelEvent::TurnStarted { .. } => {
            app.turn_active = true;
        }

        KernelEvent::TurnEnded { result, .. } => {
            app.turn_active = false;
            app.turn_count += 1;
            app.total_input_tokens += result.input_tokens;
            app.total_output_tokens += result.output_tokens;

            for entry in &mut app.entries {
                if let tui::ConversationEntry::ToolCall { status, .. } = entry
                    && let tui::ToolCallStatus::Running(start) = status
                {
                    *status = tui::ToolCallStatus::Success(start.elapsed());
                }
            }
        }

        KernelEvent::CompactionHappened { summary, .. } => {
            app.entries.push(tui::ConversationEntry::Info(format!(
                "Context compacted: freed {} tokens ({} -> {} turns)",
                summary.tokens_freed, summary.turns_before, summary.turns_after
            )));
        }

        KernelEvent::SessionStatus {
            tokens_used,
            utilization,
            turn_count,
            ..
        } => {
            app.context_tokens = *tokens_used;
            app.entries.push(tui::ConversationEntry::Info(format!(
                "Session status: {} tokens used ({:.1}% of context), {} turns",
                tokens_used,
                utilization * 100.0,
                turn_count,
            )));
            app.scroll_to_bottom();
        }

        KernelEvent::Error { error, .. } => {
            app.entries
                .push(tui::ConversationEntry::Error(error.message.clone()));
            if !error.recoverable {
                app.turn_active = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// REPL mode
// ---------------------------------------------------------------------------

fn run_repl(workspace: &std::path::Path, settings: Settings) {
    let session = setup_session(workspace, &settings);

    eprintln!("agent-kernel v0.1.0");
    eprintln!("Workspace: {}", workspace.display());
    eprintln!("---");

    let event_rx = session.event_rx;
    let perm_tx = session.permission_tx.clone();
    std::thread::spawn(move || {
        for event in event_rx {
            match event {
                KernelEvent::SessionCreated { session_id } => {
                    eprintln!("Session {session_id:?} created");
                }
                KernelEvent::ToolCallStarted {
                    tool_name, input, ..
                } => {
                    eprintln!(
                        "  [tool] {tool_name}({})",
                        format_tool_input(&tool_name, &input)
                    );
                }
                KernelEvent::ToolCompleted {
                    tool_name, result, ..
                } => {
                    let display = format_tool_result(&tool_name, &result);
                    eprintln!("  [result] {tool_name} -> {display}");
                }
                KernelEvent::ToolOutputChunk {
                    tool_name, data, ..
                } => {
                    eprintln!("  [tool] {tool_name}: {}", data.trim_end());
                }
                KernelEvent::PermissionRequired {
                    request_id,
                    request,
                    ..
                } => {
                    eprint!(
                        "  [permission] {} requires [{}]. Allow? (y/n) ",
                        request.tool_name,
                        request.capabilities.join(", ")
                    );
                    io::stderr().flush().ok();

                    let mut input = String::new();
                    let decision = if io::stdin().read_line(&mut input).is_ok() {
                        let answer = input.trim().to_lowercase();
                        if answer == "y" || answer == "yes" {
                            Decision::Allow
                        } else {
                            Decision::Deny("user denied".into())
                        }
                    } else {
                        Decision::Deny("failed to read input".into())
                    };

                    let _ = perm_tx.send(PermissionResponse {
                        request_id,
                        decision,
                    });
                }
                KernelEvent::TextOutput { text, .. } => {
                    println!("{text}");
                }
                KernelEvent::TurnStarted { .. } => {}
                KernelEvent::TurnEnded { result, .. } => {
                    if result.cache_read_input_tokens > 0 || result.cache_creation_input_tokens > 0
                    {
                        eprintln!(
                            "  [tokens] in={} out={} cache_read={} cache_write={}",
                            result.input_tokens,
                            result.output_tokens,
                            result.cache_read_input_tokens,
                            result.cache_creation_input_tokens
                        );
                    } else if result.input_tokens > 0 {
                        eprintln!(
                            "  [tokens] in={} out={}",
                            result.input_tokens, result.output_tokens
                        );
                    }
                }
                KernelEvent::CompactionHappened { summary, .. } => {
                    eprintln!("  [compaction] freed {} tokens", summary.tokens_freed);
                }
                KernelEvent::SessionStatus { .. } => {}
                KernelEvent::Error { error, .. } => {
                    eprintln!("  [error] {}", error.message);
                }
            }
        }
    });

    std::thread::sleep(Duration::from_millis(100));

    let stdin = io::stdin();
    let mut stdin_reader = stdin.lock();

    loop {
        eprint!("> ");
        io::stderr().flush().ok();

        let mut input = String::new();
        match stdin_reader.read_line(&mut input) {
            Ok(0) => break,
            Err(e) => {
                eprintln!("Error reading input: {e}");
                break;
            }
            Ok(_) => {}
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        if input == "/quit" || input == "/exit" {
            break;
        }

        let _ = session.input_tx.send(KernelRequest::AddInput {
            session_id: SessionId(0),
            text: input.to_string(),
        });
    }

    let _ = session.input_tx.send(KernelRequest::Shutdown);
    eprintln!("\nGoodbye.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel_interfaces::policy::{Policy, PolicyAction, PolicyRule};
    use kernel_interfaces::types::{Capability, Decision};

    fn policy_asking_shell() -> Policy {
        Policy {
            version: 1,
            name: "test".into(),
            rules: vec![PolicyRule {
                match_capabilities: vec!["shell:exec".into()],
                action: PolicyAction::Ask,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            }],
            resource_budgets: None,
        }
    }

    #[test]
    fn prepend_allow_rule_shadows_later_ask() {
        let mut policy = policy_asking_shell();
        assert_eq!(
            policy.evaluate(&Capability::new("shell:exec")),
            Decision::Ask
        );
        prepend_allow_rule(&mut policy, vec!["shell:exec".into()]);
        assert_eq!(
            policy.evaluate(&Capability::new("shell:exec")),
            Decision::Allow
        );
        assert_eq!(policy.rules.len(), 2);
        assert_eq!(policy.rules[0].action, PolicyAction::Allow);
    }

    #[test]
    fn prepend_allow_rule_preserves_other_capabilities() {
        let mut policy = policy_asking_shell();
        prepend_allow_rule(&mut policy, vec!["net:api.github.com".into()]);
        assert_eq!(
            policy.evaluate(&Capability::new("shell:exec")),
            Decision::Ask
        );
        assert_eq!(
            policy.evaluate(&Capability::new("net:api.github.com")),
            Decision::Allow
        );
    }
}
