mod sections;

/// Dynamic values available to prompt sections during assembly.
pub struct PromptContext {
    pub workspace: String,
    pub tool_names: Vec<String>,
}

/// Assembles a system prompt from named sections.
///
/// Each section becomes a markdown `# Heading` block in the final output.
pub struct PromptBuilder {
    sections: Vec<(&'static str, String)>,
}

impl PromptBuilder {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
        }
    }

    pub fn section(mut self, heading: &'static str, body: impl Into<String>) -> Self {
        self.sections.push((heading, body.into()));
        self
    }

    pub fn build(self) -> String {
        let mut out = String::new();
        for (i, (heading, body)) in self.sections.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("# {heading}\n\n"));
            out.push_str(body.trim());
            out.push('\n');
        }
        out
    }
}

/// Build the complete system prompt for the code-agent distribution.
pub fn build_system_prompt(ctx: &PromptContext) -> String {
    PromptBuilder::new()
        .section("System", sections::system_section(ctx))
        .section("Doing tasks", sections::doing_tasks_section())
        .section("Executing actions with care", sections::actions_section())
        .section("Tool usage", sections::tool_usage_section(ctx))
        .section("Git workflows", sections::git_section())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> PromptContext {
        PromptContext {
            workspace: "/tmp/test-workspace".into(),
            tool_names: vec![
                "file_read".into(),
                "file_write".into(),
                "file_edit".into(),
                "shell".into(),
                "grep".into(),
                "ls".into(),
            ],
        }
    }

    #[test]
    fn build_system_prompt_contains_all_sections() {
        let prompt = build_system_prompt(&test_ctx());
        assert!(prompt.contains("# System"));
        assert!(prompt.contains("# Doing tasks"));
        assert!(prompt.contains("# Executing actions with care"));
        assert!(prompt.contains("# Tool usage"));
        assert!(prompt.contains("# Git workflows"));
    }

    #[test]
    fn build_system_prompt_interpolates_workspace() {
        let prompt = build_system_prompt(&test_ctx());
        assert!(prompt.contains("/tmp/test-workspace"));
    }

    #[test]
    fn build_system_prompt_interpolates_tool_names() {
        let prompt = build_system_prompt(&test_ctx());
        assert!(prompt.contains("file_read"));
        assert!(prompt.contains("file_write"));
        assert!(prompt.contains("shell"));
        assert!(prompt.contains("grep"));
    }

    #[test]
    fn builder_joins_sections_with_headings() {
        let result = PromptBuilder::new()
            .section("Alpha", "Body one.")
            .section("Beta", "Body two.")
            .build();
        assert_eq!(result, "# Alpha\n\nBody one.\n\n# Beta\n\nBody two.\n");
    }

    #[test]
    fn builder_trims_section_bodies() {
        let result = PromptBuilder::new()
            .section("Test", "\n  Leading and trailing whitespace  \n")
            .build();
        assert!(result.contains("Leading and trailing whitespace"));
        // Should not start with a newline after the heading blank line
        assert!(result.contains("# Test\n\nLeading"));
    }
}
