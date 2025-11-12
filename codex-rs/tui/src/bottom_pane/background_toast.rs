use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

use crate::render::renderable::Renderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::WidgetRef;

const TOAST_TTL: Duration = Duration::from_secs(4);
const MAX_TOASTS: usize = 2;

pub(crate) struct BackgroundToastQueue {
    entries: RefCell<VecDeque<ToastEntry>>,
}

struct ToastEntry {
    message: String,
    expires_at: Instant,
}

impl BackgroundToastQueue {
    pub fn new() -> Self {
        Self {
            entries: RefCell::new(VecDeque::new()),
        }
    }

    pub fn push(&self, message: String) {
        self.prune_expired();
        let mut entries = self.entries.borrow_mut();
        entries.push_back(ToastEntry {
            message,
            expires_at: Instant::now() + TOAST_TTL,
        });
        while entries.len() > MAX_TOASTS {
            entries.pop_front();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }

    pub fn next_expiry(&self) -> Option<Duration> {
        self.entries
            .borrow()
            .front()
            .and_then(|toast| toast.expires_at.checked_duration_since(Instant::now()))
    }

    fn prune_expired(&self) {
        let now = Instant::now();
        let mut entries = self.entries.borrow_mut();
        while matches!(entries.front(), Some(entry) if entry.expires_at <= now) {
            entries.pop_front();
        }
    }
}

impl Renderable for BackgroundToastQueue {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.prune_expired();
        let entries = self.entries.borrow();
        if area.is_empty() || entries.is_empty() {
            return;
        }
        let mut y = area.y;
        for entry in entries.iter() {
            if y >= area.y + area.height {
                break;
            }
            let line = Line::from(entry.message.clone()).style(Style::default().fg(Color::Yellow));
            line.render_ref(Rect::new(area.x, y, area.width, 1), buf);
            y += 1;
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.entries.borrow().len() as u16
    }
}
