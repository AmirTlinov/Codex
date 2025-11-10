#![expect(clippy::expect_used)]

use crate::test_backend::VT100Backend;
use codex_tui::custom_terminal::Terminal;
use codex_tui::history_cell_test_support::code_finder_history_lines_for_test;
use codex_tui::insert_history::insert_history_lines;
use ratatui::layout::Rect;
use ratatui::text::Line;

const WIDTH: u16 = 120;
const HEIGHT: u16 = 24;

fn render_history(lines: Vec<Line<'static>>) -> String {
    let backend = VT100Backend::new(WIDTH, HEIGHT);
    let mut term = Terminal::with_options(backend).expect("terminal");
    term.set_viewport_area(Rect::new(0, HEIGHT - 3, WIDTH, 3));
    insert_history_lines(&mut term, lines).expect("insert history");
    term.backend().vt100().screen().contents()
}

#[test]
fn search_request_renders_summary() {
    const SEARCH_REQUEST: &str = r#"*** Begin Search
query: SessionID
kinds: function
languages: rust
with_refs: true
refs_limit: 1
limit: 3
*** End Search"#;

    let screen = render_history(code_finder_history_lines_for_test(
        SEARCH_REQUEST,
        None,
        WIDTH,
    ));

    assert!(
        screen.contains("• Explored") || screen.contains("• Exploring"),
        "missing explored header:\n{screen}"
    );
    assert!(
        screen.contains("Search SessionID (rust)"),
        "header missing summary:\n{screen}"
    );
    assert!(
        screen.contains("SessionID (rust)"),
        "summary missing query/language:\n{screen}"
    );
}

#[test]
fn search_results_include_hits_and_index_status() {
    const SEARCH_REQUEST: &str = r#"*** Begin Search
query: SessionID
kinds: function
languages: rust
with_refs: true
refs_limit: 1
limit: 3
*** End Search"#;

    const SEARCH_RESPONSE: &str = r#"{
  "query_id": "11111111-1111-1111-1111-111111111111",
  "hits": [
    {
      "id": "hit-1",
      "path": "core/src/lib.rs",
      "line": 42,
      "kind": "function",
      "language": "rust",
      "module": "core::lib",
      "layer": "core",
      "categories": ["source"],
      "recent": true,
      "preview": "fn session_id() -> SessionId",
      "score": 0.98,
      "references": [
        {"path": "core/tests.rs", "line": 12, "preview": "SessionID is validated here"}
      ]
    }
  ],
  "index": {
    "state": "ready",
    "symbols": 1,
    "files": 1,
    "updated_at": null,
    "progress": null,
    "schema_version": 1
  },
  "stats": {
    "took_ms": 15,
    "candidate_size": 6,
    "cache_hit": true
  }
}"#;

    let screen = render_history(code_finder_history_lines_for_test(
        SEARCH_REQUEST,
        Some(SEARCH_RESPONSE),
        WIDTH,
    ));

    assert!(
        screen.contains("• Explored") || screen.contains("• Exploring"),
        "missing explored header:\n{screen}"
    );
    assert!(screen.contains("hits: 1"), "missing hit summary:\n{screen}");
    assert!(
        screen.contains("index: Ready"),
        "missing index status:\n{screen}"
    );
    assert!(
        screen.contains("core/src/lib.rs:42"),
        "missing top hit path:\n{screen}"
    );
    assert!(
        screen.contains("recent"),
        "expected recent marker in header:\n{screen}"
    );
    assert!(
        screen.contains("refs: core/tests.rs:12"),
        "missing references summary:\n{screen}"
    );
}

#[test]
fn search_error_is_displayed() {
    const SEARCH_REQUEST: &str = r#"*** Begin Search
query: build plan
recent: true
wait_for_index: false
*** End Search"#;

    const ERROR_RESPONSE: &str = r#"{
  "query_id": null,
  "hits": [],
  "index": {
    "state": "building",
    "symbols": 0,
    "files": 0,
    "updated_at": null,
    "progress": 0.25,
    "schema_version": 1
  },
  "stats": null,
  "error": {
    "code": "INDEX_NOT_READY",
    "message": "Index is still building"
  }
}"#;

    let screen = render_history(code_finder_history_lines_for_test(
        SEARCH_REQUEST,
        Some(ERROR_RESPONSE),
        WIDTH,
    ));

    assert!(
        screen.contains("• Explored"),
        "missing explored header:\n{screen}"
    );
    assert!(
        screen.contains("index: Building"),
        "missing building status:\n{screen}"
    );
    assert!(
        screen.contains("Index is still building"),
        "error line missing:\n{screen}"
    );
}

#[test]
fn malformed_request_shows_parse_error() {
    const BAD_REQUEST: &str = r#"{"command":"*** Begin Search\nquery: oops\n*** End Search"}"#;
    const ERROR_OUTPUT: &str =
        "code_finder accepts only *** Begin <Action> blocks; JSON payloads are not supported";

    let screen = render_history(code_finder_history_lines_for_test(
        BAD_REQUEST,
        Some(ERROR_OUTPUT),
        WIDTH,
    ));

    assert!(
        screen.contains("code_finder block must start"),
        "parse error not surfaced:\n{screen}"
    );
}
