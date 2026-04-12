#[derive(Clone)]
pub enum ConversationEntry {
    UserInput(String),
    AssistantText(String),
    ToolCall {
        tool_name: String,
        input_summary: String,
        status: ToolCallStatus,
        result_summary: Option<String>,
        expanded: bool,
    },
    PermissionPrompt {
        tool_name: String,
        capabilities: Vec<String>,
        input_summary: String,
    },
    Info(String),
    Error(String),
}

#[derive(Clone)]
#[allow(dead_code)]
pub enum ToolCallStatus {
    Running(std::time::Instant),
    Success(std::time::Duration),
    Failed(String),
}

pub enum InputAction {
    Submit(String),
    SlashCommand(SlashCommand),
    PermissionDecision(bool),
    PermissionAlwaysAllow,
    Cancel,
    Quit,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SlashCommand {
    Clear,
    Compact,
    Status,
    Tools,
    Quit,
    Unknown(String),
}

pub fn parse_slash_command(text: &str) -> Option<SlashCommand> {
    let trimmed = text.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    Some(match trimmed {
        "/clear" => SlashCommand::Clear,
        "/compact" => SlashCommand::Compact,
        "/status" => SlashCommand::Status,
        "/tools" => SlashCommand::Tools,
        "/quit" | "/exit" => SlashCommand::Quit,
        other => SlashCommand::Unknown(other.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clear() {
        assert_eq!(parse_slash_command("/clear"), Some(SlashCommand::Clear));
    }

    #[test]
    fn parse_compact() {
        assert_eq!(parse_slash_command("/compact"), Some(SlashCommand::Compact));
    }

    #[test]
    fn parse_status() {
        assert_eq!(parse_slash_command("/status"), Some(SlashCommand::Status));
    }

    #[test]
    fn parse_quit_and_exit() {
        assert_eq!(parse_slash_command("/quit"), Some(SlashCommand::Quit));
        assert_eq!(parse_slash_command("/exit"), Some(SlashCommand::Quit));
    }

    #[test]
    fn parse_unknown_command() {
        assert_eq!(
            parse_slash_command("/foo"),
            Some(SlashCommand::Unknown("/foo".to_string()))
        );
    }

    #[test]
    fn parse_normal_message_is_not_a_command() {
        assert_eq!(parse_slash_command("hello world"), None);
        assert_eq!(parse_slash_command("not a /command"), None);
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(parse_slash_command("  /clear  "), Some(SlashCommand::Clear));
    }
}
