//! `hos-colorpicker`: choose a color in a window and print it.
//!
//! ```text
//! hos-colorpicker [COLOR]    # 0xAARRGGBB, #RRGGBB or a decimal integer
//! ```
//!
//! The chosen color is written to standard output as `0xAARRGGBB` and the
//! program exits with status 0. Cancelling prints nothing and exits with 1,
//! so a caller can tell "black" from "no answer". `hos-paint` uses it that
//! way; from a shell, `FG=$(hos-colorpicker 0xff72dbac)` does the same.
use hoswm::{
    client::{Client, Event, Menu, MenuItem, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT},
    desktop::Rect,
    font::Font,
    surface::Surface,
};
use std::{thread, time::Duration};

const BACKGROUND: u32 = 0xff11201b;
const PANEL: u32 = 0xff1b2b25;
const TEXT: u32 = 0xffdedede;
const DIM: u32 = 0xff8d9a93;
const ACCENT: u32 = 0xff72dbac;
const WIDTH: i32 = 420;
const HEIGHT: i32 = 260;

const MENU_USE: u32 = 1;
const MENU_CANCEL: u32 = 2;

/// The saturation/value square, for the hue the strip beside it selects.
const SQUARE: Rect = Rect {
    x: 12,
    y: 12,
    w: 200,
    h: 200,
};
const HUES: Rect = Rect {
    x: 220,
    y: 12,
    w: 24,
    h: 200,
};
const PREVIEW: Rect = Rect {
    x: 258,
    y: 12,
    w: 150,
    h: 56,
};
/// The editable `#RRGGBB` field.
const FIELD: Rect = Rect {
    x: 258,
    y: 74,
    w: 150,
    h: 22,
};
const USE: Rect = Rect {
    x: 258,
    y: 180,
    w: 72,
    h: 28,
};
const CANCEL: Rect = Rect {
    x: 336,
    y: 180,
    w: 72,
    h: 28,
};
/// Ready-made colors, in the grid under the preview.
const PRESETS: [u32; 15] = [
    0xff000000, 0xff404040, 0xff808080, 0xffc0c0c0, 0xffffffff, 0xffef6976, 0xffe4c878, 0xfff5f07a,
    0xff72dbac, 0xff9ccfd8, 0xff80afff, 0xffc4a7e7, 0xff8b4513, 0xff2e7d32, 0xff1a237e,
];
fn preset_rect(index: usize) -> Rect {
    Rect {
        x: 258 + (index % 5) as i32 * 30,
        y: 104 + (index / 5) as i32 * 24,
        w: 28,
        h: 22,
    }
}

/// Hue in degrees, saturation and value in 0..=255, to `0xffRRGGBB`.
fn hsv_to_rgb(hue: u32, saturation: u32, value: u32) -> u32 {
    let (hue, saturation, value) = (hue % 360, saturation.min(255), value.min(255));
    let sector = hue / 60;
    let offset = (hue % 60) * 255 / 60;
    let p = value * (255 - saturation) / 255;
    let q = value * (255 - saturation * offset / 255) / 255;
    let t = value * (255 - saturation * (255 - offset) / 255) / 255;
    let (r, g, b) = match sector {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };
    0xff00_0000 | (r << 16) | (g << 8) | b
}
/// The inverse, for a color that arrived as an argument or from a preset.
fn rgb_to_hsv(color: u32) -> (u32, u32, u32) {
    let (r, g, b) = (
        (color >> 16) as i32 & 0xff,
        (color >> 8) as i32 & 0xff,
        color as i32 & 0xff,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let span = max - min;
    let hue = if span == 0 {
        0
    } else if max == r {
        (360 + (g - b) * 60 / span) % 360
    } else if max == g {
        120 + (b - r) * 60 / span
    } else {
        240 + (r - g) * 60 / span
    };
    let saturation = if max == 0 { 0 } else { span * 255 / max };
    (hue as u32, saturation as u32, max as u32)
}
/// Accept `0xAARRGGBB`, `#RRGGBB`, `RRGGBB` and plain decimal integers, so a
/// caller can hand back whatever this program printed, or an `int` it holds.
pub fn parse_color(text: &str) -> Option<u32> {
    let text = text.trim();
    if let Some(color) = hoswm::config::color(text) {
        return Some(color);
    }
    if text.len() == 6 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return u32::from_str_radix(text, 16).ok().map(|c| c | 0xff00_0000);
    }
    text.parse::<u32>().ok().map(|c| c | 0xff00_0000)
}

/// What the user is dragging, so motion keeps following the same control.
#[derive(Clone, Copy, PartialEq)]
enum Grab {
    Square,
    Hues,
}
pub struct Picker {
    /// The answer itself. It is kept exactly as a preset, a typed value or the
    /// starting argument gave it: converting through hue/saturation/value and
    /// back rounds, and a caller that passes a color in and presses Use must
    /// get that same color out.
    color: u32,
    hue: u32,
    saturation: u32,
    value: u32,
    grab: Option<Grab>,
    /// Digits typed into the hex field; `None` while it is not being edited.
    editing: Option<String>,
    /// Set once the answer is known: the color to print, or nothing.
    pub done: Option<Option<u32>>,
}
impl Picker {
    pub fn new(color: u32) -> Self {
        let (hue, saturation, value) = rgb_to_hsv(color);
        Self {
            color,
            hue,
            saturation,
            value,
            grab: None,
            editing: None,
            done: None,
        }
    }
    pub fn color(&self) -> u32 {
        self.color
    }
    fn set_color(&mut self, color: u32) {
        let (hue, saturation, value) = rgb_to_hsv(color);
        // A grey has no hue of its own; keep the one the strip is showing.
        self.hue = if saturation == 0 { self.hue } else { hue };
        self.saturation = saturation;
        self.value = value;
        self.color = color | 0xff00_0000;
    }
    /// After the square or the strip moved, the color follows them.
    fn from_hsv(&mut self) {
        self.color = hsv_to_rgb(self.hue, self.saturation, self.value);
    }
    fn hex(&self) -> String {
        match &self.editing {
            Some(digits) => format!("#{digits}"),
            None => format!("#{:06x}", self.color() & 0xff_ffff),
        }
    }
    /// Finish editing the hex field, keeping the typed value when it is a
    /// complete color and dropping it otherwise.
    fn commit(&mut self) {
        if let Some(digits) = self.editing.take() {
            if digits.len() == 6 {
                if let Ok(color) = u32::from_str_radix(&digits, 16) {
                    self.set_color(color | 0xff00_0000);
                }
            }
        }
    }
    pub fn press(&mut self, x: i32, y: i32) {
        if !FIELD.contains(x, y) {
            self.commit();
        }
        if SQUARE.contains(x, y) {
            self.grab = Some(Grab::Square);
            self.drag(x, y);
        } else if HUES.contains(x, y) {
            self.grab = Some(Grab::Hues);
            self.drag(x, y);
        } else if FIELD.contains(x, y) {
            self.editing = Some(String::new());
        } else if USE.contains(x, y) {
            self.done = Some(Some(self.color()));
        } else if CANCEL.contains(x, y) {
            self.done = Some(None);
        } else if let Some(color) = PRESETS
            .iter()
            .enumerate()
            .find(|(index, _)| preset_rect(*index).contains(x, y))
            .map(|(_, color)| *color)
        {
            self.set_color(color);
        }
    }
    pub fn drag(&mut self, x: i32, y: i32) {
        match self.grab {
            Some(Grab::Square) => {
                self.saturation =
                    ((x - SQUARE.x).clamp(0, SQUARE.w - 1) * 255 / (SQUARE.w - 1)) as u32;
                self.value =
                    (255 - (y - SQUARE.y).clamp(0, SQUARE.h - 1) * 255 / (SQUARE.h - 1)) as u32;
                self.from_hsv();
            }
            Some(Grab::Hues) => {
                self.hue = ((y - HUES.y).clamp(0, HUES.h - 1) * 359 / (HUES.h - 1)) as u32;
                self.from_hsv();
            }
            None => (),
        }
    }
    pub fn release(&mut self) {
        self.grab = None;
    }
    /// Keyboard: Enter accepts, Escape cancels, and hex digits edit the field
    /// while it has the caret.
    pub fn key(&mut self, text: &str) {
        match text {
            "\r" | "\n" => {
                self.commit();
                self.done = Some(Some(self.color()));
            }
            "\u{1b}" => {
                if self.editing.take().is_none() {
                    self.done = Some(None);
                }
            }
            "\u{7f}" | "\u{8}" => {
                if let Some(digits) = &mut self.editing {
                    digits.pop();
                }
            }
            _ => {
                let Some(digits) = &mut self.editing else {
                    return;
                };
                for c in text.chars().filter(char::is_ascii_hexdigit) {
                    if digits.len() == 6 {
                        break;
                    }
                    digits.push(c.to_ascii_lowercase());
                }
                if digits.len() == 6 {
                    self.commit();
                }
            }
        }
    }
    pub fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        surface.pixels_mut().fill(BACKGROUND);
        // Saturation left to right, value bottom to top, for the current hue.
        for row in 0..SQUARE.h {
            let value = (255 - row * 255 / (SQUARE.h - 1)) as u32;
            for column in 0..SQUARE.w {
                let saturation = (column * 255 / (SQUARE.w - 1)) as u32;
                surface.set_pixel(
                    SQUARE.x + column,
                    SQUARE.y + row,
                    hsv_to_rgb(self.hue, saturation, value),
                );
            }
        }
        for row in 0..HUES.h {
            let hue = (row * 359 / (HUES.h - 1)) as u32;
            surface.fill_rect(HUES.x, HUES.y + row, HUES.w, 1, hsv_to_rgb(hue, 255, 255));
        }
        // Crosshair and hue marker, outlined so they show on any color.
        let cx = SQUARE.x + (self.saturation as i32 * (SQUARE.w - 1) / 255);
        let cy = SQUARE.y + ((255 - self.value as i32) * (SQUARE.h - 1) / 255);
        for (radius, color) in [(5, 0xff000000), (4, 0xffffffff)] {
            surface.draw_line(cx - radius, cy, cx + radius, cy, color);
            surface.draw_line(cx, cy - radius, cx, cy + radius, color);
        }
        let marker = HUES.y + (self.hue as i32 * (HUES.h - 1) / 359);
        surface.fill_rect(HUES.x - 3, marker - 1, HUES.w + 6, 3, 0xff000000);
        surface.fill_rect(HUES.x - 2, marker, HUES.w + 4, 1, 0xffffffff);

        let color = self.color();
        surface.fill_rect(PREVIEW.x, PREVIEW.y, PREVIEW.w, PREVIEW.h, 0xff000000);
        surface.fill_rect(
            PREVIEW.x + 1,
            PREVIEW.y + 1,
            PREVIEW.w - 2,
            PREVIEW.h - 2,
            color,
        );
        // The value this program would print, in the form it prints it.
        font.draw(
            surface,
            PREVIEW.x + 6,
            PREVIEW.y + PREVIEW.h - 16,
            &format!("0x{color:08x}"),
            if self.value > 140 {
                0xff101010
            } else {
                0xffe8e8e8
            },
        );
        let editing = self.editing.is_some();
        surface.fill_rect(
            FIELD.x,
            FIELD.y,
            FIELD.w,
            FIELD.h,
            if editing { 0xffffffff } else { PANEL },
        );
        surface.fill_rect(FIELD.x + 1, FIELD.y + 1, FIELD.w - 2, FIELD.h - 2, 0xff0b0f0e);
        let hex = self.hex();
        font.draw(surface, FIELD.x + 8, FIELD.y + 6, &hex, TEXT);
        if editing {
            let caret = FIELD.x + 8 + hex.chars().count() as i32 * 8;
            surface.fill_rect(caret, FIELD.y + 5, 1, 12, ACCENT);
        }
        for (index, preset) in PRESETS.iter().enumerate() {
            let r = preset_rect(index);
            let chosen = *preset == color;
            surface.fill_rect(
                r.x,
                r.y,
                r.w,
                r.h,
                if chosen { 0xffffffff } else { 0xff000000 },
            );
            surface.fill_rect(r.x + 1, r.y + 1, r.w - 2, r.h - 2, *preset);
        }
        for (r, label, primary) in [(USE, "Use", true), (CANCEL, "Cancel", false)] {
            surface.fill_rect(r.x, r.y, r.w, r.h, if primary { ACCENT } else { PANEL });
            surface.fill_rect(
                r.x + 1,
                r.y + 1,
                r.w - 2,
                r.h - 2,
                if primary { 0xff15201d } else { 0xff0b0f0e },
            );
            font.draw(
                surface,
                r.x + (r.w - label.len() as i32 * 8) / 2,
                r.y + 9,
                label,
                if primary { ACCENT } else { TEXT },
            );
        }
        let (red, green, blue) = (color >> 16 & 0xff, color >> 8 & 0xff, color & 0xff);
        font.draw(
            surface,
            SQUARE.x,
            SQUARE.y + SQUARE.h + 10,
            &format!("R {red:<4}G {green:<4}B {blue:<4}H {:<4}S {:<4}V {}", self.hue, self.saturation, self.value),
            TEXT,
        );
        font.draw(
            surface,
            SQUARE.x,
            SQUARE.y + SQUARE.h + 26,
            "Drag the square and the hue bar, or type a hex value. Enter accepts.",
            DIM,
        );
    }
}

fn menus() -> Vec<Menu> {
    vec![Menu::new(
        "Color",
        vec![
            MenuItem::new(MENU_USE, "Use this color").shortcut("Enter"),
            MenuItem::rule(),
            MenuItem::new(MENU_CANCEL, "Cancel").shortcut("Esc"),
        ],
    )]
}

fn run() -> Result<(), String> {
    let mut start = 0xff72_dbac;
    for argument in std::env::args().skip(1) {
        if argument == "--help" || argument == "-h" {
            println!("Usage: hos-colorpicker [COLOR]   # 0xAARRGGBB, #RRGGBB or an integer");
            return Ok(());
        }
        start = parse_color(&argument).ok_or(format!("{argument}: not a color"))?;
    }
    let client = Client::connect().map_err(|e| e.to_string())?;
    let window = client
        .create("Choose a color", WIDTH as u32, HEIGHT as u32, ACCENT)
        .map_err(|e| e.to_string())?;
    client
        .flags(window, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)
        .map_err(|e| e.to_string())?;
    client.set_menus(window, &menus()).map_err(|e| e.to_string())?;
    let font = Font::builtin();
    let mut surface = Surface::new(WIDTH as usize, HEIGHT as usize);
    let mut picker = Picker::new(start);
    let mut dirty = true;
    loop {
        let (_, _, minimized) = client.size(window).map_err(|e| e.to_string())?;
        for _ in 0..32 {
            let Some(Event {
                kind,
                control,
                text,
            }) = client.poll(window).map_err(|e| e.to_string())?
            else {
                break;
            };
            dirty = true;
            match kind {
                6 => picker.key(&text),
                8 => {
                    let mut parts = text.split_whitespace();
                    let x = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let action = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    match (control, action) {
                        (1, 1) => picker.press(x, y),
                        (1, 0) => picker.release(),
                        (0, _) => picker.drag(x, y),
                        _ => (),
                    }
                }
                11 => match control {
                    MENU_USE => picker.done = Some(Some(picker.color())),
                    MENU_CANCEL => picker.done = Some(None),
                    _ => (),
                },
                7 | 9 => picker.done = Some(None),
                _ => (),
            }
        }
        if let Some(answer) = picker.done {
            let _ = client.close(window);
            return match answer {
                // The answer goes to standard output; the exit status says
                // whether there is one at all.
                Some(color) => {
                    println!("0x{color:08x}");
                    Ok(())
                }
                None => std::process::exit(1),
            };
        }
        if dirty && !minimized {
            picker.draw(&mut surface, &font);
            client
                .present(window, WIDTH as u32, HEIGHT as u32, surface.pixels())
                .map_err(|e| e.to_string())?;
            dirty = false;
        }
        thread::sleep(Duration::from_millis(if minimized { 120 } else { 16 }));
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-colorpicker: {e}");
        std::process::exit(2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hue_saturation_and_value_survive_the_round_trip() {
        for color in [
            0xff000000, 0xffffffff, 0xff808080, 0xffff0000, 0xff00ff00, 0xff0000ff, 0xff72dbac,
            0xff8b4513, 0xffc4a7e7,
        ] {
            let (h, s, v) = rgb_to_hsv(color);
            let back = hsv_to_rgb(h, s, v);
            for shift in [16, 8, 0] {
                let (a, b) = ((color >> shift) & 0xff, (back >> shift) & 0xff);
                assert!(a.abs_diff(b) <= 2, "{color:08x} became {back:08x}");
            }
        }
        // Hue is preserved across a grey, which has none of its own.
        let mut picker = Picker::new(0xff00ff00);
        picker.set_color(0xff404040);
        assert_eq!(picker.hue, 120);
    }
    #[test]
    fn colors_are_parsed_in_every_form_a_caller_might_have() {
        assert_eq!(parse_color("0xff72dbac"), Some(0xff72dbac));
        assert_eq!(parse_color("#72dbac"), Some(0xff72dbac));
        assert_eq!(parse_color("72dbac"), Some(0xff72dbac));
        assert_eq!(parse_color(" 255 "), Some(0xff0000ff));
        assert_eq!(parse_color("not a color"), None);
    }
    #[test]
    fn the_square_the_strip_and_the_buttons_answer_the_pointer() {
        let mut picker = Picker::new(0xff000000);
        // The bottom-left of the square is black, the top-right the full hue.
        picker.press(SQUARE.x + SQUARE.w - 1, SQUARE.y);
        assert_eq!(picker.saturation, 255);
        assert_eq!(picker.value, 255);
        picker.drag(SQUARE.x, SQUARE.y + SQUARE.h - 1);
        assert_eq!((picker.saturation, picker.value), (0, 0));
        picker.release();
        // Motion after the release is not a drag any more.
        picker.drag(SQUARE.x + 100, SQUARE.y + 100);
        assert_eq!((picker.saturation, picker.value), (0, 0));
        picker.press(HUES.x + 2, HUES.y + HUES.h - 1);
        assert_eq!(picker.hue, 359);
        picker.release();
        // A preset sets the whole color, and Use answers with it.
        picker.press(preset_rect(8).x + 2, preset_rect(8).y + 2);
        assert_eq!(picker.color(), PRESETS[8]);
        assert!(picker.done.is_none());
        picker.press(USE.x + 2, USE.y + 2);
        assert_eq!(picker.done, Some(Some(PRESETS[8])));
        picker.done = None;
        picker.press(CANCEL.x + 2, CANCEL.y + 2);
        assert_eq!(picker.done, Some(None));
    }
    #[test]
    fn the_hex_field_edits_and_escape_leaves_it_alone() {
        let mut picker = Picker::new(0xff000000);
        picker.press(FIELD.x + 4, FIELD.y + 4);
        picker.key("7");
        picker.key("2");
        picker.key("dbac");
        assert_eq!(picker.hex(), "#72dbac");
        assert_eq!(picker.color(), 0xff72dbac, "six digits apply themselves");
        // Escape while editing drops the digits instead of cancelling.
        picker.press(FIELD.x + 4, FIELD.y + 4);
        picker.key("ff");
        picker.key("\u{1b}");
        assert!(picker.done.is_none());
        assert_eq!(picker.color(), 0xff72dbac);
        picker.key("\u{1b}");
        assert_eq!(picker.done, Some(None));
        // Backspace and non-hex characters.
        picker.done = None;
        picker.press(FIELD.x + 4, FIELD.y + 4);
        picker.key("abz");
        picker.key("\u{7f}");
        assert_eq!(picker.hex(), "#a");
        picker.key("\r");
        assert_eq!(picker.done, Some(Some(0xff72dbac)), "a partial value is dropped");
    }
    #[test]
    fn the_window_draws_without_a_session() {
        let mut surface = Surface::new(WIDTH as usize, HEIGHT as usize);
        let picker = Picker::new(0xff72dbac);
        picker.draw(&mut surface, &Font::builtin());
        let center = surface.pixels()
            [(SQUARE.y as usize + 100) * WIDTH as usize + SQUARE.x as usize + 100];
        assert_ne!(center, BACKGROUND, "the square is painted");
        let preview = surface.pixels()
            [(PREVIEW.y as usize + 6) * WIDTH as usize + PREVIEW.x as usize + 6];
        assert_eq!(preview, 0xff72dbac);
    }
}
