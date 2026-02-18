#!/usr/bin/env python3
"""Incremental Mermaid map builder for Scout context exploration.

State file (JSON) keeps nodes/edges so scouts can update architecture maps in-place:
- init: create empty map state
- merge: upsert/remove nodes and edges from YAML/JSON delta files
- render: output Mermaid (or canonical JSON) from state
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import tempfile
from pathlib import Path
from typing import Any

NODE_ID_RE = re.compile(r"[A-Za-z][A-Za-z0-9_]*$")
VALID_DIRECTIONS = {"LR", "RL", "TB", "TD", "BT"}
VALID_SHAPES = {"rect", "round", "diamond", "circle"}


def _die(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(2)


def _atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        newline="\n",
        delete=False,
        dir=path.parent,
        prefix=f".{path.name}.",
        suffix=".tmp",
    ) as tmp:
        tmp.write(content)
        tmp_path = Path(tmp.name)
    os.replace(tmp_path, path)


def _load_yaml_or_json(path: Path) -> dict[str, Any]:
    if not path.exists():
        _die(f"file not found: {path}")

    suffix = path.suffix.lower()
    if suffix == ".json":
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as err:
            _die(f"invalid JSON {path}: {err}")
    elif suffix in {".yml", ".yaml"}:
        try:
            import yaml  # type: ignore[import-not-found]
        except ImportError:
            _die("PyYAML is required for .yml/.yaml files (pip install pyyaml)")
        data = yaml.safe_load(path.read_text(encoding="utf-8"))
    else:
        _die(f"unsupported file extension for {path}; expected .json/.yml/.yaml")

    if not isinstance(data, dict):
        _die(f"{path}: expected top-level mapping")
    return data


def _validate_no_unknown_fields(obj: dict[str, Any], allowed: set[str], ctx: str) -> None:
    extra = set(obj.keys()) - allowed
    if extra:
        _die(f"{ctx}: unknown field(s): {', '.join(sorted(extra))}")


def _expect_string(value: Any, ctx: str) -> str:
    if not isinstance(value, str):
        _die(f"{ctx}: expected string")
    value = value.strip()
    if not value:
        _die(f"{ctx}: expected non-empty string")
    return value


def _expect_string_list(value: Any, ctx: str) -> list[str]:
    if value is None:
        return []
    if not isinstance(value, list):
        _die(f"{ctx}: expected list of strings")
    out: list[str] = []
    for idx, item in enumerate(value):
        out.append(_expect_string(item, f"{ctx}[{idx}]"))
    return out


def _normalize_direction(value: Any, ctx: str) -> str:
    direction = _expect_string(value, ctx).upper()
    if direction not in VALID_DIRECTIONS:
        _die(f"{ctx}: expected one of {', '.join(sorted(VALID_DIRECTIONS))}")
    return direction


def _normalize_node(node: Any, ctx: str) -> dict[str, str]:
    if not isinstance(node, dict):
        _die(f"{ctx}: expected mapping")
    _validate_no_unknown_fields(node, {"id", "label", "shape"}, ctx)

    node_id = _expect_string(node.get("id"), f"{ctx}.id")
    if not NODE_ID_RE.fullmatch(node_id):
        _die(f"{ctx}.id: expected regex {NODE_ID_RE.pattern}")

    label_raw = node.get("label")
    if label_raw is None:
        label = node_id
    else:
        label = _expect_string(label_raw, f"{ctx}.label")

    shape_raw = node.get("shape")
    if shape_raw is None:
        shape = "rect"
    else:
        shape = _expect_string(shape_raw, f"{ctx}.shape").lower()
        if shape not in VALID_SHAPES:
            _die(f"{ctx}.shape: expected one of {', '.join(sorted(VALID_SHAPES))}")

    return {"id": node_id, "label": label, "shape": shape}


def _normalize_edge(edge: Any, ctx: str) -> dict[str, str]:
    if not isinstance(edge, dict):
        _die(f"{ctx}: expected mapping")
    _validate_no_unknown_fields(edge, {"from", "to", "label"}, ctx)

    edge_from = _expect_string(edge.get("from"), f"{ctx}.from")
    edge_to = _expect_string(edge.get("to"), f"{ctx}.to")
    if not NODE_ID_RE.fullmatch(edge_from):
        _die(f"{ctx}.from: expected regex {NODE_ID_RE.pattern}")
    if not NODE_ID_RE.fullmatch(edge_to):
        _die(f"{ctx}.to: expected regex {NODE_ID_RE.pattern}")

    label_raw = edge.get("label")
    if label_raw is None:
        label = ""
    else:
        label = _expect_string(label_raw, f"{ctx}.label")

    return {"from": edge_from, "to": edge_to, "label": label}


def _edge_key(edge: dict[str, str]) -> tuple[str, str, str]:
    return edge["from"], edge["to"], edge.get("label", "")


def _load_state(path: Path) -> dict[str, Any]:
    if not path.exists():
        _die(f"state file not found: {path}")

    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as err:
        _die(f"invalid state JSON: {err}")

    if not isinstance(state, dict):
        _die("state: expected top-level mapping")
    _validate_no_unknown_fields(
        state,
        {"version", "title", "direction", "nodes", "edges"},
        "state",
    )

    version = state.get("version")
    if version != 1:
        _die("state.version: expected 1")

    title = _expect_string(state.get("title"), "state.title")
    direction = _normalize_direction(state.get("direction"), "state.direction")

    nodes_value = state.get("nodes")
    if not isinstance(nodes_value, dict):
        _die("state.nodes: expected mapping")
    nodes: dict[str, dict[str, str]] = {}
    for node_id, node_data in nodes_value.items():
        if not isinstance(node_id, str):
            _die("state.nodes keys: expected string ids")
        normalized = _normalize_node(
            {"id": node_id, **(node_data or {})},
            f"state.nodes[{node_id}]",
        )
        nodes[node_id] = {
            "label": normalized["label"],
            "shape": normalized["shape"],
        }

    edges_value = state.get("edges")
    if not isinstance(edges_value, list):
        _die("state.edges: expected list")
    edges = [
        _normalize_edge(edge, f"state.edges[{idx}]")
        for idx, edge in enumerate(edges_value)
    ]
    seen_edges: set[tuple[str, str, str]] = set()
    for edge in edges:
        key = _edge_key(edge)
        if key in seen_edges:
            _die(f"state.edges: duplicate edge {key}")
        seen_edges.add(key)

    return {
        "version": 1,
        "title": title,
        "direction": direction,
        "nodes": nodes,
        "edges": edges,
    }


def _render_node(node_id: str, label: str, shape: str) -> str:
    safe_label = label.replace('"', "\\\"")
    if shape == "round":
        return f'{node_id}("{safe_label}")'
    if shape == "diamond":
        return f'{node_id}{{"{safe_label}"}}'
    if shape == "circle":
        return f'{node_id}(("{safe_label}"))'
    return f'{node_id}["{safe_label}"]'


def _render_mermaid(state: dict[str, Any]) -> str:
    lines = [f"flowchart {state['direction']}", f"  %% {state['title']}"]

    for node_id in sorted(state["nodes"]):
        node = state["nodes"][node_id]
        lines.append(f"  {_render_node(node_id, node['label'], node['shape'])}")

    edges = sorted(state["edges"], key=_edge_key)
    for edge in edges:
        label = edge.get("label", "")
        if label:
            safe_label = label.replace("|", "\\|")
            lines.append(f"  {edge['from']} -->|{safe_label}| {edge['to']}")
        else:
            lines.append(f"  {edge['from']} --> {edge['to']}")

    return "\n".join(lines).rstrip("\n") + "\n"


def _write_state(path: Path, state: dict[str, Any]) -> None:
    serializable = {
        "version": 1,
        "title": state["title"],
        "direction": state["direction"],
        "nodes": {
            node_id: state["nodes"][node_id]
            for node_id in sorted(state["nodes"])
        },
        "edges": sorted(state["edges"], key=_edge_key),
    }
    _atomic_write(path, json.dumps(serializable, indent=2, ensure_ascii=False) + "\n")


def cmd_init(args: argparse.Namespace) -> None:
    state_path = Path(args.state)
    if state_path.exists() and not args.force:
        _die(f"state already exists: {state_path} (use --force to overwrite)")

    state = {
        "version": 1,
        "title": args.title,
        "direction": _normalize_direction(args.direction, "--direction"),
        "nodes": {},
        "edges": [],
    }
    _write_state(state_path, state)


def cmd_merge(args: argparse.Namespace) -> None:
    state_path = Path(args.state)
    delta_path = Path(args.delta)

    state = _load_state(state_path)
    delta = _load_yaml_or_json(delta_path)
    _validate_no_unknown_fields(
        delta,
        {"title", "direction", "nodes", "edges", "remove_nodes", "remove_edges"},
        "delta",
    )

    if "title" in delta and delta["title"] is not None:
        state["title"] = _expect_string(delta["title"], "delta.title")
    if "direction" in delta and delta["direction"] is not None:
        state["direction"] = _normalize_direction(delta["direction"], "delta.direction")

    for node_id in _expect_string_list(delta.get("remove_nodes"), "delta.remove_nodes"):
        state["nodes"].pop(node_id, None)

    remove_edges_value = delta.get("remove_edges")
    remove_edges: set[tuple[str, str, str]] = set()
    if remove_edges_value is not None:
        if not isinstance(remove_edges_value, list):
            _die("delta.remove_edges: expected list")
        for idx, edge in enumerate(remove_edges_value):
            remove_edges.add(_edge_key(_normalize_edge(edge, f"delta.remove_edges[{idx}]")))

    state["edges"] = [
        edge
        for edge in state["edges"]
        if edge["from"] in state["nodes"]
        and edge["to"] in state["nodes"]
        and _edge_key(edge) not in remove_edges
    ]

    nodes_value = delta.get("nodes")
    if nodes_value is not None:
        if not isinstance(nodes_value, list):
            _die("delta.nodes: expected list")
        for idx, node in enumerate(nodes_value):
            normalized = _normalize_node(node, f"delta.nodes[{idx}]")
            node_id = normalized["id"]
            state["nodes"][node_id] = {
                "label": normalized["label"],
                "shape": normalized["shape"],
            }

    edges_value = delta.get("edges")
    if edges_value is not None:
        if not isinstance(edges_value, list):
            _die("delta.edges: expected list")

        existing = {_edge_key(edge): edge for edge in state["edges"]}
        for idx, edge in enumerate(edges_value):
            normalized = _normalize_edge(edge, f"delta.edges[{idx}]")
            if normalized["from"] not in state["nodes"]:
                _die(f"delta.edges[{idx}].from: unknown node {normalized['from']}")
            if normalized["to"] not in state["nodes"]:
                _die(f"delta.edges[{idx}].to: unknown node {normalized['to']}")
            existing[_edge_key(normalized)] = normalized

        state["edges"] = sorted(existing.values(), key=_edge_key)

    _write_state(state_path, state)


def cmd_render(args: argparse.Namespace) -> None:
    state = _load_state(Path(args.state))

    if args.format == "json":
        rendered = json.dumps(
            {
                "version": 1,
                "title": state["title"],
                "direction": state["direction"],
                "nodes": {
                    node_id: state["nodes"][node_id]
                    for node_id in sorted(state["nodes"])
                },
                "edges": sorted(state["edges"], key=_edge_key),
            },
            indent=2,
            ensure_ascii=False,
        ) + "\n"
    else:
        rendered = _render_mermaid(state)

    if args.output == "-":
        sys.stdout.write(rendered)
        return

    _atomic_write(Path(args.output), rendered)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="cmd", required=True)

    init_parser = subparsers.add_parser("init", help="Create a new scout map state file")
    init_parser.add_argument("state", help="Path to map state JSON")
    init_parser.add_argument(
        "--title",
        default="Scout architecture map",
        help="Graph title used as Mermaid header comment",
    )
    init_parser.add_argument(
        "--direction",
        default="LR",
        help="Mermaid flow direction (LR, RL, TB, TD, BT)",
    )
    init_parser.add_argument(
        "--force",
        action="store_true",
        help="Overwrite existing state file",
    )
    init_parser.set_defaults(func=cmd_init)

    merge_parser = subparsers.add_parser(
        "merge",
        help="Merge a YAML/JSON delta into map state",
    )
    merge_parser.add_argument("state", help="Path to map state JSON")
    merge_parser.add_argument("delta", help="Path to delta YAML/JSON")
    merge_parser.set_defaults(func=cmd_merge)

    render_parser = subparsers.add_parser("render", help="Render Mermaid from map state")
    render_parser.add_argument("state", help="Path to map state JSON")
    render_parser.add_argument(
        "--format",
        choices=["mermaid", "json"],
        default="mermaid",
        help="Output format",
    )
    render_parser.add_argument(
        "--output",
        "-o",
        default="-",
        help='Output file path, or "-" for stdout',
    )
    render_parser.set_defaults(func=cmd_render)

    args = parser.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
