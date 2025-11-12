use crate::history_cell::HistoryCell;
use crate::render::renderable::Renderable;
use ratatui::buffer::Buffer;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use std::cell::Cell;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) type HistoryStore = Rc<RefCell<Vec<Arc<dyn HistoryCell>>>>;

pub(crate) struct HistoryPanel {
    store: HistoryStore,
    cache: RefCell<HistoryCache>,
    dirty: Cell<bool>,
}

struct HistoryCache {
    width: u16,
    lines: Vec<Line<'static>>,
}

impl HistoryPanel {
    pub(crate) fn new(store: HistoryStore) -> Self {
        Self {
            store,
            cache: RefCell::new(HistoryCache {
                width: 0,
                lines: Vec::new(),
            }),
            dirty: Cell::new(true),
        }
    }

    pub(crate) fn mark_dirty(&self) {
        self.dirty.set(true);
    }

    pub(crate) fn reset(&self) {
        self.mark_dirty();
    }

    fn rebuild_lines(&self, width: u16) {
        let cells = self.store.borrow();
        let mut lines = Vec::new();
        for cell in cells.iter() {
            let mut display = cell.display_lines(width);
            if display.is_empty() {
                continue;
            }
            if !cell.is_stream_continuation() && !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.append(&mut display);
        }
        let mut cache = self.cache.borrow_mut();
        cache.width = width;
        cache.lines = lines;
        self.dirty.set(false);
    }

    fn cached_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.dirty.get() || self.cache.borrow().width != width {
            self.rebuild_lines(width);
        }
        self.cache.borrow().lines.clone()
    }
}

impl Renderable for HistoryPanel {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let lines = self.cached_lines(area.width);
        let total = lines.len();
        let height = area.height as usize;
        let start = total.saturating_sub(height);
        let view = lines[start..].to_vec();
        Paragraph::new(Text::from(view))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        0
    }

    fn cursor_pos(&self, _area: Rect) -> Option<(u16, u16)> {
        None
    }
}
