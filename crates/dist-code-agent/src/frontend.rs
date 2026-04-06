//! Simple REPL frontend for the code-agent distribution.

use kernel_interfaces::frontend::{
    CompactionSummary, FrontendInterface, KernelError, PermissionRequest,
};
use kernel_interfaces::tool::ToolOutput;
use kernel_interfaces::types::{Decision, StreamChunk, TurnId};

use std::io::{self, Write};
use std::path::Path;

/// A minimal terminal frontend that prints events and prompts for permissions.
pub struct ReplFrontend;

impl FrontendInterface for ReplFrontend {
    fn on_turn_start(&self, _turn_id: TurnId) {}

    fn on_stream_chunk(&self, _chunk: &StreamChunk) {}

    fn on_tool_call(&self, tool_name: &str, input: &serde_json::Value) {
        // Compact display of tool calls
        let input_str = input.to_string();
        let display = if input_str.len() > 120 {
            format!("{}...", &input_str[..120])
        } else {
            input_str
        };
        eprintln!("  [tool] {tool_name}({display})");
    }

    fn on_tool_result(&self, tool_name: &str, result: &ToolOutput) {
        let result_str = result.result.to_string();
        let display = if result_str.len() > 200 {
            format!("{}...", &result_str[..200])
        } else {
            result_str
        };
        eprintln!("  [result] {tool_name} → {display}");
    }

    fn on_permission_request(&self, request: &PermissionRequest) -> Decision {
        eprint!(
            "  [permission] {} requires [{}]. Allow? (y/n) ",
            request.tool_name,
            request.capabilities.join(", ")
        );
        io::stderr().flush().ok();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let answer = input.trim().to_lowercase();
            if answer == "y" || answer == "yes" {
                Decision::Allow
            } else {
                Decision::Deny("user denied".into())
            }
        } else {
            Decision::Deny("failed to read user input".into())
        }
    }

    fn on_turn_end(&self, _turn_id: TurnId) {}

    fn on_compaction(&self, summary: &CompactionSummary) {
        eprintln!(
            "  [compaction] freed {} tokens ({} → {} turns)",
            summary.tokens_freed, summary.turns_before, summary.turns_after
        );
    }

    fn on_workspace_changed(&self, new_root: &Path) {
        eprintln!("  [workspace] changed to {}", new_root.display());
    }

    fn on_error(&self, error: &KernelError) {
        eprintln!("  [error] {}", error.message);
    }
}
