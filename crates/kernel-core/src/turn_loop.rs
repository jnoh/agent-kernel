use kernel_interfaces::frontend::FrontendEvents;
use kernel_interfaces::provider::{ProviderInterface, Response, StopReason};
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::{CompletionConfig, Content, Decision, TurnId};

use crate::context::ContextManager;
use crate::permission::PermissionEvaluator;

use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Whether the turn was cancelled via the SessionControl cancel flag.
    pub was_cancelled: bool,
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

/// Whether a tool dispatch resulted in execution or denial.
enum DispatchOutcome {
    Executed,
    Denied,
}

/// Execute a tool and record the result in context. Returns Executed on success or error
/// (both count as dispatched — the model sees the result either way).
fn execute_tool(
    tool: &dyn ToolRegistration,
    tool_name: &str,
    input: &serde_json::Value,
    context: &mut ContextManager,
    frontend: &dyn FrontendEvents,
) -> DispatchOutcome {
    match tool.execute(input.clone()) {
        Ok(output) => {
            for inv in &output.invalidations {
                context.process_invalidation(inv);
            }
            frontend.on_tool_result(tool_name, &output);
            context.append_tool_exchange(tool_name.to_string(), input.clone(), output.result);
        }
        Err(e) => {
            let error_result = serde_json::json!({
                "error": "execution_failed",
                "message": e.to_string()
            });
            context.append_tool_exchange(tool_name.to_string(), input.clone(), error_result);
        }
    }
    DispatchOutcome::Executed
}

/// Record a denied tool call in context.
fn deny_tool(
    tool_name: &str,
    input: &serde_json::Value,
    reason: &str,
    context: &mut ContextManager,
    frontend: Option<&dyn FrontendEvents>,
) -> DispatchOutcome {
    let denied = kernel_interfaces::tool::ToolOutput::denied(reason);
    if let Some(fe) = frontend {
        fe.on_tool_result(tool_name, &denied);
    }
    context.append_tool_exchange(tool_name.to_string(), input.clone(), denied.result);
    DispatchOutcome::Denied
}

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
        frontend: &dyn FrontendEvents,
        cancelled: &AtomicBool,
    ) -> Result<TurnResult, TurnError> {
        let turn_id = TurnId(self.next_turn_id);
        self.next_turn_id += 1;

        // Reset cancel flag at the start of each turn
        cancelled.store(false, Ordering::Release);

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

        // 1. Assemble prompt with tool definitions
        let mut prompt = context.assemble();

        // Inject tool schemas so the model knows what's available.
        // v0.1: send all tools every turn. Demand-paging (request_tool) is v0.2.
        if prompt.tool_definitions.is_empty() && !tools.is_empty() {
            prompt.tool_definitions = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name(),
                        "description": t.description(),
                        "input_schema": t.schema(),
                    })
                })
                .collect();
        }

        // 2. Call model
        let response = provider
            .complete(&prompt, &self.config)
            .map_err(TurnError::Provider)?;

        // 3. Parse tool calls from response
        let (text_parts, tool_calls) = parse_response(&response);

        // Record assistant text response and surface to frontend
        if !text_parts.is_empty() {
            let text = text_parts.join("");
            frontend.on_text(&text);
            context.append_assistant_response(text);
        }

        // 4. Dispatch tool calls through permission evaluator
        let mut tool_calls_dispatched = 0;
        let mut tool_calls_denied = 0;
        let mut was_cancelled = false;

        for (_call_id, tool_name, input) in &tool_calls {
            // Check cancellation between tool dispatches
            if cancelled.load(Ordering::Acquire) {
                was_cancelled = true;
                break;
            }
            if tool_calls_dispatched + tool_calls_denied >= self.max_tool_invocations_per_turn {
                let msg = format!(
                    "max tool invocations per turn ({}) exceeded",
                    self.max_tool_invocations_per_turn
                );
                frontend.on_error(&kernel_interfaces::frontend::KernelError {
                    message: msg.clone(),
                    recoverable: true,
                });
                context.append_tool_exchange(
                    tool_name.clone(),
                    input.clone(),
                    serde_json::json!({ "error": "budget_exceeded", "message": msg }),
                );
                break;
            }

            // Find the tool
            let Some(tool) = tools.iter().find(|t| t.name() == tool_name) else {
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

            let outcome = match decision {
                Decision::Allow => execute_tool(tool.as_ref(), tool_name, input, context, frontend),
                Decision::Deny(reason) => {
                    deny_tool(tool_name, input, &reason, context, Some(frontend))
                }
                Decision::Ask => {
                    let request = kernel_interfaces::frontend::PermissionRequest {
                        tool_name: tool_name.clone(),
                        capabilities: tool.capabilities().iter().map(|c| c.0.clone()).collect(),
                        input_summary: input.to_string(),
                    };
                    match frontend.on_permission_request(&request) {
                        Decision::Allow => {
                            execute_tool(tool.as_ref(), tool_name, input, context, frontend)
                        }
                        Decision::Deny(reason) => {
                            deny_tool(tool_name, input, &reason, context, None)
                        }
                        Decision::Ask => {
                            deny_tool(tool_name, input, "user did not decide", context, None)
                        }
                    }
                }
            };

            match outcome {
                DispatchOutcome::Executed => tool_calls_dispatched += 1,
                DispatchOutcome::Denied => tool_calls_denied += 1,
            }
        }

        let continues = !was_cancelled
            && response.stop_reason == StopReason::ToolUse
            && tool_calls_dispatched > 0;

        frontend.on_turn_end(turn_id);

        Ok(TurnResult {
            turn_id,
            continues,
            tool_calls_dispatched,
            tool_calls_denied,
            was_cancelled,
        })
    }
}

/// Extract text and tool calls from a model response.
fn parse_response(response: &Response) -> (Vec<String>, Vec<(String, String, serde_json::Value)>) {
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
    use crate::testutil::*;
    use kernel_interfaces::policy::{Policy, PolicyAction, PolicyRule};
    use kernel_interfaces::provider::*;
    use kernel_interfaces::tool::ToolRegistration;
    use kernel_interfaces::types::{CompletionConfig, TurnId};

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

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
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

    #[test]
    fn text_only_response_does_not_continue() {
        let (mut cm, pe) = context_and_permission();
        cm.append_user_input("Hello".into());
        let provider = FakeProvider {
            response: text_response("Hi there!"),
        };
        let frontend = RecordingFrontend::auto_allow();
        let tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

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
        let frontend = RecordingFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("fn main() {}"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

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
        let frontend = RecordingFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("data"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

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
        let frontend = RecordingFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("content"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

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
        let frontend = RecordingFrontend::auto_deny();
        let tool = FakeTool::new("shell", &["shell:exec"], serde_json::json!("done"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

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
        let frontend = RecordingFrontend::auto_allow();
        let tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        let result = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

        assert!(!result.continues);
        assert_eq!(result.tool_calls_dispatched, 0);
    }

    #[test]
    fn tool_invocation_budget_enforced() {
        let (mut cm, pe) = context_and_permission();
        cm.append_user_input("Do everything".into());

        let response = Response {
            content: vec![
                Content::ToolCall {
                    id: "1".into(),
                    name: "file_read".into(),
                    input: serde_json::json!({}),
                },
                Content::ToolCall {
                    id: "2".into(),
                    name: "file_read".into(),
                    input: serde_json::json!({}),
                },
                Content::ToolCall {
                    id: "3".into(),
                    name: "file_read".into(),
                    input: serde_json::json!({}),
                },
            ],
            usage: Usage::default(),
            stop_reason: StopReason::ToolUse,
        };
        let provider = FakeProvider { response };
        let frontend = RecordingFrontend::auto_allow();
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("data"));
        let tools: Vec<Box<dyn ToolRegistration>> = vec![Box::new(tool)];
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 2);

        let result = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

        assert_eq!(result.tool_calls_dispatched, 2);
    }

    #[test]
    fn turn_ids_increment() {
        let (mut cm, pe) = context_and_permission();
        let provider = FakeProvider {
            response: text_response("Hi"),
        };
        let frontend = RecordingFrontend::auto_allow();
        let tools: Vec<Box<dyn ToolRegistration>> = Vec::new();
        let mut turn_loop = TurnLoop::new(CompletionConfig::default(), 20);

        cm.append_user_input("Hello".into());
        let r1 = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();
        cm.append_user_input("Again".into());
        let r2 = turn_loop
            .run_turn(&provider, &mut cm, &pe, &tools, &frontend, &no_cancel())
            .unwrap();

        assert_eq!(r1.turn_id, TurnId(0));
        assert_eq!(r2.turn_id, TurnId(1));
    }
}
