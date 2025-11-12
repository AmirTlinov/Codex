use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

#[derive(Clone, Debug)]
pub struct Bookmark {
    pub name: String,
    pub line: usize,
}

pub struct ScrollView {
    lines: Vec<String>,
    scroll: usize,
    filter: Option<String>,
    matches: Vec<usize>,
    active_match: usize,
    bookmarks: Vec<Bookmark>,
    active_bookmark: Option<usize>,
}

impl ScrollView {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            scroll: 0,
            filter: None,
            matches: Vec::new(),
            active_match: 0,
            bookmarks: Vec::new(),
            active_bookmark: None,
        }
    }

    pub fn set_lines(&mut self, lines: Vec<String>) {
        self.lines = lines;
        if self.scroll >= self.lines.len() {
            self.scroll = self.lines.len().saturating_sub(1);
        }
        self.rebuild_matches();
        self.ensure_scroll_bounds();
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let current = self.scroll as isize;
        let next = (current + delta).clamp(0, self.lines.len().saturating_sub(1) as isize);
        self.scroll = next as usize;
    }

    pub fn page_by(&mut self, height: usize, delta: isize) {
        let step = (height as isize).max(1) * delta;
        self.scroll_by(step);
    }

    pub fn jump_to_start(&mut self) {
        self.scroll = 0;
    }

    pub fn jump_to_end(&mut self) {
        self.scroll = self.lines.len().saturating_sub(1);
    }

    pub fn set_filter(&mut self, query: Option<String>) {
        self.filter = query.filter(|q| !q.is_empty());
        self.rebuild_matches();
        self.active_match = 0;
        if !self.matches.is_empty() {
            let line = self.matches[0];
            self.scroll_to_line(line);
        }
    }

    pub fn next_match(&mut self) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        self.active_match = (self.active_match + 1) % self.matches.len();
        let line = self.matches[self.active_match];
        self.scroll_to_line(line);
        true
    }

    pub fn prev_match(&mut self) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        if self.active_match == 0 {
            self.active_match = self.matches.len() - 1;
        } else {
            self.active_match -= 1;
        }
        let line = self.matches[self.active_match];
        self.scroll_to_line(line);
        true
    }

    pub fn set_bookmarks(&mut self, bookmarks: Vec<Bookmark>) {
        self.bookmarks = bookmarks;
        self.active_bookmark = None;
    }

    pub fn cycle_bookmark(&mut self, forward: bool) -> bool {
        if self.bookmarks.is_empty() {
            return false;
        }
        let next = match self.active_bookmark {
            None => 0,
            Some(idx) => {
                if forward {
                    (idx + 1) % self.bookmarks.len()
                } else if idx == 0 {
                    self.bookmarks.len() - 1
                } else {
                    idx - 1
                }
            }
        };
        self.active_bookmark = Some(next);
        let target = self.bookmarks[next]
            .line
            .min(self.lines.len().saturating_sub(1));
        self.scroll_to_line(target);
        true
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let height = area.height as usize;
        let len = self.lines.len();
        let start = if len == 0 {
            0
        } else {
            self.scroll.min(len.saturating_sub(1))
        };
        let end = if len == 0 {
            0
        } else {
            (start + height).min(len)
        };
        let mut styled_lines: Vec<Line<'static>> = Vec::with_capacity(end.saturating_sub(start));
        for absolute_idx in start..end {
            let line = &self.lines[absolute_idx];
            let mut span = Span::raw(line.clone());
            if self
                .filter
                .as_ref()
                .map(|f| line.to_lowercase().contains(&f.to_lowercase()))
                .unwrap_or(false)
            {
                span = span.fg(Color::Yellow);
            }
            if self
                .active_bookmark
                .and_then(|b| self.bookmarks.get(b))
                .map(|bookmark| bookmark.line == absolute_idx)
                .unwrap_or(false)
            {
                span = span.bg(Color::Blue);
            }
            styled_lines.push(Line::from(span));
        }
        if styled_lines.is_empty() {
            styled_lines.push(Line::from("No output".dim()));
        }
        Paragraph::new(styled_lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    pub fn info_line(&self) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        if let Some(filter) = &self.filter {
            let total = self.matches.len();
            let current = if total == 0 {
                0
            } else {
                self.active_match.min(total.saturating_sub(1)) + 1
            };
            spans.push(format!("/{filter} ({current}/{total})").into());
        }
        if let Some(idx) = self.active_bookmark.and_then(|i| self.bookmarks.get(i)) {
            if !spans.is_empty() {
                spans.push(" · ".into());
            }
            spans.push(format!("Bookmark: {}", idx.name).into());
        }
        if spans.is_empty() {
            spans.push(Span::from("No filters").dim());
        }
        Line::from(spans)
    }

    fn scroll_to_line(&mut self, line: usize) {
        if self.lines.is_empty() {
            self.scroll = 0;
            return;
        }
        self.scroll = line;
    }

    fn rebuild_matches(&mut self) {
        if let Some(filter) = &self.filter {
            let lower = filter.to_lowercase();
            self.matches = self
                .lines
                .iter()
                .enumerate()
                .filter_map(|(idx, line)| {
                    if line.to_lowercase().contains(&lower) {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect();
        } else {
            self.matches.clear();
        }
        self.active_match = 0;
    }

    fn ensure_scroll_bounds(&mut self) {
        if self.lines.is_empty() {
            self.scroll = 0;
        } else if self.scroll >= self.lines.len() {
            self.scroll = self.lines.len() - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_builds_matches() {
        let mut view = ScrollView::new();
        view.set_lines(vec!["alpha".into(), "beta".into(), "alphabet".into()]);
        view.set_filter(Some("alp".into()));
        assert_eq!(view.matches, vec![0, 2]);
        assert!(view.next_match());
        assert_eq!(view.scroll, 2);
    }

    #[test]
    fn bookmarks_cycle() {
        let mut view = ScrollView::new();
        view.set_lines((0..5).map(|i| format!("line {i}")).collect());
        view.set_bookmarks(vec![
            Bookmark {
                name: "top".into(),
                line: 0,
            },
            Bookmark {
                name: "end".into(),
                line: 4,
            },
        ]);
        assert!(view.cycle_bookmark(true));
        assert_eq!(view.scroll, 0);
        assert!(view.cycle_bookmark(true));
        assert_eq!(view.scroll, 4);
    }
}
