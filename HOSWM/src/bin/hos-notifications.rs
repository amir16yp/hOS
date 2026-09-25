//! `hos-notifications`: browse the notification log written by the session.
//!
//! The log is the binary `~/.hoswm/toastdb` described in `hoswm::toast`. This
//! program only reads it, reloading when the file changes on disk.
use hoswm::{
    client::{Client, Event, Menu, MenuItem, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT, Window},
    font::Font,
    surface::Surface,
    toast::{self, Record},
};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, SystemTime},
};

const BACKGROUND: u32 = 0xff0b0f0e;
const HEADER: u32 = 0xff72dbac;
const TEXT: u32 = 0xffdfe4e1;
const DIM: u32 = 0xff8d9a93;
const ROW: i32 = 14;
const TOP: i32 = 44;

const MENU_RELOAD: u32 = 1;
const MENU_CLOSE: u32 = 2;
const MENU_NEWEST: u32 = 3;
const MENU_OLDEST: u32 = 4;
const MENU_TOP: u32 = 5;
const MENU_BOTTOM: u32 = 6;

struct Log {
    path: PathBuf,
    records: Vec<Record>,
    /// Damage report from the last read, shown instead of hidden.
    status: String,
    changed: Option<SystemTime>,
    newest_first: bool,
    offset: usize,
}
impl Log {
    fn new(path: PathBuf) -> Self {
        let mut log = Self {
            path,
            records: Vec::new(),
            status: String::new(),
            changed: None,
            newest_first: true,
            offset: 0,
        };
        log.reload();
        log
    }
    fn modified(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path).ok()?.modified().ok()
    }
    fn reload(&mut self) {
        self.changed = self.modified();
        match toast::read(&self.path) {
            Ok((mut records, damage)) => {
                if self.newest_first {
                    records.reverse();
                }
                self.status = match (&damage, records.len()) {
                    (Some(damage), _) => format!("Damaged log: {damage}"),
                    (None, 0) => "No notifications recorded yet".into(),
                    (None, n) => format!("{n} notification{}", if n == 1 { "" } else { "s" }),
                };
                self.records = records;
            }
            Err(e) => {
                self.records.clear();
                self.status = format!("{}: {e}", self.path.display());
            }
        }
        self.offset = 0;
    }
    /// Reload when the file grew or was replaced since the last read.
    fn refresh(&mut self) -> bool {
        if self.modified() != self.changed {
            let offset = self.offset;
            self.reload();
            self.offset = offset.min(self.records.len().saturating_sub(1));
            return true;
        }
        false
    }
    fn order(&mut self, newest_first: bool) -> bool {
        if self.newest_first == newest_first {
            return false;
        }
        self.newest_first = newest_first;
        self.records.reverse();
        self.offset = 0;
        true
    }
    fn scroll(&mut self, rows: i32, visible: usize) -> bool {
        let last = self.records.len().saturating_sub(visible);
        let offset = (self.offset as i64 + rows as i64).clamp(0, last as i64) as usize;
        let moved = offset != self.offset;
        self.offset = offset;
        moved
    }
    fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        surface.pixels_mut().fill(BACKGROUND);
        let width = surface.width() as i32;
        font.draw(surface, 12, 12, "Notification log", HEADER);
        let path = self.path.display().to_string();
        let room = ((width - 24) / 8).max(8) as usize;
        let path: String = if path.chars().count() > room {
            format!("...{}", &path[path.len() - room + 3..])
        } else {
            path
        };
        font.draw(surface, 12, 26, &path, DIM);
        surface.fill_rect(12, TOP - 6, width - 24, 1, 0xff26302b);
        let visible = self.visible(surface.height());
        for (row, record) in self.records.iter().skip(self.offset).take(visible).enumerate() {
            let y = TOP + row as i32 * ROW;
            surface.fill_rect(12, y + 1, 6, 8, record.color);
            font.draw(surface, 24, y, &toast::format_time(record.timestamp_ms), DIM);
            font.draw(surface, 24 + 160, y, &seconds(record.shown_ms), DIM);
            let room = ((width - 24 - 232) / 8).max(0) as usize;
            let text: String = record.text.chars().take(room).collect();
            font.draw(surface, 24 + 224, y, &text, TEXT);
        }
        let bottom = surface.height() as i32 - 18;
        surface.fill_rect(12, bottom - 6, width - 24, 1, 0xff26302b);
        let position = if self.records.len() > visible {
            format!(
                "  [{}-{} of {}]",
                self.offset + 1,
                (self.offset + visible).min(self.records.len()),
                self.records.len()
            )
        } else {
            String::new()
        };
        font.draw(
            surface,
            12,
            bottom,
            &format!(
                "{}{position}   {} first",
                self.status,
                if self.newest_first { "newest" } else { "oldest" }
            ),
            DIM,
        );
    }
    fn visible(&self, height: usize) -> usize {
        ((height as i32 - TOP - 26) / ROW).max(1) as usize
    }
}
/// Render a duration the way a person reads it: "0.9s", "4.0s", "1m02s".
fn seconds(milliseconds: u32) -> String {
    if milliseconds < 60_000 {
        format!("{}.{}s", milliseconds / 1000, milliseconds % 1000 / 100)
    } else {
        format!("{}m{:02}s", milliseconds / 60_000, milliseconds / 1000 % 60)
    }
}
fn menus(newest_first: bool) -> Vec<Menu> {
    vec![
        Menu::new(
            "Log",
            vec![
                MenuItem::new(MENU_RELOAD, "Reload").shortcut("R"),
                MenuItem::rule(),
                MenuItem::new(MENU_CLOSE, "Close").shortcut("Esc"),
            ],
        ),
        Menu::new(
            "View",
            vec![
                MenuItem::new(MENU_NEWEST, "Newest first").checked(newest_first),
                MenuItem::new(MENU_OLDEST, "Oldest first").checked(!newest_first),
                MenuItem::rule(),
                MenuItem::new(MENU_TOP, "Go to top").shortcut("Home"),
                MenuItem::new(MENU_BOTTOM, "Go to end").shortcut("End"),
            ],
        ),
    ]
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::connect()?;
    let window = client.create("Notifications", 620, 400, 0xffe4c878)?;
    client.flags(window, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)?;
    let mut log = Log::new(hoswm::config::directory().join("toastdb"));
    client.set_menus(window, &menus(log.newest_first))?;
    let font = Font::builtin();
    let (mut width, mut height, _) = client.size(window)?;
    let mut surface = Surface::new(width as usize, height as usize);
    let mut dirty = true;
    loop {
        let (new_width, new_height, minimized) = client.size(window)?;
        if (new_width, new_height) != (width, height) {
            width = new_width;
            height = new_height;
            surface.reset(width as usize, height as usize, BACKGROUND);
            dirty = true;
        }
        let visible = log.visible(height as usize);
        for _ in 0..32 {
            let Some(Event {
                kind,
                control,
                text,
            }) = client.poll(window)?
            else {
                break;
            };
            dirty = true;
            match kind {
                // Key bytes: arrows and paging scroll, R reloads, Escape quits.
                6 => match text.as_str() {
                    "\u{1b}" => return close(&client, window),
                    "r" | "R" => log.reload(),
                    "\u{1b}[A" => drop(log.scroll(-1, visible)),
                    "\u{1b}[B" => drop(log.scroll(1, visible)),
                    "\u{1b}[5~" => drop(log.scroll(-(visible as i32), visible)),
                    "\u{1b}[6~" => drop(log.scroll(visible as i32, visible)),
                    "\u{1b}[H" => drop(log.scroll(i32::MIN / 2, visible)),
                    "\u{1b}[F" => drop(log.scroll(i32::MAX / 2, visible)),
                    _ => (),
                },
                10 => {
                    // A wheel notch scrolls three rows, positive is upwards.
                    log.scroll(-(control as i32) * 3, visible);
                }
                11 => match control {
                    MENU_RELOAD => log.reload(),
                    MENU_CLOSE => return close(&client, window),
                    MENU_NEWEST | MENU_OLDEST => {
                        if log.order(control == MENU_NEWEST) {
                            client.set_menus(window, &menus(log.newest_first))?;
                        }
                    }
                    MENU_TOP => drop(log.scroll(i32::MIN / 2, visible)),
                    MENU_BOTTOM => drop(log.scroll(i32::MAX / 2, visible)),
                    _ => (),
                },
                7 | 9 => return close(&client, window),
                _ => (),
            }
        }
        dirty |= log.refresh();
        if dirty && !minimized {
            log.draw(&mut surface, &font);
            client.present(window, width, height, surface.pixels())?;
            dirty = false;
        }
        thread::sleep(Duration::from_millis(if minimized { 120 } else { 40 }));
    }
}
fn close(client: &Client, window: Window) -> Result<(), Box<dyn std::error::Error>> {
    let _ = client.close(window);
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-notifications: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_read_like_durations() {
        assert_eq!(seconds(0), "0.0s");
        assert_eq!(seconds(900), "0.9s");
        assert_eq!(seconds(4012), "4.0s");
        assert_eq!(seconds(62_500), "1m02s");
    }
    #[test]
    fn the_log_view_reloads_reorders_and_scrolls() {
        let dir = std::env::temp_dir().join(format!("hoswm-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("toastdb");
        let record = |n: u64| Record {
            timestamp_ms: 1_700_000_000_000 + n * 1000,
            requested_ms: 4000,
            shown_ms: 4000,
            color: 0xff72dbac,
            text: format!("notification {n}"),
        };
        for n in 0..5 {
            toast::append(&path, &record(n)).unwrap();
        }
        let mut log = Log::new(path.clone());
        assert_eq!(log.records.len(), 5);
        assert_eq!(log.status, "5 notifications");
        assert_eq!(log.records[0].text, "notification 4", "newest first");
        assert!(log.order(false));
        assert_eq!(log.records[0].text, "notification 0");
        assert!(!log.order(false));
        assert!(log.scroll(3, 2));
        assert_eq!(log.offset, 3);
        assert!(!log.scroll(9, 2), "scrolling stops at the end");
        assert_eq!(log.offset, 3);
        assert!(log.scroll(i32::MIN / 2, 2));
        assert_eq!(log.offset, 0);
        assert!(!log.refresh(), "an unchanged file is not reread");
        toast::append(&path, &record(9)).unwrap();
        // Modification times have coarse resolution on some filesystems.
        std::fs::File::open(&path).unwrap();
        log.changed = None;
        assert!(log.refresh());
        assert_eq!(log.records.len(), 6);
        let mut surface = Surface::new(620, 400);
        log.draw(&mut surface, &Font::builtin());
        assert!(surface.pixels().contains(&0xff72dbac), "color swatches drawn");
        // A log that is not a log reports the problem instead of failing.
        std::fs::write(&path, b"garbage").unwrap();
        log.changed = None;
        log.refresh();
        assert!(log.records.is_empty());
        assert!(log.status.contains("toastdb"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
