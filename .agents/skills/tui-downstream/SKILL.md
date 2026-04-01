---
name: tui-downstream
description: Downstream customization workflow for user-visible TUI changes in codex-rs/tui. Use when touching layout, rendering, keybindings, snapshots, or other terminal UI behavior.
---

# TUI downstream

## Use when

- the task changes terminal UI, keybindings, layout, bottom pane behavior, or
  rendered text;
- the task touches `codex-rs/tui/`;
- the user asks for TUI polish or custom TUI behavior in this fork.

## Read first

1. `codex-rs/tui/styles.md`
2. the owning files under `codex-rs/tui/src/`
3. relevant docs under `docs/tui-*.md` only if the change affects documented
   behavior

## Downstream policy

- Prefer isolated new modules over growing high-churn TUI files.
- Keep changes additive and easy to rebase.
- Follow the root `AGENTS.md` TUI conventions exactly.
- If the change is only personal workflow behavior, consider whether it belongs
  outside the fork first.

## Validation

Always run:

```bash
cd codex-rs
just fmt
cargo test -p codex-tui
```

If user-visible rendering changed, also check snapshots:

```bash
cd codex-rs
cargo insta pending-snapshots -p codex-tui
```

If the slice is large, also run:

```bash
cd codex-rs
just fix -p codex-tui
```

## Done looks like

- the TUI behavior is implemented;
- snapshot coverage is updated when required;
- the changed docs are updated when behavior changed;
- the patch remains local and rebase-friendly.
