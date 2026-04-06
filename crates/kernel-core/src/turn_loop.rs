use kernel_interfaces::frontend::FrontendInterface;
use kernel_interfaces::provider::{ProviderInterface, Response, StopReason};
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::{CompletionConfig, Content, Decision, TurnId};

use crate::context::ContextManager;
use crate::permission::PermissionEvaluator;

/// The result of running a single turn.
#[derive(Debug)]
pub struct TurnResult {
    pub turn_id: TurnId,
    /// Whether the model wants to continue (made tool calls) or yielded to the user.
    pub continues: bool,
    /// Number of tool calls dispatched this turn.
    pub tool_calls_dispatched: usize,
    /// Number of tool calls denied by policy.
    pub tool_calls_denied: usize,
}

/// Errors that can occur during a turn.
#[derive(Debug)]
pub enum TurnError {
    /// The provider returned an error.
    Provider(kernel_interfaces::provider::ProviderError),
    /// The context manager needs compaction but it failed.
    CompactionFailed(String),
    /// Resource budget exceeded.
    BudgetExceeded(String),
}

impl std::fmt::Display for TurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(e) => write!(f, "provider error: {e}"),
            Self::CompactionFailed(msg) => write!(f, "compaction failed: {msg}"),
            Self::BudgetExceeded(msg) => write!(f, "budget exceeded: {msg}"),
        }
    }
}

impl std::error::Error for TurnError {}

/// The turn loop — the heartbeat. Every other subsystem exists to serve it.
///
/// Single-threaded per session. Does not know what model it's talking to
/// (that's behind ProviderInterface) or what the frontend looks like.
pub struct TurnLoop {
    next_turn_id: u64,
    config: CompletionConfig,
    max_tool_invocations_per_turn: usize,
}

impl TurnLoop {
    pub fn new(config: CompletionConfig, max_tool_invocations_per_turn: usize) -> Self {
        Self {
            next_turn_id: 0,
            config,
            max_tool_invocations_per_turn,
        }
    }

    /// Run a single turn: assemble prompt → call model → dispatch tools → feed results back.
    ///
    /// Returns whether the model wants to continue (more tool calls) or is done.
    pub fn run_turn(
        &mut self,
        provider: &dyn ProviderInterface,
        context: &mut ContextManager,
        permission: &PermissionEvaluator,
        tools: &[Box<dyn ToolRegistration>],
        frontend: &dyn FrontendInterface,
    ) -> Result<TurnResult, TurnError> {
        let turn_id = TurnId(self.next_turn_id);
        self.next_turn_id += 1;

        frontend.on_turn_start(turn_id);

        // Check if compaction is needed before assembling prompt
        if context.should_compact() {
            match context.compact() {
                Ok(freed) => {
                    frontend.on_compaction(&kernel_interfaces::frontend::CompactionSummary {
                        turns_before: context.turn_count(),
                        turns_after: context.turn_count(),
                        tokens_freed: freed,
                    });
                }
                Err(msg) => {
                    return Err(TurnError::CompactionFailed(msg));
                }
            }
        }

        // 1. Assemble prompt
        let prompt = context.assemble();

        // 2. Call model
        let response = provider
            .complete(&prompt, &self.config)
            .map_err(TurnError::Provider)?;

        // 3. Parse tool calls from response
        let (text_parts, tool_calls) = parse_response(&response);

        // Record assistant text response
        if !text_parts.is_empty() {
            context.append_assistant_response(text_parts.join(""));
        }

        // 4. Dispatch tool calls through permission evaluator
        let mut tool_calls_dispatched = 0;
        let mut tool_calls_denied = 0;

        for (_call_id, tool_name, input) in &tool_calls {
            if tool_calls_dispatched + tool_calls_denied >= self.max_tool_invocations_per_turn {
                let msg = format!(
                    "max tool invocations per turn ({}) exceeded",
                    self.max_tool_invocations_per_turn
                );
                frontend.on_error(&kernel_interfaces::frontend::KernelError {
                    message: msg.clone(),
                    recoverable: true,
                });
                // Feed a budget error back to the model
                context.append_tool_exchange(
                    tool_name.clone(),
                    input.clone(),
                    serde_json::json!({ "error": "budget_exceeded", "message": msg }),
                );
                break;
            }

            // Find the tool
            let tool = tools.iter().find(|t| t.name() == tool_name);
            let Some(tool) = tool else {
                // Tool not found — feed error back to model
                context.append_tool_exchange(
                    tool_name.clone(),
                    input.clone(),
                    serde_json::json!({ "error": "tool_not_found", "name": tool_name }),
                );
                continue;
            };

            frontend.on_tool_call(tool_name, input);

            // L1: Dispatch gate check
            let decision = permission.evaluate(tool.as_ref());

            match decision {
                Decision::Allow => {
                    match tool.execute(input.clone()) {
                        Ok(output) => {
                            // Process invalidations
                            for inv in &output.invalidations {
                                context.process_invalidation(inv);
                            }
                            frontend.on_tool_result(tool_name, &output);
                            context.append_tool_exchange(
                                tool_name.clone(),
                                input.clone(),
                                output.result,
                            );
                            tool_calls_dispatched += 1;
                        }
                        Err(e) => {
                            let error_result = serde_json::json!({
                                "error": "execution_failed",
                                "message": e.to_string()
                            });
                            context.append_tool_exchange(
                                tool_name.clone(),
                                input.clone(),
                                error_result,
                            );
                            tool_calls_dispatched += 1;
                        }
                    }
                }
                Decision::Deny(reason) => {
                    let denied = kernel_interfaces::tool::ToolOutput::denied(&reason);
                    frontend.on_tool_result(tool_name, &denied);
                    context.append_tool_exchange(
                        tool_name.clone(),
                        input.clone(),
                        denied.result,
                    );
                    tool_calls_denied += 1;
                }
                Decision::Ask => {
                    let request = kernel_interfaces::frontend::PermissionRequest {
                        tool_name: tool_name.clone(),
                        capabilities: tool
                            .capabilities()
                            .iter()
                            .map(|c| c.0.clone())
                            .collect(),
                        input_summary: input.to_string(),
                    };
                    let user_decision = frontend.on_permission_request(&request);

                    match user_decision {
                        Decision::Allow => {
                            match tool.execute(input.clone()) {
                                Ok(output) => {
                                    for inv in &output.invalidations {
                                        context.process_invalidation(inv);
                                    }
                                    frontend.on_tool_result(tool_name, &output);
                                    context.append_tool_exchange(
                                        tool_name.clone(),
                                        input.clone(),
                                        output.result,
                                    );
                                    tool_calls_dispatched += 1;
                                }
                                Err(e) => {
                                    let error_result = serde_json::json!({
                                        "error": "execution_failed",
                                        "message": e.to_string()
                                    });
                                    context.append_tool_exchange(
                                        tool_name.clone(),
                                        input.clone(),
                                        error_result,
                                    );
                                    tool_calls_dispatched += 1;
                                }
                            }
                        }
                        Decision::Deny(reason) => {
                            let denied = kernel_interfaces::tool::ToolOutput::denied(&reason);
                            context.append_tool_exchange(
                                tool_name.clone(),
                                input.clone(),
                                denied.result,
                            );
                            tool_calls_denied += 1;
                        }
                        Decision::Ask => {
                            // User didn't decide — treat as deny
                            let denied =
                                kernel_interfaces::tool::ToolOutput::denied("user did not decide");
                            context.append_tool_exchange(
                                tool_name.clone(),
                                input.clone(),
                                denied.result,
                            );
                            tool_calls_denied += 1;
                        }
                    }
                }
            }
        }

        let continues = response.stop_reason == StopReason::ToolUse && tool_calls_dispatched > 0;

        frontend.on_turn_end(turn_id);

        Ok(TurnResult {
            turn_id,
            continues,
            tool_calls_dispatched,
            tool_calls_denied,
        })
    }
}

/// Extract text and tool calls from a model response.
fn parse_response(
    response: &Response,
) -> (Vec<String>, Vec<(String, String, serde_json::Value)>) {
    let mut text_parts = Vec::new();
    let mut tool_calls = Vec::new();

    for content in &response.content {
        match content {
            Content::Text(t) => text_parts.push(t.clone()),
            Content::ToolCall { id, name, input } => {
                tool_calls.push((id.clone(), name.clone(), input.clone()));
            }
            Content::ToolResult { .. } => {
                // Shouldn't appear in model output, ignore
            }
        }
    }

    (text_parts, tool_calls)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ContextConfig;
    use kernel_interfaces::frontend::*;
    use kernel_interfaces::policy::{Policy, PolicyAction, PolicyRule};
    use kernel_interfaces::provider::*;
    use kernel_interfaces::tool::*;
    use kernel_interfaces::types::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    // --- Test doubles ---

    /// A provider that returns a fixed response.
    struct FakeProvider {
        response: Response,
    }

    impl ProviderInterface for FakeProvider {
        fn complete(&self, _prompt: &Prompt, _config: &CompletionConfig) -> Result<Response, ProviderError> {
            Ok(self.response.clone())
        }
        fn count_tokens(&self, _content: &Content) -> usize { 10 }
        fn capabilities(&self) -> ProviderCaps { ProviderCaps::default() }
    }

    /// A tool that records whether it was called and returns a fixed value.
    struct FakeTool {
        name: String,
        caps: CapabilitySet,
        relevance: RelevanceSignal,
        called: AtomicBool,
        return_value: serde_json::Value,
    }

    impl FakeTool {
        fn new(name: &str, caps: &[&str], return_value: serde_json::Value) -> Self {
            Self {
                name: name.into(),
                caps: caps.iter().map(|c| Capability::new(*c)).collect(),
                relevance: RelevanceSignal { keywords: Vec::new(), tags: Vec::new() },
                called: AtomicBool::new(false),
                return_value,
            }
        }

        #[allow(dead_code)]
        fn was_called(&self) -> bool {
            self.called.load(Ordering::Relaxed)
        }
    }

    impl ToolRegistration for FakeTool {
        fn name(&self) -> &str { &self.name }
        fn description(&self) -> &str { "test tool" }
        fn capabilities(&self) -> &CapabilitySet { &self.caps }
        fn schema(&self) -> &serde_json::Value { &serde_json::Value::Null }
        fn cost(&self) -> TokenEstimate { TokenEstimate(10) }
        fn relevance(&self) -> &RelevanceSignal { &self.relevance }
        fn execute(&self, _input: serde_json::Value) -> Result<ToolOutput, ToolError> {
            self.called.store(true, Ordering::Relaxed);
            Ok(ToolOutput::readonly(self.return_value.clone()))
        }
    }

    /// A frontend that auto-allows permission requests and tracks calls.
    struct FakeFrontend {
        permission_response: Decision,
        turns_started: AtomicU64,
        turns_ended: AtomicU64,
    }

    impl FakeFrontend {
        fn auto_allow() -> Self {
            Self {
                permission_response: Decision::Allow,
                turns_started: AtomicU64::new(0),
                turns_ended: AtomicU64::new(0),
            }
        }

        fn auto_deny() -> Self {
            Self {
                permission_response: Decision::Deny("user denied".into()),
                turns_started: AtomicU64::new(0),
                turns_ended: AtomicU64::new(0),
            }
        }
    }

    impl FrontendInterface for FakeFrontend {
        fn on_turn_start(&self, _turn_id: TurnId) {
            self.turns_started.fetch_add(1, Ordering::Relaxed);
        }
        fn on_stream_chunk(&self, _chunk: &StreamChunk) {}
        fn on_tool_call(&self, _name: &str, _input: &serde_json::Value) {}
        fn on_tool_result(&self, _name: &str, _result: &ToolOutput) {}
        fn on_permission_request(&self, _request: &PermissionRequest) -> Decision {
            self.permission_response.clone()
        }
        fn on_turn_end(&self, _turn_id: TurnId) {
            self.turns_ended.fetch_add(1, Ordering::Relaxed);
        }
        fn on_compaction(&self, _summary: &CompactionSummary) {}
        fn on_workspace_changed(&self, _new_root: &Path) {}
        fn on_error(&self, _error: &KernelError) {}
    }

    fn allow_all_policy() -> Policy {
        Policy {
            version: 1,
            name: "allow-all".into(),
            rules: vec![PolicyRule {
                match_capabilities: vec!["fs:read".into(), "fs:write".into(), "shell:exec".into(), "net:*".into()],
                action: PolicyAction::Allow,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            }],
            resource_budgets: None,
        }
    }

    fn ask_all_policy() -> Policy {
        Policy {
            version: 1,
            name: "ask-all".into(),
            rules: vec![PolicyRule {
                match_capabilities: vec!["fs:read".into(), "shell:exec".into()],
                action: PolicyAction::Ask,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            }],
            resource_budgets: None,
        }
    }

    fn text_response(text: &str) -> Response {
        Response {
            content: vec![Content::Text(text.into())],
            usage: Usage { input_tokens: 100, output_tokens: 50 },
            stop_reason: StopReason::EndTurn,
        }
    }

    fn tool_call_response(tool_name: &str, input: serde_json::Value) -> Response {
        Response {
            content: vec![Content::ToolCall {
                id: "call_1".into(),
                name: tool_name.into(),
                input,
            }],
            usage: Usage { input_tokens: 100, output_tokens: 50 },
            stop_reason: StopReason::ToolUse,
        }
    }

    fn context_and_permission() -> (ContextManager, PermissionEvaluator) {
        let config = ContextConfig {
            context_window: 100_000,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let cm = ContextManager::new(config, "You are a helpful assistant.".into());
        let pe = PermissionEvaluator::new(allow_all_policy());
        (cm, pe)
    }

    // --- Tests ---

    #[test]
    fn text_only_response_does_not_continue() {
        let (mut cm, pe) = context_and_permission();
        cm.append_user_input("Hello".into());
        let provider = FakeProvider { response: text_response("Hi there!") };
        let frontend = FakeFrontend::auto_allow();
        let tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        assert!(!result.continues);
        assert_eq!(result.tool_calls_dispatched, 0);
        assert_eq!(frontend.turns_started.load(Ordering::Relaxed), 1);
        assert_eq!(frontend.turns_ended.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn tool_call_dispatches_and_continues() {
        let (mut cm, pe) = context_and_permission();
        cm.append_user_input("List files".into());
        let provider = FakeProvider {
            response: tool_call_response("file_read", serde_json::json!({"path": "src/main.rs"})),
        };
        let frontend = FakeFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("fn main() {}"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        assert!(result.continues);
        assert_eq!(result.tool_calls_dispatched, 1);
    }

    #[test]
    fn denied_tool_call_does_not_continue() {
        let deny_policy = Policy {
            version: 1,
            name: "deny-all".into(),
            rules: vec![PolicyRule {
                match_capabilities: vec!["fs:read".into()],
                action: PolicyAction::Deny,
                scope_paths: Vec::new(),
                scope_commands: Vec::new(),
                except: Vec::new(),
            }],
            resource_budgets: None,
        };
        let config = ContextConfig {
            context_window: 100_000,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "System.".into());
        cm.append_user_input("Read file".into());
        let pe = PermissionEvaluator::new(deny_policy);
        let provider = FakeProvider {
            response: tool_call_response("file_read", serde_json::json!({"path": "secret.env"})),
        };
        let frontend = FakeFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("data"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        assert!(!result.continues);
        assert_eq!(result.tool_calls_dispatched, 0);
        assert_eq!(result.tool_calls_denied, 1);
    }

    #[test]
    fn ask_policy_with_allow_frontend_dispatches() {
        let config = ContextConfig {
            context_window: 100_000,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "System.".into());
        cm.append_user_input("Read file".into());
        let pe = PermissionEvaluator::new(ask_all_policy());
        let provider = FakeProvider {
            response: tool_call_response("file_read", serde_json::json!({"path": "src/lib.rs"})),
        };
        let frontend = FakeFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("content"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        assert!(result.continues);
        assert_eq!(result.tool_calls_dispatched, 1);
    }

    #[test]
    fn ask_policy_with_deny_frontend_denies() {
        let config = ContextConfig {
            context_window: 100_000,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "System.".into());
        cm.append_user_input("Run command".into());
        let pe = PermissionEvaluator::new(ask_all_policy());
        let provider = FakeProvider {
            response: tool_call_response("shell", serde_json::json!({"command": "rm -rf /"})),
        };
        let frontend = FakeFrontend::auto_deny();
        let tool = FakeTool::new("shell", &["shell:exec"], serde_json::json!("done"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        assert!(!result.continues);
        assert_eq!(result.tool_calls_denied, 1);
    }

    #[test]
    fn unknown_tool_handled_gracefully() {
        let (mut cm, pe) = context_and_permission();
        cm.append_user_input("Do something".into());
        let provider = FakeProvider {
            response: tool_call_response("nonexistent_tool", serde_json::json!({})),
        };
        let frontend = FakeFrontend::auto_allow();
        let tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        // Unknown tool is not dispatched and doesn't count as continue
        assert!(!result.continues);
        assert_eq!(result.tool_calls_dispatched, 0);
    }

    #[test]
    fn tool_invocation_budget_enforced() {
        let (mut cm, pe) = context_and_permission();
        cm.append_user_input("Do everything".into());

        // Response with 3 tool calls but budget of 2
        let response = Response {
            content: vec![
                Content::ToolCall { id: "1".into(), name: "file_read".into(), input: serde_json::json!({}) },
                Content::ToolCall { id: "2".into(), name: "file_read".into(), input: serde_json::json!({}) },
                Content::ToolCall { id: "3".into(), name: "file_read".into(), input: serde_json::json!({}) },
            ],
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
        };
        let provider = FakeProvider { response };
        let frontend = FakeFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("data"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 2); // budget: 2

        let result = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        assert_eq!(result.tool_calls_dispatched, 2);
    }

    #[test]
    fn turn_ids_increment() {
        let (mut cm, pe) = context_and_permission();
        let provider = FakeProvider { response: text_response("Hi") };
        let frontend = FakeFrontend::auto_allow();
        let tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        cm.append_user_input("Hello".into());
        let r1 = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();
        cm.append_user_input("Again".into());
        let r2 = turn_loop.run_turn(&provider, &mut cm, &pe, &tools, &frontend).unwrap();

        assert_eq!(r1.turn_id, TurnId(0));
        assert_eq!(r2.turn_id, TurnId(1));
    }
}
