//! `hos-paint`: a small bitmap editor, in the spirit of the one every desktop
//! used to ship with.
//!
//! ```text
//! hos-paint [FILE.qoi] [--size WIDTHxHEIGHT]
//! ```
//!
//! Tools down the left, colors along the bottom, and the picture in between.
//! Pictures are QOI images, the format the rest of hOS reads and writes. The
//! **More colors** button runs `hos-colorpicker` and takes the color it
//! prints, so the two programs work the way a shell pipeline does.
use hoswm::{
    client::{
        Client, Event, Menu, MenuItem, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT, WINDOW_RESIZABLE,
    },
    desktop::Rect,
    font::Font,
    qoi,
    surface::Surface,
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

const BACKDROP: u32 = 0xff11201b;
const PANEL: u32 = 0xff1b2b25;
const EDGE: u32 = 0xff2f4239;
const TEXT: u32 = 0xffdedede;
const DIM: u32 = 0xff8d9a93;
const ACCENT: u32 = 0xff72dbac;
/// Chrome around the picture: the tool column and the strip underneath.
const TOOLS_W: i32 = 44;
const STATUS_H: i32 = 16;
const PALETTE_H: i32 = 44;
const CANVAS_W: usize = 640;
const CANVAS_H: usize = 400;
const MAX_IMAGE_DIMENSION: usize = 8192;
const MAX_IMAGE_PIXELS: usize = 16 * 1024 * 1024;
const SIZES: [i32; 4] = [1, 2, 4, 8];

const MENU_NEW: u32 = 1;
const MENU_RELOAD: u32 = 2;
const MENU_SAVE: u32 = 3;
const MENU_SAVE_COPY: u32 = 4;
const MENU_EXIT: u32 = 5;
const MENU_UNDO: u32 = 6;
const MENU_CLEAR: u32 = 7;
const MENU_FOREGROUND: u32 = 8;
const MENU_BACKGROUND: u32 = 9;
const MENU_SWAP: u32 = 10;
/// Tool menu items are numbered from here, in [`Tool::ALL`] order.
const MENU_TOOL: u32 = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Pencil,
    Brush,
    Eraser,
    Fill,
    Pick,
    Line,
    Rectangle,
    FilledRectangle,
    Ellipse,
    FilledEllipse,
}
impl Tool {
    const ALL: [Tool; 10] = [
        Tool::Pencil,
        Tool::Brush,
        Tool::Eraser,
        Tool::Fill,
        Tool::Pick,
        Tool::Line,
        Tool::Rectangle,
        Tool::FilledRectangle,
        Tool::Ellipse,
        Tool::FilledEllipse,
    ];
    fn name(self) -> &'static str {
        match self {
            Tool::Pencil => "Pencil",
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::Fill => "Fill",
            Tool::Pick => "Pick color",
            Tool::Line => "Line",
            Tool::Rectangle => "Rectangle",
            Tool::FilledRectangle => "Filled rectangle",
            Tool::Ellipse => "Ellipse",
            Tool::FilledEllipse => "Filled ellipse",
        }
    }
    /// Width of the mark this tool leaves, for the tools that have one.
    fn sized(self) -> bool {
        !matches!(self, Tool::Fill | Tool::Pick)
    }
    /// Shapes are rubber-banded: nothing is committed until the button is let
    /// go, and the shape in progress is drawn over the picture, not into it.
    fn shape(self) -> bool {
        matches!(
            self,
            Tool::Line
                | Tool::Rectangle
                | Tool::FilledRectangle
                | Tool::Ellipse
                | Tool::FilledEllipse
        )
    }
    fn rect(index: usize) -> Rect {
        Rect {
            x: 2,
            y: 2 + index as i32 * 30,
            w: 40,
            h: 28,
        }
    }
    /// A small glyph, drawn with the primitives rather than shipped as art.
    fn draw_glyph(self, surface: &mut Surface, r: Rect, color: u32) {
        let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
        match self {
            Tool::Pencil => {
                surface.draw_line(cx - 8, cy + 6, cx + 5, cy - 7, color);
                surface.fill_rect(cx + 4, cy - 8, 3, 3, color);
                surface.fill_rect(cx - 9, cy + 6, 2, 2, 0xffffffff);
            }
            Tool::Brush => {
                for offset in -1..=1 {
                    surface.draw_line(cx - 8 + offset, cy + 6, cx + 5 + offset, cy - 7, color);
                }
                surface.fill_rect(cx + 3, cy - 9, 5, 5, color);
            }
            Tool::Eraser => {
                surface.fill_rect(cx - 7, cy - 4, 14, 9, color);
                surface.fill_rect(cx - 6, cy - 3, 12, 7, 0xff0b0f0e);
                surface.fill_rect(cx - 6, cy + 1, 12, 3, color);
            }
            Tool::Fill => {
                for row in 0..8 {
                    surface.fill_rect(cx - 8 + row, cy - 6 + row, 16 - row * 2, 1, color);
                }
                surface.fill_rect(cx - 1, cy + 3, 2, 5, color);
            }
            Tool::Pick => {
                surface.draw_line(cx - 7, cy + 7, cx + 4, cy - 4, color);
                surface.fill_rect(cx + 3, cy - 8, 5, 5, color);
                surface.fill_rect(cx - 8, cy + 6, 3, 3, 0xffffffff);
            }
            Tool::Line => surface.draw_line(cx - 8, cy + 6, cx + 8, cy - 6, color),
            Tool::Rectangle | Tool::FilledRectangle => {
                surface.fill_rect(cx - 9, cy - 6, 18, 12, color);
                if self == Tool::Rectangle {
                    surface.fill_rect(cx - 8, cy - 5, 16, 10, 0xff0b0f0e);
                }
            }
            Tool::Ellipse | Tool::FilledEllipse => {
                let mut picture = Picture::blank(18, 12, 0);
                picture.ellipse(0, 0, 17, 11, color, self == Tool::FilledEllipse);
                for y in 0..12 {
                    for x in 0..18 {
                        let pixel = picture.surface.pixels()[y * 18 + x];
                        if pixel != 0 {
                            surface.set_pixel(cx - 9 + x as i32, cy - 6 + y as i32, pixel);
                        }
                    }
                }
            }
        }
    }
}

/// The picture being edited, and the drawing operations it supports.
pub struct Picture {
    pub surface: Surface,
}
impl Picture {
    pub fn blank(width: usize, height: usize, color: u32) -> Self {
        let mut surface = Surface::new(width.max(1), height.max(1));
        surface.pixels_mut().fill(color);
        Self { surface }
    }
    pub fn width(&self) -> i32 {
        self.surface.width() as i32
    }
    pub fn height(&self) -> i32 {
        self.surface.height() as i32
    }
    pub fn pixel(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.width() || y >= self.height() {
            return None;
        }
        Some(self.surface.pixels()[y as usize * self.surface.width() + x as usize])
    }
    /// One mark of the brush: a single pixel, or a square of the chosen width.
    pub fn plot(&mut self, x: i32, y: i32, size: i32, color: u32) {
        if size <= 1 {
            self.surface.set_pixel(x, y, color);
        } else {
            self.surface
                .fill_rect(x - size / 2, y - size / 2, size, size, color);
        }
    }
    /// A line of marks, so a dragged pointer leaves no gaps between reports.
    pub fn stroke(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, size: i32, color: u32) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut error) = (x0, y0, dx + dy);
        loop {
            self.plot(x, y, size, color);
            if x == x1 && y == y1 {
                return;
            }
            let doubled = error * 2;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }
    pub fn rectangle(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        size: i32,
        color: u32,
        fill: bool,
    ) {
        let (left, right) = (x0.min(x1), x0.max(x1));
        let (top, bottom) = (y0.min(y1), y0.max(y1));
        if fill {
            self.surface
                .fill_rect(left, top, right - left + 1, bottom - top + 1, color);
            return;
        }
        self.stroke(left, top, right, top, size, color);
        self.stroke(left, bottom, right, bottom, size, color);
        self.stroke(left, top, left, bottom, size, color);
        self.stroke(right, top, right, bottom, size, color);
    }
    /// An ellipse in the given box, outlined or filled, by the usual
    /// integer midpoint method: no floating point and no gaps.
    pub fn ellipse(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: u32, fill: bool) {
        self.ellipse_sized(x0, y0, x1, y1, 1, color, fill)
    }
    pub fn ellipse_sized(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        size: i32,
        color: u32,
        fill: bool,
    ) {
        let (left, right) = (x0.min(x1), x0.max(x1));
        let (top, bottom) = (y0.min(y1), y0.max(y1));
        let (a, b) = ((right - left) as i64 / 2, (bottom - top) as i64 / 2);
        let (cx, cy) = (left as i64 + a, top as i64 + b);
        if a <= 0 || b <= 0 {
            self.stroke(left, top, right, bottom, size, color);
            return;
        }
        let (a2, b2) = (a * a, b * b);
        let plot = |dx: i64, dy: i64, picture: &mut Self| {
            let (x, y) = ((cx + dx) as i32, (cy + dy) as i32);
            let (mx, my) = ((cx - dx) as i32, (cy - dy) as i32);
            if fill {
                picture
                    .surface
                    .fill_rect(mx, y, (x - mx + 1).max(1), 1, color);
                picture
                    .surface
                    .fill_rect(mx, my, (x - mx + 1).max(1), 1, color);
            } else {
                for (px, py) in [(x, y), (mx, y), (x, my), (mx, my)] {
                    picture.plot(px, py, size, color);
                }
            }
        };
        // First arc: the part where the curve is more horizontal than vertical.
        let (mut x, mut y) = (0i64, b);
        let mut error = b2 - a2 * b + a2 / 4;
        while a2 * y > b2 * x {
            plot(x, y, self);
            if error < 0 {
                error += b2 * (2 * x + 3);
            } else {
                error += b2 * (2 * x + 3) + a2 * (2 - 2 * y);
                y -= 1;
            }
            x += 1;
        }
        // Second arc, from the end of the first down to the side of the box.
        let mut error = b2 * (x * x + x) + a2 * (y * y - y) - a2 * b2;
        while y >= 0 {
            plot(x, y, self);
            if error > 0 {
                error += a2 * (3 - 2 * y);
            } else {
                error += b2 * (2 * x + 2) + a2 * (3 - 2 * y);
                x += 1;
            }
            y -= 1;
        }
    }
    /// Flood fill from one point, over every pixel of the color it started on.
    pub fn fill(&mut self, x: i32, y: i32, color: u32) {
        let Some(target) = self.pixel(x, y) else {
            return;
        };
        if target == color {
            return;
        }
        let width = self.surface.width();
        let mut stack = vec![(x, y)];
        while let Some((x, y)) = stack.pop() {
            if self.pixel(x, y) != Some(target) {
                continue;
            }
            // Walk this row out to both edges of the region, pushing the
            // rows above and below as their color changes.
            let mut left = x;
            while left > 0 && self.pixel(left - 1, y) == Some(target) {
                left -= 1;
            }
            let mut right = x;
            while right + 1 < self.width() && self.pixel(right + 1, y) == Some(target) {
                right += 1;
            }
            for column in left..=right {
                self.surface.pixels_mut()[y as usize * width + column as usize] = color;
                for row in [y - 1, y + 1] {
                    if self.pixel(column, row) == Some(target) {
                        stack.push((column, row));
                    }
                }
            }
        }
    }
}

/// A pointer drag in picture coordinates.
#[derive(Clone, Copy)]
struct Drag {
    from: (i32, i32),
    to: (i32, i32),
}

#[derive(Clone, Debug)]
struct NewDialog {
    width: String,
    height: String,
    focus: usize,
    error: String,
}

impl NewDialog {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width: width.to_string(),
            height: height.to_string(),
            focus: 0,
            error: String::new(),
        }
    }
}

pub struct Paint {
    pub picture: Picture,
    pub path: Option<PathBuf>,
    pub tool: Tool,
    pub size: i32,
    pub foreground: u32,
    pub background: u32,
    /// Snapshots taken when a stroke starts, newest last.
    undo: Vec<Vec<u32>>,
    drag: Option<Drag>,
    /// Where the picture sits in the canvas area, for pictures too big for it.
    scroll: (i32, i32),
    pointer: (i32, i32),
    pub modified: bool,
    /// Text for the status line: what just happened, or what went wrong.
    pub status: String,
    pub quit: bool,
    /// Set when a color must be chosen; the session runs the picker for it.
    pub wants_color: Option<bool>,
    /// Save requested by a key or menu event; true writes a new copy.
    wants_save: Option<bool>,
    new_dialog: Option<NewDialog>,
}
/// The 28 colors along the bottom, in two rows of fourteen.
const PALETTE: [u32; 28] = [
    0xff000000, 0xff404040, 0xff808080, 0xffc0c0c0, 0xffffffff, 0xff7f0000, 0xffff0000, 0xffff7f00,
    0xffffff00, 0xff007f00, 0xff00ff00, 0xff00ffff, 0xff007fff, 0xff0000ff, 0xff11201b, 0xff2b2b2b,
    0xff5a5a5a, 0xff9a9a9a, 0xffe8e8e8, 0xff7f007f, 0xffff00ff, 0xff8b4513, 0xffe4c878, 0xff2e7d32,
    0xff72dbac, 0xff9ccfd8, 0xff80afff, 0xffc4a7e7,
];
impl Paint {
    pub fn new(picture: Picture, path: Option<PathBuf>) -> Self {
        Self {
            picture,
            path,
            tool: Tool::Pencil,
            size: 1,
            foreground: 0xff000000,
            background: 0xffffffff,
            undo: Vec::new(),
            drag: None,
            scroll: (0, 0),
            pointer: (0, 0),
            modified: false,
            status: String::new(),
            quit: false,
            wants_color: None,
            wants_save: None,
            new_dialog: None,
        }
    }
    /// The area the picture is drawn in, which the window size decides.
    fn canvas_area(width: i32, height: i32) -> Rect {
        Rect {
            x: TOOLS_W,
            y: 0,
            w: (width - TOOLS_W).max(1),
            h: (height - PALETTE_H - STATUS_H).max(1),
        }
    }
    fn size_rect(index: usize) -> Rect {
        Rect {
            x: 4,
            y: 2 + Tool::ALL.len() as i32 * 30 + 6 + index as i32 * 20,
            w: 36,
            h: 18,
        }
    }
    fn swatch_rect(width: i32, height: i32, index: usize) -> Rect {
        let _ = width;
        Rect {
            x: 96 + (index % 14) as i32 * 20,
            y: height - PALETTE_H + 4 + (index / 14) as i32 * 20,
            w: 18,
            h: 18,
        }
    }
    fn more_rect(width: i32, height: i32) -> Rect {
        Rect {
            x: (96 + 14 * 20 + 6).min(width - 78),
            y: height - PALETTE_H + 8,
            w: 72,
            h: 28,
        }
    }
    fn colors_rect(height: i32) -> Rect {
        Rect {
            x: 8,
            y: height - PALETTE_H + 6,
            w: 40,
            h: 32,
        }
    }
    /// Window coordinates to picture coordinates.
    fn to_picture(&self, width: i32, height: i32, x: i32, y: i32) -> (i32, i32) {
        let area = Self::canvas_area(width, height);
        (x - area.x - self.scroll.0, y - area.y - self.scroll.1)
    }
    pub fn snapshot(&mut self) {
        if self.undo.len() == 8 {
            self.undo.remove(0);
        }
        self.undo.push(self.picture.surface.pixels().to_vec());
    }
    pub fn undo(&mut self) {
        let Some(pixels) = self.undo.pop() else {
            self.status = "Nothing to undo".into();
            return;
        };
        if pixels.len() == self.picture.surface.pixels().len() {
            self.picture.surface.pixels_mut().copy_from_slice(&pixels);
            self.modified = true;
            self.status = "Undone".into();
        }
    }
    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.drag = None;
        self.status = format!("{} tool", tool.name());
    }
    fn replace_picture(&mut self, width: usize, height: usize) {
        self.picture = Picture::blank(width, height, self.background);
        self.undo.clear();
        self.path = None;
        self.modified = false;
        self.scroll = (0, 0);
        self.status = "New picture".into();
    }
    pub fn new_picture(&mut self) {
        self.replace_picture(CANVAS_W, CANVAS_H);
    }
    fn open_new_dialog(&mut self) {
        self.new_dialog = Some(NewDialog::new(
            self.picture.width() as usize,
            self.picture.height() as usize,
        ));
    }
    fn create_from_new_dialog(&mut self) {
        let Some(dialog) = &self.new_dialog else {
            return;
        };
        let result = image_dimensions(&dialog.width, &dialog.height);
        match result {
            Ok((width, height)) => {
                self.replace_picture(width, height);
                self.new_dialog = None;
            }
            Err(error) => {
                if let Some(dialog) = &mut self.new_dialog {
                    dialog.error = error;
                }
            }
        }
    }
    fn new_dialog_key(&mut self, text: &str) {
        if self.new_dialog.is_none() {
            return;
        }
        match text {
            "\u{1b}" => self.new_dialog = None,
            "\r" | "\n" => self.create_from_new_dialog(),
            _ => {
                let Some(dialog) = &mut self.new_dialog else {
                    return;
                };
                match text {
                    "\t" => dialog.focus = (dialog.focus + 1) % 2,
                    "\u{8}" | "\u{7f}" => {
                        let value = if dialog.focus == 0 {
                            &mut dialog.width
                        } else {
                            &mut dialog.height
                        };
                        value.pop();
                    }
                    value if value.chars().all(|c| c.is_ascii_digit()) && !value.is_empty() => {
                        let field = if dialog.focus == 0 {
                            &mut dialog.width
                        } else {
                            &mut dialog.height
                        };
                        if field.len() < 5 {
                            field.push_str(value);
                        }
                    }
                    _ => return,
                }
                dialog.error.clear();
            }
        }
    }
    fn new_dialog_click(&mut self, width: i32, height: i32, x: i32, y: i32) {
        let (dialog, fields, ok, cancel) = new_dialog_layout(width, height);
        if self.new_dialog.is_none() {
            return;
        }
        if ok.contains(x, y) {
            self.create_from_new_dialog();
        } else if cancel.contains(x, y) || !dialog.contains(x, y) {
            self.new_dialog = None;
        } else if let Some(state) = &mut self.new_dialog {
            if fields[0].contains(x, y) {
                state.focus = 0;
                state.error.clear();
            } else if fields[1].contains(x, y) {
                state.focus = 1;
                state.error.clear();
            }
        }
    }
    /// Press, drag and release, in window coordinates. `button` is 1 for the
    /// left button and 2 for the right one, which paints with the background
    /// color as it always has.
    pub fn press(&mut self, width: i32, height: i32, x: i32, y: i32, button: u32) {
        self.pointer = (x, y);
        // The regions are tested in the order they are drawn in, so a window
        // small enough for them to overlap answers with the one on top.
        if y >= height - PALETTE_H {
            if Self::more_rect(width, height).contains(x, y) {
                self.wants_color = Some(button != 2);
            } else if Self::colors_rect(height).contains(x, y) {
                std::mem::swap(&mut self.foreground, &mut self.background);
            } else if let Some(color) = PALETTE
                .iter()
                .enumerate()
                .find(|(index, _)| Self::swatch_rect(width, height, *index).contains(x, y))
                .map(|(_, color)| *color)
            {
                if button == 2 {
                    self.background = color;
                } else {
                    self.foreground = color;
                }
            }
            return;
        }
        if x < TOOLS_W {
            if let Some(tool) = Tool::ALL
                .iter()
                .enumerate()
                .find(|(index, _)| Tool::rect(*index).contains(x, y))
                .map(|(_, tool)| *tool)
            {
                self.set_tool(tool);
            } else if let Some(size) = SIZES
                .iter()
                .enumerate()
                .find(|(index, _)| Self::size_rect(*index).contains(x, y))
                .map(|(_, size)| *size)
            {
                self.size = size;
                self.status = format!("{size} px");
            }
            return;
        }
        let area = Self::canvas_area(width, height);
        if !area.contains(x, y) {
            return;
        }
        let (px, py) = self.to_picture(width, height, x, y);
        let color = self.color(button);
        match self.tool {
            Tool::Pick => {
                if let Some(picked) = self.picture.pixel(px, py) {
                    if button == 2 {
                        self.background = picked;
                    } else {
                        self.foreground = picked;
                    }
                    self.status = format!("Picked 0x{picked:08x}");
                }
            }
            Tool::Fill => {
                if self.picture.pixel(px, py).is_some() {
                    self.snapshot();
                    self.picture.fill(px, py, color);
                    self.modified = true;
                }
            }
            tool => {
                self.snapshot();
                self.drag = Some(Drag {
                    from: (px, py),
                    to: (px, py),
                });
                if !tool.shape() {
                    self.picture.plot(px, py, self.size, color);
                    self.modified = true;
                }
            }
        }
    }
    pub fn motion(&mut self, width: i32, height: i32, x: i32, y: i32, button: u32) {
        self.pointer = (x, y);
        let (px, py) = self.to_picture(width, height, x, y);
        let Some(drag) = &mut self.drag else {
            return;
        };
        let from = drag.to;
        drag.to = (px, py);
        if !self.tool.shape() {
            let color = self.color(button);
            self.picture
                .stroke(from.0, from.1, px, py, self.size, color);
            self.modified = true;
        }
    }
    pub fn release(&mut self, width: i32, height: i32, x: i32, y: i32, button: u32) {
        let (px, py) = self.to_picture(width, height, x, y);
        let Some(drag) = self.drag.take() else {
            return;
        };
        if !self.tool.shape() {
            return;
        }
        let color = self.color(button);
        self.commit_shape(drag.from, (px, py), color);
        self.modified = true;
    }
    fn color(&self, button: u32) -> u32 {
        match (self.tool, button) {
            (Tool::Eraser, _) => self.background,
            (_, 2) => self.background,
            _ => self.foreground,
        }
    }
    fn commit_shape(&mut self, from: (i32, i32), to: (i32, i32), color: u32) {
        let size = self.size.max(1);
        match self.tool {
            Tool::Line => self.picture.stroke(from.0, from.1, to.0, to.1, size, color),
            Tool::Rectangle => self
                .picture
                .rectangle(from.0, from.1, to.0, to.1, size, color, false),
            Tool::FilledRectangle => self
                .picture
                .rectangle(from.0, from.1, to.0, to.1, size, color, true),
            Tool::Ellipse => self
                .picture
                .ellipse_sized(from.0, from.1, to.0, to.1, size, color, false),
            Tool::FilledEllipse => self
                .picture
                .ellipse_sized(from.0, from.1, to.0, to.1, size, color, true),
            _ => (),
        }
    }
    pub fn scroll(&mut self, dx: i32, dy: i32, width: i32, height: i32) {
        let area = Self::canvas_area(width, height);
        // Scrolling only matters for a picture larger than its area; it never
        // pulls the whole picture off the screen.
        self.scroll.0 = (self.scroll.0 + dx).clamp((area.w - self.picture.width()).min(0), 0);
        self.scroll.1 = (self.scroll.1 + dy).clamp((area.h - self.picture.height()).min(0), 0);
    }
    pub fn key(&mut self, text: &str, modifiers: u32, width: i32, height: i32) {
        let ctrl = modifiers & 1 != 0;
        match text {
            "\u{1b}" => {
                // Escape abandons a shape in progress before it leaves.
                if self.drag.take().is_some() {
                    self.undo();
                } else {
                    self.quit = true;
                }
            }
            "[" => self.size = SIZES[0],
            "]" => self.size = *SIZES.last().unwrap(),
            "\u{1b}[A" => self.scroll(0, 40, width, height),
            "\u{1b}[B" => self.scroll(0, -40, width, height),
            "\u{1b}[D" => self.scroll(40, 0, width, height),
            "\u{1b}[C" => self.scroll(-40, 0, width, height),
            _ if ctrl => match text.as_bytes() {
                [19] | [b's'] | [b'S'] => self.wants_save = Some(false),
                [26] => self.undo(),
                [14] => self.open_new_dialog(),
                _ => (),
            },
            _ => {
                if let Some(digit) = text.chars().next().and_then(|c| c.to_digit(10)) {
                    if let Some(tool) = Tool::ALL.get(((digit + 9) % 10) as usize) {
                        self.set_tool(*tool);
                    }
                }
            }
        }
    }
    /// Write the picture, to its own file or to a new one in the pictures
    /// directory. The path it used comes back for the notification.
    pub fn save(&mut self, directory: &Path, as_copy: bool) -> Result<PathBuf, String> {
        let path = match self.path.clone().filter(|_| !as_copy) {
            Some(path) => path,
            None => {
                std::fs::create_dir_all(directory)
                    .map_err(|e| format!("{}: {e}", directory.display()))?;
                // Named by the moment it was saved, down to the millisecond,
                // the way screenshots are: two copies in the same second must
                // not be the same file.
                let now = hoswm::toast::now_ms();
                let stamp = hoswm::toast::format_time(now);
                let stem = format!(
                    "painting-{}-{}-{:03}",
                    stamp[..10].replace('-', ""),
                    stamp[11..].replace(':', ""),
                    now % 1000
                );
                // A second copy in the same millisecond gets its own name
                // rather than overwriting the first.
                let mut candidate = directory.join(format!("{stem}.qoi"));
                let mut next = 2;
                while candidate.exists() {
                    candidate = directory.join(format!("{stem}-{next}.qoi"));
                    next += 1;
                }
                candidate
            }
        };
        qoi::save_surface(&path, &self.picture.surface)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.path = Some(path.clone());
        self.modified = false;
        Ok(path)
    }
    pub fn reload(&mut self) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            return Err("This picture has not been saved yet".into());
        };
        let image = qoi::load(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.picture = Picture::from_image(&image);
        self.undo.clear();
        self.modified = false;
        Ok(())
    }
    pub fn title(&self) -> String {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into());
        format!("{name}{}", if self.modified { " *" } else { "" })
    }
    pub fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        let (width, height) = (surface.width() as i32, surface.height() as i32);
        surface.pixels_mut().fill(BACKDROP);
        let area = Self::canvas_area(width, height);
        // The picture, and a border so its edge is visible against the desk.
        surface.draw_surface(
            area.x + self.scroll.0,
            area.y + self.scroll.1,
            &self.picture.surface,
        );
        surface.fill_rect(
            area.x + self.scroll.0 - 1,
            area.y + self.scroll.1 - 1,
            self.picture.width() + 2,
            1,
            EDGE,
        );
        surface.fill_rect(
            area.x + self.scroll.0 - 1,
            area.y + self.scroll.1 + self.picture.height(),
            self.picture.width() + 2,
            1,
            EDGE,
        );
        surface.fill_rect(
            area.x + self.scroll.0 - 1,
            area.y + self.scroll.1,
            1,
            self.picture.height(),
            EDGE,
        );
        surface.fill_rect(
            area.x + self.scroll.0 + self.picture.width(),
            area.y + self.scroll.1,
            1,
            self.picture.height(),
            EDGE,
        );
        // The shape being dragged, over the picture but not yet part of it.
        if let Some(drag) = self.drag.filter(|_| self.tool.shape()) {
            let mut preview = Picture {
                surface: Surface::new(self.picture.surface.width(), self.picture.surface.height()),
            };
            preview.surface.pixels_mut().fill(0);
            let mut sketch = Paint {
                picture: preview,
                ..Paint::new(Picture::blank(1, 1, 0), None)
            };
            sketch.tool = self.tool;
            sketch.size = self.size;
            sketch.commit_shape(drag.from, drag.to, self.foreground | 0xff00_0000);
            for y in 0..sketch.picture.height() {
                for x in 0..sketch.picture.width() {
                    let pixel = sketch.picture.pixel(x, y).unwrap_or(0);
                    if pixel != 0 {
                        surface.set_pixel(
                            area.x + self.scroll.0 + x,
                            area.y + self.scroll.1 + y,
                            pixel,
                        );
                    }
                }
            }
        }
        // Tool column.
        surface.fill_rect(0, 0, TOOLS_W, height, PANEL);
        surface.fill_rect(TOOLS_W - 1, 0, 1, height, EDGE);
        for (index, tool) in Tool::ALL.iter().enumerate() {
            let r = Tool::rect(index);
            let chosen = *tool == self.tool;
            surface.fill_rect(r.x, r.y, r.w, r.h, if chosen { ACCENT } else { EDGE });
            surface.fill_rect(r.x + 1, r.y + 1, r.w - 2, r.h - 2, 0xff0b0f0e);
            tool.draw_glyph(surface, r, if chosen { ACCENT } else { TEXT });
        }
        // The width column is dimmed for the tools that have no width.
        let sized = self.tool.sized();
        for (index, size) in SIZES.iter().enumerate() {
            let r = Self::size_rect(index);
            let chosen = sized && *size == self.size;
            surface.fill_rect(r.x, r.y, r.w, r.h, if chosen { ACCENT } else { EDGE });
            surface.fill_rect(r.x + 1, r.y + 1, r.w - 2, r.h - 2, 0xff0b0f0e);
            surface.fill_rect(
                r.x + 4,
                r.y + r.h / 2 - size / 2,
                r.w - 8,
                (*size).max(1),
                match (chosen, sized) {
                    (true, _) => ACCENT,
                    (_, true) => DIM,
                    _ => EDGE,
                },
            );
        }
        // Colors: the two in use, the palette, and the way to a new one.
        surface.fill_rect(0, height - PALETTE_H, width, PALETTE_H, PANEL);
        surface.fill_rect(0, height - PALETTE_H, width, 1, EDGE);
        let colors = Self::colors_rect(height);
        surface.fill_rect(colors.x + 10, colors.y + 10, 24, 22, 0xff000000);
        surface.fill_rect(colors.x + 11, colors.y + 11, 22, 20, self.background);
        surface.fill_rect(colors.x, colors.y, 24, 22, 0xff000000);
        surface.fill_rect(colors.x + 1, colors.y + 1, 22, 20, self.foreground);
        for (index, color) in PALETTE.iter().enumerate() {
            let r = Self::swatch_rect(width, height, index);
            surface.fill_rect(r.x, r.y, r.w, r.h, 0xff000000);
            surface.fill_rect(r.x + 1, r.y + 1, r.w - 2, r.h - 2, *color);
        }
        let more = Self::more_rect(width, height);
        surface.fill_rect(more.x, more.y, more.w, more.h, EDGE);
        surface.fill_rect(more.x + 1, more.y + 1, more.w - 2, more.h - 2, 0xff0b0f0e);
        font.draw(surface, more.x + 6, more.y + 4, "More", TEXT);
        font.draw(surface, more.x + 6, more.y + 15, "colors", DIM);
        // Status line: tool, width, pointer and picture size, then the message.
        let status_y = height - PALETTE_H - STATUS_H;
        surface.fill_rect(0, status_y, width, STATUS_H, 0xff0b0f0e);
        let (px, py) = self.to_picture(width, height, self.pointer.0, self.pointer.1);
        let left = format!(
            "{}  {}  {}x{}  {},{}",
            self.tool.name(),
            if sized {
                format!("{} px", self.size)
            } else {
                "-".into()
            },
            self.picture.width(),
            self.picture.height(),
            px,
            py
        );
        font.draw(surface, 6, status_y + 4, &left, DIM);
        if !self.status.is_empty() {
            let text: String = self.status.chars().take(46).collect();
            let x = (width - text.chars().count() as i32 * 8 - 8).max(6);
            font.draw(surface, x, status_y + 4, &text, ACCENT);
        }
        if let Some(dialog) = &self.new_dialog {
            let (panel, fields, ok, cancel) = new_dialog_layout(width, height);
            surface.fill_rect(0, 0, width, height, 0x99000000);
            surface.fill_rect(panel.x, panel.y, panel.w, panel.h, PANEL);
            surface.fill_rect(panel.x, panel.y, panel.w, 1, ACCENT);
            font.draw(surface, panel.x + 16, panel.y + 16, "New image", TEXT);
            font.draw(surface, panel.x + 16, panel.y + 42, "Width", DIM);
            font.draw(surface, panel.x + 176, panel.y + 42, "Height", DIM);
            for (index, field) in fields.iter().enumerate() {
                let selected = dialog.focus == index;
                surface.fill_rect(
                    field.x,
                    field.y,
                    field.w,
                    field.h,
                    if selected { ACCENT } else { EDGE },
                );
                surface.fill_rect(
                    field.x + 1,
                    field.y + 1,
                    field.w - 2,
                    field.h - 2,
                    0xff0b0f0e,
                );
                let value = if index == 0 {
                    &dialog.width
                } else {
                    &dialog.height
                };
                font.draw(surface, field.x + 8, field.y + 7, value, TEXT);
            }
            if !dialog.error.is_empty() {
                font.draw(
                    surface,
                    panel.x + 16,
                    panel.y + 92,
                    &dialog.error,
                    0xffef6976,
                );
            }
            for (button, label) in [(ok, "Create"), (cancel, "Cancel")] {
                surface.fill_rect(button.x, button.y, button.w, button.h, EDGE);
                surface.fill_rect(
                    button.x + 1,
                    button.y + 1,
                    button.w - 2,
                    button.h - 2,
                    0xff0b0f0e,
                );
                font.draw(surface, button.x + 10, button.y + 7, label, TEXT);
            }
        }
    }
}

fn new_dialog_layout(width: i32, height: i32) -> (Rect, [Rect; 2], Rect, Rect) {
    let panel = Rect {
        x: (width - 330).max(4) / 2,
        y: (height - 170).max(4) / 2,
        w: 330,
        h: 170,
    };
    let fields = [
        Rect {
            x: panel.x + 16,
            y: panel.y + 52,
            w: 140,
            h: 28,
        },
        Rect {
            x: panel.x + 174,
            y: panel.y + 52,
            w: 140,
            h: 28,
        },
    ];
    let ok = Rect {
        x: panel.x + 174,
        y: panel.y + 128,
        w: 66,
        h: 28,
    };
    let cancel = Rect {
        x: panel.x + 248,
        y: panel.y + 128,
        w: 66,
        h: 28,
    };
    (panel, fields, ok, cancel)
}

fn image_dimensions(width: &str, height: &str) -> Result<(usize, usize), String> {
    let width = width
        .parse::<usize>()
        .map_err(|_| "Width must be a number".to_string())?;
    let height = height
        .parse::<usize>()
        .map_err(|_| "Height must be a number".to_string())?;
    if width == 0 || width > MAX_IMAGE_DIMENSION || height == 0 || height > MAX_IMAGE_DIMENSION {
        return Err(format!("Size must be 1..{MAX_IMAGE_DIMENSION} pixels"));
    }
    if width
        .checked_mul(height)
        .is_none_or(|pixels| pixels > MAX_IMAGE_PIXELS)
    {
        return Err(format!("Image cannot exceed {MAX_IMAGE_PIXELS} pixels"));
    }
    Ok((width, height))
}

fn parse_size(value: &str) -> Result<(usize, usize), String> {
    let (width, height) = value
        .split_once('x')
        .or_else(|| value.split_once('X'))
        .ok_or_else(|| "Size must be WIDTHxHEIGHT".to_string())?;
    image_dimensions(width, height)
}

#[derive(Debug, PartialEq, Eq)]
struct PaintArgs {
    path: Option<PathBuf>,
    size: Option<(usize, usize)>,
}

fn parse_args<I>(arguments: I) -> Result<Option<PaintArgs>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut path = None;
    let mut size = None;
    let mut width = None;
    let mut height = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == "--help" || argument == "-h" {
            println!("Usage: hos-paint [FILE.qoi] [--size WIDTHxHEIGHT]");
            println!("       hos-paint [FILE.qoi] [-w WIDTH] [--height HEIGHT]");
            return Ok(None);
        }
        if let Some(value) = argument.strip_prefix("--size=") {
            size = Some(parse_size(value)?);
        } else if argument == "--size" || argument == "--new" {
            size = Some(parse_size(
                &arguments.next().ok_or("--size needs WIDTHxHEIGHT")?,
            )?);
        } else if let Some(value) = argument.strip_prefix("--width=") {
            width = Some(value.to_string());
        } else if argument == "--width" || argument == "-w" {
            width = Some(arguments.next().ok_or("--width needs a value")?);
        } else if let Some(value) = argument.strip_prefix("--height=") {
            height = Some(value.to_string());
        } else if argument == "--height" {
            height = Some(arguments.next().ok_or("--height needs a value")?);
        } else if path.is_none() {
            path = Some(PathBuf::from(argument));
        } else {
            return Err(format!("unexpected argument: {argument}"));
        }
    }
    if width.is_some() || height.is_some() {
        if size.is_some() {
            return Err("use --size or --width/--height, not both".into());
        }
        size = Some(image_dimensions(
            &width.ok_or("--width and --height are both required")?,
            &height.ok_or("--width and --height are both required")?,
        )?);
    }
    if path.is_some() && size.is_some() {
        return Err("an image size can only be used when creating a new picture".into());
    }
    Ok(Some(PaintArgs { path, size }))
}

impl Picture {
    pub fn from_image(image: &qoi::Image) -> Self {
        let mut surface = Surface::new(image.width.max(1), image.height.max(1));
        surface
            .pixels_mut()
            .iter_mut()
            .zip(&image.pixels)
            .for_each(|(destination, source)| *destination = *source | 0xff00_0000);
        Self { surface }
    }
}

fn menus(app: &Paint) -> Vec<Menu> {
    let tools = Tool::ALL
        .iter()
        .enumerate()
        .map(|(index, tool)| {
            MenuItem::new(MENU_TOOL + index as u32, tool.name())
                .shortcut(format!("{}", (index + 1) % 10))
                .checked(*tool == app.tool)
        })
        .collect();
    vec![
        Menu::new(
            "File",
            vec![
                MenuItem::new(MENU_NEW, "New").shortcut("Ctrl+N"),
                MenuItem::new(MENU_RELOAD, "Reload from disk").enabled(app.path.is_some()),
                MenuItem::rule(),
                MenuItem::new(MENU_SAVE, "Save").shortcut("Ctrl+S"),
                MenuItem::new(MENU_SAVE_COPY, "Save a copy"),
                MenuItem::rule(),
                MenuItem::new(MENU_EXIT, "Exit").shortcut("Esc"),
            ],
        ),
        Menu::new(
            "Edit",
            vec![
                MenuItem::new(MENU_UNDO, "Undo").shortcut("Ctrl+Z"),
                MenuItem::new(MENU_CLEAR, "Clear picture"),
            ],
        ),
        Menu::new("Tools", tools),
        Menu::new(
            "Colors",
            vec![
                MenuItem::new(MENU_FOREGROUND, "Choose drawing color..."),
                MenuItem::new(MENU_BACKGROUND, "Choose background color..."),
                MenuItem::rule(),
                MenuItem::new(MENU_SWAP, "Swap the two"),
            ],
        ),
    ]
}

/// Run `hos-colorpicker` and read the color it prints. A cancelled picker
/// exits nonzero and prints nothing, which is not an error here.
fn choose_color(current: u32) -> Result<Option<u32>, String> {
    let program = ["/bin/hos-colorpicker", "hos-colorpicker"]
        .into_iter()
        .find(|path| Path::new(path).is_file() || !path.starts_with('/'))
        .unwrap_or("hos-colorpicker");
    let output = Command::new(program)
        .arg(format!("0x{current:08x}"))
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(hoswm::config::color(text.trim()))
}

fn run() -> Result<(), String> {
    let Some(arguments) = parse_args(std::env::args().skip(1))? else {
        return Ok(());
    };
    let PaintArgs { path, size } = arguments;
    let client = Client::connect().map_err(|e| e.to_string())?;
    let picture = match &path {
        Some(path) => match qoi::load(path) {
            Ok(image) => Picture::from_image(&image),
            Err(e) => {
                let _ = client.ask(
                    "Cannot open picture",
                    &format!("{}: {e}", path.display()),
                    hoswm::client::ANSWER_BUTTONS_OK,
                    hoswm::client::SEVERITY_ERROR,
                );
                return Err(format!("{}: {e}", path.display()));
            }
        },
        None => {
            let (width, height) = size.unwrap_or((CANVAS_W, CANVAS_H));
            Picture::blank(width, height, 0xffffffff)
        }
    };
    let mut app = Paint::new(picture, path);
    let pictures = hoswm::config::directory().join("paintings");
    let window = client
        .create("Paint", 700, 460, ACCENT)
        .map_err(|e| e.to_string())?;
    client
        .flags(
            window,
            WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE | WINDOW_RESIZABLE,
        )
        .map_err(|e| e.to_string())?;
    let font = Font::builtin();
    let (mut width, mut height, _) = client.size(window).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(width as usize, height as usize);
    let mut menu_state = String::new();
    let mut dirty = true;
    loop {
        let (new_width, new_height, minimized) = client.size(window).map_err(|e| e.to_string())?;
        if (new_width, new_height) != (width, height) {
            width = new_width;
            height = new_height;
            surface.reset(width as usize, height as usize, BACKDROP);
            app.scroll(0, 0, width as i32, height as i32);
            dirty = true;
        }
        let (w, h) = (width as i32, height as i32);
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
                6 => {
                    if app.new_dialog.is_some() {
                        app.new_dialog_key(&text);
                    } else {
                        app.key(&text, control, w, h);
                    }
                }
                8 => {
                    let mut parts = text.split_whitespace();
                    let x = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let action = parts
                        .next()
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(0);
                    if app.new_dialog.is_some() {
                        if control == 1 && action == 1 {
                            app.new_dialog_click(w, h, x, y);
                        }
                        continue;
                    }
                    match (control, action) {
                        (1, 1) => app.press(w, h, x, y, 1),
                        (1, 0) => app.release(w, h, x, y, 1),
                        (0, _) => app.motion(w, h, x, y, 1),
                        // The right button arrives as a click on its own, so
                        // it chooses colors and picks them rather than drags.
                        (2, _) => app.press(w, h, x, y, 2),
                        _ => (),
                    }
                }
                10 => {
                    let axis = text
                        .split_whitespace()
                        .nth(2)
                        .and_then(|v| v.parse::<u32>().ok())
                        .unwrap_or(0);
                    let delta = control as i32 * 40;
                    if axis == 0 {
                        app.scroll(0, delta, w, h);
                    } else {
                        app.scroll(delta, 0, w, h);
                    }
                }
                11 => match control {
                    MENU_NEW => app.open_new_dialog(),
                    MENU_RELOAD => match app.reload() {
                        Ok(()) => app.status = "Reloaded".into(),
                        Err(e) => app.status = e,
                    },
                    MENU_SAVE | MENU_SAVE_COPY => {
                        app.wants_save = Some(control == MENU_SAVE_COPY);
                    }
                    MENU_EXIT => app.quit = true,
                    MENU_UNDO => app.undo(),
                    MENU_CLEAR => {
                        app.snapshot();
                        let background = app.background;
                        app.picture.surface.pixels_mut().fill(background);
                        app.modified = true;
                    }
                    MENU_FOREGROUND => app.wants_color = Some(true),
                    MENU_BACKGROUND => app.wants_color = Some(false),
                    MENU_SWAP => std::mem::swap(&mut app.foreground, &mut app.background),
                    id if (MENU_TOOL..MENU_TOOL + Tool::ALL.len() as u32).contains(&id) => {
                        app.set_tool(Tool::ALL[(id - MENU_TOOL) as usize]);
                    }
                    _ => (),
                },
                7 | 9 => app.quit = true,
                _ => (),
            }
            // Save before consuming another event, which may change the picture.
            if let Some(as_copy) = app.wants_save.take() {
                match app.save(&pictures, as_copy) {
                    Ok(path) => {
                        app.status = format!("Saved {}", path.display());
                        let _ = client.toast(&app.status, ACCENT, 0);
                    }
                    Err(e) => {
                        app.status = e.clone();
                        let _ = client.toast(&e, 0xffef6976, 6000);
                    }
                }
            }
        }
        if let Some(foreground) = app.wants_color.take() {
            let current = if foreground {
                app.foreground
            } else {
                app.background
            };
            match choose_color(current) {
                Ok(Some(color)) if foreground => app.foreground = color,
                Ok(Some(color)) => app.background = color,
                Ok(None) => app.status = "No color chosen".into(),
                Err(e) => app.status = e,
            }
            dirty = true;
        }
        if app.quit {
            let _ = client.close(window);
            return Ok(());
        }
        let state = format!("{:?}{}{}", app.tool, app.path.is_some(), app.title());
        if state != menu_state {
            client
                .set_menus(window, &menus(&app))
                .map_err(|e| e.to_string())?;
            menu_state = state;
        }
        if dirty && !minimized {
            app.draw(&mut surface, &font);
            client
                .present(window, width, height, surface.pixels())
                .map_err(|e| e.to_string())?;
            dirty = false;
        }
        thread::sleep(Duration::from_millis(if minimized { 120 } else { 12 }));
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-paint: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paint() -> Paint {
        Paint::new(Picture::blank(64, 48, 0xffffffff), None)
    }
    #[test]
    fn new_image_sizes_are_checked() {
        assert_eq!(image_dimensions("320", "200"), Ok((320, 200)));
        assert!(image_dimensions("0", "200").is_err());
        assert!(image_dimensions("8193", "1").is_err());
        assert!(image_dimensions("4096", "4096").is_err());
        assert_eq!(parse_size("800x600"), Ok((800, 600)));
        assert!(parse_size("800").is_err());
    }
    #[test]
    fn command_line_can_create_a_sized_picture() {
        assert_eq!(
            parse_args(vec!["--size".into(), "320x240".into()]),
            Ok(Some(PaintArgs {
                path: None,
                size: Some((320, 240)),
            }))
        );
        assert_eq!(
            parse_args(vec![
                "--width".into(),
                "320".into(),
                "--height".into(),
                "240".into()
            ]),
            Ok(Some(PaintArgs {
                path: None,
                size: Some((320, 240)),
            }))
        );
        assert!(parse_args(vec!["picture.qoi".into(), "--size".into(), "1x1".into()]).is_err());
    }
    #[test]
    fn strokes_shapes_and_fills_land_on_the_picture() {
        let mut picture = Picture::blank(32, 32, 0xffffffff);
        picture.stroke(0, 0, 31, 31, 1, 0xff000000);
        assert_eq!(picture.pixel(0, 0), Some(0xff000000));
        assert_eq!(picture.pixel(16, 16), Some(0xff000000));
        assert_eq!(picture.pixel(31, 31), Some(0xff000000));
        assert_eq!(picture.pixel(0, 31), Some(0xffffffff));
        // A wider mark covers its neighbours, and stays inside the picture.
        picture.plot(0, 0, 4, 0xffff0000);
        assert_eq!(picture.pixel(1, 1), Some(0xffff0000));
        assert_eq!(picture.pixel(31, 0), Some(0xffffffff));
        // An outlined rectangle is a border, not a block.
        let mut picture = Picture::blank(32, 32, 0xffffffff);
        picture.rectangle(4, 4, 20, 16, 1, 0xff000000, false);
        assert_eq!(picture.pixel(4, 4), Some(0xff000000));
        assert_eq!(picture.pixel(12, 4), Some(0xff000000));
        assert_eq!(picture.pixel(12, 10), Some(0xffffffff));
        picture.rectangle(4, 4, 20, 16, 1, 0xff00ff00, true);
        assert_eq!(picture.pixel(12, 10), Some(0xff00ff00));
        // The ellipse touches the middle of each side of its box.
        let mut picture = Picture::blank(32, 32, 0xffffffff);
        picture.ellipse(2, 2, 30, 22, 0xff0000ff, false);
        assert_eq!(picture.pixel(16, 2), Some(0xff0000ff));
        assert_eq!(picture.pixel(16, 22), Some(0xff0000ff));
        assert_eq!(picture.pixel(2, 12), Some(0xff0000ff));
        assert_eq!(picture.pixel(30, 12), Some(0xff0000ff));
        assert_eq!(picture.pixel(16, 12), Some(0xffffffff), "and is hollow");
        // Filling inside it stops at the outline.
        picture.fill(16, 12, 0xffff0000);
        assert_eq!(picture.pixel(16, 12), Some(0xffff0000));
        assert_eq!(
            picture.pixel(0, 0),
            Some(0xffffffff),
            "outside is untouched"
        );
        picture.fill(0, 0, 0xff101010);
        assert_eq!(picture.pixel(31, 31), Some(0xff101010));
        assert_eq!(picture.pixel(16, 12), Some(0xffff0000));
    }
    #[test]
    fn a_drag_paints_and_a_shape_waits_for_the_release() {
        let mut app = paint();
        let (w, h) = (700, 460);
        app.press(w, h, TOOLS_W + 2, 2, 1);
        app.motion(w, h, TOOLS_W + 10, 2, 1);
        assert_eq!(
            app.picture.pixel(6, 2),
            Some(0xff000000),
            "the pencil draws"
        );
        assert!(app.modified);
        app.release(w, h, TOOLS_W + 10, 2, 1);
        // A shape only exists once the button comes up.
        app.set_tool(Tool::Rectangle);
        app.press(w, h, TOOLS_W + 20, 20, 1);
        app.motion(w, h, TOOLS_W + 40, 40, 1);
        assert_eq!(app.picture.pixel(20, 20), Some(0xffffffff));
        app.release(w, h, TOOLS_W + 40, 40, 1);
        assert_eq!(app.picture.pixel(20, 20), Some(0xff000000));
        assert_eq!(app.picture.pixel(30, 30), Some(0xffffffff), "not filled");
        // Undo restores what the last stroke covered.
        app.undo();
        assert_eq!(app.picture.pixel(20, 20), Some(0xffffffff));
        app.undo();
        assert_eq!(app.picture.pixel(6, 2), Some(0xffffffff));
        app.undo();
        assert_eq!(app.status, "Nothing to undo");
    }
    #[test]
    fn the_right_button_and_the_picker_choose_colors() {
        let mut app = paint();
        let (w, h) = (700, 460);
        let swatch = Paint::swatch_rect(w, h, 6);
        app.press(w, h, swatch.x + 2, swatch.y + 2, 1);
        assert_eq!(app.foreground, PALETTE[6]);
        app.press(w, h, swatch.x + 2, swatch.y + 2, 2);
        assert_eq!(app.background, PALETTE[6]);
        // The two swatches swap, and the eraser always uses the background.
        app.foreground = 0xff123456;
        let colors = Paint::colors_rect(h);
        app.press(w, h, colors.x + 2, colors.y + 2, 1);
        assert_eq!(app.background, 0xff123456);
        app.set_tool(Tool::Eraser);
        assert_eq!(app.color(1), app.background);
        // The right button paints with the background color.
        app.set_tool(Tool::Pencil);
        assert_eq!(app.color(2), app.background);
        // "More colors" asks the session to run the picker.
        let more = Paint::more_rect(w, h);
        app.press(w, h, more.x + 2, more.y + 2, 1);
        assert_eq!(app.wants_color, Some(true));
        app.wants_color = None;
        app.press(w, h, more.x + 2, more.y + 2, 2);
        assert_eq!(
            app.wants_color,
            Some(false),
            "the right button is the other one"
        );
        // Picking reads a color out of the picture.
        app.picture.plot(4, 4, 1, 0xff00ff00);
        app.set_tool(Tool::Pick);
        app.press(w, h, TOOLS_W + 4, 4, 1);
        assert_eq!(app.foreground, 0xff00ff00);
    }
    #[test]
    fn tools_and_widths_come_from_the_column() {
        let mut app = paint();
        let (w, h) = (700, 460);
        let r = Tool::rect(3);
        app.press(w, h, r.x + 2, r.y + 2, 1);
        assert_eq!(app.tool, Tool::ALL[3]);
        let r = Paint::size_rect(2);
        app.press(w, h, r.x + 2, r.y + 2, 1);
        assert_eq!(app.size, SIZES[2]);
        // Digits select tools too, 1 first and 0 last.
        app.key("1", 0, w, h);
        assert_eq!(app.tool, Tool::Pencil);
        app.key("0", 0, w, h);
        assert_eq!(app.tool, Tool::FilledEllipse);
    }
    #[test]
    fn save_shortcut_requires_control() {
        let mut app = paint();
        for text in ["\u{13}", "s", "S"] {
            app.key(text, 0, 700, 460);
            assert_eq!(app.wants_save, None);
            app.key(text, 1, 700, 460);
            assert_eq!(app.wants_save.take(), Some(false));
        }
    }
    #[test]
    fn failed_save_preserves_unsaved_changes() {
        let mut app = paint();
        // A file cannot be used as the parent directory of the destination.
        app.path = Some(PathBuf::from(file!()).join("picture.qoi"));
        app.modified = true;
        let path = app.path.clone();
        assert!(app.save(Path::new("."), false).is_err());
        assert_eq!(app.path, path);
        assert!(app.modified);
    }
    #[test]
    fn saving_writes_a_picture_that_opens_again() {
        let dir = std::env::temp_dir().join(format!("hos-paint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut app = paint();
        app.picture.plot(3, 3, 1, 0xff00ff00);
        app.modified = true;
        app.key("\u{13}", 1, 700, 460);
        let as_copy = app.wants_save.take().unwrap();
        let path = app.save(&dir, as_copy).unwrap();
        assert!(path.starts_with(&dir) && path.extension().is_some_and(|e| e == "qoi"));
        assert!(!app.modified);
        assert_eq!(app.title(), path.file_name().unwrap().to_string_lossy());
        // Saving again overwrites the same file; a copy makes another.
        app.snapshot();
        app.picture.plot(3, 3, 1, 0xffff0000);
        app.modified = true;
        app.key("\u{13}", 1, 700, 460);
        let as_copy = app.wants_save.take().unwrap();
        let again = app.save(&dir, as_copy).unwrap();
        assert_eq!(again, path);
        assert_eq!(qoi::load(&path).unwrap().pixels[3 * 64 + 3], 0xffff0000);
        app.undo();
        assert!(app.modified, "undo after saving leaves unsaved changes");
        app.save(&dir, false).unwrap();
        let copy = app.save(&dir, true).unwrap();
        assert_ne!(copy, path);
        // What comes back is the same picture.
        app.picture.surface.pixels_mut().fill(0xff000000);
        app.path = Some(path);
        app.reload().unwrap();
        assert_eq!(app.picture.pixel(3, 3), Some(0xff00ff00));
        assert_eq!(app.picture.width(), 64);
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn the_window_draws_without_a_session() {
        let mut app = paint();
        app.status = "Ready".into();
        let (w, h) = (400, 300);
        let mut surface = Surface::new(w, h);
        app.draw(&mut surface, &Font::builtin());
        // The picture sits in the canvas area, the tool column beside it.
        assert_eq!(surface.pixels()[10 * w + TOOLS_W as usize + 10], 0xffffffff);
        assert_ne!(surface.pixels()[10 * w + 4], 0xffffffff);
        // A shape in progress is drawn without being committed.
        app.set_tool(Tool::Line);
        app.press(w as i32, h as i32, TOOLS_W + 2, 2, 1);
        app.motion(w as i32, h as i32, TOOLS_W + 30, 2, 1);
        app.draw(&mut surface, &Font::builtin());
        assert_eq!(surface.pixels()[2 * w + TOOLS_W as usize + 20], 0xff000000);
        assert_eq!(app.picture.pixel(20, 2), Some(0xffffffff));
    }
}
