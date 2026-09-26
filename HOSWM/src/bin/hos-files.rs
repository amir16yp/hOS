//! `hos-files`: a tabbed file browser with copy and paste, image previews and
//! "open terminal here".
//!
//! Files are copied through the session clipboard, so a path copied in one
//! window can be pasted in another. Previews come from the cache shared with
//! `hos-image`, which this program fills for images it has not seen before.
use hoswm::{
    client::{
        Client, Event, Menu, MenuItem, Window, ANSWER_BUTTONS_YES_NO, ANSWER_YES, SEVERITY_ERROR,
        SEVERITY_QUESTION, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT,
    },
    config,
    desktop::Rect,
    font::Font,
    preview, qoi,
    surface::Surface,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

const BACKGROUND: u32 = 0xff0b0f0e;
const PANEL: u32 = 0xff141a18;
const EDGE: u32 = 0xff303936;
const ACCENT: u32 = 0xff72dbac;
const TEXT: u32 = 0xffdfe4e1;
const DIM: u32 = 0xff8d9a93;
const SELECTED: u32 = 0xff315b83;
const TABS: i32 = 22;
const PATH: i32 = 18;
const TOP: i32 = TABS + PATH;
const ROW: i32 = 14;
const STATUS: i32 = 20;
const PREVIEW: i32 = 168;
const TAB_WIDTH: i32 = 120;
/// Clipboard marker, so file operations ignore ordinary copied text.
const MARK: &str = "hoswm-files:";

const CMD_OPEN: u32 = 1;
const CMD_TERMINAL: u32 = 2;
const CMD_COPY: u32 = 3;
const CMD_CUT: u32 = 4;
const CMD_PASTE: u32 = 5;
const CMD_DELETE: u32 = 6;
const CMD_NEW_TAB: u32 = 7;
const CMD_CLOSE_TAB: u32 = 8;
const CMD_REFRESH: u32 = 9;
const CMD_PREVIEWS: u32 = 10;
const CMD_HOME: u32 = 11;
const CMD_PARENT: u32 = 12;
const CMD_CLOSE: u32 = 13;

#[derive(Clone)]
struct Entry {
    name: String,
    directory: bool,
    size: u64,
}
impl Entry {
    fn image(&self) -> bool {
        !self.directory && self.name.to_ascii_lowercase().ends_with(".qoi")
    }
}
/// One open directory: its listing, selection and scroll position.
struct Tab {
    path: PathBuf,
    entries: Vec<Entry>,
    selected: usize,
    offset: usize,
}
impl Tab {
    fn new(path: PathBuf) -> (Self, Option<String>) {
        let mut tab = Self {
            path,
            entries: Vec::new(),
            selected: 0,
            offset: 0,
        };
        let error = tab.reload();
        (tab, error)
    }
    fn title(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
    /// Read the directory: parent first, then directories, then files.
    fn reload(&mut self) -> Option<String> {
        let mut entries = Vec::new();
        let mut error = None;
        if self.path.parent().is_some() {
            entries.push(Entry {
                name: "..".into(),
                directory: true,
                size: 0,
            });
        }
        match fs::read_dir(&self.path) {
            Ok(listing) => {
                let mut items: Vec<Entry> = listing
                    .flatten()
                    .map(|entry| {
                        let meta = entry.metadata().ok();
                        Entry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            directory: meta.as_ref().is_some_and(|m| m.is_dir()),
                            size: meta.as_ref().map_or(0, |m| m.len()),
                        }
                    })
                    .collect();
                items.sort_by(|a, b| {
                    b.directory.cmp(&a.directory).then_with(|| {
                        a.name.to_lowercase().cmp(&b.name.to_lowercase())
                    })
                });
                entries.extend(items);
            }
            Err(e) => error = Some(format!("{}: {e}", self.path.display())),
        }
        self.entries = entries;
        self.selected = self.selected.min(self.entries.len().saturating_sub(1));
        error
    }
    fn selection(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }
    /// Absolute path of the selection, except the parent entry.
    fn selected_path(&self) -> Option<PathBuf> {
        let entry = self.selection()?;
        (entry.name != "..").then(|| self.path.join(&entry.name))
    }
    fn go(&mut self, path: PathBuf) -> Option<String> {
        self.path = path;
        self.selected = 0;
        self.offset = 0;
        self.reload()
    }
    fn move_selection(&mut self, rows: i32, visible: usize) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as i64 - 1;
        self.selected = (self.selected as i64 + rows as i64).clamp(0, last) as usize;
        self.offset = self
            .offset
            .min(self.selected)
            .max((self.selected + 1).saturating_sub(visible));
    }
}

/// What a copy or cut put on the session clipboard.
struct Pending {
    path: PathBuf,
    cut: bool,
}
fn parse_clipboard(text: &str) -> Option<Pending> {
    let rest = text.strip_prefix(MARK)?;
    let (action, path) = rest.split_once('\n')?;
    Some(Pending {
        path: PathBuf::from(path),
        cut: action == "cut",
    })
}
fn human(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = size;
    let mut unit = 0;
    while value >= 10_000 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    format!("{value}{}", UNITS[unit])
}
/// A destination that does not exist yet, so a paste never overwrites.
fn unique(directory: &Path, name: &str) -> PathBuf {
    let candidate = directory.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    for attempt in 1..1000 {
        let candidate = directory.join(format!("{stem} ({attempt}){extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    directory.join(name)
}
/// Copy a file, or a directory and everything under it.
fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(source)?;
    if meta.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if meta.file_type().is_symlink() {
        let target = fs::read_link(source)?;
        return std::os::unix::fs::symlink(target, destination);
    }
    fs::copy(source, destination).map(|_| ())
}

struct Browser {
    tabs: Vec<Tab>,
    active: usize,
    previews: bool,
    status: String,
    /// Decoded preview of the current selection, and the file it came from.
    preview: Option<(PathBuf, qoi::Image)>,
    popup: Option<(i32, i32)>,
}
impl Browser {
    fn new(path: PathBuf) -> Self {
        let (tab, error) = Tab::new(path);
        Self {
            tabs: vec![tab],
            active: 0,
            previews: true,
            status: error.unwrap_or_default(),
            preview: None,
            popup: None,
        }
    }
    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }
    fn current(&self) -> &Tab {
        &self.tabs[self.active]
    }
    fn visible(&self, height: i32) -> usize {
        ((height - TOP - STATUS) / ROW).max(1) as usize
    }
    fn list_width(&self, width: i32) -> i32 {
        if self.previews { width - PREVIEW } else { width }
    }
    /// Load the preview for the selection, from the cache or by generating it.
    fn refresh_preview(&mut self) {
        let wanted = self
            .previews
            .then(|| self.current().selection().filter(|e| e.image()))
            .flatten()
            .and_then(|_| self.current().selected_path());
        match wanted {
            Some(path) if self.preview.as_ref().is_none_or(|(p, _)| *p != path) => {
                self.preview = preview::thumbnail(&path, preview::SIZE)
                    .ok()
                    .map(|image| (path, image));
            }
            None => self.preview = None,
            _ => (),
        }
    }
    fn rows(&self, width: i32, height: i32) -> Vec<(Rect, usize)> {
        let visible = self.visible(height);
        (0..visible.min(self.current().entries.len().saturating_sub(self.current().offset)))
            .map(|row| {
                (
                    Rect {
                        x: 0,
                        y: TOP + row as i32 * ROW,
                        w: self.list_width(width),
                        h: ROW,
                    },
                    self.current().offset + row,
                )
            })
            .collect()
    }
    fn tab_rects(&self) -> Vec<Rect> {
        (0..self.tabs.len())
            .map(|index| Rect {
                x: index as i32 * TAB_WIDTH,
                y: 0,
                w: TAB_WIDTH - 2,
                h: TABS - 2,
            })
            .collect()
    }
    fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        let (width, height) = (surface.width() as i32, surface.height() as i32);
        surface.pixels_mut().fill(BACKGROUND);
        // Tabs.
        surface.fill_rect(0, 0, width, TABS, PANEL);
        for (index, rect) in self.tab_rects().into_iter().enumerate() {
            let active = index == self.active;
            surface.fill_rect(rect.x, rect.y, rect.w, rect.h, if active { SELECTED } else { 0xff1d2623 });
            let title: String = self.tabs[index].title().chars().take(13).collect();
            font.draw(
                surface,
                rect.x + 8,
                rect.y + 6,
                &title,
                if active { 0xffffffff } else { DIM },
            );
        }
        let plus = self.tabs.len() as i32 * TAB_WIDTH;
        font.draw(surface, plus + 6, 6, "+", ACCENT);
        // Path.
        surface.fill_rect(0, TABS, width, PATH, 0xff101614);
        let path = self.current().path.display().to_string();
        let room = ((width - 16) / 8).max(4) as usize;
        let path: String = if path.chars().count() > room {
            path.chars().skip(path.chars().count() - room + 3).collect::<String>()
        } else {
            path
        };
        font.draw(surface, 8, TABS + 5, &path, ACCENT);
        // Listing.
        let list = self.list_width(width);
        for (rect, index) in self.rows(width, height) {
            let entry = &self.current().entries[index];
            if index == self.current().selected {
                surface.fill_rect(rect.x, rect.y, rect.w, rect.h, SELECTED);
            }
            let color = if entry.directory { ACCENT } else { TEXT };
            let room = ((list - 90) / 8).max(4) as usize;
            let name: String = entry.name.chars().take(room).collect();
            font.draw(surface, 8, rect.y + 3, &name, color);
            let detail = if entry.directory {
                if entry.name == ".." { String::new() } else { "dir".into() }
            } else {
                human(entry.size)
            };
            font.draw(
                surface,
                list - 8 - detail.chars().count() as i32 * 8,
                rect.y + 3,
                &detail,
                DIM,
            );
        }
        // Preview pane.
        if self.previews {
            surface.fill_rect(list, TOP, PREVIEW, height - TOP - STATUS, PANEL);
            surface.fill_rect(list, TOP, 1, height - TOP - STATUS, EDGE);
            match &self.preview {
                Some((_, image)) => {
                    let (w, h) = preview::fit(image.width, image.height, PREVIEW as usize - 16);
                    surface.draw_image_smooth(
                        list + (PREVIEW - w as i32) / 2,
                        TOP + 16,
                        w as i32,
                        h as i32,
                        image.view(),
                    );
                    font.draw(
                        surface,
                        list + 8,
                        TOP + 24 + h as i32,
                        &format!("{}x{}", image.width, image.height),
                        DIM,
                    );
                }
                None => font.draw(surface, list + 8, TOP + 16, "No preview", DIM),
            }
        }
        // Status.
        let bar = height - STATUS;
        surface.fill_rect(0, bar, width, STATUS, PANEL);
        surface.fill_rect(0, bar, width, 1, EDGE);
        let status = if self.status.is_empty() {
            format!("{} items", self.current().entries.len())
        } else {
            self.status.clone()
        };
        let status: String = status.chars().take(((width - 16) / 8).max(4) as usize).collect();
        font.draw(surface, 8, bar + 6, &status, DIM);
        if let Some((x, y)) = self.popup {
            let menu = self.context_menu();
            let rect = menu.rect_at(x, y, width, height);
            menu.draw(surface, font, rect, x, y);
        }
    }
    fn context_menu(&self) -> Menu {
        let entry = self.current().selection();
        let file = self.current().selected_path().is_some();
        Menu::new(
            "Context",
            vec![
                MenuItem::new(CMD_OPEN, "Open").enabled(entry.is_some()),
                MenuItem::new(CMD_TERMINAL, "Open terminal here"),
                MenuItem::rule(),
                MenuItem::new(CMD_COPY, "Copy").shortcut("Ctrl+C").enabled(file),
                MenuItem::new(CMD_CUT, "Cut").shortcut("Ctrl+X").enabled(file),
                MenuItem::new(CMD_PASTE, "Paste").shortcut("Ctrl+V"),
                MenuItem::new(CMD_DELETE, "Delete").shortcut("Del").enabled(file),
                MenuItem::rule(),
                MenuItem::new(CMD_NEW_TAB, "New tab").shortcut("Ctrl+T"),
                MenuItem::new(CMD_REFRESH, "Refresh").shortcut("R"),
            ],
        )
    }
}
fn menus(browser: &Browser) -> Vec<Menu> {
    vec![
        Menu::new(
            "File",
            vec![
                MenuItem::new(CMD_NEW_TAB, "New tab").shortcut("Ctrl+T"),
                MenuItem::new(CMD_CLOSE_TAB, "Close tab")
                    .shortcut("Ctrl+W")
                    .enabled(browser.tabs.len() > 1),
                MenuItem::rule(),
                MenuItem::new(CMD_TERMINAL, "Open terminal here"),
                MenuItem::rule(),
                MenuItem::new(CMD_CLOSE, "Close").shortcut("Esc"),
            ],
        ),
        Menu::new(
            "Edit",
            vec![
                MenuItem::new(CMD_COPY, "Copy").shortcut("Ctrl+C"),
                MenuItem::new(CMD_CUT, "Cut").shortcut("Ctrl+X"),
                MenuItem::new(CMD_PASTE, "Paste").shortcut("Ctrl+V"),
                MenuItem::rule(),
                MenuItem::new(CMD_DELETE, "Delete").shortcut("Del"),
            ],
        ),
        Menu::new(
            "Go",
            vec![
                MenuItem::new(CMD_PARENT, "Parent directory").shortcut("Backspace"),
                MenuItem::new(CMD_HOME, "Home").shortcut("H"),
                MenuItem::rule(),
                MenuItem::new(CMD_REFRESH, "Refresh").shortcut("R"),
            ],
        ),
        Menu::new(
            "View",
            vec![MenuItem::new(CMD_PREVIEWS, "Image previews").checked(browser.previews)],
        ),
    ]
}
/// Everything a command needs from the session.
struct Session<'a> {
    client: &'a Client,
    window: Window,
}
impl Session<'_> {
    fn toast(&self, text: &str, color: u32) {
        let _ = self.client.toast(text, color, 0);
    }
}
fn terminal_program() -> String {
    std::env::var("HOS_TERMINAL").unwrap_or_else(|_| "/bin/hos-terminal".into())
}
fn run() -> Result<(), String> {
    let start = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| config::home()));
    let client = Client::connect().map_err(|e| e.to_string())?;
    let window = client
        .create("Files", 700, 420, ACCENT)
        .map_err(|e| e.to_string())?;
    client
        .flags(window, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)
        .map_err(|e| e.to_string())?;
    let session = Session {
        client: &client,
        window,
    };
    let mut browser = Browser::new(start);
    client
        .set_menus(window, &menus(&browser))
        .map_err(|e| e.to_string())?;
    let font = Font::builtin();
    let (mut width, mut height, _) = client.size(window).map_err(|e| e.to_string())?;
    let mut surface = Surface::new(width as usize, height as usize);
    let mut dirty = true;
    let mut tab_count = browser.tabs.len();
    let mut previews = browser.previews;
    let mut last_click: Option<(usize, Instant)> = None;
    loop {
        let (new_width, new_height, minimized) = client.size(window).map_err(|e| e.to_string())?;
        if (new_width, new_height) != (width, height) {
            width = new_width;
            height = new_height;
            surface.reset(width as usize, height as usize, BACKGROUND);
            dirty = true;
        }
        let (w, h) = (width as i32, height as i32);
        let visible = browser.visible(h);
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
                6 => match text.as_bytes() {
                    b"\x1b" => return close(&client, window),
                    [3] => command(&mut browser, &session, CMD_COPY),
                    [24] => command(&mut browser, &session, CMD_CUT),
                    [22] => command(&mut browser, &session, CMD_PASTE),
                    [20] => command(&mut browser, &session, CMD_NEW_TAB),
                    [23] => command(&mut browser, &session, CMD_CLOSE_TAB),
                    b"\x1b[3~" => command(&mut browser, &session, CMD_DELETE),
                    b"\r" => command(&mut browser, &session, CMD_OPEN),
                    [127] | [8] => command(&mut browser, &session, CMD_PARENT),
                    b"r" | b"R" => command(&mut browser, &session, CMD_REFRESH),
                    b"h" | b"H" => command(&mut browser, &session, CMD_HOME),
                    b"p" | b"P" => command(&mut browser, &session, CMD_PREVIEWS),
                    b"t" | b"T" => command(&mut browser, &session, CMD_TERMINAL),
                    b"\x1b[A" => browser.tab().move_selection(-1, visible),
                    b"\x1b[B" => browser.tab().move_selection(1, visible),
                    b"\x1b[5~" => browser.tab().move_selection(-(visible as i32), visible),
                    b"\x1b[6~" => browser.tab().move_selection(visible as i32, visible),
                    b"\x1b[H" => browser.tab().move_selection(i32::MIN / 2, visible),
                    b"\x1b[F" => browser.tab().move_selection(i32::MAX / 2, visible),
                    _ => (),
                },
                8 => {
                    let mut parts = text.split_whitespace();
                    let x: i32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y: i32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let action = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    match (control, action) {
                        (2, _) => browser.popup = Some((x, y)),
                        (1, 1) => {
                            // A click in the popup runs its command; a click
                            // outside closes it without selecting anything.
                            if let Some((px, py)) = browser.popup.take() {
                                let menu = browser.context_menu();
                                let rect = menu.rect_at(px, py, w, h);
                                if let Some(index) = menu.hit(rect, x, y) {
                                    command(&mut browser, &session, menu.items[index].id);
                                }
                                continue;
                            }
                            if y < TABS {
                                if let Some(index) =
                                    browser.tab_rects().iter().position(|r| r.contains(x, y))
                                {
                                    browser.active = index;
                                } else if x >= browser.tabs.len() as i32 * TAB_WIDTH {
                                    command(&mut browser, &session, CMD_NEW_TAB);
                                }
                                continue;
                            }
                            if let Some((rect, index)) = browser
                                .rows(w, h)
                                .into_iter()
                                .find(|(rect, _)| rect.contains(x, y))
                            {
                                let _ = rect;
                                browser.tab().selected = index;
                                // A second click on the same row opens it.
                                let double = last_click.is_some_and(|(row, at)| {
                                    row == index && at.elapsed() < Duration::from_millis(400)
                                });
                                last_click = Some((index, Instant::now()));
                                if double {
                                    command(&mut browser, &session, CMD_OPEN);
                                }
                            }
                        }
                        _ => (),
                    }
                }
                10 => browser
                    .tab()
                    .move_selection(-(control as i32) * 3, visible),
                11 => command(&mut browser, &session, control),
                7 | 9 => return close(&client, window),
                _ => (),
            }
        }
        browser.refresh_preview();
        if browser.tabs.len() != tab_count || browser.previews != previews {
            tab_count = browser.tabs.len();
            previews = browser.previews;
            client
                .set_menus(window, &menus(&browser))
                .map_err(|e| e.to_string())?;
        }
        if dirty && !minimized {
            browser.draw(&mut surface, &font);
            client
                .present(window, width, height, surface.pixels())
                .map_err(|e| e.to_string())?;
            dirty = false;
        }
        thread::sleep(Duration::from_millis(if minimized { 120 } else { 24 }));
    }
}
/// Run one menu, context menu or keyboard command.
fn command(browser: &mut Browser, session: &Session<'_>, id: u32) {
    browser.popup = None;
    browser.status.clear();
    match id {
        CMD_OPEN => {
            let Some(entry) = browser.current().selection().cloned() else {
                return;
            };
            if entry.name == ".." {
                return command(browser, session, CMD_PARENT);
            }
            let path = browser.current().path.join(&entry.name);
            if entry.directory {
                if let Some(error) = browser.tab().go(path) {
                    browser.status = error;
                }
            } else if entry.image() {
                spawn(browser, session, "/bin/hos-image", &[path.as_path()], None);
            } else {
                session.toast(
                    &format!("No application for {}", entry.name),
                    0xffe4c878,
                );
            }
        }
        CMD_TERMINAL => {
            let directory = browser.current().path.clone();
            spawn(
                browser,
                session,
                &terminal_program(),
                &[],
                Some(directory.as_path()),
            );
        }
        CMD_COPY | CMD_CUT => {
            let Some(path) = browser.current().selected_path() else {
                return;
            };
            let action = if id == CMD_CUT { "cut" } else { "copy" };
            let text = format!("{MARK}{action}\n{}", path.display());
            if let Err(e) = session.client.set_clipboard(&text) {
                browser.status = e.to_string();
                return;
            }
            browser.status = format!("{} {}", if id == CMD_CUT { "Cut" } else { "Copied" }, path.display());
        }
        CMD_PASTE => {
            let Some(pending) = session
                .client
                .clipboard()
                .ok()
                .as_deref()
                .and_then(parse_clipboard)
            else {
                browser.status = "The clipboard holds no file".into();
                return;
            };
            let directory = browser.current().path.clone();
            let name = pending
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                return;
            }
            let destination = unique(&directory, &name);
            let result = if pending.cut && fs::rename(&pending.path, &destination).is_ok() {
                Ok(())
            } else {
                copy_tree(&pending.path, &destination).and_then(|()| {
                    if pending.cut {
                        remove(&pending.path)
                    } else {
                        Ok(())
                    }
                })
            };
            match result {
                Ok(()) => {
                    browser.status = format!(
                        "{} to {}",
                        if pending.cut { "Moved" } else { "Copied" },
                        destination.display()
                    );
                    let _ = session.client.set_clipboard("");
                    reload_all(browser, &directory);
                }
                Err(e) => {
                    browser.status = format!("{}: {e}", pending.path.display());
                    let _ = session.client.ask(
                        "Paste failed",
                        &browser.status,
                        hoswm::client::ANSWER_BUTTONS_OK,
                        SEVERITY_ERROR,
                    );
                }
            }
        }
        CMD_DELETE => {
            let Some(path) = browser.current().selected_path() else {
                return;
            };
            let directory = browser.current().path.clone();
            let answer = session.client.ask(
                "Delete",
                &format!(
                    "Delete {}?\n\nThis cannot be undone.",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ),
                ANSWER_BUTTONS_YES_NO,
                SEVERITY_QUESTION,
            );
            if answer.unwrap_or(0) != ANSWER_YES {
                return;
            }
            match remove(&path) {
                Ok(()) => {
                    browser.status = format!("Deleted {}", path.display());
                    reload_all(browser, &directory);
                }
                Err(e) => browser.status = format!("{}: {e}", path.display()),
            }
        }
        CMD_NEW_TAB => {
            let (tab, error) = Tab::new(browser.current().path.clone());
            browser.tabs.push(tab);
            browser.active = browser.tabs.len() - 1;
            browser.status = error.unwrap_or_default();
        }
        CMD_CLOSE_TAB => {
            if browser.tabs.len() > 1 {
                browser.tabs.remove(browser.active);
                browser.active = browser.active.min(browser.tabs.len() - 1);
            }
        }
        CMD_REFRESH => {
            if let Some(error) = browser.tab().reload() {
                browser.status = error;
            }
            browser.preview = None;
        }
        CMD_PREVIEWS => browser.previews = !browser.previews,
        CMD_HOME => {
            let home = config::home();
            if let Some(error) = browser.tab().go(home) {
                browser.status = error;
            }
        }
        CMD_PARENT => {
            let parent = browser.current().path.parent().map(Path::to_path_buf);
            if let Some(parent) = parent {
                // Land on the directory just left, as other browsers do.
                let name = browser
                    .current()
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                if let Some(error) = browser.tab().go(parent) {
                    browser.status = error;
                }
                if let Some(index) = name.and_then(|name| {
                    browser.current().entries.iter().position(|e| e.name == name)
                }) {
                    browser.tab().selected = index;
                }
            }
        }
        CMD_CLOSE => {
            let _ = session.client.close(session.window);
            std::process::exit(0);
        }
        _ => (),
    }
}
fn remove(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}
/// Reload every tab showing a directory whose contents changed.
fn reload_all(browser: &mut Browser, directory: &Path) {
    for tab in &mut browser.tabs {
        if tab.path == directory || tab.path.parent() == Some(directory) {
            tab.reload();
        }
    }
    browser.preview = None;
}
fn spawn(
    browser: &mut Browser,
    session: &Session<'_>,
    program: &str,
    arguments: &[&Path],
    directory: Option<&Path>,
) {
    let mut command = Command::new(program);
    command.args(arguments);
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    match command.spawn() {
        Ok(_) => browser.status = format!("Started {program}"),
        Err(e) => {
            browser.status = format!("{program}: {e}");
            session.toast(&browser.status, 0xffef6976);
        }
    }
}
fn close(client: &Client, window: Window) -> Result<(), String> {
    let _ = client.close(window);
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-files: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("hoswm-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("sub/deep")).unwrap();
        fs::write(root.join("b.txt"), b"text").unwrap();
        fs::write(root.join("sub/deep/leaf"), b"leaf").unwrap();
        hoswm::qoi::save(&root.join("a.qoi"), 4, 2, &[0xff112233; 8]).unwrap();
        root
    }
    #[test]
    fn listings_put_the_parent_and_directories_first() {
        let root = tree("list");
        let (tab, error) = Tab::new(root.clone());
        assert_eq!(error, None);
        let names: Vec<&str> = tab.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "sub", "a.qoi", "b.txt"]);
        assert!(tab.entries[2].image() && !tab.entries[3].image());
        assert_eq!(tab.selected_path(), None, "the parent entry has no path");
        let (missing, error) = Tab::new(root.join("nowhere"));
        assert!(error.is_some(), "a listing failure is reported, not fatal");
        assert_eq!(missing.entries.len(), 1, "only the parent entry");
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn selection_moves_and_scrolls_within_the_window() {
        let root = tree("select");
        let mut browser = Browser::new(root.clone());
        browser.tab().move_selection(2, 2);
        assert_eq!(browser.current().selected, 2);
        assert_eq!(browser.current().offset, 1, "the view follows the selection");
        browser.tab().move_selection(-9, 2);
        assert_eq!((browser.current().selected, browser.current().offset), (0, 0));
        browser.tab().move_selection(99, 2);
        assert_eq!(browser.current().selected, 3);
        assert_eq!(browser.current().path, root);
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn copying_and_cutting_move_whole_directories() {
        let root = tree("copy");
        let destination = root.join("sub");
        copy_tree(&root.join("b.txt"), &destination.join("b.txt")).unwrap();
        assert_eq!(fs::read(destination.join("b.txt")).unwrap(), b"text");
        copy_tree(&root.join("sub/deep"), &destination.join("copy")).unwrap();
        assert_eq!(fs::read(destination.join("copy/leaf")).unwrap(), b"leaf");
        // Pasting beside an existing name never overwrites it.
        assert_eq!(unique(&root, "b.txt"), root.join("b (1).txt"));
        fs::write(root.join("b (1).txt"), b"x").unwrap();
        assert_eq!(unique(&root, "b.txt"), root.join("b (2).txt"));
        assert_eq!(unique(&root, "new.txt"), root.join("new.txt"));
        remove(&root.join("sub/copy")).unwrap();
        assert!(!destination.join("copy").exists());
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn the_clipboard_only_accepts_file_operations() {
        let pending = parse_clipboard("hoswm-files:cut\n/tmp/a").unwrap();
        assert!(pending.cut && pending.path == PathBuf::from("/tmp/a"));
        assert!(!parse_clipboard("hoswm-files:copy\n/tmp/a").unwrap().cut);
        assert!(parse_clipboard("/tmp/a").is_none(), "plain text is ignored");
        assert!(parse_clipboard("hoswm-files:copy").is_none());
        assert_eq!(human(512), "512B");
        assert_eq!(human(99_999), "97K");
        assert_eq!(human(5_000_000_000), "4768M", "units change at five digits");
    }
    #[test]
    fn previews_come_from_the_cache_and_the_window_draws_them() {
        let root = tree("preview");
        let mut browser = Browser::new(root.clone());
        browser.tab().selected = 2; // a.qoi
        browser.refresh_preview();
        let (path, image) = browser.preview.as_ref().expect("preview generated");
        assert_eq!(path, &root.join("a.qoi"));
        assert_eq!((image.width, image.height), (4, 2));
        let mut surface = Surface::new(700, 420);
        browser.draw(&mut surface, &Font::builtin());
        assert!(surface.pixels().contains(&0xff112233), "preview drawn");
        // A directory has no preview, and previews can be turned off.
        browser.tab().selected = 1;
        browser.refresh_preview();
        assert!(browser.preview.is_none());
        browser.previews = false;
        assert_eq!(browser.list_width(700), 700);
        browser.draw(&mut surface, &Font::builtin());
        // The context menu offers only what applies to the selection.
        browser.popup = Some((20, 60));
        let menu = browser.context_menu();
        assert!(menu.items[1].enabled, "a terminal can always be opened here");
        browser.draw(&mut surface, &Font::builtin());
        let _ = fs::remove_dir_all(preview::directory());
        fs::remove_dir_all(&root).unwrap();
    }
}
