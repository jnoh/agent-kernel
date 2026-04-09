# TUI Roadmap

Current state: ratatui TUI with markdown rendering, input history, multiline input, tool call boxes with spinners and inline results, timestamps, token budget, and theme support.

## Completed

- Markdown rendering (pulldown-cmark)
- Input history (Up/Down) and multiline input (Shift+Enter)
- Spinner memory leak fix
- Word-wrap-aware scrolling (unicode-width)
- Inline tool results with human-readable input/output summaries
- Syntax-aware file content in results (line numbers, code block styling)
- Compact tool call display (collapse successful read-only tools)
- Shell command output styling (monospace, exit code, stderr distinction)
- Progress for long-running tools (elapsed time next to spinner)
- Token budget indicator and timestamps
- Color theme struct
- Daemon reconnection support
- Duplicate tool call fix

## Open

### G1. Slash commands — [ ]
- `/clear` — clear conversation display
- `/compact` — send `RequestCompaction` to daemon
- `/status` — send `QuerySession`, show result inline
- `/quit` — already exists
- Scope: command parser in `InputAction::Submit` path

### G2. Permission "always allow" — [ ]
- `a` key during permission prompt = always allow this tool for the session
- Updates policy via `SetPolicy` message to daemon
- Scope: `handle_key` permission mode, policy construction
