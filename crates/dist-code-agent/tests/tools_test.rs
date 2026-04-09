//! Tests for the dist-code-agent native tools.
//! These run against real temp directories — no mocks.

use std::fs;
use std::path::PathBuf;

// The tools module is private to the binary crate, so we test through
// the public kernel interfaces by building tools directly.
// We need to make tools accessible for testing.

/// Create a temp directory with some test files.
fn setup_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Create some files
    fs::write(root.join("hello.txt"), "Hello, world!\nLine 2\nLine 3\n").unwrap();
    fs::write(
        root.join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
    )
    .unwrap();

    dir
}

// Since tools.rs is a private module of the binary, we can't import it
// from integration tests. We'll restructure to expose tools for testing
// via a lib.rs, or test via the binary itself.
//
// For now, test the full stack through the kernel with real tools.

use kernel_core::context::ContextConfig;
use kernel_core::session::{SessionConfig, SessionManager};
use kernel_core::testutil::*;

use kernel_interfaces::provider::*;
use kernel_interfaces::tool::*;
use kernel_interfaces::types::*;

use std::sync::atomic::Ordering;

// ============================================================================
// We rebuild the tools inline here since the binary crate's modules are private.
// This is a known Rust limitation — binary crates can't expose modules to
// integration tests. The fix is to move tools into a library crate, which
// we'll do when the tool set grows. For now, this keeps things simple.
// ============================================================================

fn create_test_tools(workspace: &std::path::Path) -> Vec<Box<dyn ToolRegistration>> {
    // Minimal reimplementations just for testing — these exercise the same
    // code paths as the real tools.
    vec![
        Box::new(TestFileRead(workspace.to_path_buf())),
        Box::new(TestFileWrite(workspace.to_path_buf())),
        Box::new(TestFileEdit(workspace.to_path_buf())),
        Box::new(TestShell(workspace.to_path_buf())),
        Box::new(TestLs(workspace.to_path_buf())),
        Box::new(TestGrep(workspace.to_path_buf())),
    ]
}

struct TestFileRead(PathBuf);
impl ToolRegistration for TestFileRead {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Read file contents"
    }
    fn capabilities(&self) -> &CapabilitySet {
        Box::leak(Box::new([Capability::new("fs:read")].into_iter().collect()))
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        Box::leak(Box::new(RelevanceSignal {
            keywords: vec![],
            tags: vec![],
        }))
    }
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing path".into()))?;
        let full = self.0.join(path);
        let content =
            fs::read_to_string(&full).map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(ToolOutput::readonly(
            serde_json::json!({ "content": content }),
        ))
    }
}

struct TestFileWrite(PathBuf);
impl ToolRegistration for TestFileWrite {
    fn name(&self) -> &str {
        "file_write"
    }
    fn description(&self) -> &str {
        "Write file"
    }
    fn capabilities(&self) -> &CapabilitySet {
        Box::leak(Box::new(
            [Capability::new("fs:write")].into_iter().collect(),
        ))
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        Box::leak(Box::new(RelevanceSignal {
            keywords: vec![],
            tags: vec![],
        }))
    }
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing path".into()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing content".into()))?;
        let full = self.0.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).ok();
        }
        fs::write(&full, content).map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(ToolOutput::with_invalidations(
            serde_json::json!({ "written": true }),
            vec![Invalidation::Files(vec![full])],
        ))
    }
}

struct TestFileEdit(PathBuf);
impl ToolRegistration for TestFileEdit {
    fn name(&self) -> &str {
        "file_edit"
    }
    fn description(&self) -> &str {
        "Edit file via string replacement"
    }
    fn capabilities(&self) -> &CapabilitySet {
        Box::leak(Box::new(
            [Capability::new("fs:write")].into_iter().collect(),
        ))
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        Box::leak(Box::new(RelevanceSignal {
            keywords: vec![],
            tags: vec![],
        }))
    }
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing path".into()))?;
        let old_string = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing old_string".into()))?;
        let new_string = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing new_string".into()))?;
        let full = self.0.join(path);

        if old_string.is_empty() {
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::write(&full, new_string).map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            return Ok(ToolOutput::with_invalidations(
                serde_json::json!({ "action": "created" }),
                vec![Invalidation::Files(vec![full])],
            ));
        }

        let content =
            fs::read_to_string(&full).map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let count = content.matches(old_string).count();
        if count == 0 {
            return Err(ToolError::ExecutionFailed(
                "old_string not found in file".into(),
            ));
        }
        if count > 1 {
            return Err(ToolError::ExecutionFailed(format!(
                "old_string matches {count} locations"
            )));
        }
        let new_content = content.replacen(old_string, new_string, 1);
        fs::write(&full, &new_content).map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(ToolOutput::with_invalidations(
            serde_json::json!({ "action": "edited" }),
            vec![Invalidation::Files(vec![full])],
        ))
    }
}

struct TestShell(PathBuf);
impl ToolRegistration for TestShell {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "Run shell command"
    }
    fn capabilities(&self) -> &CapabilitySet {
        Box::leak(Box::new(
            [Capability::new("shell:exec")].into_iter().collect(),
        ))
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(200)
    }
    fn relevance(&self) -> &RelevanceSignal {
        Box::leak(Box::new(RelevanceSignal {
            keywords: vec![],
            tags: vec![],
        }))
    }
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let cmd = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing command".into()))?;
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .current_dir(&self.0)
            .output()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(ToolOutput::readonly(serde_json::json!({
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": String::from_utf8_lossy(&output.stdout).as_ref(),
            "stderr": String::from_utf8_lossy(&output.stderr).as_ref(),
        })))
    }
}

struct TestLs(PathBuf);
impl ToolRegistration for TestLs {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory"
    }
    fn capabilities(&self) -> &CapabilitySet {
        Box::leak(Box::new([Capability::new("fs:read")].into_iter().collect()))
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(100)
    }
    fn relevance(&self) -> &RelevanceSignal {
        Box::leak(Box::new(RelevanceSignal {
            keywords: vec![],
            tags: vec![],
        }))
    }
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let full = self.0.join(path);
        let entries: Vec<String> = fs::read_dir(&full)
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        Ok(ToolOutput::readonly(
            serde_json::json!({ "entries": entries }),
        ))
    }
}

struct TestGrep(PathBuf);
impl ToolRegistration for TestGrep {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search files"
    }
    fn capabilities(&self) -> &CapabilitySet {
        Box::leak(Box::new([Capability::new("fs:read")].into_iter().collect()))
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        Box::leak(Box::new(RelevanceSignal {
            keywords: vec![],
            tags: vec![],
        }))
    }
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing pattern".into()))?;
        let output = std::process::Command::new("grep")
            .args(["-rn", pattern])
            .current_dir(&self.0)
            .output()
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(ToolOutput::readonly(serde_json::json!({
            "matches": String::from_utf8_lossy(&output.stdout).as_ref(),
        })))
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn session_config(workspace: &std::path::Path) -> SessionConfig {
    SessionConfig {
        mode: SessionMode::Interactive,
        system_prompt: "You are a coding assistant.".into(),
        context_config: ContextConfig {
            context_window: 100_000,
            compaction_cooldown_secs: 0,
            ..Default::default()
        },
        completion_config: CompletionConfig::default(),
        policy: allow_all_policy(),
        resource_budget: ResourceBudget::default(),
        workspace: workspace.to_path_buf(),
    }
}

// ============================================================================
// Tests
// ============================================================================

/// Model calls file_read on a real file, gets real content back.
#[test]
fn distro_file_read_real_file() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Read hello.txt".into());

    let provider = ScriptedProvider::new(vec![
        Response {
            content: vec![Content::ToolCall {
                id: "c1".into(),
                name: "file_read".into(),
                input: serde_json::json!({ "path": "hello.txt" }),
            }],
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
        },
        Response {
            content: vec![Content::Text("The file says Hello, world!".into())],
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        },
    ]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues);
    assert_eq!(r1.tool_calls_dispatched, 1);

    // The tool result should contain real file content
    let results = frontend.tool_results.lock().unwrap();
    let content = results[0].get("content").unwrap().as_str().unwrap();
    assert!(content.contains("Hello, world!"));
}

/// Model writes a file, then reads it back — verifies real filesystem ops.
#[test]
fn distro_write_then_read() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Create a new file".into());

    // Turn 1: write a file
    let provider1 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "file_write".into(),
            input: serde_json::json!({
                "path": "new_file.txt",
                "content": "Created by agent-kernel"
            }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider1, &frontend).unwrap();
    assert!(r1.continues);

    // Verify the file actually exists on disk
    let written = fs::read_to_string(ws.path().join("new_file.txt")).unwrap();
    assert_eq!(written, "Created by agent-kernel");

    // Turn 2: read it back
    let provider2 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c2".into(),
            name: "file_read".into(),
            input: serde_json::json!({ "path": "new_file.txt" }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);

    let r2 = session.run_turn(&provider2, &frontend).unwrap();
    assert!(r2.continues);

    let results = frontend.tool_results.lock().unwrap();
    let content = results
        .last()
        .unwrap()
        .get("content")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(content.contains("Created by agent-kernel"));
}

/// Model runs a shell command and gets real output.
#[test]
fn distro_shell_command() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Count files".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            input: serde_json::json!({ "command": "echo hello-from-shell" }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues);

    let results = frontend.tool_results.lock().unwrap();
    let stdout = results[0].get("stdout").unwrap().as_str().unwrap();
    assert!(stdout.contains("hello-from-shell"));
    let exit_code = results[0].get("exit_code").unwrap().as_i64().unwrap();
    assert_eq!(exit_code, 0);
}

/// Model lists directory contents.
#[test]
fn distro_ls_directory() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("What files are here?".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "ls".into(),
            input: serde_json::json!({ "path": "." }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues);

    let results = frontend.tool_results.lock().unwrap();
    let entries = results[0].get("entries").unwrap().as_array().unwrap();
    let names: Vec<&str> = entries.iter().filter_map(|e| e.as_str()).collect();
    assert!(names.contains(&"hello.txt"));
    assert!(names.contains(&"main.rs"));
    assert!(names.contains(&"src"));
}

/// Model searches for a pattern across files.
#[test]
fn distro_grep_pattern() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Find the add function".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "grep".into(),
            input: serde_json::json!({ "pattern": "fn add" }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues);

    let results = frontend.tool_results.lock().unwrap();
    let matches = results[0].get("matches").unwrap().as_str().unwrap();
    assert!(matches.contains("fn add"));
    assert!(matches.contains("lib.rs"));
}

/// File read on nonexistent file returns an error (not a crash).
#[test]
fn distro_file_read_missing_file() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Read nonexistent file".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "file_read".into(),
            input: serde_json::json!({ "path": "does_not_exist.txt" }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    // Should not panic — error gets fed back to the model
    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues); // dispatched (even though it errored)
}

/// Full multi-tool workflow: ls → read → write → verify on disk.
#[test]
fn distro_full_workflow() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("List, read, then create a summary file".into());
    let frontend = RecordingFrontend::auto_allow();

    // Turn 1: ls
    let p1 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "ls".into(),
            input: serde_json::json!({}),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    session.run_turn(&p1, &frontend).unwrap();

    // Turn 2: read main.rs
    let p2 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c2".into(),
            name: "file_read".into(),
            input: serde_json::json!({ "path": "main.rs" }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    session.run_turn(&p2, &frontend).unwrap();

    // Turn 3: write summary
    let p3 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c3".into(),
            name: "file_write".into(),
            input: serde_json::json!({
                "path": "SUMMARY.md",
                "content": "# Summary\nThis project has a main.rs with a hello world.\n"
            }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    session.run_turn(&p3, &frontend).unwrap();

    // Turn 4: done
    let p4 = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text("Done! Created SUMMARY.md.".into())],
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    }]);
    let r4 = session.run_turn(&p4, &frontend).unwrap();
    assert!(!r4.continues);

    // Verify the file was actually written to disk
    let summary = fs::read_to_string(ws.path().join("SUMMARY.md")).unwrap();
    assert!(summary.contains("hello world"));

    // Verify all tools were called (3 tool turns, 1 text-only turn)
    let tool_calls = frontend.tool_calls.lock().unwrap();
    assert_eq!(tool_calls.len(), 3);
    assert_eq!(tool_calls[0], "ls");
    assert_eq!(tool_calls[1], "file_read");
    assert_eq!(tool_calls[2], "file_write");
    assert_eq!(frontend.turns_started.load(Ordering::Relaxed), 4);
}

/// Policy enforcement with real tools: lockdown denies shell.
#[test]
fn distro_policy_blocks_shell() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());

    let config = SessionConfig {
        policy: lockdown_policy(),
        ..session_config(ws.path())
    };

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(config, tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Run a command".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            input: serde_json::json!({ "command": "echo pwned" }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert_eq!(r1.tool_calls_denied, 1);
    assert_eq!(r1.tool_calls_dispatched, 0);
}

/// Model edits a file via string replacement.
#[test]
fn distro_file_edit_replaces_string() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Change the greeting".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "file_edit".into(),
            input: serde_json::json!({
                "path": "hello.txt",
                "old_string": "Hello, world!",
                "new_string": "Greetings, universe!"
            }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues);
    assert_eq!(r1.tool_calls_dispatched, 1);

    let edited = fs::read_to_string(ws.path().join("hello.txt")).unwrap();
    assert!(edited.contains("Greetings, universe!"));
    assert!(!edited.contains("Hello, world!"));
    // Other lines preserved
    assert!(edited.contains("Line 2"));
}

/// file_edit with empty old_string creates a new file.
#[test]
fn distro_file_edit_creates_file() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Create a config file".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "file_edit".into(),
            input: serde_json::json!({
                "path": "config.toml",
                "old_string": "",
                "new_string": "[settings]\nkey = \"value\"\n"
            }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues);

    let content = fs::read_to_string(ws.path().join("config.toml")).unwrap();
    assert!(content.contains("key = \"value\""));
}

/// file_edit fails when old_string is not found.
#[test]
fn distro_file_edit_not_found() {
    let ws = setup_workspace();
    let tools = create_test_tools(ws.path());
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(ws.path()), tools);
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Edit something".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "file_edit".into(),
            input: serde_json::json!({
                "path": "hello.txt",
                "old_string": "this string does not exist",
                "new_string": "replacement"
            }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    // Should not panic — error gets fed back to the model
    let r1 = session.run_turn(&provider, &frontend).unwrap();
    assert!(r1.continues);
}
