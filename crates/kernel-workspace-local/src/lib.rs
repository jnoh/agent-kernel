//! Local filesystem workspace — the first-party `ToolSet` shipped with
//! `agent-kernel`. Exposes six tools (`file_read`, `file_write`,
//! `file_edit`, `shell`, `ls`, `grep`) scoped to a workspace root
//! directory.
//!
//! Spec 0015 registers this crate in the daemon's factory table under
//! `kind = "workspace.local"`. The factory (`from_entry`) parses the
//! manifest entry's opaque `config` block for a `root` field (defaults
//! to `"."`) and returns a boxed `ToolSet`. The toolset runs entirely
//! in-process in 0015; spec 0016 will re-home it behind an MCP stdio
//! transport without touching the kernel side of the trait.

use kernel_interfaces::manifest::ToolsetEntry;
use kernel_interfaces::tool::{ToolError, ToolExecutionCtx, ToolOutput, ToolRegistration};
use kernel_interfaces::toolset::ToolSet;
use kernel_interfaces::types::{
    Capability, CapabilitySet, Invalidation, RelevanceSignal, TokenEstimate,
};

use std::path::PathBuf;
use std::process::Command;

/// Names of every tool this toolset exposes, in registration order.
/// Exported so distributions can build prompt text without reaching
/// into the individual tool structs.
pub const TOOL_NAMES: &[&str] = &[
    "file_read",
    "file_write",
    "file_edit",
    "shell",
    "ls",
    "grep",
];

/// A workspace rooted at a local directory. Implements `ToolSet` by
/// returning the six filesystem/shell tools scoped to that directory.
pub struct LocalWorkspace {
    id: String,
    root: PathBuf,
}

impl LocalWorkspace {
    pub fn new(id: impl Into<String>, root: PathBuf) -> Self {
        Self {
            id: id.into(),
            root,
        }
    }
}

impl ToolSet for LocalWorkspace {
    fn id(&self) -> &str {
        &self.id
    }

    fn tools(&self) -> Vec<Box<dyn ToolRegistration>> {
        let r = self.root.clone();
        vec![
            Box::new(FileReadTool::new(r.clone())),
            Box::new(FileWriteTool::new(r.clone())),
            Box::new(FileEditTool::new(r.clone())),
            Box::new(ShellTool::new(r.clone())),
            Box::new(LsTool::new(r.clone())),
            Box::new(GrepTool::new(r)),
        ]
    }
}

/// Factory function registered under `kind = "workspace.local"`.
///
/// Reads `root` from `entry.config` (defaulting to the current directory
/// if absent). The `id` field on the manifest entry, if present, becomes
/// the toolset's identifier; otherwise we fall back to `"workspace.local"`.
pub fn from_entry(entry: &ToolsetEntry) -> Result<Box<dyn ToolSet>, String> {
    let root = entry
        .config
        .get("root")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    let id = entry
        .id
        .clone()
        .unwrap_or_else(|| "workspace.local".to_string());
    Ok(Box::new(LocalWorkspace::new(id, PathBuf::from(root))))
}

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

    fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, ToolError> {
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

    fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let path_str = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'path'".into()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("missing 'content'".into()))?;

        let full_path = self.workspace.join(path_str);

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

    fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, ToolError> {
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

    fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, ToolError> {
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

    fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, ToolError> {
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

    fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolExecutionCtx<'_>,
    ) -> Result<ToolOutput, ToolError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ctx() -> ToolExecutionCtx<'static> {
        ToolExecutionCtx::null()
    }

    fn ws() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path().to_path_buf();
        (dir, root)
    }

    #[test]
    fn from_entry_defaults_root_to_dot() {
        let entry = ToolsetEntry {
            kind: "workspace.local".into(),
            id: None,
            config: toml::Value::Table(Default::default()),
        };
        let toolset = from_entry(&entry).expect("factory");
        assert_eq!(toolset.id(), "workspace.local");
        let tools = toolset.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for expected in TOOL_NAMES {
            assert!(names.contains(expected), "missing tool: {expected}");
        }
    }

    #[test]
    fn from_entry_uses_id_when_provided() {
        let entry = ToolsetEntry {
            kind: "workspace.local".into(),
            id: Some("ws1".into()),
            config: toml::Value::Table(Default::default()),
        };
        let toolset = from_entry(&entry).expect("factory");
        assert_eq!(toolset.id(), "ws1");
    }

    #[test]
    fn file_read_and_write_round_trip() {
        let (_dir, root) = ws();
        let write = FileWriteTool::new(root.clone());
        write
            .execute(
                serde_json::json!({ "path": "hello.txt", "content": "hi\nworld" }),
                &ctx(),
            )
            .expect("write");

        let read = FileReadTool::new(root.clone());
        let out = read
            .execute(serde_json::json!({ "path": "hello.txt" }), &ctx())
            .expect("read");
        let content = out.result.get("content").unwrap().as_str().unwrap();
        assert!(content.contains("hi"));
        assert!(content.contains("world"));
    }

    #[test]
    fn file_edit_replaces_unique_string() {
        let (_dir, root) = ws();
        FileWriteTool::new(root.clone())
            .execute(
                serde_json::json!({ "path": "a.txt", "content": "foo bar baz" }),
                &ctx(),
            )
            .expect("write");

        FileEditTool::new(root.clone())
            .execute(
                serde_json::json!({
                    "path": "a.txt",
                    "old_string": "bar",
                    "new_string": "BAR",
                }),
                &ctx(),
            )
            .expect("edit");

        let out = FileReadTool::new(root)
            .execute(serde_json::json!({ "path": "a.txt" }), &ctx())
            .expect("read");
        assert!(
            out.result
                .get("content")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("foo BAR baz")
        );
    }

    #[test]
    fn shell_executes_command_in_workspace() {
        let (_dir, root) = ws();
        let out = ShellTool::new(root)
            .execute(serde_json::json!({ "command": "echo hello" }), &ctx())
            .expect("shell");
        let stdout = out.result.get("stdout").unwrap().as_str().unwrap();
        assert!(stdout.contains("hello"));
    }

    #[test]
    fn ls_lists_entries() {
        let (_dir, root) = ws();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();
        let out = LsTool::new(root)
            .execute(serde_json::json!({}), &ctx())
            .expect("ls");
        let entries = out.result.get("entries").unwrap().as_array().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn local_workspace_exposes_all_tool_names() {
        let (_dir, root) = ws();
        let workspace = LocalWorkspace::new("test", root);
        let tools = workspace.tools();
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        for expected in TOOL_NAMES {
            assert!(names.contains(expected), "missing: {expected}");
        }
    }
}
