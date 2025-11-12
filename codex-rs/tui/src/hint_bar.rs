use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

#[derive(Clone, Debug, Default)]
pub struct HintBar {
    lines: Vec<Line<'static>>,
}

impl HintBar {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || self.lines.is_empty() {
            return;
        }
        let mut y = area.y;
        for line in self.lines.iter().take(area.height as usize) {
            Paragraph::new(line.clone())
                .style(Style::default().fg(Color::White))
                .wrap(Wrap { trim: true })
                .render(Rect::new(area.x, y, area.width, 1), buf);
            y = y.saturating_add(1);
            if y >= area.bottom() {
                break;
            }
        }
    }
}
