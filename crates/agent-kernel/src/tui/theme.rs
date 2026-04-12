use ratatui::style::Color;

#[derive(Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub user_input: Color,
    pub assistant_text: Color,
    pub tool_border: Color,
    pub tool_running: Color,
    pub tool_success: Color,
    pub tool_failed: Color,
    pub tool_result: Color,
    pub permission: Color,
    pub info: Color,
    pub error: Color,
    pub status_bar_bg: Color,
    pub status_bar_fg: Color,
    pub input_prompt: Color,
    pub timestamp: Color,
    pub code_block: Color,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            user_input: Color::Cyan,
            assistant_text: Color::Reset,
            tool_border: Color::DarkGray,
            tool_running: Color::Yellow,
            tool_success: Color::Green,
            tool_failed: Color::Red,
            tool_result: Color::DarkGray,
            permission: Color::Yellow,
            info: Color::DarkGray,
            error: Color::Red,
            status_bar_bg: Color::DarkGray,
            status_bar_fg: Color::White,
            input_prompt: Color::Cyan,
            timestamp: Color::DarkGray,
            code_block: Color::Green,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}
