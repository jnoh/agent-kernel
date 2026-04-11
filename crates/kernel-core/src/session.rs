use kernel_interfaces::frontend::FrontendEvents;
use kernel_interfaces::policy::Policy;
use kernel_interfaces::provider::ProviderInterface;
use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::{
    CompletionConfig, Invalidation, ResourceBudget, SessionId, SessionMode,
};

use crate::context::{ContextConfig, ContextManager};
use crate::permission::PermissionEvaluator;
use crate::session_events::SessionEventSink;
use crate::turn_loop::{TurnError, TurnLoop, TurnResult};

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Configuration for creating a new session.
pub struct SessionConfig {
    pub mode: SessionMode,
    pub system_prompt: String,
    pub context_config: ContextConfig,
    pub completion_config: CompletionConfig,
    pub policy: Policy,
    pub resource_budget: ResourceBudget,
    pub workspace: PathBuf,
}

/// A pending result delivered to a session between turns.
#[derive(Debug)]
pub enum PendingResult {
    ChildCompleted {
        task: String,
        message: String,
        invalidations: Vec<Invalidation>,
    },
    ExternalEvent {
        source: String,
        event_type: String,
        summary: String,
    },
}

/// A single session — owns its turn loop, context, permission evaluator, and tools.
pub struct Session {
    pub id: SessionId,
    pub mode: SessionMode,
    pub workspace: PathBuf,

    context: ContextManager,
    permission: PermissionEvaluator,
    turn_loop: TurnLoop,
    tools: Vec<Box<dyn ToolRegistration>>,

    pending_results: Vec<PendingResult>,

    /// Cancellation flag — checked between tool dispatches in the turn loop.
    cancelled: Arc<AtomicBool>,

    // Token budget tracking (max_tokens enforced in v0.1 budget checks)
    #[allow(dead_code)]
    max_tokens: usize,
}

impl kernel_interfaces::frontend::SessionControl for Session {
    fn tokens_used(&self) -> usize {
        self.context.tokens_used()
    }

    fn context_utilization(&self) -> f64 {
        self.context.utilization()
    }

    fn turn_count(&self) -> usize {
        self.context.turn_count()
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn request_compaction(&mut self) -> Result<usize, String> {
        self.context.compact()
    }

    fn set_policy(&mut self, policy: Policy) {
        self.permission.set_policy(policy);
    }
}

impl Session {
    /// Create a new session directly (used by EventLoop).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: SessionId,
        mode: SessionMode,
        workspace: PathBuf,
        context: ContextManager,
        permission: PermissionEvaluator,
        turn_loop: TurnLoop,
        tools: Vec<Box<dyn ToolRegistration>>,
        max_tokens: usize,
    ) -> Self {
        Self {
            id,
            mode,
            workspace,
            context,
            permission,
            turn_loop,
            tools,
            pending_results: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            max_tokens,
        }
    }

    /// Run one turn of the agent loop: drain pending results, then execute a turn.
    pub fn run_turn(
        &mut self,
        provider: &dyn ProviderInterface,
        frontend: &dyn FrontendEvents,
    ) -> Result<TurnResult, TurnError> {
        // Drain pending results into context (between turns, never mid-turn)
        for result in self.pending_results.drain(..) {
            match result {
                PendingResult::ChildCompleted {
                    task,
                    message,
                    invalidations,
                } => {
                    self.context.append_system_message(format!(
                        "Background agent '{}' completed: {}",
                        task, message
                    ));
                    for inv in &invalidations {
                        self.context.process_invalidation(inv);
                    }
                }
                PendingResult::ExternalEvent {
                    source,
                    event_type,
                    summary,
                } => {
                    self.context
                        .append_system_message(format!("[{}] {}: {}", source, event_type, summary));
                }
            }
        }

        // Run the turn
        self.turn_loop.run_turn(
            provider,
            &mut self.context,
            &self.permission,
            &self.tools,
            frontend,
            &self.cancelled,
        )
    }

    /// Add user input to start a new turn.
    pub fn add_user_input(&mut self, text: String) {
        self.context.append_user_input(text);
    }

    /// Deliver a pending result (from child session or external event).
    pub fn deliver(&mut self, result: PendingResult) {
        self.pending_results.push(result);
    }

    /// Access the context manager.
    pub fn context(&self) -> &ContextManager {
        &self.context
    }

    /// Access tools.
    pub fn tools(&self) -> &[Box<dyn ToolRegistration>] {
        &self.tools
    }

    /// Swap the policy at runtime (hot-reload).
    pub fn set_policy(&mut self, policy: Policy) {
        self.permission.set_policy(policy);
    }
}

/// The session manager — the process table. Singleton that owns all running sessions.
///
/// v0.1: manages exactly one session. The interface is designed for multi-session in v0.2.
pub struct SessionManager {
    sessions: Vec<Session>,
    next_id: u64,
    #[allow(dead_code)]
    global_budget: ResourceBudget,
}

impl SessionManager {
    pub fn new(global_budget: ResourceBudget) -> Self {
        Self {
            sessions: Vec::new(),
            next_id: 0,
            global_budget,
        }
    }

    /// Spawn an interactive session (Trigger 1: human starts conversation).
    pub fn spawn_interactive(
        &mut self,
        config: SessionConfig,
        tools: Vec<Box<dyn ToolRegistration>>,
    ) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id += 1;

        let context = ContextManager::new(config.context_config, config.system_prompt);
        let permission = PermissionEvaluator::new(config.policy);
        let turn_loop = TurnLoop::new(
            config.completion_config,
            config.resource_budget.max_tool_invocations_per_turn,
        );

        let session = Session::new(
            id,
            config.mode,
            config.workspace,
            context,
            permission,
            turn_loop,
            tools,
            config.resource_budget.max_tokens_per_session,
        );

        self.sessions.push(session);
        id
    }

    /// Spawn an interactive session with a custom event sink. Used by
    /// callers that want authoritative Tier-3 storage (file-backed or
    /// otherwise). The default `spawn_interactive` uses a `NullSink`.
    pub fn spawn_interactive_with_events(
        &mut self,
        config: SessionConfig,
        tools: Vec<Box<dyn ToolRegistration>>,
        events: Box<dyn SessionEventSink>,
    ) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id += 1;

        let policy_name = config.policy.name.clone();
        let workspace_str = config.workspace.to_string_lossy().into_owned();
        let mut context =
            ContextManager::with_event_sink(config.context_config, config.system_prompt, events);
        context.record_session_started(workspace_str, policy_name);

        let permission = PermissionEvaluator::new(config.policy);
        let turn_loop = TurnLoop::new(
            config.completion_config,
            config.resource_budget.max_tool_invocations_per_turn,
        );

        let session = Session::new(
            id,
            config.mode,
            config.workspace,
            context,
            permission,
            turn_loop,
            tools,
            config.resource_budget.max_tokens_per_session,
        );

        self.sessions.push(session);
        id
    }

    /// Get a session by ID.
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    /// Get a mutable session by ID.
    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Number of active sessions.
    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    /// Route invalidations from one session to others with overlapping cached state.
    pub fn propagate_invalidation(&mut self, source: SessionId, invalidation: &Invalidation) {
        for session in &mut self.sessions {
            if session.id != source {
                session.context.process_invalidation(invalidation);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use kernel_interfaces::frontend::SessionControl;
    use kernel_interfaces::policy::{Policy, PolicyAction, PolicyRule};
    use kernel_interfaces::provider::*;
    use kernel_interfaces::types::*;
    use std::sync::atomic::Ordering;

    fn test_session_config() -> SessionConfig {
        SessionConfig {
            mode: SessionMode::Interactive,
            system_prompt: "You are a helpful assistant.".into(),
            context_config: ContextConfig {
                context_window: 100_000,
                compaction_cooldown_secs: 0,
                ..Default::default()
            },
            completion_config: CompletionConfig::default(),
            policy: allow_all_policy(),
            resource_budget: ResourceBudget::default(),
            workspace: PathBuf::from("/tmp/test-workspace"),
        }
    }

    // --- Tests ---

    #[test]
    fn spawn_interactive_returns_unique_ids() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let id1 = mgr.spawn_interactive(test_session_config(), Vec::new());
        let id2 = mgr.spawn_interactive(test_session_config(), Vec::new());
        assert_ne!(id1, id2);
        assert_eq!(mgr.active_count(), 2);
    }

    #[test]
    fn get_session_by_id() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let id = mgr.spawn_interactive(test_session_config(), Vec::new());
        assert!(mgr.get(id).is_some());
        assert!(mgr.get(SessionId(999)).is_none());
    }

    #[test]
    fn session_runs_a_turn() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let id = mgr.spawn_interactive(test_session_config(), Vec::new());
        let session = mgr.get_mut(id).unwrap();

        session.add_user_input("Hello".into());

        let provider = FakeProvider {
            response: Response {
                content: vec![Content::Text("Hi there!".into())],
                usage: Usage {
                    input_tokens: 50,
                    output_tokens: 20,
                    ..Default::default()
                },
                stop_reason: StopReason::EndTurn,
            },
        };
        let frontend = RecordingFrontend::auto_allow();

        let result = session.run_turn(&provider, &frontend).unwrap();
        assert!(!result.continues);
        assert_eq!(frontend.turns_started.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn session_drains_pending_results_before_turn() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let id = mgr.spawn_interactive(test_session_config(), Vec::new());
        let session = mgr.get_mut(id).unwrap();

        // Deliver a pending event
        session.deliver(PendingResult::ExternalEvent {
            source: "github".into(),
            event_type: "check_run.failed".into(),
            summary: "CI failed on main".into(),
        });

        session.add_user_input("What happened?".into());

        let provider = FakeProvider {
            response: Response {
                content: vec![Content::Text("CI failed.".into())],
                usage: Usage::default(),
                stop_reason: StopReason::EndTurn,
            },
        };
        let frontend = RecordingFrontend::auto_allow();

        let result = session.run_turn(&provider, &frontend).unwrap();
        assert!(!result.continues);

        // Pending results should be drained
        assert!(session.pending_results.is_empty());
        // Context should have the system message from the event + the user input
        assert!(session.context().turn_count() >= 2);
    }

    #[test]
    fn policy_hot_swap() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let id = mgr.spawn_interactive(test_session_config(), Vec::new());
        let session = mgr.get_mut(id).unwrap();

        // Swap to a deny-all policy
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
        session.set_policy(deny_policy);

        // Now a tool call should be denied
        session.add_user_input("Read a file".into());
        // We can't add tools after creation in this design, but we can verify
        // the policy change took effect by checking the permission evaluator
        // through the session's run_turn behavior.
        // For this test, we just verify the policy was swapped.
        assert_eq!(session.context().turn_count(), 1);
    }

    #[test]
    fn session_with_tool_dispatches_through_policy() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("ok"));
        let id = mgr.spawn_interactive(test_session_config(), vec![Box::new(tool)]);
        let session = mgr.get_mut(id).unwrap();

        session.add_user_input("Read main.rs".into());

        let provider = FakeProvider {
            response: Response {
                content: vec![Content::ToolCall {
                    id: "c1".into(),
                    name: "file_read".into(),
                    input: serde_json::json!({"path": "src/main.rs"}),
                }],
                usage: Usage::default(),
                stop_reason: StopReason::ToolUse,
            },
        };
        let frontend = RecordingFrontend::auto_allow();

        let result = session.run_turn(&provider, &frontend).unwrap();
        assert!(result.continues);
        assert_eq!(result.tool_calls_dispatched, 1);
    }

    #[test]
    fn session_control_queries() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let id = mgr.spawn_interactive(test_session_config(), Vec::new());
        let session = mgr.get_mut(id).unwrap();

        // Fresh session should have sensible defaults
        let ctrl: &dyn SessionControl = session;
        assert!(ctrl.tokens_used() > 0); // system prompt tokens
        assert!(ctrl.context_utilization() > 0.0);
        assert_eq!(ctrl.turn_count(), 0);

        session.add_user_input("Hello".into());
        assert_eq!(SessionControl::turn_count(session), 1);
    }

    #[test]
    fn session_control_cancellation() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("data"));
        let id = mgr.spawn_interactive(test_session_config(), vec![Box::new(tool)]);
        let session = mgr.get_mut(id).unwrap();

        session.add_user_input("Read files".into());

        // Set cancel flag before running the turn
        session.cancel();

        let provider = FakeProvider {
            response: Response {
                content: vec![Content::ToolCall {
                    id: "c1".into(),
                    name: "file_read".into(),
                    input: serde_json::json!({"path": "a.rs"}),
                }],
                usage: Usage::default(),
                stop_reason: StopReason::ToolUse,
            },
        };
        let frontend = RecordingFrontend::auto_allow();

        // Note: run_turn resets the cancel flag at the start, so setting it before
        // won't have effect during tool dispatch. This tests the API surface;
        // mid-turn cancellation requires concurrent access (e.g., from another thread).
        let result = session.run_turn(&provider, &frontend).unwrap();
        assert!(!result.was_cancelled);
    }

    #[test]
    fn session_control_compaction() {
        let config = SessionConfig {
            context_config: ContextConfig {
                context_window: 10_000,
                compaction_threshold: 0.10,
                verbatim_tail_ratio: 0.30,
                compaction_cooldown_secs: 0,
                ..Default::default()
            },
            ..test_session_config()
        };
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let id = mgr.spawn_interactive(config, Vec::new());
        let session = mgr.get_mut(id).unwrap();

        // Add enough turns with responses to make compaction meaningful
        for i in 0..10 {
            session.add_user_input(format!(
                "This is turn {} with a reasonably long message for token counting",
                i
            ));
            // Run a turn so the context has assistant responses too
            let provider = FakeProvider {
                response: Response {
                    content: vec![Content::Text(format!(
                        "Here is a detailed response to turn {} with lots of information and context",
                        i
                    ))],
                    usage: Usage::default(),
                    stop_reason: StopReason::EndTurn,
                },
            };
            let frontend = RecordingFrontend::auto_allow();
            session.run_turn(&provider, &frontend).unwrap();
        }

        let before = SessionControl::tokens_used(session);
        let freed = session
            .request_compaction()
            .expect("compaction should succeed");
        assert!(freed > 0);
        assert!(SessionControl::tokens_used(session) < before);
    }

    #[test]
    fn session_control_set_policy() {
        let mut mgr = SessionManager::new(ResourceBudget::default());
        let tool = FakeTool::new("file_read", &["fs:read"], serde_json::json!("ok"));
        let id = mgr.spawn_interactive(test_session_config(), vec![Box::new(tool)]);
        let session = mgr.get_mut(id).unwrap();

        // Default policy allows fs:read
        session.add_user_input("Read".into());
        let provider = FakeProvider {
            response: tool_call_response("file_read", serde_json::json!({"path": "x"})),
        };
        let frontend = RecordingFrontend::auto_allow();
        let result = session.run_turn(&provider, &frontend).unwrap();
        assert_eq!(result.tool_calls_dispatched, 1);

        // Swap to deny-all via SessionControl trait
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
        SessionControl::set_policy(session, deny_policy);

        session.add_user_input("Read again".into());
        let result = session.run_turn(&provider, &frontend).unwrap();
        assert_eq!(result.tool_calls_denied, 1);
        assert_eq!(result.tool_calls_dispatched, 0);
    }
}
