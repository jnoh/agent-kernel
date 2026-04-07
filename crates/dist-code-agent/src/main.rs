mod tools;

use kernel_interfaces::framing::{read_message, write_message};
use kernel_interfaces::protocol::{KernelEvent, KernelRequest, SessionCreateConfig};
use kernel_interfaces::types::{CompletionConfig, Decision, ResourceBudget, SessionMode};

use std::env;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

fn main() {
    let workspace = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Parse args
    let socket_path = env::args()
        .position(|a| a == "--socket")
        .and_then(|i| env::args().nth(i + 1))
        .map(PathBuf::from);

    let socket_path = match socket_path {
        Some(p) => p,
        None => {
            // Try to find a running daemon socket
            // Try to find a running daemon socket in /tmp
            let found = std::fs::read_dir("/tmp").ok().and_then(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .find(|e| {
                        e.file_name().to_string_lossy().starts_with("agent-kernel-")
                            && e.file_name().to_string_lossy().ends_with(".sock")
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

    // Connect to daemon
    let stream = UnixStream::connect(&socket_path).unwrap_or_else(|e| {
        eprintln!("Failed to connect to {}: {e}", socket_path.display());
        std::process::exit(1);
    });

    eprintln!("Connected to daemon at {}", socket_path.display());

    let write_stream = stream.try_clone().expect("clone stream");
    let read_stream = stream;

    let writer = std::sync::Arc::new(std::sync::Mutex::new(BufWriter::new(write_stream)));
    let writer_for_reader = writer.clone();

    // Create local tools
    let local_tools = tools::create_tools(&workspace);
    let tool_schemas: Vec<_> = local_tools
        .iter()
        .map(|t| tools::to_schema(t.as_ref()))
        .collect();
    let tool_names: Vec<&str> = local_tools.iter().map(|t| t.name()).collect();

    eprintln!("agent-kernel v0.1.0 — code-agent distribution (IPC client)");
    eprintln!("Workspace: {}", workspace.display());
    eprintln!("Tools: {}", tool_names.join(", "));
    eprintln!("---");

    // Build default policy
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

    // Send RegisterTools
    {
        let mut w = writer.lock().unwrap();
        write_message(
            &mut *w,
            &KernelRequest::RegisterTools {
                tools: tool_schemas,
            },
        )
        .expect("send RegisterTools");

        // Send CreateSession
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

    // Spawn reader thread that handles incoming KernelEvents
    let local_tools_for_reader: Vec<_> = tools::create_tools(&workspace);
    let reader_handle = std::thread::spawn(move || {
        let mut reader = BufReader::new(read_stream);
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
                    // Execute the tool locally
                    let input_str = input.to_string();
                    let display = if input_str.len() > 120 {
                        format!("{}...", &input_str[..120])
                    } else {
                        input_str
                    };
                    eprintln!("  [tool] {tool_name}({display})");

                    let (result, invalidations) = if let Some(tool) = local_tools_for_reader
                        .iter()
                        .find(|t| t.name() == tool_name)
                    {
                        match tool.execute(input) {
                            Ok(output) => {
                                let result_str = output.result.to_string();
                                let display = if result_str.len() > 200 {
                                    format!("{}...", &result_str[..200])
                                } else {
                                    result_str
                                };
                                eprintln!("  [result] {tool_name} → {display}");
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

                KernelEvent::ToolCallStarted { .. } => {
                    // Already handled in ExecuteTool
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

    // Wait for SessionCreated before starting REPL
    std::thread::sleep(std::time::Duration::from_millis(100));

    // REPL: read stdin, send AddInput
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

    // Shutdown
    {
        let mut w = writer.lock().unwrap();
        let _ = write_message(&mut *w, &KernelRequest::Shutdown);
    }

    let _ = reader_handle.join();
    eprintln!("\nGoodbye.");
}
