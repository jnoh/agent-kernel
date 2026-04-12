//! MCP stdio transport — a `ToolSet` implementation that proxies tools
//! across a JSON-RPC 2.0 pipe to a subprocess.
//!
//! The daemon's factory registry gains a `mcp.stdio` kind (added in
//! spec 0016) that routes to [`from_entry`]. The factory spawns the
//! command named in `entry.config.command` with `entry.config.args`,
//! runs an MCP `initialize` + `tools/list` handshake, caches the
//! advertised schemas, and returns a boxed [`McpStdioToolSet`]. The
//! daemon's `ToolsetPool` treats it identically to any in-process
//! `ToolSet` — `tools()` hands back [`McpToolHandle`] instances whose
//! `execute` path serializes `tools/call` through a `Mutex`-guarded
//! [`McpClient`].
//!
//! **Wire format.** Newline-delimited JSON-RPC (one message per line).
//! The MCP spec uses Content-Length headers, but agent-kernel controls
//! both ends of its first-party pipe, so newline framing is enough for
//! v0.2. A later spec will upgrade to proper framing before real
//! third-party MCP servers are supported.
//!
//! **Streaming.** When a `tools/call` sends back
//! `notifications/progress` with an `agent_kernel/chunk` extension in
//! `params._meta`, [`McpToolHandle::execute`] forwards each one
//! through [`ToolExecutionCtx::emit_chunk`] and also buffers the
//! concatenated text so it can feed the model a single `tool_result`
//! at call end. If the final response carries non-empty `content`, that
//! wins; otherwise the buffered chunks become the result.
//!
//! **Crash recovery.** A stdout EOF during a call flips a `dead` flag
//! on the client. The next `tools/call` attempts one synchronous
//! respawn via the stored [`ToolsetEntry`] + `initialize` payload. No
//! backoff, no retries, no health checks.

use kernel_interfaces::manifest::ToolsetEntry;
use kernel_interfaces::tool::{
    ToolChunk, ToolChunkStream, ToolError, ToolExecutionCtx, ToolOutput, ToolRegistration,
};
use kernel_interfaces::toolset::ToolSet;
use kernel_interfaces::types::{Capability, CapabilitySet, RelevanceSignal, TokenEstimate};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

/// A tool advertised by an MCP server, cached at handshake time so
/// repeated `tools()` calls don't re-query the subprocess.
#[derive(Debug, Clone)]
pub struct CachedTool {
    name: String,
    description: String,
    input_schema: Value,
    capabilities: CapabilitySet,
    cost: TokenEstimate,
    relevance: RelevanceSignal,
}

/// Guess capabilities from a tool name. MCP's `tools/list` response
/// does not advertise capabilities, and 0016 ships the first-party
/// workspace server which controls both ends of the wire — a proper
/// schema-level capability convention is a follow-up spec.
fn infer_capabilities(name: &str) -> CapabilitySet {
    let mut set = CapabilitySet::new();
    let lower = name.to_ascii_lowercase();
    if lower == "shell" || lower.contains("exec") {
        set.insert(Capability::new("shell:exec"));
    } else if lower.contains("write") || lower.contains("edit") || lower.contains("create") {
        set.insert(Capability::new("fs:write"));
    } else {
        set.insert(Capability::new("fs:read"));
    }
    set
}

/// Low-level JSON-RPC client over a child's stdio pipes. Not public —
/// every caller goes through [`McpStdioToolSet`] / [`McpToolHandle`].
struct McpClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    next_progress: u64,
    dead: bool,
}

impl McpClient {
    fn spawn(command: &str, args: &[String]) -> Result<Self, String> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("failed to spawn {command:?}: {e}"))?;
        let stdin = BufWriter::new(child.stdin.take().expect("stdin piped"));
        let stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
        Ok(Self {
            child,
            stdin,
            stdout,
            next_id: 1,
            next_progress: 1,
            dead: false,
        })
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn alloc_progress(&mut self) -> String {
        let n = self.next_progress;
        self.next_progress += 1;
        format!("p{n}")
    }

    fn write_request(&mut self, req: &Value) -> Result<(), String> {
        let line = req.to_string();
        if let Err(e) = writeln!(self.stdin, "{line}") {
            self.dead = true;
            return Err(format!("stdin write: {e}"));
        }
        if let Err(e) = self.stdin.flush() {
            self.dead = true;
            return Err(format!("stdin flush: {e}"));
        }
        Ok(())
    }

    /// Read one JSON-RPC message. `Ok(None)` means EOF.
    fn read_message(&mut self) -> Result<Option<Value>, String> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| format!("stdout read: {e}"))?;
            if n == 0 {
                self.dead = true;
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            let msg: Value = serde_json::from_str(line.trim())
                .map_err(|e| format!("malformed MCP message: {e}"))?;
            return Ok(Some(msg));
        }
    }

    /// Blocking request/response, dropping unrelated traffic.
    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.alloc_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_request(&req)?;
        loop {
            match self.read_message()? {
                None => return Err(format!("subprocess EOF while waiting for {method}")),
                Some(msg) => {
                    if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        if let Some(err) = msg.get("error") {
                            return Err(format!("MCP error on {method}: {err}"));
                        }
                        return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                    }
                    // Drop unrelated notifications / other ids.
                }
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A `ToolSet` whose tools live in an MCP subprocess.
pub struct McpStdioToolSet {
    id: String,
    client: Arc<Mutex<McpClient>>,
    cache: Arc<Vec<CachedTool>>,
    /// Stored for synchronous respawn on crash.
    respawn: RespawnInfo,
}

#[derive(Clone)]
struct RespawnInfo {
    command: String,
    args: Vec<String>,
    init_params: Value,
}

impl McpStdioToolSet {
    /// Open a new MCP subprocess, run `initialize` + `tools/list`,
    /// and cache the advertised tool schemas.
    fn connect(
        id: String,
        command: String,
        args: Vec<String>,
        init_params: Value,
    ) -> Result<Self, String> {
        let mut client = McpClient::spawn(&command, &args)?;
        let init_request = json!({
            "protocolVersion": "2024-11-05",
            "clientInfo": { "name": "agent-kernel", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": {},
            "agent_kernel": init_params.clone(),
        });
        let _ = client.request("initialize", init_request)?;
        let list = client.request("tools/list", json!({}))?;
        let cache = parse_tools_list(&list)?;
        Ok(Self {
            id,
            client: Arc::new(Mutex::new(client)),
            cache: Arc::new(cache),
            respawn: RespawnInfo {
                command,
                args,
                init_params,
            },
        })
    }
}

fn parse_tools_list(result: &Value) -> Result<Vec<CachedTool>, String> {
    let arr = result
        .get("tools")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "tools/list response missing 'tools' array".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for t in arr {
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "tool entry missing name".to_string())?
            .to_string();
        let description = t
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let input_schema = t.get("inputSchema").cloned().unwrap_or(Value::Null);
        let capabilities = infer_capabilities(&name);
        out.push(CachedTool {
            name,
            description,
            input_schema,
            capabilities,
            cost: TokenEstimate(150),
            relevance: RelevanceSignal {
                keywords: Vec::new(),
                tags: vec!["mcp".into()],
            },
        });
    }
    Ok(out)
}

impl ToolSet for McpStdioToolSet {
    fn id(&self) -> &str {
        &self.id
    }

    fn tools(&self) -> Vec<Box<dyn ToolRegistration>> {
        (0..self.cache.len())
            .map(|idx| {
                Box::new(McpToolHandle {
                    idx,
                    cache: Arc::clone(&self.cache),
                    client: Arc::clone(&self.client),
                    respawn: self.respawn.clone(),
                }) as Box<dyn ToolRegistration>
            })
            .collect()
    }
}

/// One tool handle pointing at a cached schema entry and sharing the
/// toolset's subprocess client.
pub struct McpToolHandle {
    idx: usize,
    cache: Arc<Vec<CachedTool>>,
    client: Arc<Mutex<McpClient>>,
    respawn: RespawnInfo,
}

impl McpToolHandle {
    fn entry(&self) -> &CachedTool {
        &self.cache[self.idx]
    }
}

impl ToolRegistration for McpToolHandle {
    fn name(&self) -> &str {
        &self.entry().name
    }
    fn description(&self) -> &str {
        &self.entry().description
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.entry().capabilities
    }
    fn schema(&self) -> &Value {
        &self.entry().input_schema
    }
    fn cost(&self) -> TokenEstimate {
        self.entry().cost
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.entry().relevance
    }

    fn execute(&self, input: Value, ctx: &ToolExecutionCtx<'_>) -> Result<ToolOutput, ToolError> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| ToolError::Transport("MCP client mutex poisoned".into()))?;

        match self.try_call(&mut client, input.clone(), ctx) {
            Ok(out) => Ok(out),
            Err(CallError::Execution(e)) => Err(ToolError::ExecutionFailed(e)),
            Err(CallError::Transport(_)) => {
                // One synchronous respawn attempt, per the spec's
                // crash-recovery model. On success, retry the call
                // once; on failure return Transport.
                let fresh = McpClient::spawn(&self.respawn.command, &self.respawn.args)
                    .map_err(|e| ToolError::Transport(format!("respawn failed: {e}")))?;
                *client = fresh;
                let init_req = json!({
                    "protocolVersion": "2024-11-05",
                    "clientInfo": { "name": "agent-kernel", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": {},
                    "agent_kernel": self.respawn.init_params.clone(),
                });
                client
                    .request("initialize", init_req)
                    .map_err(|e| ToolError::Transport(format!("respawn initialize: {e}")))?;
                client
                    .request("tools/list", json!({}))
                    .map_err(|e| ToolError::Transport(format!("respawn tools/list: {e}")))?;
                match self.try_call(&mut client, input, ctx) {
                    Ok(out) => Ok(out),
                    Err(CallError::Execution(e)) => Err(ToolError::ExecutionFailed(e)),
                    Err(CallError::Transport(e)) => Err(ToolError::Transport(e)),
                }
            }
        }
    }
}

enum CallError {
    Transport(String),
    Execution(String),
}

impl McpToolHandle {
    fn try_call(
        &self,
        client: &mut McpClient,
        input: Value,
        ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, CallError> {
        let id = client.alloc_id();
        let progress_token = client.alloc_progress();
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": self.entry().name,
                "arguments": input,
                "_meta": { "progressToken": progress_token },
            }
        });
        client.write_request(&req).map_err(CallError::Transport)?;

        let mut chunk_buf = String::new();
        let response: Value = loop {
            let msg = client.read_message().map_err(CallError::Transport)?;
            let Some(msg) = msg else {
                return Err(CallError::Transport(
                    "subprocess EOF during tools/call".into(),
                ));
            };

            if msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(err) = msg.get("error") {
                    return Err(CallError::Execution(format!("{err}")));
                }
                break msg.get("result").cloned().unwrap_or(Value::Null);
            }

            if msg.get("method").and_then(|v| v.as_str()) == Some("notifications/progress") {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                let matches = params
                    .get("progressToken")
                    .and_then(|v| v.as_str())
                    .map(|s| s == progress_token)
                    .unwrap_or(false);
                if matches
                    && let Some(chunk) = params
                        .get("_meta")
                        .and_then(|m| m.get("agent_kernel/chunk"))
                {
                    let data = chunk
                        .get("data")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let stream = match chunk.get("stream").and_then(|v| v.as_str()) {
                        Some("stderr") => ToolChunkStream::Stderr,
                        Some("text") => ToolChunkStream::Text,
                        _ => ToolChunkStream::Stdout,
                    };
                    let emitted = if data.ends_with('\n') {
                        data
                    } else {
                        format!("{data}\n")
                    };
                    chunk_buf.push_str(&emitted);
                    ctx.emit_chunk(ToolChunk {
                        stream,
                        data: emitted,
                    });
                }
            }
            // Unrelated traffic dropped.
        };

        // If the server returned content, concatenate text items into
        // the final result; otherwise fall back to the buffered chunks.
        let text_result = response
            .get("content")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or(chunk_buf);

        let result_value: Value =
            serde_json::from_str(&text_result).unwrap_or(Value::String(text_result));

        Ok(ToolOutput::readonly(result_value))
    }
}

/// Factory registered under `mcp.stdio` in the daemon's factory
/// registry. Reads `command` (required string) and `args` (optional
/// array of strings) from the manifest entry's config; the remainder
/// of the config table is forwarded as the `initialize` request's
/// `agent_kernel` extension.
pub fn from_entry(entry: &ToolsetEntry) -> Result<Box<dyn ToolSet>, String> {
    let command = entry
        .config
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "mcp.stdio toolset requires config.command (string)".to_string())?
        .to_string();
    let args: Vec<String> = entry
        .config
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    // Build the init params: every other key under [toolset.config]
    // gets forwarded. The manifest config is a `toml::Value`; convert
    // to `serde_json::Value` via the existing toml serialize impl so
    // the subprocess sees a normal JSON object.
    let mut init_params = toml_to_json(&entry.config);
    if let Value::Object(ref mut map) = init_params {
        map.remove("command");
        map.remove("args");
    }

    let id = entry.id.clone().unwrap_or_else(|| "mcp.stdio".to_string());
    let toolset = McpStdioToolSet::connect(id, command, args, init_params)?;
    Ok(Box::new(toolset))
}

fn toml_to_json(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::String(s.clone()),
        toml::Value::Integer(i) => Value::Number((*i).into()),
        toml::Value::Float(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Datetime(d) => Value::String(d.to_string()),
        toml::Value::Array(arr) => Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(map) => {
            let mut obj = serde_json::Map::new();
            for (k, val) in map {
                obj.insert(k.clone(), toml_to_json(val));
            }
            Value::Object(obj)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// Build a tiny scripted MCP server as a Python script so the unit
    /// tests don't depend on the kernel-workspace-local binary. The
    /// integration-level smoke test against the real binary lives in
    /// the workspace-local crate. Python is used (rather than shell
    /// with printf/sed) because we need to emit JSON with embedded
    /// escape sequences reliably.
    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create script");
        f.write_all(body.as_bytes()).expect("write script");
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    /// A canned MCP server with two tools: `echo` (non-streaming) and
    /// `stream_shell` (emits two progress chunks then an empty
    /// `content` array so the client falls back to buffered chunks).
    const SCRIPT_BODY: &str = r#"#!/usr/bin/env python3
import json, sys
for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        resp = {"jsonrpc":"2.0","id":rid,"result":{"serverInfo":{"name":"canned","version":"0"},"capabilities":{}}}
        print(json.dumps(resp), flush=True)
    elif method == "tools/list":
        resp = {"jsonrpc":"2.0","id":rid,"result":{"tools":[
            {"name":"echo","description":"echoes","inputSchema":{"type":"object"}},
            {"name":"stream_shell","description":"streams","inputSchema":{"type":"object"}},
        ]}}
        print(json.dumps(resp), flush=True)
    elif method == "tools/call":
        params = req.get("params", {})
        name = params.get("name")
        token = params.get("_meta", {}).get("progressToken")
        if name == "stream_shell":
            for data in ["first\n", "second\n"]:
                notif = {"jsonrpc":"2.0","method":"notifications/progress","params":{
                    "progressToken":token,
                    "_meta":{"agent_kernel/chunk":{"stream":"stdout","data":data}}
                }}
                print(json.dumps(notif), flush=True)
            resp = {"jsonrpc":"2.0","id":rid,"result":{"content":[]}}
            print(json.dumps(resp), flush=True)
        else:
            resp = {"jsonrpc":"2.0","id":rid,"result":{"content":[{"type":"text","text":"\"echoed\""}]}}
            print(json.dumps(resp), flush=True)
"#;

    const CRASH_BODY: &str = r#"#!/usr/bin/env python3
import json, sys
for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        print(json.dumps({"jsonrpc":"2.0","id":rid,"result":{"serverInfo":{"name":"c","version":"0"},"capabilities":{}}}), flush=True)
    elif method == "tools/list":
        print(json.dumps({"jsonrpc":"2.0","id":rid,"result":{"tools":[{"name":"ping","description":"p","inputSchema":{"type":"object"}}]}}), flush=True)
    elif method == "tools/call":
        print(json.dumps({"jsonrpc":"2.0","id":rid,"result":{"content":[{"type":"text","text":"\"ok\""}]}}), flush=True)
        sys.exit(0)
"#;

    #[test]
    fn handshake_discovers_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "mcp.py", SCRIPT_BODY);
        let toolset = McpStdioToolSet::connect(
            "test".into(),
            script.to_string_lossy().into_owned(),
            vec![],
            json!({}),
        )
        .expect("connect");
        let tools = toolset.tools();
        let names: Vec<_> = tools.iter().map(|t| t.name().to_string()).collect();
        assert!(names.contains(&"echo".to_string()));
        assert!(names.contains(&"stream_shell".to_string()));
    }

    #[test]
    fn non_streaming_call_returns_content_text() {
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "mcp.py", SCRIPT_BODY);
        let toolset = McpStdioToolSet::connect(
            "test".into(),
            script.to_string_lossy().into_owned(),
            vec![],
            json!({}),
        )
        .unwrap();
        let tools = toolset.tools();
        let echo = tools.iter().find(|t| t.name() == "echo").unwrap();
        let out = echo
            .execute(json!({"hi": 1}), &ToolExecutionCtx::null())
            .expect("echo");
        assert_eq!(out.result, Value::String("echoed".into()));
    }

    #[test]
    fn streaming_call_forwards_chunks() {
        use std::sync::Mutex as StdMutex;
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "mcp.py", SCRIPT_BODY);
        let toolset = McpStdioToolSet::connect(
            "test".into(),
            script.to_string_lossy().into_owned(),
            vec![],
            json!({}),
        )
        .unwrap();
        let tools = toolset.tools();
        let shell = tools.iter().find(|t| t.name() == "stream_shell").unwrap();

        let captured: StdMutex<Vec<ToolChunk>> = StdMutex::new(Vec::new());
        let sink = |c: ToolChunk| captured.lock().unwrap().push(c);
        let ctx = ToolExecutionCtx::with_sink(&sink);
        let out = shell.execute(json!({}), &ctx).expect("stream_shell");

        let got = captured.lock().unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, "first\n");
        assert_eq!(got[1].data, "second\n");
        // Empty `content` → result falls back to buffered chunks.
        assert_eq!(out.result, Value::String("first\nsecond\n".into()));
    }

    #[test]
    fn crash_triggers_respawn_once() {
        // Script that serves one full call then exits. The client
        // should flip `dead` on the next read, then synchronously
        // respawn via the stored command and succeed on retry.
        let tmp = tempfile::tempdir().unwrap();
        let script = write_script(tmp.path(), "mcp.py", CRASH_BODY);
        let toolset = McpStdioToolSet::connect(
            "test".into(),
            script.to_string_lossy().into_owned(),
            vec![],
            json!({}),
        )
        .unwrap();
        let tools = toolset.tools();
        let ping = tools.iter().find(|t| t.name() == "ping").unwrap();

        let first = ping
            .execute(json!({}), &ToolExecutionCtx::null())
            .expect("first call");
        assert_eq!(first.result, Value::String("ok".into()));

        // The server exited after serving the first call. The next
        // call should flip dead=true on the read loop, then respawn
        // and succeed.
        let second = ping
            .execute(json!({}), &ToolExecutionCtx::null())
            .expect("second call after respawn");
        assert_eq!(second.result, Value::String("ok".into()));
    }
}
