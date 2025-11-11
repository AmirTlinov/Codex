# Code Finder Overview

The canonical usage guide lives in `code-finder/code_finder_tool_instructions.md` and is
embedded into the tool handler via `include_str!`. Read that file for quick commands,
JSON schemas, profiles, and parsing rules. This page captures the operational pieces
that engineers most often need when integrating Code Finder into the CLI, TUI, or other
agents.

## Launching the Daemon

- The daemon is not a standalone binary. All spawns happen through the `codex` executable
  (or a custom path set via `CODE_FINDER_LAUNCHER`).
- To point at a bespoke launcher, export `CODE_FINDER_LAUNCHER=/abs/path/to/codex` before
  running any Code Finder command.
- Index data is stored under `${CODEX_HOME:-$HOME/.codex}/code-finder/<project-hash>`.
  Removing that directory forces a rebuild the next time a tool runs.

## Protocol Compatibility

- All requests must set `schema_version: 3`; the daemon rejects older payloads with a
  `VERSION_MISMATCH` error.
- `hints` and `stats.autocorrections` may be absent in responses. Clients must treat
  them as optional.
- When you see `code_finder requires protocol v3`, delete the cached daemon metadata or
  run any Code Finder command again so the CLI respawns a fresh daemon.

## Troubleshooting Checklist

1. Run `codex code-finder nav --project-root <repo> --limit 5 term`. If it hangs, the
   index is still building; wait for the footer notice or `/index-code` in the TUI.
2. If the CLI prints `failed to spawn code-finder daemon`, ensure `CODE_FINDER_LAUNCHER`
   points to a real `codex` binary and that `CODEX_HOME` is writable.
3. Delete `~/.codex/code-finder/<project-hash>` when switching branches with huge file
   churn to avoid stale trees.
