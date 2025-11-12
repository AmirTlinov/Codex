use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub trait VirtualListAdapter {
    fn len(&self) -> usize;
    fn item_height(&self, index: usize, width: u16) -> u16;
    fn render_item(&self, index: usize, area: Rect, buf: &mut Buffer, selected: bool);
}

#[derive(Default)]
pub struct VirtualListState {
    first_visible: usize,
}

impl VirtualListState {
    pub fn new() -> Self {
        Self { first_visible: 0 }
    }

    pub fn ensure_visible<A: VirtualListAdapter>(
        &mut self,
        adapter: &A,
        selected: usize,
        area: Rect,
    ) {
        if area.height == 0 || adapter.len() == 0 {
            self.first_visible = 0;
            return;
        }
        let max_index = adapter.len().saturating_sub(1);
        let selected = selected.min(max_index);
        if self.first_visible > max_index {
            self.first_visible = max_index;
        }
        if selected < self.first_visible {
            self.first_visible = selected;
        }
        let height = area.height;
        let width = area.width;
        // Shift window up until selected fits.
        loop {
            if self.window_contains(adapter, selected, width, height) {
                break;
            }
            if self.first_visible == 0 {
                break;
            }
            self.first_visible -= 1;
        }
        // If selected is still below window, scroll down just enough.
        while !self.window_contains(adapter, selected, width, height)
            && self.first_visible < max_index
        {
            self.first_visible += 1;
        }
    }

    pub fn render<A: VirtualListAdapter>(
        &mut self,
        adapter: &A,
        selected: usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        self.ensure_visible(adapter, selected, area);
        let mut y = area.y;
        let mut index = self.first_visible;
        while index < adapter.len() && y < area.bottom() {
            let item_height = adapter.item_height(index, area.width).max(1);
            let draw_height = item_height.min(area.bottom().saturating_sub(y));
            let rect = Rect::new(area.x, y, area.width, draw_height);
            adapter.render_item(index, rect, buf, index == selected);
            y = y.saturating_add(draw_height);
            index += 1;
        }
        while y < area.bottom() {
            for x in area.x..area.right() {
                buf[(x, y)].reset();
            }
            y += 1;
        }
    }

    pub fn page_step<A: VirtualListAdapter>(&mut self, adapter: &A, area: Rect) -> usize {
        if area.height == 0 {
            return 1;
        }
        let mut used = 0;
        let mut count = 0;
        let width = area.width;
        let len = adapter.len();
        let mut idx = self.first_visible;
        while idx < len && used < area.height {
            let h = adapter.item_height(idx, width).max(1);
            used += h;
            if used > area.height {
                break;
            }
            count += 1;
            idx += 1;
        }
        count.max(1)
    }

    fn window_contains<A: VirtualListAdapter>(
        &self,
        adapter: &A,
        target: usize,
        width: u16,
        height: u16,
    ) -> bool {
        if adapter.len() == 0 || height == 0 {
            return false;
        }
        let mut used = 0;
        let mut idx = self.first_visible;
        while idx < adapter.len() && used < height {
            let h = adapter.item_height(idx, width).max(1);
            used += h;
            if idx == target {
                return used <= height || idx == self.first_visible;
            }
            idx += 1;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestAdapter(Vec<u16>);

    impl VirtualListAdapter for TestAdapter {
        fn len(&self) -> usize {
            self.0.len()
        }

        fn item_height(&self, index: usize, _width: u16) -> u16 {
            self.0[index]
        }

        fn render_item(&self, _index: usize, _area: Rect, _buf: &mut Buffer, _selected: bool) {}
    }

    #[test]
    fn selected_stays_visible() {
        let adapter = TestAdapter(vec![1, 1, 1, 1, 1]);
        let mut state = VirtualListState::new();
        let area = Rect::new(0, 0, 10, 3);
        state.ensure_visible(&adapter, 4, area);
        assert!(state.first_visible >= 2);
        state.ensure_visible(&adapter, 0, area);
        assert_eq!(state.first_visible, 0);
    }

    #[test]
    fn page_step_counts_items() {
        let adapter = TestAdapter(vec![1, 2, 3, 4]);
        let mut state = VirtualListState::new();
        let area = Rect::new(0, 0, 10, 4);
        let step = state.page_step(&adapter, area);
        assert_eq!(step, 2);
    }
}
