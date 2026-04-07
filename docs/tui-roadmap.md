# TUI Roadmap

Current state: ratatui-based TUI in `crates/dist-code-agent/src/tui.rs` with scrollable conversation pane, bordered tool-call boxes with spinners, inline permission prompts (y/n), status bar (model, tokens, turns), and single-line input with cursor editing.

## Status key

- [ ] Not started
- [~] In progress
- [x] Done

## Tier 1: Usability basics

### 1.1 Markdown rendering in assistant output — [x]
- Render headers, **bold**, `inline code`, and fenced code blocks with syntax highlighting
- Code blocks are highest priority (coding agent)
- Options: `tui-markdown` crate, or manual span styling from a lightweight markdown parser
- Scope: `ConversationEntry::AssistantText` rendering in `draw_conversation`

### 1.2 Input history — [x]
- Up/Down in input area cycles through previous user inputs
- Move conversation scroll to Ctrl+Up/Down or Alt+Up/Down
- Store history in `App` as `Vec<String>` with an index cursor
- Scope: `handle_key`, `App` state

### 1.3 Multiline input — [x]
- Shift+Enter or Alt+Enter inserts a newline
- Enter submits
- Input area grows (or scrolls) to accommodate multiple lines
- Scope: `handle_key`, `draw_input`, input area `Constraint`

### 1.4 Fix spinner memory leak — [x]
- `draw_input` line 420 uses `Box::leak` to work around a lifetime issue
- Leaks a string allocation every frame (~60/sec if polling)
- Fix: pre-allocate spinner strings as `&'static str` or restructure to avoid the borrow
- Scope: `draw_input`

## Tier 2: Visual polish

### 2.1 Word-wrap-aware scrolling — [ ]
- Scroll calculations don't account for lines that wrap within the viewport
- `rendered_lines` counts logical lines, not visual lines after wrap
- Need to compute wrapped line count per entry or use ratatui's line-counting
- Scope: `draw_conversation`, `App::scroll_up/down`

### 2.2 Inline tool results — [ ]
- Currently tool results appear as `AssistantText` in the conversation
- Show results inside the tool call box (collapsed by default)
- Key or click to expand/collapse
- Scope: `ConversationEntry::ToolCall` gains a `result: Option<String>`, `draw_conversation`

### 2.3 Token budget indicator — [ ]
- Status bar shows `tokens: 12k in / 3k out` but not the budget
- Add context utilization: `12k/200k (6%)` or a small progress bar
- Data available via `QuerySession` → `SessionStatus.utilization`
- Scope: `draw_status_bar`, periodic `QuerySession` polling in main loop

### 2.4 Timestamps — [ ]
- Light timestamps on conversation entries (e.g., `14:32` in dark gray)
- Useful for long sessions; optional/toggleable
- Scope: `ConversationEntry` gains a `timestamp: Instant` field, `draw_conversation`

### 2.5 Color theme — [ ]
- Hardcoded colors (Cyan user, Yellow permission, Green success, Red error)
- At minimum: detect light/dark terminal and adjust
- Stretch: user-configurable theme via config file
- Scope: extract colors into a `Theme` struct, pass to draw functions

## Tier 3: Interaction improvements

### 3.1 Copy to clipboard — [ ]
- Select region in conversation pane, copy to system clipboard
- Ctrl+Shift+C or platform-native shortcut
- Requires `clipboard` or `arboard` crate
- Scope: new selection state in `App`, render highlight, copy action

### 3.2 Search in conversation — [ ]
- Ctrl+F opens a search bar, highlights matches, n/N to navigate
- Scope: new `SearchState` in `App`, overlay search bar, highlight spans in `draw_conversation`

### 3.3 Slash commands — [ ]
- `/clear` — clear conversation display (not context)
- `/compact` — send `RequestCompaction` to daemon
- `/status` — send `QuerySession`, display result
- `/policy <path>` — load and send `SetPolicy`
- `/quit` — already exists
- Scope: `handle_key` Submit path, command parser in main loop

### 3.4 Permission "always allow" / "always deny" — [ ]
- Beyond y/n: `a` = always allow this tool, `d` = always deny this tool for the session
- Sends `SetPolicy` to daemon with updated rules
- Scope: `handle_key` permission mode, policy mutation in main loop

## Tier 4: Advanced

### 4.1 Split pane file viewer — [ ]
- When agent reads a file, show contents in a side pane instead of inline
- Toggle with Ctrl+P or tab
- Syntax highlighting for code files
- Scope: new layout variant, file viewer widget, triggered from `ToolCall` results

### 4.2 Session save/resume — [ ]
- `/save [name]` — persist conversation state to disk
- `/load [name]` — restore from disk, reconnect to daemon
- Leverages `ContextStore` trait (durable backend)
- Scope: serialization of `App.entries`, daemon-side session snapshot

### 4.3 Multiple sessions — [ ]
- Tab between sessions in the same TUI
- Each tab is a separate session on the daemon
- Status bar shows tab indicators
- Scope: `App` becomes multi-session, tab switching keybinds, per-session state
