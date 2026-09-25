//! Retained text selection and shared drawing/hit-test layout (UTF-8 byte offsets).
use crate::{font::Font, surface::Surface};
use std::ops::Range;
pub const SELECTION_COLOR: u32 = 0xff315b83;
#[derive(Default, Clone, Debug)]
pub struct TextSelection {
    pub selectable: bool,
    pub anchor: usize,
    pub caret: usize,
}
impl TextSelection {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }
    pub fn reset(&mut self, text: &str) {
        self.anchor = text.len();
        self.caret = text.len();
    }
    pub fn selected<'a>(&self, text: &'a str) -> &'a str {
        if self.selectable {
            text.get(self.range()).unwrap_or("")
        } else {
            ""
        }
    }
    pub fn replace(&mut self, text: &mut String, value: &str) -> bool {
        let range = if self.selectable {
            self.range()
        } else {
            text.len()..text.len()
        };
        if text.len() - range.len() + value.len() > 1024 || text.get(range.clone()).is_none() {
            return false;
        }
        let old = text.clone();
        let end = range.start + value.len();
        text.replace_range(range, value);
        self.anchor = end;
        self.caret = end;
        *text != old
    }
}
// Each row retains its original byte range, including explicit line breaks.
pub struct TextLayout {
    rows: Vec<(usize, usize)>,
    x: i32,
    y: i32,
}
impl TextLayout {
    pub fn new(text: &str, width: i32, height: i32, label: bool, caret: usize) -> Self {
        if !label {
            let cap = ((width - 12) / 8).max(1) as usize;
            let cursor = text[..caret.min(text.len())].chars().count();
            let start_char = cursor.saturating_sub(cap);
            let start = text
                .char_indices()
                .nth(start_char)
                .map_or(text.len(), |(i, _)| i);
            let end = text[start..]
                .char_indices()
                .nth(cap)
                .map_or(text.len(), |(i, _)| start + i);
            return Self {
                rows: vec![(start, end)],
                x: 6,
                y: (height - 9) / 2,
            };
        }
        let cap = (width / 8).max(1) as usize;
        let mut rows = Vec::new();
        let (mut start, mut col) = (0, 0);
        for (i, ch) in text.char_indices() {
            if ch == '\n' {
                rows.push((start, i));
                start = i + 1;
                col = 0;
            } else {
                if col == cap {
                    rows.push((start, i));
                    start = i;
                    col = 0;
                }
                col += 1;
            }
        }
        rows.push((start, text.len()));
        Self { rows, x: 0, y: 0 }
    }
    pub fn hit(&self, text: &str, x: i32, y: i32) -> usize {
        let row = ((y - self.y).max(0) / 12) as usize;
        let &(start, end) = &self.rows[row.min(self.rows.len() - 1)];
        let col = ((x - self.x + 4).max(0) / 8) as usize;
        text[start..end]
            .char_indices()
            .nth(col)
            .map_or(end, |(i, _)| start + i)
    }
    pub fn draw(
        &self,
        text: &str,
        selection: &TextSelection,
        dst: &mut Surface,
        font: &Font<'_>,
        caret: bool,
    ) {
        let range = selection.range();
        for (row, &(start, end)) in self.rows.iter().enumerate() {
            let y = self.y + row as i32 * 12;
            if y >= dst.height() as i32 {
                break;
            }
            let mut x = self.x;
            for (i, ch) in text[start..end].char_indices() {
                if selection.selectable && range.contains(&(start + i)) {
                    dst.fill_rect(x, y, 8, 12, SELECTION_COLOR);
                }
                font.draw(dst, x, y, &ch.to_string(), 0xffdedede);
                x += 8;
            }
            if caret && (start..=end).contains(&selection.caret) {
                let x = self.x + text[start..selection.caret].chars().count() as i32 * 8;
                dst.fill_rect(x.min(dst.width() as i32 - 3), y, 1, 10, 0xff72dbac);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utf8_selection_and_wrapped_hit_testing() {
        let mut text = "é界abc".to_string();
        let mut selection = TextSelection {
            selectable: true,
            anchor: 5,
            caret: 0,
        };
        assert_eq!(selection.selected(&text), "é界");
        assert!(selection.replace(&mut text, "Q"));
        assert_eq!(text, "Qabc");
        let layout = TextLayout::new("ab\n界d", 16, 40, true, 0);
        assert_eq!(layout.hit("ab\n界d", 8, 12), 6);
        assert_eq!(layout.hit("ab\n界d", 100, 100), 7);
        assert!(!TextSelection::default().selectable);
    }
}
