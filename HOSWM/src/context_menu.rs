//! Text context menu presentation and hit testing.
use crate::{desktop::Rect, font::Font, shortcuts::Action, surface::Surface};
pub struct ContextMenu {
    pub window: u32,
    pub control: Option<u32>,
    pub rect: Rect,
    items: Vec<(&'static str, Action, bool)>,
}
impl ContextMenu {
    pub fn new(
        window: u32,
        control: Option<u32>,
        x: i32,
        y: i32,
        selected: bool,
        editable: bool,
        paste: bool,
    ) -> Self {
        Self {
            window,
            control,
            rect: Rect {
                x: x.clamp(0, 800 - 176),
                y: y.clamp(0, 600 - 120),
                w: 176,
                h: 120,
            },
            items: vec![
                ("Copy", Action::Copy, selected),
                ("Cut", Action::Cut, selected && editable),
                ("Paste", Action::Paste, paste),
                ("Delete", Action::Delete, selected && editable),
                ("Select all", Action::SelectAll, true),
            ],
        }
    }
    pub fn hit(&self, x: i32, y: i32) -> Option<Action> {
        if !self.rect.contains(x, y) {
            return None;
        }
        self.items
            .get(((y - self.rect.y) / 24) as usize)
            .and_then(|(_, a, on)| on.then_some(*a))
    }
    pub fn draw(&self, dst: &mut Surface, font: &Font<'_>, x: i32, y: i32) {
        let r = self.rect;
        dst.fill_rect(r.x, r.y, r.w, r.h, 0xff82958b);
        dst.fill_rect(r.x + 1, r.y + 1, r.w - 2, r.h - 2, 0xff202a25);
        for (i, (label, _, enabled)) in self.items.iter().enumerate() {
            let row = Rect {
                x: r.x + 1,
                y: r.y + i as i32 * 24 + 1,
                w: r.w - 2,
                h: 22,
            };
            if *enabled && row.contains(x, y) {
                dst.fill_rect(row.x, row.y, row.w, row.h, 0xff315b83);
            }
            font.draw(
                dst,
                row.x + 8,
                row.y + 6,
                label,
                if *enabled { 0xffeeeeee } else { 0xff78827c },
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn menu_clamps_and_disables_destructive_actions_for_readonly_text() {
        let m = ContextMenu::new(1, None, 799, 599, true, false, true);
        assert_eq!(m.rect.x + m.rect.w, 800);
        assert_eq!(m.rect.y + m.rect.h, 600);
        assert_eq!(m.hit(m.rect.x + 5, m.rect.y + 5), Some(Action::Copy));
        assert_eq!(m.hit(m.rect.x + 5, m.rect.y + 25), None);
        assert_eq!(m.hit(0, 0), None);
    }
}
