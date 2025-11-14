## Advanced

If you already lean on Codex every day and just need a little more control, this page collects the knobs you are most likely to reach for: tweak defaults in [Config](./config.md), add extra tools through [Model Context Protocol support](#model-context-protocol), and script full runs with [`codex exec`](./exec.md). Jump to the section you need and keep building.

## Config quickstart {#config-quickstart}

Most day-to-day tuning lives in `config.toml`: set approval + sandbox presets, pin model defaults, and add MCP server launchers. The [Config guide](./config.md) walks through every option and provides copy-paste examples for common setups.

## Background shell controls {#background-shell}

Codex now runs _every_ shell command through the background shell manager instead of invoking a one-off `shell` tool. The implications:

- The CLI no longer supports `!cmd` shortcuts. Ask Codex to run commands; it will decide when to keep them in the foreground vs. background.
- The toolset is fixed to `shell_run`, `shell_summary`, `shell_log`, `shell_kill`, and `shell_resume`. No other shell-like tools are exposed to the model.
- Foreground executions get a 60 s budget. When the timer expires—or when you press `Ctrl+R`—Codex moves the process to the background and posts a status message explaining why.
- Each process renders exactly one card in the chat history. Use `Ctrl+Shift+S` (or press ↓ then Enter on the `N Shell` footer counter) to open the Shell panel for full-screen management: arrow keys to pick a process, `k` to kill, `r` to resume, `Ctrl+R` to force background, `d` for diagnostics, `Enter` for details, and `Esc` to exit.
- Cards and the Shell panel now show a live tail (~2KiB/16 lines) of stdout/stderr so you can track progress without opening logs; truncated tails are annotated inline.
- Each Shell card labels its run mode (foreground/background) and the chat stream posts a single-line summary ("Kill shell-7 (sleep 500)", "Completed shell-3 …") when a process finishes so you never miss the outcome.
- Every promotion or termination also produces a `[shell-id] …` system note in the transcript, so headless clients or log scrapers see who moved or killed the process without watching the panel.
- Every shell summary/event carries the OS PID, so `shell_kill` accepts either the `shell_id` (e.g., `shell-7`) or a numeric `pid` argument when you need to target a process.
- Legacy `!cmd` shortcuts are removed: the TUI shows an info hint instead of running them, and the backend emits a warning if a client still sends `RunUserShellCommand`.
- The bottom footer rotates between Navigator status (`Indexing / Index ready`) and the remaining context budget: when you type, the traditional “90% context left” indicator appears for a few seconds so you can watch the window; after you pause, it automatically returns to the Index capsule so you can monitor background indexing progress.

When you need to inspect output after a command has gone background, prefer the tools:

```text
shell_summary      # list running/completed/failed processes
shell_log         # retrieve incremental logs (tail or diagnostics)
shell_kill        # stop a running shell
shell_resume      # bring a completed/failed command back to the foreground
```

`shell_kill` accepts either `shell_id` or `pid` along with an optional `reason` plus an `initiator`
hint (`"agent"`, `"user"`, or `"system"`). Calls coming from the model default to `agent`, while
the TUI sends `user` for Ctrl+Shift+S kills so attribution in history stays accurate.

Every `shell_run` invocation **must** include an explicit `timeout_ms`. Foreground commands should
typically use something near the 60 s budget (with a little buffer) while background tasks can
request hours or days (e.g., `timeout_ms: 172_800_000` for 48 h). This keeps long-lived jobs from
being killed by the default 10 s exec timeout.

These tools mirror what the TUI shows inside the Shell panel, so you can automate the same workflows from automation or headless clients.

## Tracing / verbose logging {#tracing-verbose-logging}

Because Codex is written in Rust, it honors the `RUST_LOG` environment variable to configure its logging behavior.

The TUI defaults to `RUST_LOG=codex_core=info,codex_tui=info,codex_rmcp_client=info` and log messages are written to `~/.codex/log/codex-tui.log`, so you can leave the following running in a separate terminal to monitor log messages as they are written:

```bash
tail -F ~/.codex/log/codex-tui.log
```

By comparison, the non-interactive mode (`codex exec`) defaults to `RUST_LOG=error`, but messages are printed inline, so there is no need to monitor a separate file.

See the Rust documentation on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for more information on the configuration options.

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
