//! Native Rust tool implementations for the code-agent distribution.

use kernel_interfaces::tool::{ToolError, ToolOutput, ToolRegistration};
use kernel_interfaces::types::{
    Capability, CapabilitySet, Invalidation, RelevanceSignal, TokenEstimate,
};

use std::path::{Path, PathBuf};
use std::process::Command;

// ============================================================================
// file_read
// ============================================================================

pub struct FileReadTool {
    caps: CapabilitySet,
    schema: serde_json::Value,
    relevance: RelevanceSignal,
    workspace: PathBuf,
}

impl FileReadTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            caps: [Capability::new("fs:read")].into_iter().collect(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace" },
                    "offset": { "type": "integer", "description": "Line number to start reading from (1-based)" },
                    "limit": { "type": "integer", "description": "Max number of lines to read" }
                },
                "required": ["path"]
            }),
            relevance: RelevanceSignal {
                keywords: vec![
                    "read".into(),
                    "file".into(),
                    "cat".into(),
                    "show".into(),
                    "contents".into(),
                ],
                tags: vec!["filesystem".into()],
            },
            workspace,
        }
    }

    fn resolve_path(&self, rel: &str) -> PathBuf {
        self.workspace.join(rel)
    }
}

impl ToolRegistration for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }
    fn description(&self) -> &str {
        "Read file contents with optional line range"
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' parameter".into()))?;

        let full_path = self.resolve_path(path_str);
        let content = std::fs::read_to_string(&full_path).map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to read {}: {e}", full_path.display()))
        })?;

        let lines: Vec<&str> = content.lines().collect();
        let offset = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let start = offset.saturating_sub(1).min(lines.len());
        let end = match limit {
            Some(lim) => (start + lim).min(lines.len()),
            None => lines.len(),
        };

        let numbered: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}\t{}", start + i + 1, line))
            .collect();

        Ok(ToolOutput::readonly(serde_json::json!({
            "path": path_str,
            "content": numbered.join("\n"),
            "total_lines": lines.len(),
        })))
    }
}

// ============================================================================
// file_write
// ============================================================================

pub struct FileWriteTool {
    caps: CapabilitySet,
    schema: serde_json::Value,
    relevance: RelevanceSignal,
    workspace: PathBuf,
}

impl FileWriteTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            caps: [Capability::new("fs:write")].into_iter().collect(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace" },
                    "content": { "type": "string", "description": "Full file content to write" }
                },
                "required": ["path", "content"]
            }),
            relevance: RelevanceSignal {
                keywords: vec!["write".into(), "create".into(), "save".into()],
                tags: vec!["filesystem".into()],
            },
            workspace,
        }
    }
}

impl ToolRegistration for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }
    fn description(&self) -> &str {
        "Write full file contents"
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'path'".into()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'content'".into()))?;

        let full_path = self.workspace.join(path_str);

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                ToolError::ExecutionFailed(format!("failed to create directories: {e}"))
            })?;
        }

        std::fs::write(&full_path, content).map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to write {}: {e}", full_path.display()))
        })?;

        Ok(ToolOutput::with_invalidations(
            serde_json::json!({ "path": path_str, "bytes_written": content.len() }),
            vec![Invalidation::Files(vec![full_path])],
        ))
    }
}

// ============================================================================
// shell
// ============================================================================

pub struct ShellTool {
    caps: CapabilitySet,
    schema: serde_json::Value,
    relevance: RelevanceSignal,
    workspace: PathBuf,
}

impl ShellTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            caps: [Capability::new("shell:exec")].into_iter().collect(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" }
                },
                "required": ["command"]
            }),
            relevance: RelevanceSignal {
                keywords: vec![
                    "run".into(),
                    "exec".into(),
                    "shell".into(),
                    "command".into(),
                    "bash".into(),
                ],
                tags: vec!["shell".into()],
            },
            workspace,
        }
    }
}

impl ToolRegistration for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "Execute a shell command in the workspace"
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(200)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let command = input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'command'".into()))?;

        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.workspace)
            .output()
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to spawn shell: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Truncate large outputs
        let max_len = 50_000;
        let stdout_truncated = if stdout.len() > max_len {
            format!(
                "{}...\n[truncated, {} total bytes]",
                &stdout[..max_len],
                stdout.len()
            )
        } else {
            stdout.into_owned()
        };

        Ok(ToolOutput::readonly(serde_json::json!({
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": stdout_truncated,
            "stderr": stderr.as_ref(),
        })))
    }
}

// ============================================================================
// ls
// ============================================================================

pub struct LsTool {
    caps: CapabilitySet,
    schema: serde_json::Value,
    relevance: RelevanceSignal,
    workspace: PathBuf,
}

impl LsTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            caps: [Capability::new("fs:read")].into_iter().collect(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory path relative to workspace (default: '.')" }
                }
            }),
            relevance: RelevanceSignal {
                keywords: vec![
                    "list".into(),
                    "ls".into(),
                    "directory".into(),
                    "files".into(),
                ],
                tags: vec!["filesystem".into()],
            },
            workspace,
        }
    }
}

impl ToolRegistration for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List directory contents"
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(100)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path_str = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let full_path = self.workspace.join(path_str);

        let entries: Vec<serde_json::Value> = std::fs::read_dir(&full_path)
            .map_err(|e| ToolError::ExecutionFailed(format!("failed to read directory: {e}")))?
            .filter_map(|entry| entry.ok())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                serde_json::json!({
                    "name": if is_dir { format!("{name}/") } else { name },
                    "type": if is_dir { "directory" } else { "file" },
                })
            })
            .collect();

        Ok(ToolOutput::readonly(serde_json::json!({
            "path": path_str,
            "entries": entries,
        })))
    }
}

// ============================================================================
// grep
// ============================================================================

pub struct GrepTool {
    caps: CapabilitySet,
    schema: serde_json::Value,
    relevance: RelevanceSignal,
    workspace: PathBuf,
}

impl GrepTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            caps: [Capability::new("fs:read")].into_iter().collect(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Search pattern (passed to grep -rn)" },
                    "path": { "type": "string", "description": "Directory to search in (default: '.')" }
                },
                "required": ["pattern"]
            }),
            relevance: RelevanceSignal {
                keywords: vec![
                    "search".into(),
                    "grep".into(),
                    "find".into(),
                    "where".into(),
                ],
                tags: vec!["search".into()],
            },
            workspace,
        }
    }
}

impl ToolRegistration for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents with grep"
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let pattern = input
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'pattern'".into()))?;
        let path_str = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let search_path = self.workspace.join(path_str);

        let output = Command::new("grep")
            .args(["-rn", "--include=*", pattern])
            .arg(&search_path)
            .output()
            .map_err(|e| ToolError::ExecutionFailed(format!("grep failed: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Limit results
        let lines: Vec<&str> = stdout.lines().take(100).collect();
        let total_matches = stdout.lines().count();

        Ok(ToolOutput::readonly(serde_json::json!({
            "matches": lines.join("\n"),
            "total_matches": total_matches,
            "truncated": total_matches > 100,
        })))
    }
}

// ============================================================================
// file_edit
// ============================================================================

pub struct FileEditTool {
    caps: CapabilitySet,
    schema: serde_json::Value,
    relevance: RelevanceSignal,
    workspace: PathBuf,
}

impl FileEditTool {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            caps: [Capability::new("fs:write")].into_iter().collect(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace" },
                    "old_string": { "type": "string", "description": "The exact text to find and replace (must match uniquely)" },
                    "new_string": { "type": "string", "description": "The replacement text" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
            relevance: RelevanceSignal {
                keywords: vec![
                    "edit".into(),
                    "replace".into(),
                    "change".into(),
                    "modify".into(),
                    "update".into(),
                ],
                tags: vec!["filesystem".into()],
            },
            workspace,
        }
    }
}

impl ToolRegistration for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }
    fn description(&self) -> &str {
        "Replace an exact string in a file with new text"
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    fn schema(&self) -> &serde_json::Value {
        &self.schema
    }
    fn cost(&self) -> TokenEstimate {
        TokenEstimate(150)
    }
    fn relevance(&self) -> &RelevanceSignal {
        &self.relevance
    }

    fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'path' parameter".into()))?;
        let old_string = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'old_string' parameter".into()))?;
        let new_string = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'new_string' parameter".into()))?;

        let full_path = self.workspace.join(path_str);

        // Creating a new file: old_string is empty, new_string is the content
        if old_string.is_empty() {
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    ToolError::ExecutionFailed(format!("failed to create directories: {e}"))
                })?;
            }
            std::fs::write(&full_path, new_string).map_err(|e| {
                ToolError::ExecutionFailed(format!("failed to write {}: {e}", full_path.display()))
            })?;
            return Ok(ToolOutput::with_invalidations(
                serde_json::json!({
                    "path": path_str,
                    "action": "created",
                }),
                vec![Invalidation::Files(vec![full_path])],
            ));
        }

        let content = std::fs::read_to_string(&full_path).map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to read {}: {e}", full_path.display()))
        })?;

        // Check uniqueness
        let match_count = content.matches(old_string).count();
        if match_count == 0 {
            return Err(ToolError::ExecutionFailed(
                "old_string not found in file".into(),
            ));
        }
        if match_count > 1 {
            return Err(ToolError::ExecutionFailed(format!(
                "old_string matches {match_count} locations — include more surrounding context to make it unique"
            )));
        }

        let new_content = content.replacen(old_string, new_string, 1);
        std::fs::write(&full_path, &new_content).map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to write {}: {e}", full_path.display()))
        })?;

        Ok(ToolOutput::with_invalidations(
            serde_json::json!({
                "path": path_str,
                "action": "edited",
            }),
            vec![Invalidation::Files(vec![full_path])],
        ))
    }
}

// ============================================================================
// Factory
// ============================================================================

/// Extract a ToolSchema from a ToolRegistration (serializable metadata for the kernel protocol).
pub fn to_schema(tool: &dyn ToolRegistration) -> kernel_interfaces::protocol::ToolSchema {
    kernel_interfaces::protocol::ToolSchema {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        capabilities: tool.capabilities().clone(),
        schema: tool.schema().clone(),
        cost: tool.cost(),
        relevance: tool.relevance().clone(),
    }
}

/// All tool IDs the `dist-code-agent` distribution implements.
/// Used as the default enable list when the distribution manifest
/// has no `[tools]` section.
pub const TOOL_IDS: &[&str] = &[
    "file_read",
    "file_write",
    "file_edit",
    "shell",
    "ls",
    "grep",
];

/// Create distribution tools for the given workspace, filtered to the
/// set of IDs the caller names.
///
/// - `enabled = None` → every tool in `TOOL_IDS` (backwards-compat default).
/// - `enabled = Some(&[])` → no tools at all. Explicit.
/// - `enabled = Some(&["file_read", "grep"])` → only those two.
///
/// Unknown IDs log a stderr warning and are otherwise ignored.
pub fn create_tools(
    workspace: &Path,
    enabled: Option<&[String]>,
) -> Vec<Box<dyn ToolRegistration>> {
    let ws = workspace.to_path_buf();

    let build_all: Vec<Box<dyn ToolRegistration>> = vec![
        Box::new(FileReadTool::new(ws.clone())),
        Box::new(FileWriteTool::new(ws.clone())),
        Box::new(FileEditTool::new(ws.clone())),
        Box::new(ShellTool::new(ws.clone())),
        Box::new(LsTool::new(ws.clone())),
        Box::new(GrepTool::new(ws)),
    ];

    match enabled {
        None => build_all,
        Some(ids) => {
            // Warn on unknown IDs so typos in the manifest are visible.
            for id in ids {
                if !TOOL_IDS.contains(&id.as_str()) {
                    eprintln!(
                        "warning: manifest lists unknown tool id {id:?}; known IDs are {TOOL_IDS:?}"
                    );
                }
            }
            build_all
                .into_iter()
                .filter(|tool| ids.iter().any(|id| id == tool.name()))
                .collect()
        }
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn none_enables_every_tool() {
        let tools = create_tools(&PathBuf::from("/tmp"), None);
        assert_eq!(tools.len(), TOOL_IDS.len());
    }

    #[test]
    fn empty_list_disables_everything() {
        let tools = create_tools(&PathBuf::from("/tmp"), Some(&[]));
        assert!(tools.is_empty());
    }

    #[test]
    fn filter_narrows_to_named_tools() {
        let enabled: Vec<String> = vec!["file_read".into(), "grep".into()];
        let tools = create_tools(&PathBuf::from("/tmp"), Some(&enabled));
        assert_eq!(tools.len(), 2);
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"file_read"));
        assert!(names.contains(&"grep"));
    }

    #[test]
    fn unknown_id_is_warned_and_dropped() {
        let enabled: Vec<String> = vec!["file_read".into(), "doesnt_exist".into()];
        let tools = create_tools(&PathBuf::from("/tmp"), Some(&enabled));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "file_read");
    }
}
