//! End-to-end tests exercising the full kernel stack:
//! SessionManager → Session → TurnLoop → ContextManager + PermissionEvaluator
//!
//! These tests use fake providers that return scripted multi-turn conversations
//! to verify the complete flow from user input to final output.

use kernel_core::context::ContextConfig;
use kernel_core::session::{PendingResult, SessionConfig, SessionManager};
use kernel_core::testutil::*;

use kernel_interfaces::provider::*;
use kernel_interfaces::tool::*;
use kernel_interfaces::types::*;

use std::path::PathBuf;
use std::sync::atomic::Ordering;

// ============================================================================
// Helpers
// ============================================================================

fn default_context_config() -> ContextConfig {
    ContextConfig {
        context_window: 100_000,
        compaction_cooldown_secs: 0,
        ..Default::default()
    }
}

fn session_config(policy: kernel_interfaces::policy::Policy) -> SessionConfig {
    SessionConfig {
        mode: SessionMode::Interactive,
        system_prompt: "You are a helpful coding assistant.".into(),
        context_config: default_context_config(),
        completion_config: CompletionConfig::default(),
        policy,
        resource_budget: ResourceBudget::default(),
        workspace: PathBuf::from("/tmp/test-workspace"),
    }
}

// ============================================================================
// End-to-end tests
// ============================================================================

/// Simplest case: user says hello, model responds with text, no tools.
#[test]
fn e2e_simple_text_conversation() {
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(allow_all_policy()), Vec::new());
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Hello!".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text("Hi! How can I help you today?".into())],
        usage: Usage {
            input_tokens: 50,
            output_tokens: 30,
            ..Default::default()
        },
        stop_reason: StopReason::EndTurn,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let result = session.run_turn(&provider, &frontend).unwrap();

    assert!(!result.continues);
    assert_eq!(result.tool_calls_dispatched, 0);
    assert_eq!(frontend.turns_started.load(Ordering::Relaxed), 1);
    assert_eq!(frontend.turns_ended.load(Ordering::Relaxed), 1);
    assert!(frontend.tool_calls.lock().unwrap().is_empty());
}

/// Multi-turn agent loop: model calls a tool, gets result, then responds with text.
#[test]
fn e2e_multi_turn_tool_use() {
    let file_read = RecordingTool::new(
        "file_read",
        &["fs:read"],
        vec![ToolOutput::readonly(serde_json::json!({
            "content": "fn main() {\n    println!(\"Hello\");\n}"
        }))],
    );

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(
        session_config(allow_all_policy()),
        vec![Box::new(file_read)],
    );
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("What's in src/main.rs?".into());

    // Turn 1: model calls file_read
    let provider_turn1 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "call_1".into(),
            name: "file_read".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let result1 = session.run_turn(&provider_turn1, &frontend).unwrap();
    assert!(result1.continues, "model made a tool call, should continue");
    assert_eq!(result1.tool_calls_dispatched, 1);

    // Turn 2: model sees the tool result and responds with text
    let provider_turn2 = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text(
            "The file contains a simple Rust main function that prints \"Hello\".".into(),
        )],
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    }]);

    let result2 = session.run_turn(&provider_turn2, &frontend).unwrap();
    assert!(!result2.continues, "model yielded to user");

    // Verify frontend saw the full lifecycle
    assert_eq!(frontend.turns_started.load(Ordering::Relaxed), 2);
    assert_eq!(frontend.turns_ended.load(Ordering::Relaxed), 2);
    assert_eq!(frontend.tool_calls.lock().unwrap().len(), 1);
    assert_eq!(frontend.tool_calls.lock().unwrap()[0], "file_read");

    // Verify context has the user input turn (tool exchanges are appended to it,
    // and the second model call adds to the same conversation)
    assert!(session.context().turn_count() >= 1);
}

/// Model calls multiple tools in one turn.
#[test]
fn e2e_multiple_tools_single_turn() {
    let grep_tool = RecordingTool::new(
        "grep",
        &["fs:read"],
        vec![ToolOutput::readonly(serde_json::json!({
            "matches": [{"file": "src/lib.rs", "line": 42, "text": "fn connect()"}]
        }))],
    );
    let file_read = RecordingTool::new(
        "file_read",
        &["fs:read"],
        vec![ToolOutput::readonly(serde_json::json!({
            "content": "pub fn connect() -> Result<(), Error> { ... }"
        }))],
    );

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(
        session_config(allow_all_policy()),
        vec![Box::new(grep_tool), Box::new(file_read)],
    );
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Find the connect function and show me its implementation".into());

    // Model issues two tool calls in one response
    let provider = ScriptedProvider::new(vec![Response {
        content: vec![
            Content::ToolCall {
                id: "call_1".into(),
                name: "grep".into(),
                input: serde_json::json!({"pattern": "fn connect", "path": "src/"}),
            },
            Content::ToolCall {
                id: "call_2".into(),
                name: "file_read".into(),
                input: serde_json::json!({"path": "src/lib.rs", "line_start": 40, "line_end": 50}),
            },
        ],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let result = session.run_turn(&provider, &frontend).unwrap();

    assert!(result.continues);
    assert_eq!(result.tool_calls_dispatched, 2);
    assert_eq!(frontend.tool_calls.lock().unwrap().len(), 2);
}

/// Write tool produces file invalidation, context manager clears cache.
#[test]
fn e2e_tool_invalidation_flow() {
    let file_edit = RecordingTool::new(
        "file_edit",
        &["fs:read", "fs:write"],
        vec![ToolOutput::with_invalidations(
            serde_json::json!({"edited": true}),
            vec![kernel_interfaces::types::Invalidation::Files(vec![
                PathBuf::from("src/main.rs"),
            ])],
        )],
    );

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(
        session_config(allow_all_policy()),
        vec![Box::new(file_edit)],
    );
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Fix the typo in main.rs".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "call_1".into(),
            name: "file_edit".into(),
            input: serde_json::json!({
                "path": "src/main.rs",
                "old": "pritnln",
                "new": "println"
            }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let result = session.run_turn(&provider, &frontend).unwrap();

    assert!(result.continues);
    assert_eq!(result.tool_calls_dispatched, 1);
    // The invalidation was processed — we can't directly check the cache was cleared
    // from the public API, but the tool was dispatched and the turn completed without error.
}

/// Lockdown policy: read allowed, shell denied, write asks user.
#[test]
fn e2e_policy_enforcement() {
    let file_read = RecordingTool::new(
        "file_read",
        &["fs:read"],
        vec![ToolOutput::readonly(serde_json::json!("file contents"))],
    );
    let shell = RecordingTool::new("shell", &["shell:exec"], vec![]);
    let file_write = RecordingTool::new(
        "file_write",
        &["fs:write"],
        vec![ToolOutput::readonly(serde_json::json!({"written": true}))],
    );

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(
        session_config(lockdown_policy()),
        vec![Box::new(file_read), Box::new(shell), Box::new(file_write)],
    );
    let session = mgr.get_mut(id).unwrap();

    // Model tries all three tools
    session.add_user_input("Read, write, and run a command".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![
            // fs:read → allowed by policy
            Content::ToolCall {
                id: "c1".into(),
                name: "file_read".into(),
                input: serde_json::json!({"path": "README.md"}),
            },
            // shell:exec → denied by policy
            Content::ToolCall {
                id: "c2".into(),
                name: "shell".into(),
                input: serde_json::json!({"command": "ls"}),
            },
            // fs:write → ask, frontend auto-allows
            Content::ToolCall {
                id: "c3".into(),
                name: "file_write".into(),
                input: serde_json::json!({"path": "out.txt", "content": "hello"}),
            },
        ],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let result = session.run_turn(&provider, &frontend).unwrap();

    // file_read dispatched (allowed), shell denied, file_write dispatched (ask→user allowed)
    assert_eq!(result.tool_calls_dispatched, 2);
    assert_eq!(result.tool_calls_denied, 1);

    // Frontend was asked about file_write
    let perm_requests = frontend.permission_requests.lock().unwrap();
    assert_eq!(perm_requests.len(), 1);
    assert_eq!(perm_requests[0], "file_write");
}

/// Same as above but user denies the write — should get 2 denied, 1 dispatched.
#[test]
fn e2e_policy_user_denies_write() {
    let file_read = RecordingTool::new(
        "file_read",
        &["fs:read"],
        vec![ToolOutput::readonly(serde_json::json!("contents"))],
    );
    let shell = RecordingTool::new("shell", &["shell:exec"], vec![]);
    let file_write = RecordingTool::new("file_write", &["fs:write"], vec![]);

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(
        session_config(lockdown_policy()),
        vec![Box::new(file_read), Box::new(shell), Box::new(file_write)],
    );
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Read, write, and run".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![
            Content::ToolCall {
                id: "c1".into(),
                name: "file_read".into(),
                input: serde_json::json!({}),
            },
            Content::ToolCall {
                id: "c2".into(),
                name: "shell".into(),
                input: serde_json::json!({}),
            },
            Content::ToolCall {
                id: "c3".into(),
                name: "file_write".into(),
                input: serde_json::json!({}),
            },
        ],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_deny();

    let result = session.run_turn(&provider, &frontend).unwrap();

    // file_read allowed, shell denied by policy, file_write denied by user
    assert_eq!(result.tool_calls_dispatched, 1);
    assert_eq!(result.tool_calls_denied, 2);
}

/// Hot-swap policy mid-session: start permissive, switch to lockdown.
#[test]
fn e2e_policy_hot_swap() {
    let shell = RecordingTool::new(
        "shell",
        &["shell:exec"],
        vec![
            ToolOutput::readonly(serde_json::json!({"output": "file1.rs\nfile2.rs"})),
            // Second call should never happen after policy swap
        ],
    );

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(allow_all_policy()), vec![Box::new(shell)]);
    let session = mgr.get_mut(id).unwrap();

    // Turn 1: permissive policy, shell allowed
    session.add_user_input("List files".into());
    let provider1 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            input: serde_json::json!({"command": "ls"}),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let r1 = session.run_turn(&provider1, &frontend).unwrap();
    assert_eq!(
        r1.tool_calls_dispatched, 1,
        "shell allowed under permissive"
    );

    // Swap to lockdown policy
    session.set_policy(lockdown_policy());

    // Turn 2: same tool call, now denied
    session.add_user_input("Run ls again".into());
    let provider2 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c2".into(),
            name: "shell".into(),
            input: serde_json::json!({"command": "ls"}),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);

    let r2 = session.run_turn(&provider2, &frontend).unwrap();
    assert_eq!(r2.tool_calls_denied, 1, "shell denied under lockdown");
    assert_eq!(r2.tool_calls_dispatched, 0);
}

/// External event delivered between turns appears in context.
#[test]
fn e2e_pending_event_delivery() {
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(allow_all_policy()), Vec::new());
    let session = mgr.get_mut(id).unwrap();

    // Deliver an external event before the user's turn
    session.deliver(PendingResult::ExternalEvent {
        source: "github-ci".into(),
        event_type: "check_run.failed".into(),
        summary: "Test suite failed: 3 failures in auth module".into(),
    });

    session.add_user_input("What happened with CI?".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text(
            "CI failed with 3 test failures in the auth module.".into(),
        )],
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let result = session.run_turn(&provider, &frontend).unwrap();
    assert!(!result.continues);

    // The event was drained into context — turn count includes the system message + user input
    assert!(session.context().turn_count() >= 2);
}

/// Child session completion delivered as pending result.
#[test]
fn e2e_child_completion_delivery() {
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(session_config(allow_all_policy()), Vec::new());
    let session = mgr.get_mut(id).unwrap();

    // Simulate a child session completing with file invalidations
    session.deliver(PendingResult::ChildCompleted {
        task: "lint-check".into(),
        message: "Found 2 linting issues, auto-fixed".into(),
        invalidations: vec![Invalidation::Files(vec![
            PathBuf::from("src/lib.rs"),
            PathBuf::from("src/utils.rs"),
        ])],
    });

    session.add_user_input("How did the lint check go?".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text(
            "The lint check found and fixed 2 issues.".into(),
        )],
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    let result = session.run_turn(&provider, &frontend).unwrap();
    assert!(!result.continues);
}

/// Compaction triggers when context fills up.
#[test]
fn e2e_compaction_triggers() {
    let config = SessionConfig {
        context_config: ContextConfig {
            context_window: 500, // very small window to force compaction
            compaction_threshold: 0.50,
            verbatim_tail_ratio: 0.30,
            compaction_cooldown_secs: 0,
            ..Default::default()
        },
        ..session_config(allow_all_policy())
    };

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(config, Vec::new());
    let session = mgr.get_mut(id).unwrap();

    let frontend = RecordingFrontend::auto_allow();

    // Fill context with enough turns to trigger compaction.
    // Use FakeProvider (unlimited responses) rather than ScriptedProvider —
    // projection-based compaction (spec 0004) makes the turn loop call the
    // provider for every compaction pass, which would exhaust a single-
    // response script on the first iteration.
    let provider = FakeProvider {
        response: Response {
            content: vec![Content::Text(
                "Acknowledged, here is a reasonably long response to consume tokens.".into(),
            )],
            usage: Usage::default(),
            stop_reason: StopReason::EndTurn,
        },
    };
    for i in 0..15 {
        session.add_user_input(format!(
            "This is message number {} with enough text to consume tokens in the context window",
            i
        ));

        // Some turns may fail compaction if the death spiral guard triggers,
        // but at least some should succeed.
        let _ = session.run_turn(&provider, &frontend);
    }

    // Compaction should have fired at least once
    assert!(
        frontend.compactions.load(Ordering::Relaxed) > 0,
        "compaction should have triggered with a 500-token window"
    );
}

/// Full agent workflow: user asks to fix a bug, model reads file, edits it, runs tests.
#[test]
fn e2e_full_agent_workflow() {
    let file_read = RecordingTool::new(
        "file_read",
        &["fs:read"],
        vec![ToolOutput::readonly(serde_json::json!({
            "content": "fn add(a: i32, b: i32) -> i32 {\n    a - b  // BUG: should be +\n}"
        }))],
    );
    let file_edit = RecordingTool::new(
        "file_edit",
        &["fs:read", "fs:write"],
        vec![ToolOutput::with_invalidations(
            serde_json::json!({"success": true}),
            vec![Invalidation::Files(vec![PathBuf::from("src/math.rs")])],
        )],
    );
    let shell = RecordingTool::new(
        "shell",
        &["shell:exec"],
        vec![ToolOutput::readonly(serde_json::json!({
            "exit_code": 0,
            "stdout": "test result: ok. 5 passed; 0 failed",
            "stderr": ""
        }))],
    );

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(
        session_config(allow_all_policy()),
        vec![Box::new(file_read), Box::new(file_edit), Box::new(shell)],
    );
    let session = mgr.get_mut(id).unwrap();
    let frontend = RecordingFrontend::auto_allow();

    // Turn 1: User asks to fix the bug
    session.add_user_input(
        "The add function in src/math.rs has a bug. Fix it and run the tests.".into(),
    );

    // Model reads the file
    let provider1 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "file_read".into(),
            input: serde_json::json!({"path": "src/math.rs"}),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let r1 = session.run_turn(&provider1, &frontend).unwrap();
    assert!(r1.continues);

    // Turn 2: Model edits the file
    let provider2 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c2".into(),
            name: "file_edit".into(),
            input: serde_json::json!({
                "path": "src/math.rs",
                "old": "a - b  // BUG: should be +",
                "new": "a + b"
            }),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let r2 = session.run_turn(&provider2, &frontend).unwrap();
    assert!(r2.continues);

    // Turn 3: Model runs tests
    let provider3 = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c3".into(),
            name: "shell".into(),
            input: serde_json::json!({"command": "cargo test"}),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let r3 = session.run_turn(&provider3, &frontend).unwrap();
    assert!(r3.continues);

    // Turn 4: Model reports success
    let provider4 = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text(
            "Fixed the bug in `add()` — it was using `-` instead of `+`. All 5 tests pass.".into(),
        )],
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    }]);
    let r4 = session.run_turn(&provider4, &frontend).unwrap();
    assert!(!r4.continues);

    // Verify the full workflow
    assert_eq!(frontend.turns_started.load(Ordering::Relaxed), 4);
    let tool_calls = frontend.tool_calls.lock().unwrap();
    assert_eq!(tool_calls.len(), 3);
    assert_eq!(tool_calls[0], "file_read");
    assert_eq!(tool_calls[1], "file_edit");
    assert_eq!(tool_calls[2], "shell");
}

/// Two sessions in the same manager share invalidations.
#[test]
fn e2e_cross_session_invalidation() {
    let mut mgr = SessionManager::new(ResourceBudget::default());

    let id1 = mgr.spawn_interactive(session_config(allow_all_policy()), Vec::new());
    let id2 = mgr.spawn_interactive(session_config(allow_all_policy()), Vec::new());

    // Propagate a file invalidation from session 1
    let invalidation = Invalidation::Files(vec![PathBuf::from("shared/config.toml")]);
    mgr.propagate_invalidation(id1, &invalidation);

    // Session 2 should have processed it (we can verify it didn't panic
    // and the session is still accessible)
    assert!(mgr.get(id2).is_some());
    assert_eq!(mgr.active_count(), 2);
}

/// Tool execution error is handled gracefully — doesn't crash the session.
#[test]
fn e2e_tool_execution_error() {
    let failing_tool = RecordingTool::new("buggy_tool", &["fs:read"], vec![]);
    // Override with a tool that always fails
    struct FailingTool;
    impl ToolRegistration for FailingTool {
        fn name(&self) -> &str {
            "buggy_tool"
        }
        fn description(&self) -> &str {
            "always fails"
        }
        fn capabilities(&self) -> &CapabilitySet {
            Box::leak(Box::new(
                ["fs:read"]
                    .iter()
                    .map(|c| Capability::new(*c))
                    .collect::<CapabilitySet>(),
            ))
        }
        fn schema(&self) -> &serde_json::Value {
            &serde_json::Value::Null
        }
        fn cost(&self) -> TokenEstimate {
            TokenEstimate(10)
        }
        fn relevance(&self) -> &RelevanceSignal {
            Box::leak(Box::new(RelevanceSignal {
                keywords: Vec::new(),
                tags: Vec::new(),
            }))
        }
        fn execute(&self, _: serde_json::Value) -> Result<ToolOutput, ToolError> {
            Err(ToolError::ExecutionFailed("disk on fire".into()))
        }
    }

    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive(
        session_config(allow_all_policy()),
        vec![Box::new(FailingTool)],
    );
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("Read something".into());

    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::ToolCall {
            id: "c1".into(),
            name: "buggy_tool".into(),
            input: serde_json::json!({}),
        }],
        usage: Usage::default(),
        stop_reason: StopReason::ToolUse,
    }]);
    let frontend = RecordingFrontend::auto_allow();

    // Should not panic — error is fed back to the model as a tool result
    let result = session.run_turn(&provider, &frontend).unwrap();

    // The tool was "dispatched" (attempted) even though it failed
    assert_eq!(result.tool_calls_dispatched, 1);

    // We can still run another turn — session is not broken
    session.add_user_input("Try something else".into());
    let provider2 = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text("OK, let me try another approach.".into())],
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    }]);
    let r2 = session.run_turn(&provider2, &frontend).unwrap();
    assert!(!r2.continues);

    // Suppress unused variable warning
    drop(failing_tool);
}

/// A single turn end-to-end produces a non-empty session event file that
/// includes `SessionStarted` and `UserInput` events, proving the Tier-3
/// event stream is wired through `spawn_interactive_with_events`.
#[test]
fn e2e_session_events_written_to_file() {
    use kernel_core::session_events::{FileSink, SessionEvent};
    use std::io::{BufRead, BufReader};

    let tmp = tempfile::tempdir().unwrap();
    let log_path = tmp.path().join("session-0").join("events.jsonl");

    let sink = FileSink::new(SessionId(0), &log_path).expect("open file sink");
    let mut mgr = SessionManager::new(ResourceBudget::default());
    let id = mgr.spawn_interactive_with_events(
        session_config(allow_all_policy()),
        Vec::new(),
        Box::new(sink),
    );
    let session = mgr.get_mut(id).unwrap();

    session.add_user_input("What files are here?".into());
    let provider = ScriptedProvider::new(vec![Response {
        content: vec![Content::Text("Let me check.".into())],
        usage: Usage::default(),
        stop_reason: StopReason::EndTurn,
    }]);
    let frontend = RecordingFrontend::auto_allow();
    session.run_turn(&provider, &frontend).unwrap();

    // Drop the SessionManager so the FileSink inside the session flushes
    // and closes its BufWriter before we read the file back.
    drop(mgr);

    let file = std::fs::File::open(&log_path).expect("event file exists");
    let lines: Vec<String> = BufReader::new(file)
        .lines()
        .collect::<Result<_, _>>()
        .expect("read event lines");
    let events: Vec<SessionEvent> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("parse event"))
        .collect();

    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::SessionStarted { .. })),
        "expected SessionStarted event"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            SessionEvent::UserInput { text, .. } if text == "What files are here?"
        )),
        "expected UserInput event with matching text"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::AssistantResponse { .. })),
        "expected AssistantResponse event"
    );
}
