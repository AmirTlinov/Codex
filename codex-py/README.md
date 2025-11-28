# Codex Python

Python implementation of Codex TUI/CLI - an AI coding assistant.

## Installation

```bash
uv sync
```

## Usage

### Interactive TUI

```bash
uv run codex
```

### Exec Mode (SDK Compatible)

```bash
uv run codex-exec --experimental-json
```

## Features

- Interactive terminal UI with Textual
- Streaming API responses
- Command execution with PTY
- Background shell management
- MCP client support
- JSONL event protocol (SDK compatible)

## Project Structure

```
codex-py/
├── src/
│   ├── codex_protocol/    # Event/Item types for SDK compatibility
│   ├── codex_core/        # Core engine, config, API client
│   ├── codex_shell/       # Shell execution, PTY, background processes
│   └── codex_tui/         # Textual-based TUI
└── tests/
```

## License

MIT
