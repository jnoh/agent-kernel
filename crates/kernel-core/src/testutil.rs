//! Shared test doubles for kernel-core tests.
//!
//! Provides reusable fake implementations of ProviderInterface, ToolRegistration,
//! and FrontendInterface to avoid duplicating test doubles across modules.

use kernel_interfaces::frontend::*;
use kernel_interfaces::policy::{Policy, PolicyAction, PolicyRule};
use kernel_interfaces::provider::*;
use kernel_interfaces::tool::*;
use kernel_interfaces::types::*;

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

// ============================================================================
// Provider doubles
// ============================================================================

/// A provider that returns a fixed response every time.
pub struct FakeProvider {
    pub response: Response,
}

impl ProviderInterface for FakeProvider {
    fn complete(&self, _: &Prompt, _: &CompletionConfig) -> Result<Response, ProviderError> {
        Ok(self.response.clone())
    }
    fn count_tokens(&self, _: &Content) -> usize {
        10
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps::default()
    }
}

/// A provider that returns a sequence of scripted responses, one per call.
pub struct ScriptedProvider {
    responses: Mutex<Vec<Response>>,
}

impl ScriptedProvider {
    pub fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

impl ProviderInterface for ScriptedProvider {
    fn complete(&self, _: &Prompt, _: &CompletionConfig) -> Result<Response, ProviderError> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            Err(ProviderError::Api {
                status: 500,
                message: "no more scripted responses".into(),
            })
        } else {
            Ok(responses.remove(0))
        }
    }
    fn count_tokens(&self, _: &Content) -> usize {
        10
    }
    fn capabilities(&self) -> ProviderCaps {
        ProviderCaps {
            supports_tool_use: true,
            supports_vision: false,
            supports_streaming: false,
            max_context_tokens: 200_000,
        }
    }
}

// ============================================================================
// Tool doubles
// ============================================================================

/// A tool with configurable capabilities that returns a fixed value.
pub struct FakeTool {
    name: String,
    caps: CapabilitySet,
    relevance: RelevanceSignal,
    called: AtomicBool,
    return_value: serde_json::Value,
}

impl FakeTool {
    pub fn new(name: &str, caps: &[&str], return_value: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            caps: caps.iter().map(|c| Capability::new(*c)).collect(),
            relevance: RelevanceSignal {
                keywords: Vec::new(),
                tags: Vec::new(),
            },
            called: AtomicBool::new(false),
            return_value,
        }
    }

    /// A tool with no capabilities (kernel-internal).
    pub fn internal(name: &str) -> Self {
        Self::new(name, &[], serde_json::Value::Null)
    }

    pub fn was_called(&self) -> bool {
        self.called.load(Ordering::Relaxed)
    }
}

impl ToolRegistration for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(10)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }
    fn execute(&self, _input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        self.called.store(true, Ordering::Relaxed);
        Ok(ToolOutput::readonly(self.return_value.clone()))
    }
}

/// A tool that tracks all inputs and returns scripted outputs.
pub struct RecordingTool {
    name: String,
    description: String,
    caps: CapabilitySet,
    relevance: RelevanceSignal,
    invocations: Mutex<Vec<serde_json::Value>>,
    outputs: Mutex<Vec<ToolOutput>>,
}

impl RecordingTool {
    pub fn new(name: &str, caps: &[&str], outputs: Vec<ToolOutput>) -> Self {
        Self {
            name: name.into(),
            description: format!("Test tool: {name}"),
            caps: caps.iter().map(|c| Capability::new(*c)).collect(),
            relevance: RelevanceSignal {
                keywords: Vec::new(),
                tags: Vec::new(),
            },
            invocations: Mutex::new(Vec::new()),
            outputs: Mutex::new(outputs),
        }
    }

    #[allow(dead_code)]
    pub fn invocation_count(&self) -> usize {
        self.invocations.lock().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn last_input(&self) -> Option<serde_json::Value> {
        self.invocations.lock().unwrap().last().cloned()
    }
}

impl ToolRegistration for RecordingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &serde_json::Value::Null
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(50)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }
    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        self.invocations.lock().unwrap().push(input);
        let mut outputs = self.outputs.lock().unwrap();
        if outputs.is_empty() {
            Ok(ToolOutput::readonly(serde_json::json!("default output")))
        } else {
            Ok(outputs.remove(0))
        }
    }
}

// ============================================================================
// Frontend doubles
// ============================================================================

/// A frontend that records all events for assertion.
pub struct RecordingFrontend {
    pub turns_started: AtomicU64,
    pub turns_ended: AtomicU64,
    pub tool_calls: Mutex<Vec<String>>,
    pub tool_results: Mutex<Vec<serde_json::Value>>,
    pub permission_requests: Mutex<Vec<String>>,
    pub compactions: AtomicU64,
    pub errors: Mutex<Vec<String>>,
    permission_response: Decision,
}

impl RecordingFrontend {
    pub fn auto_allow() -> Self {
        Self {
            turns_started: AtomicU64::new(0),
            turns_ended: AtomicU64::new(0),
            tool_calls: Mutex::new(Vec::new()),
            tool_results: Mutex::new(Vec::new()),
            permission_requests: Mutex::new(Vec::new()),
            compactions: AtomicU64::new(0),
            errors: Mutex::new(Vec::new()),
            permission_response: Decision::Allow,
        }
    }

    pub fn auto_deny() -> Self {
        Self {
            permission_response: Decision::Deny("user denied".into()),
            ..Self::auto_allow()
        }
    }
}

impl FrontendInterface for RecordingFrontend {
    fn on_turn_start(&self, _: TurnId) {
        self.turns_started.fetch_add(1, Ordering::Relaxed);
    }
    fn on_text(&self, _: &str) {}
    fn on_stream_chunk(&self, _: &StreamChunk) {}
    fn on_tool_call(&self, name: &str, _: &serde_json::Value) {
        self.tool_calls.lock().unwrap().push(name.into());
    }
    fn on_tool_result(&self, _: &str, result: &ToolOutput) {
        self.tool_results.lock().unwrap().push(result.result.clone());
    }
    fn on_permission_request(&self, request: &PermissionRequest) -> Decision {
        self.permission_requests
            .lock()
            .unwrap()
            .push(request.tool_name.clone());
        self.permission_response.clone()
    }
    fn on_turn_end(&self, _: TurnId) {
        self.turns_ended.fetch_add(1, Ordering::Relaxed);
    }
    fn on_compaction(&self, _: &CompactionSummary) {
        self.compactions.fetch_add(1, Ordering::Relaxed);
    }
    fn on_workspace_changed(&self, _: &Path) {}
    fn on_error(&self, error: &KernelError) {
        self.errors.lock().unwrap().push(error.message.clone());
    }
}

// ============================================================================
// Common policy helpers
// ============================================================================

pub fn allow_all_policy() -> Policy {
    Policy {
        version: 1,
        name: "allow-all".into(),
        rules: vec![PolicyRule {
            match_capabilities: vec![
                "fs:read".into(),
                "fs:write".into(),
                "shell:exec".into(),
                "net:*".into(),
            ],
            action: PolicyAction::Allow,
            scope_paths: Vec::new(),
            scope_commands: Vec::new(),
            except: Vec::new(),
        }],
        resource_budgets: None,
    }
}

pub fn lockdown_policy() -> Policy {
    Policy {
        version: 1,
        name: "lockdown".into(),
        rules: vec![
            PolicyRule {
                match_capabilities: vec!["fs:read".into()],
                action: PolicyAction::Allow,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            },
            PolicyRule {
                match_capabilities: vec!["fs:write".into()],
                action: PolicyAction::Ask,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            },
            PolicyRule {
                match_capabilities: vec!["shell:exec".into()],
                action: PolicyAction::Deny,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            },
            PolicyRule {
                match_capabilities: vec!["net:*".into()],
                action: PolicyAction::Deny,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            },
        ],
        resource_budgets: None,
    }
}

/// Convenience: text-only model response.
pub fn text_response(text: &str) -> Response {
    Response {
        content: vec![Content::Text(text.into())],
        usage: Usage {
            input_tokens: 100,
            output_tokens: 50,
        },
        stop_reason: StopReason::EndTurn,
    }
}

/// Convenience: model response with a single tool call.
pub fn tool_call_response(tool_name: &str, input: serde_json::Value) -> Response {
    Response {
        content: vec![Content::ToolCall {
            id: "call_1".into(),
            name: tool_name.into(),
            input,
        }],
        usage: Usage {
            input_tokens: 100,
            output_tokens: 50,
        },
        stop_reason: StopReason::ToolUse,
    }
}
