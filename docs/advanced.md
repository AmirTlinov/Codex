## Advanced

If you already lean on Codex every day and just need a little more control, this page collects the knobs you are most likely to reach for: tweak defaults in [Config](./config.md), add extra tools through [Model Context Protocol support](#model-context-protocol), and script full runs with [`codex exec`](./exec.md). Jump to the section you need and keep building.

## Config quickstart {#config-quickstart}

Most day-to-day tuning lives in `config.toml`: set approval + sandbox presets, pin model defaults, and add MCP server launchers. The [Config guide](./config.md) walks through every option and provides copy-paste examples for common setups.

## Tracing / verbose logging {#tracing-verbose-logging}

Because Codex is written in Rust, it honors the `RUST_LOG` environment variable to configure its logging behavior.

The TUI defaults to `RUST_LOG=codex_core=info,codex_tui=info,codex_rmcp_client=info` and log messages are written to `~/.codex/log/codex-tui.log`, so you can leave the following running in a separate terminal to monitor log messages as they are written:

```bash
tail -F ~/.codex/log/codex-tui.log
```

By comparison, the non-interactive mode (`codex exec`) defaults to `RUST_LOG=error`, but messages are printed inline, so there is no need to monitor a separate file.

See the Rust documentation on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for more information on the configuration options.

## Background shell workflow {#background-shell}

Long-running shell commands no longer have to pin the UI. Codex now exposes a
first-class background shell manager that you can drive from both the agent and
the TUI:

- **`run_in_background` flag** – Every shell tool call (including `!` user
  commands) accepts `run_in_background: true|false` plus optional
  `bookmark=` / `description=` tokens. Example user command:
  ``! npm start run_in_background: true bookmark=dev description="watch mode"``
  Codex auto-promotes any foreground command that runs for ~10 seconds,
  so the flag is mainly for proactively long jobs.
- **New RPCs & helpers** – `shell_summary` gives the model a compact list of all
  background shells, `shell_log` returns recent stdout/stderr for a single
  shell (by id or bookmark), and `shell_kill` terminates a runaway job. Under
  the hood, these pair with `Op::BackgroundShellSummary`,
  `Op::PollBackgroundShell`, and `Op::KillBackgroundShell`, so the agent can
  list, tail, and stop shells without
  dumping logs into the chat. Summaries include the latest 10 lines, bookmark,
  and a compact command preview.
- **Ring buffer + retention** – Each shell keeps ~10 MB of stdout/stderr for up
  to one hour after completion. Older lines fall off the buffer (FIFO) and the
  CLI surfaces a `truncated` flag when it happens.
- **Concurrency guard** – At most 10 shells may run simultaneously. Attempts to
  exceed the cap receive a friendly error so you can recycle unused sessions.
- **Ctrl+R / Ctrl+B shortcuts** – Press Ctrl+R to move the most-recent running
  command into the background immediately. Press Ctrl+B (or focus the mini
  widget with ↓ / Enter) to open the Background Tasks view for filtering,
  tailing (`t`), refreshing (`p`), or killing (`k`) processes. Toasts and a
  pulsing indicator call attention to new completions without adding noise to
  the chat transcript.

These behaviors are enabled by default; no config changes are required.

## Model Context Protocol (MCP) {#model-context-protocol}

The Codex CLI and IDE extension is a MCP client which means that it can be configured to connect to MCP servers. For more information, refer to the [`config docs`](./config.md#mcp-integration).

## Using Codex as an MCP Server {#mcp-server}

The Codex CLI can also be run as an MCP _server_ via `codex mcp-server`. For example, you can use `codex mcp-server` to make Codex available as a tool inside of a multi-agent framework like the OpenAI [Agents SDK](https://platform.openai.com/docs/guides/agents). Use `codex mcp` separately to add/list/get/remove MCP server launchers in your configuration.

### Codex MCP Server Quickstart {#mcp-server-quickstart}

You can launch a Codex MCP server with the [Model Context Protocol Inspector](https://modelcontextprotocol.io/legacy/tools/inspector):

```bash
npx @modelcontextprotocol/inspector codex mcp-server
```

Send a `tools/list` request and you will see that there are two tools available:

**`codex`** - Run a Codex session. Accepts configuration parameters matching the Codex Config struct. The `codex` tool takes the following properties:

| Property                | Type   | Description                                                                                                                                            |
| ----------------------- | ------ | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **`prompt`** (required) | string | The initial user prompt to start the Codex conversation.                                                                                               |
| `approval-policy`       | string | Approval policy for shell commands generated by the model: `untrusted`, `on-failure`, `on-request`, `never`.                                           |
| `base-instructions`     | string | The set of instructions to use instead of the default ones.                                                                                            |
| `config`                | object | Individual [config settings](https://github.com/openai/codex/blob/main/docs/config.md#config) that will override what is in `$CODEX_HOME/config.toml`. |
| `cwd`                   | string | Working directory for the session. If relative, resolved against the server process's current directory.                                               |
| `model`                 | string | Optional override for the model name (e.g. `o3`, `o4-mini`).                                                                                           |
| `profile`               | string | Configuration profile from `config.toml` to specify default options.                                                                                   |
| `sandbox`               | string | Sandbox mode: `read-only`, `workspace-write`, or `danger-full-access`.                                                                                 |

**`codex-reply`** - Continue a Codex session by providing the conversation id and prompt. The `codex-reply` tool takes the following properties:

| Property                        | Type   | Description                                              |
| ------------------------------- | ------ | -------------------------------------------------------- |
| **`prompt`** (required)         | string | The next user prompt to continue the Codex conversation. |
| **`conversationId`** (required) | string | The id of the conversation to continue.                  |

### Trying it Out {#mcp-server-trying-it-out}

> [!TIP]
> Codex often takes a few minutes to run. To accommodate this, adjust the MCP inspector's Request and Total timeouts to 600000ms (10 minutes) under ⛭ Configuration.

Use the MCP inspector and `codex mcp-server` to build a simple tic-tac-toe game with the following settings:

**approval-policy:** never

**prompt:** Implement a simple tic-tac-toe game with HTML, JavaScript, and CSS. Write the game in a single file called index.html.

**sandbox:** workspace-write

Click "Run Tool" and you should see a list of events emitted from the Codex MCP server as it builds the game.
