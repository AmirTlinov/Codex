## Advanced

If you already lean on Codex every day and just need a little more control, this page collects the knobs you are most likely to reach for: tweak defaults in [Config](./config.md), add extra tools through [Model Context Protocol support](#model-context-protocol), and script full runs with [`codex exec`](./exec.md). Jump to the section you need and keep building.

## Config quickstart {#config-quickstart}

Most day-to-day tuning lives in `config.toml`: set approval + sandbox presets, pin model defaults, and add MCP server launchers. The [Config guide](./config.md) walks through every option and provides copy-paste examples for common setups.

### TUI word wrapping {#tui-word-wrap}

The transcript renderer now respects the `wrap_break_long_words` knob in `config.toml`. Leave it at the default `true` to let Codex break extremely long tokens mid-word (best when streaming minified code or base64 blobs), or flip it to `false` for strict token boundaries. Toggle it via config overrides (`codex -c wrap_break_long_words=false`), by editing `~/.codex/config.toml`, or directly from the new `/settings` popup inside the TUI:

```toml
wrap_break_long_words = false
```

The setting is read at startup and pushed down to both the TUI and inline markdown stream collector, so the composer, transcript, and overlays stay in sync. When you use `/settings`, Codex updates the running session immediately and persists the preference back to `config.toml`.

### Agents context auto-attach {#agents-context-auto-attach}

Legacy builds always injected `AGENTS.md` (and the rest of `.agents/context`) into every prompt, which made it easy to forget the files were even there. The new `auto_attach_agents_context` toggle restores that behavior by default but lets you opt out globally via `config.toml` *or* on a per-session basis from `/settings`. Disable it when you need a clean slate and re-enable it later without restarting the TUI.

### Desktop notifications {#desktop-notifications}

Need a ping when Codex finishes a turn or pauses for approval? Set `tui.notifications = true` in `config.toml` (or edit a custom allowlist such as `tui.notifications = ["agent-turn-complete", "approval-requested"]`). You can also flip the boolean directly from `/settings`; the toggle updates the running session immediately and persists the value back to `config.toml`. If you previously configured a custom allowlist, the toggle resets it to a simple on/off switch—revisit the config file to fine-tune the list again later.

## Tracing / verbose logging {#tracing-verbose-logging}

Because Codex is written in Rust, it honors the `RUST_LOG` environment variable to configure its logging behavior.

The TUI defaults to `RUST_LOG=codex_core=info,codex_tui=info,codex_rmcp_client=info` and log messages are written to `~/.codex/log/codex-tui.log`, so you can leave the following running in a separate terminal to monitor log messages as they are written:

```
tail -F ~/.codex/log/codex-tui.log
```

By comparison, the non-interactive mode (`codex exec`) defaults to `RUST_LOG=error`, but messages are printed inline, so there is no need to monitor a separate file.

See the Rust documentation on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for more information on the configuration options.

## Model Context Protocol (MCP) {#model-context-protocol}

The Codex CLI and IDE extension is a MCP client which means that it can be configured to connect to MCP servers. For more information, refer to the [`config docs`](./config.md#mcp-integration).

## Background shells, Live Exec, and the process manager {#background-shells}

Long-running shell commands no longer need to monopolize the composer. Promote them to a managed background shell, keep an eye on the aggregated output in the Live Exec overlay, and drill into logs or send follow-up stdin from the new process manager overlay.

### Promote long commands to a background shell (`Ctrl+B`)

- While a foreground shell command is running, press `Ctrl+B` to promote the **most recent running command** into a background shell.
- The TUI hides the inline status indicator for that command, emits a short informational message (“Background shell `shell_42`: npm run dev”), and keeps streaming log deltas through the Live Exec queue.
- Each promotion automatically captures run metadata (the command line, cwd, and optional description). You can edit the description later via `/note` or by assigning one when calling the `shell` tool with the `description` field.

### Live Exec overlay (`Ctrl+R`)

- `Ctrl+R` toggles the Live Exec overlay in place. It shows every active foreground/background shell, their most recent stdout chunk, and whether Codex is still waiting for exit codes.
- Use it as a lightweight dashboard while commands are running. When you are done, press `Ctrl+R` (or `Esc`) again to return to the main transcript.

### Process manager overlay (`Ctrl+Shift+B`)

Press `Ctrl+Shift+B` to open the dedicated overlay for background shells. The table lists running + recently exited sessions (ID, status, command preview, timing), and the footer reminds you of the available shortcuts:

| Key | Action |
| --- | --- |
| `↑` / `↓` | Move selection |
| `PgUp` / `PgDn` | Page through the table |
| `r` | Refresh session list (pull latest state/output preview) |
| `o` | Open the selected session’s log window |
| `i` | Send stdin to the selected session (opens a composer prompt) |
| `k` | Kill the selected session |
| `d` | Remove the session from history once you have read the logs |
| `Esc` / `Ctrl+C` | Close the overlay |

Codex surfaces a condensed process badge above the composer while background work is running, so you can see whether anything is still active without leaving the main chat.

### Log windows, paging, and exporting

When you press `o` in the process manager, Codex opens a scrollable log window for that session:

- `↑/↓`, `PgUp/PgDn`, `g/G` (top/bottom) scroll inside the current window; `r` jumps back to the live tail.
- Log data is streamed in 2 MiB windows. Use `Alt+PgUp` to request the previous window and `Alt+PgDn` to shift forward without leaving the overlay.
- Press `e` to open the export prompt (pre-fills a filename based on the session ID); Codex writes the chosen log slice to disk.
- `Esc` or `Ctrl+C` closes the window and returns you to the manager.

### Sending stdin after promotion

Use `i` inside the manager to open a focused prompt for the selected session. The input is delivered verbatim (no surrounding quotes) to the shell’s stdin, so you can answer an interactive prompt or nudge a REPL without stopping the process.

Need a quick inline toggle instead? Type `/logs [stdout|stderr|both] [show|hide|toggle] [call-id]` in the composer to expand or collapse the corresponding block for the most recent (or a specific) command directly inside the transcript.

### Tooling / API hooks (`run_in_background`, `bash_output`, `kill_shell`)

Programs talking to Codex through the Responses API have access to the same plumbing:

- The `shell` tool now accepts `run_in_background: true` plus an optional human-readable `description`. It immediately returns a payload like:

```json
{"shell_id":"shell_42","running":true,"exit_code":null,"initial_output":"Bundler warming up..."}
```

- Poll incremental output with the `bash_output` tool. Pass the `bash_id` (the `shell_id` above) and an optional `filter` regular expression to only receive matching lines.
- Call `kill_shell` with the `shell_id` to stop the process. The response echoes the final exit code and the aggregated output so the agent can summarize what happened.

These APIs power the TUI features described above, so anything you build on top of them stays in sync with the native process manager and Live Exec displays.

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

**prompt:** Implement a simple tic-tac-toe game with HTML, Javascript, and CSS. Write the game in a single file called index.html.

**sandbox:** workspace-write

Click "Run Tool" and you should see a list of events emitted from the Codex MCP server as it builds the game.
