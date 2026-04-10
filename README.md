# copilot-session-tui

A terminal user interface for managing GitHub Copilot CLI sessions. Browse, search, filter, rename, and resume sessions — all from a lightweight TUI that works over SSH.

## Features

- **Full session list** — see ALL your Copilot sessions with virtual scrolling
- **Project filter** — filter sessions by working directory (auto-detects current project)
- **Fuzzy search** — find sessions by name, project, or ID
- **Session details** — preview edited files, last message, and session stats
- **Resume** — press Enter to launch `copilot --resume` directly
- **Rename** — rename sessions inline with `r`
- **Delete** — remove old sessions with `d` (with confirmation)
- **Sort** — cycle through sort orders (last used, created, name, project)
- **Active detection** — see which sessions are currently in use

## Installation

### Prerequisites

- [Rust](https://rustup.rs/) 1.70+
- [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/) (Windows) or GCC (Linux/macOS)

### Build from source

```bash
git clone <repo-url>
cd copilot-session-tui
cargo build --release
```

The binary will be at `target/release/copilot-session-tui` (or `.exe` on Windows).

### Usage

```bash
# Run in any directory — auto-filters to sessions from that project
copilot-session-tui

# Run without auto-filter to see all sessions
copilot-session-tui --auto-filter false

# Use a custom copilot config directory
copilot-session-tui --copilot-home /path/to/.copilot
```

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `↑/k` `↓/j` | Navigate sessions |
| `Home` / `End` | Jump to first/last |
| `Enter` | Resume selected session |
| `r` | Rename session |
| `d` | Delete session (with confirmation) |
| `/` | Fuzzy search |
| `f` / `p` | Filter by project |
| `c` | Clear project filter |
| `s` | Cycle sort order |
| `?` | Show help |
| `q` / `Esc` | Quit |
| `Ctrl+C` | Force quit |

## How It Works

The TUI reads session data directly from `~/.copilot/session-state/` (or `COPILOT_HOME`):

- **`workspace.yaml`** — session metadata (ID, working directory, summary, timestamps)
- **`events.jsonl`** — event log (parsed on-demand for edited files and messages)
- **`inuse.*.lock`** — lock files used to detect active sessions

When you resume a session, the TUI exits cleanly and launches `copilot --resume=<session-id>`.

## License

MIT
