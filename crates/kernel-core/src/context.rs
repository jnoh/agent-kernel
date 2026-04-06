use kernel_interfaces::tool::ToolRegistration;
use kernel_interfaces::types::{
    Content, Invalidation, Message, Prompt, Role,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for the context manager.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    /// Total context window size in tokens.
    pub context_window: usize,
    /// Trigger compaction at this fraction of capacity (0.60-0.70).
    pub compaction_threshold: f64,
    /// Max fraction of context the system prompt can consume.
    pub system_prompt_budget: f64,
    /// Fraction of conversation to keep uncompressed at the tail.
    pub verbatim_tail_ratio: f64,
    /// Minimum seconds between compactions (death spiral guard).
    pub compaction_cooldown_secs: u64,
    /// Max consecutive compaction failures before halting.
    pub max_compaction_failures: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            context_window: 200_000,
            compaction_threshold: 0.65,
            system_prompt_budget: 0.15,
            verbatim_tail_ratio: 0.30,
            compaction_cooldown_secs: 30,
            max_compaction_failures: 3,
        }
    }
}

/// A turn in the conversation history with token cost tracking.
#[derive(Debug, Clone)]
pub struct Turn {
    /// The user or system input that started this turn.
    pub input: Message,
    /// The assistant's response.
    pub response: Option<Message>,
    /// Tool calls and results within this turn.
    pub tool_exchanges: Vec<ToolExchange>,
    /// Estimated total tokens for this turn.
    pub token_estimate: usize,
    /// Whether this turn has been summarized (compressed).
    pub summarized: bool,
}

#[derive(Debug, Clone)]
pub struct ToolExchange {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub result: serde_json::Value,
    pub token_estimate: usize,
}

/// The scratchpad — Tier 1 working memory that survives compaction.
#[derive(Debug, Clone, Default)]
pub struct Scratchpad {
    /// Task plan steps.
    pub plan: Vec<PlanStep>,
    /// Constraints the model must not forget (e.g., "don't modify auth module").
    pub constraints: Vec<String>,
    /// Free-form notes.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PlanStep {
    pub description: String,
    pub completed: bool,
}

/// The context manager owns the token budget. It is the only subsystem
/// with global visibility across all context sources.
pub struct ContextManager {
    config: ContextConfig,

    // Tier 1: Working Memory (reserved, never evicted)
    system_prompt: String,
    scratchpad: Scratchpad,

    // Tier 2: Short-Term Memory (managed, evictable)
    turns: Vec<Turn>,

    // File content cache (evictable, re-readable from disk)
    file_cache: HashMap<PathBuf, String>,

    // Tool registry for demand-paging
    tool_definitions_in_context: Vec<serde_json::Value>,
    tool_names_in_context: Vec<String>,

    // Token accounting
    tokens_used: usize,

    // Compaction state (death spiral guards)
    consecutive_compaction_failures: u32,
    last_compaction_time: Option<std::time::Instant>,
}

impl ContextManager {
    pub fn new(config: ContextConfig, system_prompt: String) -> Self {
        let system_prompt_tokens = estimate_tokens(&system_prompt);
        let max_system = (config.context_window as f64 * config.system_prompt_budget) as usize;

        if system_prompt_tokens > max_system {
            // Log warning — system prompt exceeds budget cap.
            // In production this would surface to the frontend.
            eprintln!(
                "warning: system prompt ({} tokens) exceeds budget cap ({} tokens)",
                system_prompt_tokens, max_system
            );
        }

        Self {
            config,
            system_prompt,
            scratchpad: Scratchpad::default(),
            turns: Vec::new(),
            file_cache: HashMap::new(),
            tool_definitions_in_context: Vec::new(),
            tool_names_in_context: Vec::new(),
            tokens_used: system_prompt_tokens,
            consecutive_compaction_failures: 0,
            last_compaction_time: None,
        }
    }

    /// Current token usage.
    pub fn tokens_used(&self) -> usize {
        self.tokens_used
    }

    /// Total context window size.
    pub fn context_window(&self) -> usize {
        self.config.context_window
    }

    /// Fraction of context currently used.
    pub fn utilization(&self) -> f64 {
        self.tokens_used as f64 / self.config.context_window as f64
    }

    /// Whether compaction should be triggered.
    pub fn should_compact(&self) -> bool {
        self.utilization() >= self.config.compaction_threshold
    }

    /// Access the scratchpad (Tier 1 — survives compaction).
    pub fn scratchpad(&self) -> &Scratchpad {
        &self.scratchpad
    }

    pub fn scratchpad_mut(&mut self) -> &mut Scratchpad {
        &mut self.scratchpad
    }

    /// Number of turns in history.
    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }

    /// Append a user input as a new turn.
    pub fn append_user_input(&mut self, text: String) {
        let tokens = estimate_tokens(&text);
        self.turns.push(Turn {
            input: Message {
                role: Role::User,
                content: vec![Content::Text(text)],
            },
            response: None,
            tool_exchanges: Vec::new(),
            token_estimate: tokens,
            summarized: false,
        });
        self.tokens_used += tokens;
    }

    /// Record the assistant's response for the current turn.
    pub fn append_assistant_response(&mut self, text: String) {
        let tokens = estimate_tokens(&text);
        if let Some(turn) = self.turns.last_mut() {
            turn.response = Some(Message {
                role: Role::Assistant,
                content: vec![Content::Text(text)],
            });
            turn.token_estimate += tokens;
        }
        self.tokens_used += tokens;
    }

    /// Record a tool call and its result in the current turn.
    pub fn append_tool_exchange(
        &mut self,
        tool_name: String,
        input: serde_json::Value,
        result: serde_json::Value,
    ) {
        let tokens = estimate_tokens(&input.to_string()) + estimate_tokens(&result.to_string());
        if let Some(turn) = self.turns.last_mut() {
            turn.tool_exchanges.push(ToolExchange {
                tool_name,
                input,
                result,
                token_estimate: tokens,
            });
            turn.token_estimate += tokens;
        }
        self.tokens_used += tokens;
    }

    /// Append a system message (e.g., from child session completion or external event).
    pub fn append_system_message(&mut self, text: String) {
        let tokens = estimate_tokens(&text);
        self.turns.push(Turn {
            input: Message {
                role: Role::System,
                content: vec![Content::Text(text)],
            },
            response: None,
            tool_exchanges: Vec::new(),
            token_estimate: tokens,
            summarized: false,
        });
        self.tokens_used += tokens;
    }

    /// Process invalidations from a tool result.
    pub fn invalidate_files(&mut self, paths: &[PathBuf]) {
        for path in paths {
            self.file_cache.remove(path);
        }
    }

    pub fn invalidate_all_files(&mut self) {
        self.file_cache.clear();
    }

    pub fn note_env_change(&mut self, vars: &[String]) {
        let note = format!("Environment changed: {}", vars.join(", "));
        self.scratchpad.notes.push(note);
    }

    /// Process a single invalidation.
    pub fn process_invalidation(&mut self, invalidation: &Invalidation) {
        match invalidation {
            Invalidation::Files(paths) => self.invalidate_files(paths),
            Invalidation::WorkingDirectory(_) => self.invalidate_all_files(),
            Invalidation::ToolRegistry => {
                // Re-scan would happen here; for now clear cached definitions
                self.tool_definitions_in_context.clear();
                self.tool_names_in_context.clear();
            }
            Invalidation::Environment(vars) => self.note_env_change(vars),
        }
    }

    /// Page a tool's definition into context (demand-paging).
    /// Returns false if the tool's schema doesn't fit in the remaining budget.
    pub fn page_in_tool(&mut self, tool: &dyn ToolRegistration) -> bool {
        let cost = tool.cost().0;
        if self.tokens_used + cost > self.config.context_window {
            return false;
        }

        self.tool_definitions_in_context.push(tool.schema().clone());
        self.tool_names_in_context.push(tool.name().to_string());
        self.tokens_used += cost;
        true
    }

    /// Remove a tool's definition from context.
    pub fn page_out_tool(&mut self, tool_name: &str) -> bool {
        if let Some(idx) = self.tool_names_in_context.iter().position(|n| n == tool_name) {
            self.tool_definitions_in_context.remove(idx);
            self.tool_names_in_context.remove(idx);
            // Token reclaim is approximate — we don't track per-tool cost after insertion.
            // A full recount would be more accurate but expensive.
            true
        } else {
            false
        }
    }

    /// Assemble the full prompt for the model.
    pub fn assemble(&self) -> Prompt {
        let mut system = self.system_prompt.clone();

        // Append scratchpad to system prompt (Tier 1)
        if !self.scratchpad.plan.is_empty()
            || !self.scratchpad.constraints.is_empty()
            || !self.scratchpad.notes.is_empty()
        {
            system.push_str("\n\n<scratchpad>\n");
            for constraint in &self.scratchpad.constraints {
                system.push_str(&format!("CONSTRAINT: {}\n", constraint));
            }
            for (i, step) in self.scratchpad.plan.iter().enumerate() {
                let marker = if step.completed { "x" } else { " " };
                system.push_str(&format!("[{}] {}. {}\n", marker, i + 1, step.description));
            }
            for note in &self.scratchpad.notes {
                system.push_str(&format!("NOTE: {}\n", note));
            }
            system.push_str("</scratchpad>");
        }

        // Build messages from turns (Tier 2)
        let messages: Vec<Message> = self
            .turns
            .iter()
            .flat_map(|turn| {
                let mut msgs = vec![turn.input.clone()];

                // Include tool calls and results so the model sees the full exchange
                if !turn.tool_exchanges.is_empty() {
                    // Assistant message with tool calls
                    let tool_call_content: Vec<Content> = turn
                        .tool_exchanges
                        .iter()
                        .enumerate()
                        .map(|(i, ex)| Content::ToolCall {
                            id: format!("call_{i}"),
                            name: ex.tool_name.clone(),
                            input: ex.input.clone(),
                        })
                        .collect();
                    msgs.push(Message {
                        role: Role::Assistant,
                        content: tool_call_content,
                    });

                    // Tool results
                    let tool_result_content: Vec<Content> = turn
                        .tool_exchanges
                        .iter()
                        .enumerate()
                        .map(|(i, ex)| Content::ToolResult {
                            id: format!("call_{i}"),
                            result: ex.result.clone(),
                        })
                        .collect();
                    msgs.push(Message {
                        role: Role::User,
                        content: tool_result_content,
                    });
                }

                if let Some(ref response) = turn.response {
                    msgs.push(response.clone());
                }
                msgs
            })
            .collect();

        Prompt {
            system,
            messages,
            tool_definitions: self.tool_definitions_in_context.clone(),
        }
    }

    /// Run compaction — summarize older turns to free token budget.
    /// Returns the number of tokens freed, or an error message.
    pub fn compact(&mut self) -> Result<usize, String> {
        // Death spiral guard: cooldown check
        if let Some(last) = self.last_compaction_time
            && last.elapsed().as_secs() < self.config.compaction_cooldown_secs
        {
            return Err("compaction cooldown active".into());
        }

        // Death spiral guard: failure count
        if self.consecutive_compaction_failures >= self.config.max_compaction_failures {
            return Err(format!(
                "compaction halted after {} consecutive failures",
                self.consecutive_compaction_failures
            ));
        }

        let total_turns = self.turns.len();
        if total_turns < 2 {
            self.consecutive_compaction_failures += 1;
            return Err("not enough turns to compact".into());
        }

        // Keep the verbatim tail (last N% of turns)
        let tail_count = ((total_turns as f64) * self.config.verbatim_tail_ratio).ceil() as usize;
        let tail_count = tail_count.max(1);
        let compact_up_to = total_turns.saturating_sub(tail_count);

        if compact_up_to == 0 {
            self.consecutive_compaction_failures += 1;
            return Err("nothing to compact — all turns in verbatim tail".into());
        }

        let mut tokens_freed = 0;

        // Summarize turns in the compaction zone that haven't been summarized yet
        for turn in &mut self.turns[..compact_up_to] {
            if !turn.summarized {
                let original_tokens = turn.token_estimate;

                // Generate a summary — in production this would call the model.
                // For now, we truncate to a short summary.
                let summary = summarize_turn(turn);
                let summary_tokens = estimate_tokens(&summary);

                turn.input = Message {
                    role: turn.input.role,
                    content: vec![Content::Text(summary)],
                };
                turn.response = None;
                turn.tool_exchanges.clear();
                turn.token_estimate = summary_tokens;
                turn.summarized = true;

                tokens_freed += original_tokens.saturating_sub(summary_tokens);
            }
        }

        self.tokens_used = self.tokens_used.saturating_sub(tokens_freed);
        self.consecutive_compaction_failures = 0;
        self.last_compaction_time = Some(std::time::Instant::now());

        Ok(tokens_freed)
    }
}

/// Simple token estimation: ~4 chars per token.
/// In production, this would use the provider's count_tokens.
fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Produce a brief summary of a turn.
/// In production, this would call the model for an actual summary.
fn summarize_turn(turn: &Turn) -> String {
    let input_preview: String = turn
        .input
        .content
        .iter()
        .filter_map(|c| match c {
            Content::Text(t) => Some(t.chars().take(100).collect::<String>()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");

    let tool_summary = if turn.tool_exchanges.is_empty() {
        String::new()
    } else {
        format!(
            " [tools: {}]",
            turn.tool_exchanges
                .iter()
                .map(|e| e.tool_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    format!("[summary] {}{}", input_preview, tool_summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ContextConfig {
        ContextConfig {
            context_window: 1000,
            compaction_threshold: 0.65,
            compaction_cooldown_secs: 0, // no cooldown in tests
            ..Default::default()
        }
    }

    #[test]
    fn new_context_manager_tracks_system_prompt_tokens() {
        let cm = ContextManager::new(test_config(), "You are a helpful assistant.".into());
        assert!(cm.tokens_used() > 0);
        assert!(cm.utilization() > 0.0);
    }

    #[test]
    fn append_user_input_increases_tokens() {
        let mut cm = ContextManager::new(test_config(), "System.".into());
        let before = cm.tokens_used();
        cm.append_user_input("Hello, what files are in this directory?".into());
        assert!(cm.tokens_used() > before);
        assert_eq!(cm.turn_count(), 1);
    }

    #[test]
    fn append_tool_exchange_tracks_in_current_turn() {
        let mut cm = ContextManager::new(test_config(), "System.".into());
        cm.append_user_input("List files".into());
        cm.append_tool_exchange(
            "ls".into(),
            serde_json::json!({"path": "."}),
            serde_json::json!({"files": ["a.rs", "b.rs"]}),
        );
        assert_eq!(cm.turns.last().unwrap().tool_exchanges.len(), 1);
    }

    #[test]
    fn should_compact_triggers_at_threshold() {
        let config = ContextConfig {
            context_window: 100,
            compaction_threshold: 0.50,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "Sys.".into());

        // Fill up past threshold
        for i in 0..20 {
            cm.append_user_input(format!("Message number {} with some padding text", i));
        }

        assert!(cm.should_compact());
    }

    #[test]
    fn compact_frees_tokens() {
        let config = ContextConfig {
            context_window: 10_000,
            compaction_threshold: 0.10,
            verbatim_tail_ratio: 0.30,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "System.".into());

        // Add several turns with content
        for i in 0..10 {
            cm.append_user_input(format!(
                "This is turn {} with a reasonably long message to have meaningful tokens",
                i
            ));
            cm.append_assistant_response(format!(
                "Here is a detailed response to turn {} with information",
                i
            ));
        }

        let before = cm.tokens_used();
        let freed = cm.compact().expect("compaction should succeed");

        assert!(freed > 0, "should free some tokens");
        assert!(cm.tokens_used() < before, "token usage should decrease");
    }

    #[test]
    fn compact_preserves_verbatim_tail() {
        let config = ContextConfig {
            context_window: 10_000,
            compaction_threshold: 0.10,
            verbatim_tail_ratio: 0.30,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "System.".into());

        for i in 0..10 {
            cm.append_user_input(format!("Turn {}", i));
        }

        cm.compact().expect("compaction should succeed");

        // Last ~30% of turns should NOT be summarized
        let tail_count = (10.0_f64 * 0.30).ceil() as usize; // 3
        for turn in &cm.turns[cm.turns.len() - tail_count..] {
            assert!(!turn.summarized, "tail turns should not be summarized");
        }
    }

    #[test]
    fn compact_death_spiral_guard() {
        let config = ContextConfig {
            context_window: 10_000,
            compaction_cooldown_secs: 0,
            max_compaction_failures: 3,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "System.".into());

        // With only 1 turn, compaction should fail
        cm.append_user_input("Solo turn".into());

        for _ in 0..3 {
            assert!(cm.compact().is_err());
        }

        // After 3 failures, should be halted
        let result = cm.compact();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("halted"));
    }

    #[test]
    fn scratchpad_survives_compaction() {
        let config = ContextConfig {
            context_window: 10_000,
            compaction_threshold: 0.10,
            verbatim_tail_ratio: 0.30,
            compaction_cooldown_secs: 0,
            ..Default::default()
        };
        let mut cm = ContextManager::new(config, "System.".into());

        cm.scratchpad_mut().constraints.push("Don't modify auth module".into());
        cm.scratchpad_mut().plan.push(PlanStep {
            description: "Fix the bug".into(),
            completed: false,
        });

        for i in 0..10 {
            cm.append_user_input(format!("Turn {}", i));
            cm.append_assistant_response(format!("Response {}", i));
        }

        cm.compact().expect("compaction should succeed");

        assert_eq!(cm.scratchpad().constraints.len(), 1);
        assert_eq!(cm.scratchpad().constraints[0], "Don't modify auth module");
        assert_eq!(cm.scratchpad().plan.len(), 1);
    }

    #[test]
    fn assemble_includes_scratchpad_in_system() {
        let mut cm = ContextManager::new(test_config(), "You are helpful.".into());
        cm.scratchpad_mut()
            .constraints
            .push("No auth changes".into());

        let prompt = cm.assemble();
        assert!(prompt.system.contains("CONSTRAINT: No auth changes"));
    }

    #[test]
    fn invalidate_files_removes_from_cache() {
        let mut cm = ContextManager::new(test_config(), "System.".into());
        cm.file_cache
            .insert(PathBuf::from("src/main.rs"), "fn main() {}".into());

        cm.invalidate_files(&[PathBuf::from("src/main.rs")]);
        assert!(!cm.file_cache.contains_key(&PathBuf::from("src/main.rs")));
    }

    #[test]
    fn page_in_tool_respects_budget() {
        use kernel_interfaces::tool::{ToolError, ToolOutput};
        use kernel_interfaces::types::{CapabilitySet, RelevanceSignal, TokenEstimate};

        struct BigTool {
            capabilities: CapabilitySet,
            relevance: RelevanceSignal,
        }
        impl BigTool {
            fn new() -> Self {
                Self {
                    capabilities: CapabilitySet::new(),
                    relevance: RelevanceSignal { keywords: Vec::new(), tags: Vec::new() },
                }
            }
        }
        impl ToolRegistration for BigTool {
            fn name(&self) -> &str { "big_tool" }
            fn description(&self) -> &str { "A huge tool" }
            fn capabilities(&self) -> &CapabilitySet { &self.capabilities }
            fn schema(&self) -> &serde_json::Value { &serde_json::Value::Null }
            fn cost(&self) -> TokenEstimate { TokenEstimate(999_999) }
            fn relevance(&self) -> &RelevanceSignal { &self.relevance }
            fn execute(&self, _: serde_json::Value) -> Result<ToolOutput, ToolError> {
                unreachable!()
            }
        }

        let cm_config = ContextConfig {
            context_window: 1000,
            ..Default::default()
        };
        let mut cm = ContextManager::new(cm_config, "Sys.".into());
        assert!(!cm.page_in_tool(&BigTool::new()));
    }
}
