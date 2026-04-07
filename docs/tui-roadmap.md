# TUI Roadmap

Current state: ratatui TUI with markdown rendering, input history, multiline input, tool call boxes with spinners and inline results, timestamps, token budget, and theme support.

## Status key

- [ ] Not started
- [~] In progress
- [x] Done

## Completed

- [x] Markdown rendering (pulldown-cmark)
- [x] Input history (Up/Down)
- [x] Multiline input (Shift+Enter)
- [x] Spinner memory leak fix
- [x] Word-wrap-aware scrolling (unicode-width)
- [x] Inline tool results
- [x] Token budget indicator
- [x] Timestamps
- [x] Color theme struct
- [x] Daemon reconnection support
- [x] Duplicate tool call fix

## Tool Use Output

### T1. Human-readable tool input summary — [x]
- Currently shows raw JSON: `{"path":"src/main.rs","offset":0}`
- Should show: `path: src/main.rs` (extract key fields per tool)
- For `file_read`: show path (and line range if offset/limit given)
- For `shell`: show command
- For `grep`: show pattern and path
- For `ls`: show path
- For `file_write`: show path
- Scope: format function in main.rs where `input_summary` is built from `input.to_string()`

### T2. Human-readable tool result summary — [x]
- Currently shows raw JSON string of the result
- For `file_read`: show the file content directly (it's already text)
- For `shell`: show stdout, and exit code if non-zero
- For `grep`: show match lines
- For `ls`: show file listing
- For `file_write`: show "wrote N bytes to path"
- For errors: show error message clearly in red
- Scope: result formatting in reader thread where `result_summary` is built

### T3. Syntax-aware file content in results — [ ]
- File read results should render with line numbers
- Optionally detect language from file extension and apply code block styling
- Reuse the markdown code block style (green) for consistency
- Scope: `draw_conversation` tool result rendering

### T4. Compact tool call display — [ ]
- Successful read-only tools (file_read, ls, grep) should be visually compact
- Collapse result by default when the tool succeeds and the model continues
- Only expand on failure or when it's the last tool call before assistant text
- Reduce visual noise during multi-tool turns
- Scope: `ToolCallStatus` gains a `collapsed: bool`, render logic

### T5. Shell command output styling — [ ]
- Shell results should look like terminal output (monospace, dim)
- Show exit code prominently if non-zero
- Truncate long output with "show more" indicator
- Stderr should be visually distinct (yellow or red)
- Scope: tool result rendering, possibly new `ConversationEntry` variant

### T6. Progress for long-running tools — [ ]
- Shell commands can take seconds — show elapsed time next to spinner
- Update the spinner line with `⠋ shell (3.2s)` while running
- Scope: `ToolCallStatus::Running` gains a start time, render updates

## General UI

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
