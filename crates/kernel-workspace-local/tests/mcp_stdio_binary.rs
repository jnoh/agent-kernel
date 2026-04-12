//! End-to-end smoke test for the `kernel-workspace-local` MCP binary.
//!
//! Spawns the binary as a subprocess, drives it with newline-delimited
//! JSON-RPC over its pipes, and asserts `initialize`, `tools/list`, and
//! a non-streaming `file_read` all produce well-formed responses.
//! Streaming shell calls are covered by the kernel-core mcp_stdio tests.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn binary_path() -> PathBuf {
    // Cargo places integration-test binaries next to the crate's own
    // binary in the same target dir. $CARGO_BIN_EXE_<name> is set by
    // cargo for any `[[bin]]` target defined in the same crate.
    PathBuf::from(env!("CARGO_BIN_EXE_kernel-workspace-local"))
}

struct Driver {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Driver {
    fn spawn() -> Self {
        let mut child = Command::new(binary_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn binary");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn call(&mut self, req: &Value) -> Value {
        let line = req.to_string();
        writeln!(self.stdin, "{line}").expect("write");
        self.stdin.flush().ok();
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).expect("read");
        serde_json::from_str(&buf).expect("parse response")
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn initialize_and_list_tools() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut driver = Driver::spawn();

    let init = driver.call(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "clientInfo": { "name": "test", "version": "0" },
            "capabilities": {},
            "agent_kernel": { "root": tmp.path() }
        }
    }));
    assert_eq!(init["id"], 1);
    assert!(init["result"]["serverInfo"]["name"].is_string());

    let list = driver.call(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "file_read",
        "file_write",
        "file_edit",
        "shell",
        "ls",
        "grep",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
}

#[test]
fn file_read_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("hello.txt"), "hi\nworld\n").expect("seed file");

    let mut driver = Driver::spawn();
    let _ = driver.call(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": { "agent_kernel": { "root": tmp.path() } }
    }));

    let call = driver.call(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "file_read",
            "arguments": { "path": "hello.txt" }
        }
    }));
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    assert!(text.contains("hi"));
    assert!(text.contains("world"));
}
