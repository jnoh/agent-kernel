mod tools;
mod tui;

use kernel_interfaces::framing::{read_message, write_message};
use kernel_interfaces::protocol::{KernelEvent, KernelRequest, SessionCreateConfig};
use kernel_interfaces::types::{CompletionConfig, Decision, ResourceBudget, SessionMode};

use std::env;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Tool display formatting
// ---------------------------------------------------------------------------

/// Format a tool's input JSON into a human-readable one-liner.
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

/// Format a tool's result JSON into a human-readable summary.
fn format_tool_result(tool_name: &str, result: &serde_json::Value) -> String {
    // Check for errors first
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
            // Result is typically a JSON array of filenames
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

    // Parse args
    let args: Vec<String> = env::args().collect();
    let repl_mode = args.iter().any(|a| a == "--repl");

    let socket_path = args
        .iter()
        .position(|a| a == "--socket")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);

    let socket_path = match socket_path {
        Some(p) => p,
        None => {
            // Find the most recently modified daemon socket
            let found = std::fs::read_dir("/tmp").ok().and_then(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name().to_string_lossy().starts_with("agent-kernel-")
                            && e.file_name().to_string_lossy().ends_with(".sock")
                    })
                    .max_by_key(|e| {
                        e.metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                    })
                    .map(|e| e.path())
            });
            match found {
                Some(p) => p,
                None => {
                    eprintln!("No daemon socket found. Start the daemon first:");
                    eprintln!("  agent-kernel-daemon");
                    eprintln!("Or specify --socket <path>");
                    std::process::exit(1);
                }
            }
        }
    };

    if repl_mode {
        run_repl(&socket_path, &workspace);
    } else {
        run_tui(&socket_path, &workspace);
    }
}

// ---------------------------------------------------------------------------
// Shared: connect + register + create session
// ---------------------------------------------------------------------------

struct DaemonConnection {
    writer: std::sync::Arc<std::sync::Mutex<BufWriter<UnixStream>>>,
    reader: BufReader<UnixStream>,
}

fn connect_and_setup(
    socket_path: &std::path::Path,
    workspace: &std::path::Path,
    local_tools: &[Box<dyn kernel_interfaces::tool::ToolRegistration>],
) -> DaemonConnection {
    let stream = UnixStream::connect(socket_path).unwrap_or_else(|e| {
        eprintln!("Failed to connect to {}: {e}", socket_path.display());
        std::process::exit(1);
    });

    let write_stream = stream.try_clone().expect("clone stream");
    let read_stream = stream;

    let writer = std::sync::Arc::new(std::sync::Mutex::new(BufWriter::new(write_stream)));

    let tool_schemas: Vec<_> = local_tools
        .iter()
        .map(|t| tools::to_schema(t.as_ref()))
        .collect();

    let policy = kernel_interfaces::policy::Policy {
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
    };

    let tool_names: Vec<&str> = local_tools.iter().map(|t| t.name()).collect();

    {
        let mut w = writer.lock().unwrap();
        write_message(
            &mut *w,
            &KernelRequest::RegisterTools {
                tools: tool_schemas,
            },
        )
        .expect("send RegisterTools");

        write_message(
            &mut *w,
            &KernelRequest::CreateSession {
                config: SessionCreateConfig {
                    mode: SessionMode::Interactive,
                    system_prompt: format!(
                        "You are a coding assistant. You have access to the following tools: {}. \
                         The workspace root is {}. \
                         Use tools to help the user with their coding tasks. \
                         Be concise and direct.",
                        tool_names.join(", "),
                        workspace.display()
                    ),
                    completion_config: CompletionConfig::default(),
                    policy,
                    resource_budget: ResourceBudget::default(),
                    workspace: workspace.to_string_lossy().into_owned(),
                },
            },
        )
        .expect("send CreateSession");
    }

    let reader = BufReader::new(read_stream);
    DaemonConnection { writer, reader }
}

// ---------------------------------------------------------------------------
// TUI mode
// ---------------------------------------------------------------------------

fn run_tui(socket_path: &std::path::Path, workspace: &std::path::Path) {
    let local_tools = tools::create_tools(workspace);
    let conn = connect_and_setup(socket_path, workspace, &local_tools);
    let writer = conn.writer;

    // Channel: reader thread sends KernelEvents to the TUI main loop
    let (event_tx, event_rx) = mpsc::channel::<KernelEvent>();

    // Spawn reader thread that receives KernelEvents and executes tools
    let writer_for_reader = writer.clone();
    let local_tools_for_reader = tools::create_tools(workspace);
    std::thread::spawn(move || {
        let mut reader = conn.reader;
        loop {
            let kernel_event: KernelEvent = match read_message(&mut reader) {
                Ok(e) => e,
                Err(e) => {
                    if e.kind() != io::ErrorKind::UnexpectedEof {
                        let _ = event_tx.send(KernelEvent::Error {
                            session_id: None,
                            error: kernel_interfaces::frontend::KernelError {
                                message: format!("Read error: {e}"),
                                recoverable: false,
                            },
                        });
                    }
                    break;
                }
            };

            // Handle tool execution on this thread, but also forward the
            // event to the TUI for display.
            match &kernel_event {
                KernelEvent::ExecuteTool {
                    request_id,
                    tool_name,
                    input,
                    session_id,
                } => {
                    // Forward to TUI to show Running entry
                    let _ = event_tx.send(kernel_event.clone());

                    let (result, invalidations) = if let Some(tool) = local_tools_for_reader
                        .iter()
                        .find(|t| t.name() == tool_name)
                    {
                        match tool.execute(input.clone()) {
                            Ok(output) => (output.result, output.invalidations),
                            Err(e) => (serde_json::json!({"error": e.to_string()}), vec![]),
                        }
                    } else {
                        (
                            serde_json::json!({"error": "tool not found", "name": tool_name}),
                            vec![],
                        )
                    };

                    // Send result summary to TUI via ToolCallStarted
                    // (repurposed as "tool completed" notification)
                    let result_summary = format_tool_result(tool_name, &result);
                    let _ = event_tx.send(KernelEvent::ToolCallStarted {
                        session_id: *session_id,
                        tool_name: tool_name.clone(),
                        input: serde_json::json!({ "__result": result_summary }),
                    });

                    let mut w = writer_for_reader.lock().unwrap();
                    let _ = write_message(
                        &mut *w,
                        &KernelRequest::ToolResult {
                            request_id: *request_id,
                            result,
                            invalidations,
                        },
                    );
                }
                _ => {
                    let _ = event_tx.send(kernel_event);
                }
            }
        }
    });

    // Initialize terminal
    let mut terminal = tui::init_terminal().unwrap_or_else(|e| {
        eprintln!("Failed to init terminal: {e}");
        std::process::exit(1);
    });

    let mut app = tui::App::new();

    // Wait briefly for SessionCreated
    std::thread::sleep(Duration::from_millis(100));

    // Drain any initial events
    while let Ok(ev) = event_rx.try_recv() {
        apply_event(&mut app, &ev);
    }

    // Main TUI event loop
    let result = run_tui_loop(&mut terminal, &mut app, &event_rx, &writer);

    // Shutdown
    {
        let mut w = writer.lock().unwrap();
        let _ = write_message(&mut *w, &KernelRequest::Shutdown);
    }

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
    writer: &std::sync::Arc<std::sync::Mutex<BufWriter<UnixStream>>>,
) -> io::Result<()> {
    loop {
        // Only redraw when something changed
        if app.dirty {
            terminal.draw(|frame| tui::draw(frame, app))?;
            app.dirty = false;
        }

        // Poll for crossterm events (keyboard + mouse) with a short timeout
        // so we can also check for kernel events.
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
            // Any action (even None from typing/scrolling) means input was processed
            app.dirty = true;
            match action {
                tui::InputAction::Submit(text) => {
                    app.entries.push(tui::ConversationEntry::UserInput(
                        text.clone(),
                        std::time::SystemTime::now(),
                    ));
                    app.scroll_to_bottom();
                    app.turn_active = true;

                    let mut w = writer.lock().unwrap();
                    let _ = write_message(
                        &mut *w,
                        &KernelRequest::AddInput {
                            session_id: kernel_interfaces::types::SessionId(0),
                            text,
                        },
                    );
                }
                tui::InputAction::PermissionDecision(allow) => {
                    if let Some(req_id) = app.pending_permission_request_id.take() {
                        let decision = if allow {
                            Decision::Allow
                        } else {
                            Decision::Deny("user denied".into())
                        };

                        // Remove the permission prompt entry
                        app.entries.retain(|e| {
                            !matches!(e, tui::ConversationEntry::PermissionPrompt { .. })
                        });

                        app.awaiting_permission = false;

                        let mut w = writer.lock().unwrap();
                        let _ = write_message(
                            &mut *w,
                            &KernelRequest::PermissionResponse {
                                request_id: kernel_interfaces::protocol::RequestId(req_id),
                                decision,
                            },
                        );
                    }
                }
                tui::InputAction::Cancel => {
                    let mut w = writer.lock().unwrap();
                    let _ = write_message(
                        &mut *w,
                        &KernelRequest::CancelTurn {
                            session_id: kernel_interfaces::types::SessionId(0),
                        },
                    );
                }
                tui::InputAction::Quit => return Ok(()),
                tui::InputAction::None => {}
            }
        }

        // Drain kernel events
        let mut turn_ended = false;
        while let Ok(ev) = event_rx.try_recv() {
            if matches!(ev, KernelEvent::TurnEnded { .. }) {
                turn_ended = true;
            }
            apply_event(app, &ev);
            app.dirty = true;
        }

        // Query session status after a turn completes to update context usage
        if turn_ended {
            let mut w = writer.lock().unwrap();
            let _ = write_message(
                &mut *w,
                &KernelRequest::QuerySession {
                    session_id: kernel_interfaces::types::SessionId(0),
                },
            );
        }

        // Advance spinner (only triggers redraw when turn is active)
        if app.turn_active {
            app.spinner_tick = app.spinner_tick.wrapping_add(1);
            app.dirty = true;
        }
    }
}

/// Map a KernelEvent into App state mutations.
fn apply_event(app: &mut tui::App, event: &KernelEvent) {
    match event {
        KernelEvent::SessionCreated { .. } => {
            app.entries
                .push(tui::ConversationEntry::Info("Session created.".into()));
        }

        KernelEvent::TextOutput { text, .. } => {
            // Merge consecutive assistant text entries
            if let Some(tui::ConversationEntry::AssistantText(existing, _)) = app.entries.last_mut()
            {
                existing.push('\n');
                existing.push_str(text);
            } else {
                app.entries.push(tui::ConversationEntry::AssistantText(
                    text.clone(),
                    std::time::SystemTime::now(),
                ));
            }
            app.scroll_to_bottom();
        }

        KernelEvent::ExecuteTool {
            tool_name, input, ..
        } => {
            app.entries.push(tui::ConversationEntry::ToolCall {
                tool_name: tool_name.clone(),
                input_summary: format_tool_input(tool_name, input),
                status: tui::ToolCallStatus::Running(std::time::Instant::now()),
                result_summary: None,
                collapsed: false,
            });
            app.scroll_to_bottom();
        }

        KernelEvent::ToolCallStarted {
            tool_name, input, ..
        } => {
            // Check if this is a result notification (from our reader thread)
            if let Some(result_str) = input.get("__result").and_then(|v| v.as_str()) {
                // Find the matching Running entry and update it with the result
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
                        *status = tui::ToolCallStatus::Success;
                        *result_summary = Some(result_str.to_string());
                        break;
                    }
                }
                app.scroll_to_bottom();
                return;
            }

            // Normal ToolCallStarted from daemon — ignored because
            // ExecuteTool already creates the entry.
            {
                app.scroll_to_bottom();
            }
        }

        KernelEvent::PermissionRequired {
            request_id,
            request,
            ..
        } => {
            app.awaiting_permission = true;
            app.pending_permission_request_id = Some(request_id.0);
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

            // Mark any remaining Running tool calls as Success
            for entry in &mut app.entries {
                if let tui::ConversationEntry::ToolCall { status, .. } = entry
                    && matches!(status, tui::ToolCallStatus::Running(_))
                {
                    *status = tui::ToolCallStatus::Success;
                }
            }

            // Auto-collapse successful tool calls except the last one,
            // to reduce visual noise in multi-tool turns.
            let mut last_tool_idx = None;
            for (i, entry) in app.entries.iter().enumerate().rev() {
                if matches!(
                    entry,
                    tui::ConversationEntry::ToolCall {
                        status: tui::ToolCallStatus::Success,
                        ..
                    }
                ) {
                    last_tool_idx = Some(i);
                    break;
                }
            }
            for (i, entry) in app.entries.iter_mut().enumerate() {
                if let tui::ConversationEntry::ToolCall {
                    status: tui::ToolCallStatus::Success,
                    collapsed,
                    ..
                } = entry
                    && Some(i) != last_tool_idx
                {
                    *collapsed = true;
                }
            }
        }

        KernelEvent::CompactionHappened { summary, .. } => {
            app.entries.push(tui::ConversationEntry::Info(format!(
                "Context compacted: freed {} tokens ({} -> {} turns)",
                summary.tokens_freed, summary.turns_before, summary.turns_after
            )));
        }

        KernelEvent::SessionStatus { tokens_used, .. } => {
            app.context_tokens = *tokens_used;
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
// REPL mode (original behavior, for --repl flag)
// ---------------------------------------------------------------------------

fn run_repl(socket_path: &std::path::Path, workspace: &std::path::Path) {
    let local_tools = tools::create_tools(workspace);
    let tool_names: Vec<&str> = local_tools.iter().map(|t| t.name()).collect();
    let conn = connect_and_setup(socket_path, workspace, &local_tools);
    let writer = conn.writer;

    eprintln!("agent-kernel v0.1.0 — code-agent distribution (IPC client)");
    eprintln!("Workspace: {}", workspace.display());
    eprintln!("Tools: {}", tool_names.join(", "));
    eprintln!("---");

    let writer_for_reader = writer.clone();
    let local_tools_for_reader = tools::create_tools(workspace);
    let reader_handle = std::thread::spawn(move || {
        let mut reader = conn.reader;
        loop {
            let event: KernelEvent = match read_message(&mut reader) {
                Ok(e) => e,
                Err(e) => {
                    if e.kind() != io::ErrorKind::UnexpectedEof {
                        eprintln!("Read error: {e}");
                    }
                    break;
                }
            };

            match event {
                KernelEvent::SessionCreated { session_id } => {
                    eprintln!("Session {session_id:?} created");
                }

                KernelEvent::ExecuteTool {
                    request_id,
                    tool_name,
                    input,
                    ..
                } => {
                    eprintln!(
                        "  [tool] {tool_name}({})",
                        format_tool_input(&tool_name, &input)
                    );

                    let (result, invalidations) = if let Some(tool) = local_tools_for_reader
                        .iter()
                        .find(|t| t.name() == tool_name)
                    {
                        match tool.execute(input) {
                            Ok(output) => {
                                let display = format_tool_result(&tool_name, &output.result);
                                eprintln!("  [result] {tool_name} -> {display}");
                                (output.result, output.invalidations)
                            }
                            Err(e) => (serde_json::json!({"error": e.to_string()}), vec![]),
                        }
                    } else {
                        (
                            serde_json::json!({"error": "tool not found", "name": tool_name}),
                            vec![],
                        )
                    };

                    let mut w = writer_for_reader.lock().unwrap();
                    let _ = write_message(
                        &mut *w,
                        &KernelRequest::ToolResult {
                            request_id,
                            result,
                            invalidations,
                        },
                    );
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

                    let mut w = writer_for_reader.lock().unwrap();
                    let _ = write_message(
                        &mut *w,
                        &KernelRequest::PermissionResponse {
                            request_id,
                            decision,
                        },
                    );
                }

                KernelEvent::TextOutput { text, .. } => {
                    println!("{text}");
                }

                KernelEvent::ToolCallStarted { .. } => {}

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

    std::thread::sleep(std::time::Duration::from_millis(100));

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

        let mut w = writer.lock().unwrap();
        let _ = write_message(
            &mut *w,
            &KernelRequest::AddInput {
                session_id: kernel_interfaces::types::SessionId(0),
                text: input.to_string(),
            },
        );
    }

    {
        let mut w = writer.lock().unwrap();
        let _ = write_message(&mut *w, &KernelRequest::Shutdown);
    }

    let _ = reader_handle.join();
    eprintln!("\nGoodbye.");
}
