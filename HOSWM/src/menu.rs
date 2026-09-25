//! Window menus and the screen-top menu bar that presents the focused window.
//!
//! Menus are retained per window and drawn by the window manager, so every
//! application shows the same bar in the same place, as on classic desktops.
use crate::{desktop::Rect, font::Font, surface::Surface};

/// Height of the bar reserved at the top of the screen.
pub const HEIGHT: i32 = 24;
/// Advance of the bundled fixed-width font.
const CHAR: i32 = 8;
const ROW: i32 = 22;
const RULE: i32 = 7;
const BAR: u32 = 0xff141a18;
const EDGE: u32 = 0xff303936;
const PANEL: u32 = 0xff202a25;
const HIGHLIGHT: u32 = 0xff315b83;
const LABEL: u32 = 0xffeeeeee;
const DISABLED: u32 = 0xff78827c;
const DIM: u32 = 0xff9aa69f;

pub const ITEM_DISABLED: u32 = 1;
pub const ITEM_SEPARATOR: u32 = 2;
pub const ITEM_CHECKED: u32 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItem {
    pub id: u32,
    pub label: String,
    /// Display-only shortcut hint, right-aligned in the drop-down.
    pub shortcut: String,
    pub enabled: bool,
    pub separator: bool,
    pub checked: bool,
}
impl MenuItem {
    pub fn new(id: u32, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            shortcut: String::new(),
            enabled: true,
            separator: false,
            checked: false,
        }
    }
    /// A horizontal rule. Separators are never reported as activated.
    pub fn rule() -> Self {
        Self {
            separator: true,
            ..Self::new(0, "")
        }
    }
    pub fn shortcut(mut self, hint: impl Into<String>) -> Self {
        self.shortcut = hint.into();
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
    pub fn flags(&self) -> u32 {
        u32::from(!self.enabled)
            | (u32::from(self.separator) << 1)
            | (u32::from(self.checked) << 2)
    }
    pub fn from_flags(id: u32, flags: u32, label: String, shortcut: String) -> Self {
        Self {
            id,
            label,
            shortcut,
            enabled: flags & ITEM_DISABLED == 0,
            separator: flags & ITEM_SEPARATOR != 0,
            checked: flags & ITEM_CHECKED != 0,
        }
    }
    fn height(&self) -> i32 {
        if self.separator { RULE } else { ROW }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Menu {
    pub title: String,
    pub items: Vec<MenuItem>,
}
impl Menu {
    pub fn new(title: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self {
            title: title.into(),
            items,
        }
    }
    fn width(&self) -> i32 {
        let widest = self
            .items
            .iter()
            .map(|i| {
                (i.label.chars().count() + i.shortcut.chars().count()) as i32 * CHAR
                    + if i.shortcut.is_empty() { 0 } else { 24 }
            })
            .max()
            .unwrap_or(0);
        (widest + 40).clamp(140, 320)
    }
    fn height(&self) -> i32 {
        self.items.iter().map(MenuItem::height).sum::<i32>() + 8
    }
    /// Drop-down placement below a bar title, kept inside the screen.
    pub fn rect(&self, title_x: i32, screen: i32) -> Rect {
        let w = self.width();
        Rect {
            x: title_x.clamp(0, (screen - w).max(0)),
            y: HEIGHT,
            w,
            h: self.height(),
        }
    }
    /// Placement for a context menu at a point inside an application window,
    /// kept within its bounds. Hit testing and drawing are shared with the bar.
    pub fn rect_at(&self, x: i32, y: i32, width: i32, height: i32) -> Rect {
        let (w, h) = (self.width(), self.height());
        Rect {
            x: x.clamp(0, (width - w).max(0)),
            y: y.clamp(0, (height - h).max(0)),
            w,
            h,
        }
    }
    /// Index of the activatable item at a point, if any.
    pub fn hit(&self, rect: Rect, x: i32, y: i32) -> Option<usize> {
        if !rect.contains(x, y) {
            return None;
        }
        let mut top = rect.y + 4;
        for (index, item) in self.items.iter().enumerate() {
            let bottom = top + item.height();
            if y >= top && y < bottom {
                return (item.enabled && !item.separator).then_some(index);
            }
            top = bottom;
        }
        None
    }
    pub fn draw(&self, fb: &mut Surface, font: &Font<'_>, rect: Rect, x: i32, y: i32) {
        fb.fill_rect(rect.x, rect.y, rect.w, rect.h, 0xff82958b);
        fb.fill_rect(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2, PANEL);
        let mut top = rect.y + 4;
        for item in &self.items {
            if item.separator {
                fb.fill_rect(rect.x + 8, top + 3, rect.w - 16, 1, EDGE);
                top += item.height();
                continue;
            }
            let hovered = item.enabled && rect.contains(x, y) && y >= top && y < top + ROW;
            if hovered {
                fb.fill_rect(rect.x + 2, top, rect.w - 4, ROW, HIGHLIGHT);
            }
            let color = if item.enabled { LABEL } else { DISABLED };
            if item.checked {
                font.draw(fb, rect.x + 8, top + 6, "*", color);
            }
            let room = ((rect.w - 36) / CHAR).max(0) as usize;
            let label: String = item.label.chars().take(room).collect();
            font.draw(fb, rect.x + 22, top + 6, &label, color);
            if !item.shortcut.is_empty() {
                let width = item.shortcut.chars().count() as i32 * CHAR;
                font.draw(
                    fb,
                    rect.x + rect.w - 10 - width,
                    top + 6,
                    &item.shortcut,
                    if item.enabled { DIM } else { DISABLED },
                );
            }
            top += ROW;
        }
    }
}

/// Bar title rectangles, left to right, following the application name.
pub fn title_rects(app: &str, menus: &[Menu]) -> Vec<Rect> {
    let mut x = 10 + app.chars().count() as i32 * CHAR + 18;
    menus
        .iter()
        .map(|menu| {
            let w = menu.title.chars().count() as i32 * CHAR + 16;
            let rect = Rect {
                x,
                y: 0,
                w,
                h: HEIGHT,
            };
            x += w;
            rect
        })
        .collect()
}

/// Index of the bar title under a point, if the point is on the bar.
pub fn title_at(app: &str, menus: &[Menu], x: i32, y: i32) -> Option<usize> {
    if y >= HEIGHT {
        return None;
    }
    title_rects(app, menus)
        .into_iter()
        .position(|r| r.contains(x, y))
}

/// Draw the bar itself; the open drop-down is drawn separately, above windows.
pub fn draw_bar(
    fb: &mut Surface,
    font: &Font<'_>,
    app: &str,
    accent: u32,
    menus: &[Menu],
    open: Option<usize>,
    pointer: (i32, i32),
    clock: &str,
) {
    let width = fb.width() as i32;
    fb.fill_rect(0, 0, width, HEIGHT, BAR);
    fb.fill_rect(0, HEIGHT - 1, width, 1, EDGE);
    font.draw(fb, 10, 8, app, accent);
    for (index, rect) in title_rects(app, menus).into_iter().enumerate() {
        let active = open == Some(index);
        if active || (open.is_none() && rect.contains(pointer.0, pointer.1)) {
            fb.fill_rect(rect.x, 0, rect.w, HEIGHT - 1, if active { HIGHLIGHT } else { EDGE });
        }
        font.draw(fb, rect.x + 8, 8, &menus[index].title, LABEL);
    }
    if !clock.is_empty() {
        let x = width - 10 - clock.chars().count() as i32 * CHAR;
        font.draw(fb, x, 8, clock, DIM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Vec<Menu> {
        vec![
            Menu::new(
                "File",
                vec![
                    MenuItem::new(1, "New").shortcut("Ctrl+N"),
                    MenuItem::rule(),
                    MenuItem::new(2, "Close").enabled(false),
                ],
            ),
            Menu::new("Edit", vec![MenuItem::new(3, "Copy").checked(true)]),
        ]
    }
    #[test]
    fn bar_titles_follow_the_application_name() {
        let menus = sample();
        let rects = title_rects("Terminal", &menus);
        assert_eq!(rects[0].x, 10 + 8 * 8 + 18);
        assert_eq!(rects[0].w, 4 * 8 + 16);
        assert_eq!(rects[1].x, rects[0].x + rects[0].w);
        assert_eq!(title_at("Terminal", &menus, rects[1].x + 4, 4), Some(1));
        assert_eq!(title_at("Terminal", &menus, rects[1].x + 4, HEIGHT), None);
        assert_eq!(title_at("Terminal", &menus, 0, 4), None);
    }
    #[test]
    fn drop_downs_stay_on_screen_and_skip_rules_and_disabled_items() {
        let menus = sample();
        let rect = menus[0].rect(700, 800);
        assert_eq!(rect.x + rect.w, 800);
        assert_eq!(rect.y, HEIGHT);
        assert_eq!(rect.h, ROW * 2 + RULE + 8);
        assert_eq!(menus[0].hit(rect, rect.x + 4, rect.y + 6), Some(0));
        assert_eq!(menus[0].hit(rect, rect.x + 4, rect.y + 4 + ROW + 2), None);
        assert_eq!(
            menus[0].hit(rect, rect.x + 4, rect.y + 4 + ROW + RULE + 2),
            None,
            "disabled items are not activatable"
        );
        assert_eq!(menus[0].hit(rect, rect.x - 1, rect.y + 6), None);
    }
    #[test]
    fn item_flags_round_trip() {
        let item = MenuItem::new(7, "Paste")
            .shortcut("Ctrl+V")
            .enabled(false)
            .checked(true);
        assert_eq!(item.flags(), ITEM_DISABLED | ITEM_CHECKED);
        assert_eq!(
            MenuItem::from_flags(7, item.flags(), "Paste".into(), "Ctrl+V".into()),
            item
        );
        assert!(MenuItem::rule().separator);
    }
    #[test]
    fn draws_bar_and_menu_without_leaving_the_surface() {
        let mut fb = Surface::new(800, 600);
        let menus = sample();
        draw_bar(
            &mut fb,
            &Font::builtin(),
            "Terminal",
            0xff72dbac,
            &menus,
            Some(0),
            (120, 4),
            "12:30",
        );
        let rect = menus[0].rect(120, 800);
        menus[0].draw(&mut fb, &Font::builtin(), rect, rect.x + 4, rect.y + 6);
        assert!(fb.pixels().iter().any(|p| *p == HIGHLIGHT));
        assert_eq!(fb.pixels()[799 * 600 - 1], 0, "nothing drawn below the menu");
    }
}
