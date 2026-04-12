//! MCP stdio server for the local-workspace toolset.
//!
//! Blocking newline-delimited JSON-RPC 2.0 loop on stdin/stdout.
//! Implements the minimal MCP subset agent-kernel needs:
//!
//!   * `initialize` — read `params.agent_kernel.root` to pick the
//!     workspace directory, then hold a `LocalWorkspace` for the
//!     process lifetime.
//!   * `tools/list` — advertise every tool from `LocalWorkspace::tools()`.
//!   * `tools/call` — dispatch by name into the cached tool list. The
//!     `shell` tool runs on `ShellTool::run_streaming` and emits each
//!     line as a `notifications/progress` notification carrying an
//!     `agent_kernel/chunk` extension in `params._meta`. Every other
//!     tool routes through the existing in-process `execute` with
//!     `ToolExecutionCtx::null()`.
//!
//! Framing is newline-delimited JSON (one message per line) rather than
//! Content-Length headers. This is an intentional simplification —
//! agent-kernel controls both ends of the first-party pipe. Spec 0016
//! notes that real third-party MCP servers will require proper framing.

use kernel_interfaces::tool::{ToolExecutionCtx, ToolRegistration};
use kernel_interfaces::toolset::ToolSet;
use kernel_workspace_local::{LocalWorkspace, ShellStream, ShellTool};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // State set by `initialize`. The server refuses most requests until
    // `initialize` has been processed.
    let mut workspace: Option<LocalWorkspace> = None;
    let mut tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
    let mut root_path: PathBuf = PathBuf::from(".");

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                send(&mut out, &parse_error(e.to_string()));
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                if let Some(root) = params
                    .get("agent_kernel")
                    .and_then(|v| v.get("root"))
                    .and_then(|v| v.as_str())
                {
                    root_path = PathBuf::from(root);
                }
                let ws = LocalWorkspace::new("workspace.local", root_path.clone());
                tools = ws.tools();
                workspace = Some(ws);
                let result = json!({
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "kernel-workspace-local",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "capabilities": {
                        "tools": { "listChanged": false },
                    },
                });
                send(&mut out, &ok_response(id, result));
            }

            "tools/list" => {
                if workspace.is_none() {
                    send(&mut out, &err_response(id, -32002, "not initialized"));
                    continue;
                }
                let advertised: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name(),
                            "description": t.description(),
                            "inputSchema": t.schema(),
                        })
                    })
                    .collect();
                send(&mut out, &ok_response(id, json!({ "tools": advertised })));
            }

            "tools/call" => {
                if workspace.is_none() {
                    send(&mut out, &err_response(id, -32002, "not initialized"));
                    continue;
                }
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let progress_token = params
                    .get("_meta")
                    .and_then(|v| v.get("progressToken"))
                    .cloned();

                if name == "shell" {
                    let command = match arguments.get("command").and_then(|v| v.as_str()) {
                        Some(c) => c.to_string(),
                        None => {
                            send(
                                &mut out,
                                &err_response(id, -32602, "missing 'command' argument"),
                            );
                            continue;
                        }
                    };
                    // Build a one-shot ShellTool rooted at the workspace
                    // so we can call run_streaming directly. Reusing the
                    // boxed ToolRegistration from `tools` would force us
                    // through the non-streaming `execute` path.
                    let shell = ShellTool::new(root_path.clone());
                    let token = progress_token.clone();
                    let emit = |stream: ShellStream, data: &str| {
                        if let Some(tok) = token.as_ref() {
                            let stream_str = match stream {
                                ShellStream::Stdout => "stdout",
                                ShellStream::Stderr => "stderr",
                            };
                            let notif = json!({
                                "jsonrpc": "2.0",
                                "method": "notifications/progress",
                                "params": {
                                    "progressToken": tok,
                                    "_meta": {
                                        "agent_kernel/chunk": {
                                            "stream": stream_str,
                                            "data": data,
                                        }
                                    }
                                }
                            });
                            // Each notification is one line. If stdout
                            // is broken, the outer loop will exit on the
                            // next read anyway.
                            let _ = writeln!(io::stdout().lock(), "{notif}");
                            let _ = io::stdout().lock().flush();
                        }
                    };
                    match shell.run_streaming(&command, emit) {
                        Ok(result) => {
                            let body = json!({
                                "exit_code": result.exit_code,
                                "stdout": result.stdout,
                                "stderr": result.stderr,
                            });
                            send(
                                &mut out,
                                &ok_response(
                                    id,
                                    json!({
                                        "content": [
                                            { "type": "text", "text": body.to_string() }
                                        ]
                                    }),
                                ),
                            );
                        }
                        Err(e) => {
                            send(&mut out, &err_response(id, -32000, &e));
                        }
                    }
                } else {
                    let tool = tools.iter().find(|t| t.name() == name);
                    let Some(tool) = tool else {
                        send(&mut out, &err_response(id, -32601, "unknown tool"));
                        continue;
                    };
                    match tool.execute(arguments, &ToolExecutionCtx::null()) {
                        Ok(output) => {
                            send(
                                &mut out,
                                &ok_response(
                                    id,
                                    json!({
                                        "content": [
                                            { "type": "text", "text": output.result.to_string() }
                                        ]
                                    }),
                                ),
                            );
                        }
                        Err(e) => {
                            send(&mut out, &err_response(id, -32000, &e.to_string()));
                        }
                    }
                }
            }

            "shutdown" | "exit" => {
                send(&mut out, &ok_response(id, Value::Null));
                break;
            }

            _ => {
                if id.is_some() {
                    send(&mut out, &err_response(id, -32601, "method not found"));
                }
                // Notifications (no id) with unknown methods are silently ignored.
            }
        }
    }
}

fn send<W: Write>(w: &mut W, msg: &Value) {
    let _ = writeln!(w, "{msg}");
    let _ = w.flush();
}

fn ok_response(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result,
    })
}

fn err_response(id: Option<Value>, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": message },
    })
}

fn parse_error(message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": { "code": -32700, "message": format!("parse error: {message}") },
    })
}
