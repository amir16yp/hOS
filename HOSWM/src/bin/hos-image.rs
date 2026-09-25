//! `hos-image`: view a QOI image, and generate the previews `hos-files` shows.
//!
//! ```text
//! hos-image FILE.qoi              open the viewer
//! hos-image --preview FILE.qoi    write a cached preview and print its path
//! ```
use hoswm::{
    client::{Client, Event, Menu, MenuItem, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT, Window},
    font::Font,
    preview, qoi,
    surface::Surface,
};
use std::{path::PathBuf, thread, time::Duration};

const BACKGROUND: u32 = 0xff0b0f0e;
const STATUS: i32 = 20;
const ACCENT: u32 = 0xff80afff;
const DIM: u32 = 0xff8d9a93;
/// Zoom steps in percent, from a thumbnail to pixel inspection.
const ZOOMS: [u32; 12] = [5, 10, 25, 50, 75, 100, 150, 200, 300, 400, 600, 800];

const MENU_RELOAD: u32 = 1;
const MENU_CLOSE: u32 = 2;
const MENU_ZOOM_IN: u32 = 3;
const MENU_ZOOM_OUT: u32 = 4;
const MENU_ACTUAL: u32 = 5;
const MENU_FIT: u32 = 6;
const MENU_NEAREST: u32 = 7;
const MENU_SMOOTH: u32 = 8;

struct View {
    path: PathBuf,
    image: qoi::Image,
    /// Percent of the original size.
    zoom: u32,
    /// Top-left of the image in window coordinates.
    x: i32,
    y: i32,
    smooth: bool,
    fitted: bool,
}
impl View {
    fn open(path: PathBuf) -> Result<Self, String> {
        let image = qoi::load(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            path,
            image,
            zoom: 100,
            x: 0,
            y: 0,
            smooth: true,
            fitted: true,
        })
    }
    fn reload(&mut self) -> Result<(), String> {
        self.image = qoi::load(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok(())
    }
    fn size(&self) -> (i32, i32) {
        (
            (self.image.width as u32 * self.zoom / 100).max(1) as i32,
            (self.image.height as u32 * self.zoom / 100).max(1) as i32,
        )
    }
    /// Scale the image to the window, never enlarging past its own size.
    fn fit(&mut self, width: i32, height: i32) {
        let area = height - STATUS;
        let zoom = (width as u32 * 100 / self.image.width.max(1) as u32)
            .min(area.max(1) as u32 * 100 / self.image.height.max(1) as u32)
            .clamp(ZOOMS[0], 100);
        self.zoom = zoom;
        self.fitted = true;
        self.center(width, height);
    }
    fn center(&mut self, width: i32, height: i32) {
        let (w, h) = self.size();
        self.x = (width - w) / 2;
        self.y = (height - STATUS - h) / 2;
    }
    /// Zoom one step, keeping the window centre over the same image point.
    fn step(&mut self, inwards: bool, width: i32, height: i32) {
        let index = ZOOMS.iter().position(|z| *z >= self.zoom).unwrap_or(0);
        let next = if inwards {
            ZOOMS.get(index + 1).copied().or(ZOOMS.last().copied())
        } else {
            index.checked_sub(1).and_then(|i| ZOOMS.get(i).copied())
        };
        let Some(next) = next.filter(|next| *next != self.zoom) else {
            return;
        };
        let (cx, cy) = (width / 2, (height - STATUS) / 2);
        self.x = cx - (cx - self.x) * next as i32 / self.zoom as i32;
        self.y = cy - (cy - self.y) * next as i32 / self.zoom as i32;
        self.zoom = next;
        self.fitted = false;
        self.clamp(width, height);
    }
    fn set_zoom(&mut self, zoom: u32, width: i32, height: i32) {
        self.zoom = zoom;
        self.fitted = false;
        self.center(width, height);
    }
    /// Keep part of the image on screen whichever way it is panned.
    fn clamp(&mut self, width: i32, height: i32) {
        let (w, h) = self.size();
        let area = height - STATUS;
        self.x = self.x.clamp((width - w).min(0) - 40, (width - w).max(0) + 40);
        self.y = self.y.clamp((area - h).min(0) - 40, (area - h).max(0) + 40);
    }
    fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        surface.pixels_mut().fill(BACKGROUND);
        let (w, h) = self.size();
        if self.smooth {
            surface.draw_image_smooth(self.x, self.y, w, h, self.image.view());
        } else {
            surface.draw_image_scaled(self.x, self.y, w, h, self.image.view());
        }
        let width = surface.width() as i32;
        let bar = surface.height() as i32 - STATUS;
        surface.fill_rect(0, bar, width, STATUS, 0xff141a18);
        surface.fill_rect(0, bar, width, 1, 0xff303936);
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let room = ((width - 210) / 8).max(4) as usize;
        let name: String = name.chars().take(room).collect();
        font.draw(surface, 8, bar + 6, &name, ACCENT);
        font.draw(
            surface,
            width - 200,
            bar + 6,
            &format!(
                "{}x{}  {}%  {}",
                self.image.width,
                self.image.height,
                self.zoom,
                if self.smooth { "smooth" } else { "nearest" }
            ),
            DIM,
        );
    }
}
fn menus(view: &View) -> Vec<Menu> {
    vec![
        Menu::new(
            "Image",
            vec![
                MenuItem::new(MENU_RELOAD, "Reload").shortcut("R"),
                MenuItem::rule(),
                MenuItem::new(MENU_CLOSE, "Close").shortcut("Esc"),
            ],
        ),
        Menu::new(
            "View",
            vec![
                MenuItem::new(MENU_ZOOM_IN, "Zoom in").shortcut("+"),
                MenuItem::new(MENU_ZOOM_OUT, "Zoom out").shortcut("-"),
                MenuItem::new(MENU_ACTUAL, "Actual size").shortcut("1"),
                MenuItem::new(MENU_FIT, "Fit to window").shortcut("0").checked(view.fitted),
                MenuItem::rule(),
                MenuItem::new(MENU_NEAREST, "Nearest pixels").checked(!view.smooth),
                MenuItem::new(MENU_SMOOTH, "Smooth scaling").checked(view.smooth),
            ],
        ),
    ]
}
fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let mut file = None;
    let mut size = preview::SIZE;
    let mut generate = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "-h" | "--help" => {
                println!(
                    "Usage: hos-image FILE.qoi\n       hos-image --preview FILE.qoi [--size N]"
                );
                return Ok(());
            }
            "--preview" => generate = true,
            "--size" => {
                size = arguments
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|n| (8..=1024).contains(n))
                    .ok_or("--size needs a number between 8 and 1024")?;
            }
            other if other.starts_with('-') && file.is_none() => {
                return Err(format!("{other}: unknown option"));
            }
            other => file = Some(PathBuf::from(other)),
        }
    }
    let file = file.ok_or("no image given; try hos-image --help")?;
    if generate {
        let path = preview::path(&file, size).map_err(|e| format!("{}: {e}", file.display()))?;
        preview::generate(&file, size).map_err(|e| format!("{}: {e}", file.display()))?;
        println!("{}", path.display());
        return Ok(());
    }
    let client = Client::connect().map_err(|e| e.to_string())?;
    let mut view = match View::open(file) {
        Ok(view) => view,
        Err(e) => {
            // Report the failure through the session rather than a dead window.
            let _ = client.ask(
                "Cannot open image",
                &e,
                hoswm::client::ANSWER_BUTTONS_OK,
                hoswm::client::SEVERITY_ERROR,
            );
            return Err(e);
        }
    };
    let window = client
        .create("Image", 640, 420, ACCENT)
        .map_err(|e| e.to_string())?;
    client
        .flags(window, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)
        .map_err(|e| e.to_string())?;
    let font = Font::builtin();
    let (mut width, mut height, _) = client.size(window).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(width as usize, height as usize);
    view.fit(width as i32, height as i32);
    let mut menu_state = String::new();
    let mut drag: Option<(i32, i32, i32, i32)> = None;
    let mut dirty = true;
    loop {
        let (new_width, new_height, minimized) = client.size(window).map_err(|e| e.to_string())?;
        if (new_width, new_height) != (width, height) {
            width = new_width;
            height = new_height;
            surface.reset(width as usize, height as usize, BACKGROUND);
            if view.fitted {
                view.fit(width as i32, height as i32);
            } else {
                view.clamp(width as i32, height as i32);
            }
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
                6 => match text.as_str() {
                    "\u{1b}" => return close(&client, window),
                    "+" | "=" => view.step(true, w, h),
                    "-" | "_" => view.step(false, w, h),
                    "0" => view.fit(w, h),
                    "1" => view.set_zoom(100, w, h),
                    "f" | "F" => view.smooth = !view.smooth,
                    "r" | "R" => view.reload()?,
                    "\u{1b}[A" => view.y += 40,
                    "\u{1b}[B" => view.y -= 40,
                    "\u{1b}[D" => view.x += 40,
                    "\u{1b}[C" => view.x -= 40,
                    _ => (),
                },
                // Raw pointer: press starts a drag, motion pans, release ends.
                8 => {
                    let mut parts = text.split_whitespace();
                    let x = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let action = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    match (control, action) {
                        (1, 1) => drag = Some((x, y, view.x, view.y)),
                        (1, 0) => drag = None,
                        (0, _) => {
                            if let Some((sx, sy, ox, oy)) = drag {
                                view.x = ox + x - sx;
                                view.y = oy + y - sy;
                                view.fitted = false;
                            }
                        }
                        _ => (),
                    }
                }
                10 => view.step(control as i32 > 0, w, h),
                11 => match control {
                    MENU_RELOAD => view.reload()?,
                    MENU_CLOSE => return close(&client, window),
                    MENU_ZOOM_IN => view.step(true, w, h),
                    MENU_ZOOM_OUT => view.step(false, w, h),
                    MENU_ACTUAL => view.set_zoom(100, w, h),
                    MENU_FIT => view.fit(w, h),
                    MENU_NEAREST => view.smooth = false,
                    MENU_SMOOTH => view.smooth = true,
                    _ => (),
                },
                7 | 9 => return close(&client, window),
                _ => (),
            }
            view.clamp(w, h);
        }
        // Menu check marks follow the view, so only resend them when they move.
        let state = format!("{}{}", view.fitted, view.smooth);
        if state != menu_state {
            client
                .set_menus(window, &menus(&view))
                .map_err(|e| e.to_string())?;
            menu_state = state;
        }
        if dirty && !minimized {
            view.draw(&mut surface, &font);
            client
                .present(window, width, height, surface.pixels())
                .map_err(|e| e.to_string())?;
            dirty = false;
        }
        thread::sleep(Duration::from_millis(if minimized { 120 } else { 16 }));
    }
}
fn close(client: &Client, window: Window) -> Result<(), String> {
    let _ = client.close(window);
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-image: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view(width: usize, height: usize) -> View {
        View {
            path: PathBuf::from("/tmp/example.qoi"),
            image: qoi::Image {
                width,
                height,
                pixels: vec![0xff112233; width * height],
            },
            zoom: 100,
            x: 0,
            y: 0,
            smooth: true,
            fitted: true,
        }
    }
    #[test]
    fn fitting_never_enlarges_and_centres_the_image() {
        let mut v = view(1600, 800);
        v.fit(640, 420);
        assert_eq!(v.zoom, 40, "limited by the width");
        assert_eq!(v.size(), (640, 320));
        assert_eq!((v.x, v.y), (0, 40));
        let mut small = view(40, 20);
        small.fit(640, 420);
        assert_eq!(small.zoom, 100, "small images stay at their own size");
        assert_eq!((small.x, small.y), (300, 190));
    }
    #[test]
    fn zoom_steps_stop_at_the_ends_and_hold_the_centre() {
        let mut v = view(400, 400);
        v.set_zoom(100, 640, 420);
        let centre = (640 / 2 - v.x, (420 - STATUS) / 2 - v.y);
        v.step(true, 640, 420);
        assert_eq!(v.zoom, 150);
        assert!(!v.fitted);
        let scaled = (640 / 2 - v.x, (420 - STATUS) / 2 - v.y);
        assert_eq!(scaled.0, centre.0 * 3 / 2, "the centre point is preserved");
        for _ in 0..20 {
            v.step(true, 640, 420);
        }
        assert_eq!(v.zoom, *ZOOMS.last().unwrap());
        for _ in 0..20 {
            v.step(false, 640, 420);
        }
        assert_eq!(v.zoom, ZOOMS[0]);
    }
    #[test]
    fn panning_keeps_part_of_the_image_visible() {
        let mut v = view(4000, 4000);
        v.set_zoom(100, 640, 420);
        v.x = 100_000;
        v.y = -100_000;
        v.clamp(640, 420);
        assert!(v.x <= 640 + 40 && v.y >= (420 - STATUS - 4000) - 40);
        assert!(v.x >= -4000 && v.y <= 420);
    }
    #[test]
    fn the_viewer_draws_the_image_and_its_status_bar() {
        let mut v = view(8, 8);
        v.set_zoom(400, 200, 120);
        let mut surface = Surface::new(200, 120);
        v.draw(&mut surface, &Font::builtin());
        assert!(surface.pixels().contains(&0xff112233), "image drawn");
        assert_eq!(surface.pixels()[(120 - STATUS as usize) * 200], 0xff303936);
        v.smooth = false;
        v.draw(&mut surface, &Font::builtin());
        assert!(surface.pixels().contains(&0xff112233));
        assert_eq!(menus(&v)[1].items[6].checked, false, "the mode is shown");
    }
}
