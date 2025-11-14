#!/usr/bin/env python3
"""Interactive ASCII prototype for the Shell panel UX."""

import curses
import os
import tempfile


TABS = [
    (
        "RUNNING",
        [
            {
                "id": "shell-4",
                "label": "sleep 120",
                "info": "ETA 01:43 · auto background",
                "status": "running",
                "mode": "background",
                "ended_by": "agent",
                "pid": 53210,
                "promoted_by": "system",
                "started": "12:03:15",
                "finished": None,
                "log": ["remaining 45", "remaining 44", "remaining 43"],
            },
            {
                "id": "shell-2",
                "label": "python countdown",
                "info": "ETA 00:20",
                "status": "running",
                "mode": "foreground",
                "ended_by": "agent",
                "pid": 53211,
                "promoted_by": None,
                "started": "12:00:10",
                "finished": None,
                "log": [
                    "T-20",
                    "T-19",
                    "T-18",
                ],
            },
            {
                "id": "shell-7",
                "label": "tar backup",
                "info": "progress 35%",
                "status": "running",
                "mode": "foreground",
                "ended_by": "agent",
                "pid": 53212,
                "promoted_by": None,
                "started": "11:55:02",
                "finished": None,
                "log": ["packing src/", "packing docs/", "packing tests/"],
            },
        ],
    ),
    (
        "COMPLETED",
        [
            {
                "id": "shell-1",
                "label": "git status",
                "info": "ok (12:01:22)",
                "status": "completed",
                "mode": "foreground",
                "summary": "Completed shell-1 (git status)",
                "ended_by": "agent",
                "pid": 53190,
                "promoted_by": None,
                "started": "12:01:10",
                "finished": "12:01:22",
                "log": ["On branch feature/background-shell", "nothing to commit"],
            }
        ],
    ),
    (
        "FAILED",
        [
            {
                "id": "shell-3",
                "label": "sleep 120",
                "info": "killed by user",
                "status": "failed",
                "mode": "background",
                "summary": "Kill shell-3 (sleep 120)",
                "ended_by": "user",
                "pid": 53150,
                "promoted_by": None,
                "started": "11:59:00",
                "finished": "12:00:30",
                "log": ["timeout occurred", "process terminated"],
            }
        ],
    ),
]


def draw_panel(stdscr, tab_index, selection_index):
    stdscr.clear()
    height, width = stdscr.getmaxyx()

    tabs_line = [name.lower() for name, _ in TABS]
    tabs_line[tab_index] = f"[{tabs_line[tab_index].upper()}]"
    stdscr.addstr(0, 0, " - ".join(tabs_line)[: width - 1], curses.A_BOLD)

    _, entries = TABS[tab_index]
    max_rows = max(1, height - 4)  # leave space for title and controls
    total = len(entries)
    if total > max_rows:
        start = min(max(0, selection_index - max_rows // 2), total - max_rows)
    else:
        start = 0
    for row, entry in enumerate(entries[start : start + max_rows]):
        idx = start + row
        prefix = "> " if idx == selection_index else "  "
        line = f"{prefix}{entry['id']:<8} {entry['label']:<20} {entry['info']}"
        attr = curses.A_REVERSE if idx == selection_index else curses.A_NORMAL
        stdscr.addstr(2 + row, 0, line[: width - 1], attr)

    controls = (
        "←/→ tabs · ↑/↓ select · Enter details · k kill · d diagnostics · r resume · "
        "Ctrl+R background · q/Esc exit"
    )
    stdscr.addstr(height - 1, 0, controls[: width - 1], curses.A_DIM)
    stdscr.refresh()


def show_detail_screen(stdscr, entry):
    offset = 0
    status_msg = ""
    while True:
        stdscr.clear()
        height, width = stdscr.getmaxyx()
        header = f"[PROCESS]: {entry['id']} ({entry['label']})"
        stdscr.addstr(0, 0, header[: width - 1], curses.A_BOLD)

        status = entry.get('status', 'unknown')
        started = entry.get('started', '-')
        finished = entry.get('finished')
        lines = [f"Status: {status}", f"Started: {started}"]
        mode = entry.get("mode")
        if mode:
            lines.append(f"Mode: {mode}")
        if status.lower() in {"completed", "failed"}:
            lines.append(f"Finished: {finished or '-'}")
            lines.append(f"Ended by: {entry.get('ended_by', 'agent')}")
        promoted_by = entry.get('promoted_by')
        if promoted_by:
            lines.append(f"Promoted by: {promoted_by}")
        pid = entry.get('pid')
        if pid:
            lines.append(f"PID: {pid}")
        summary = entry.get("summary")
        if summary:
            lines.append(f"Summary: {summary}")
        lines.append("Logs:")

        y = 1
        for line in lines:
            stdscr.addstr(y, 0, line[: width - 1])
            y += 1

        stdscr.hline(y, 0, ord("─"), width)
        y += 1
        log_lines = entry["log"]
        available = max(1, height - y - 3)
        max_offset = max(0, len(log_lines) - available)
        offset = max(0, min(offset, max_offset))
        for line in log_lines[offset : offset + available]:
            stdscr.addstr(y, 2, line[: width - 3])
            y += 1

        stdscr.hline(y, 0, ord("─"), width)
        y += 1
        if status_msg:
            stdscr.addstr(y, 0, status_msg[: width - 1], curses.A_DIM)

        controls = "↑/↓ scroll · d diagnostics · c copy · q/Esc back"
        stdscr.addstr(height - 1, 0, controls[: width - 1], curses.A_DIM)
        stdscr.refresh()

        key = stdscr.getch()
        if key == curses.KEY_UP:
            offset = max(0, offset - 1)
        elif key == curses.KEY_DOWN:
            offset = min(max_offset, offset + 1)
        elif key in (ord('c'), ord('C')):
            path = os.path.join(tempfile.gettempdir(), "shell_panel_log.txt")
            with open(path, "w", encoding="utf-8") as fp:
                fp.write("\n".join(log_lines))
            status_msg = f"Log copied to {path}"
        elif key in (27, ord('q'), curses.KEY_ENTER, 10, 13):
            break


def main(stdscr):
    curses.curs_set(0)
    stdscr.nodelay(False)
    tab_index = 0
    selection_index = 0

    while True:
        entries = TABS[tab_index][1]
        if entries:
            selection_index = max(0, min(selection_index, len(entries) - 1))
        else:
            selection_index = 0
        draw_panel(stdscr, tab_index, selection_index)

        key = stdscr.getch()
        if key in (curses.KEY_LEFT, ord('h')):
            tab_index = (tab_index - 1) % len(TABS)
        elif key in (curses.KEY_RIGHT, ord('l')):
            tab_index = (tab_index + 1) % len(TABS)
        elif key in (curses.KEY_UP, ord('k')):
            selection_index = max(0, selection_index - 1)
        elif key in (curses.KEY_DOWN, ord('j')):
            entries = TABS[tab_index][1]
            selection_index = min(len(entries) - 1, selection_index + 1) if entries else 0
        elif key in (curses.KEY_ENTER, 10, 13):
            entries = TABS[tab_index][1]
            if entries:
                show_detail_screen(stdscr, entries[selection_index])
        elif key in (ord('k'), ord('K')):
            pass  # TODO: hook into kill action
        elif key in (ord('d'), ord('D')):
            pass  # TODO: hook into diagnostics
        elif key in (ord('r'), ord('R')):
            pass  # TODO: hook into resume
        elif key in (27, ord('q')):
            break


if __name__ == "__main__":
    curses.wrapper(main)
