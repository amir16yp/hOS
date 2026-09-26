//! Retained windows and a deliberately small, fixed-coordinate GUI toolkit.
use crate::{
    config::{Config, DockCommand},
    context_menu::ContextMenu,
    menu::{self, Menu, MenuItem},
    qoi,
    shortcuts::{Action, WindowCycle},
    text::{TextLayout, TextSelection},
    toast::Toasts,
};
use crate::{font::Font, surface::Surface};
use std::collections::VecDeque;
pub const BLACK: u32 = 0xff000000;
pub const ACCENT: u32 = 0xff72dbac;
const TITLE: i32 = 28;
/// Height reserved for the menu bar; windows never cover it.
pub const MENUBAR: i32 = menu::HEIGHT;
/// First row below the menu bar that a window may occupy.
fn top(config: &Config) -> i32 {
    if config.menubar {
        MENUBAR
    } else {
        0
    }
}
/// Default screen size used before the first frame tells the desktop its real
/// dimensions. The runtime replaces this with the framebuffer size.
const DEFAULT_SCREEN: (i32, i32) = (800, 600);
#[cfg(test)]
const FLOOR: i32 = 530;
/// How far from a resizable window's edge the pointer grabs that edge. The
/// band reaches the same distance outside the frame, so the thin border is
/// not the only thing to aim at.
const GRIP: i32 = 4;
/// The smallest content a window may be resized to, matching what a client is
/// allowed to ask for when it creates one.
const MIN_CONTENT: (i32, i32) = (180, 60);
// Built-in menu commands, which no window can collide with: window menu items
// are dispatched through their own window.
const SYSTEM_SCREENSHOT: u32 = 1;
const SYSTEM_SCREENSHOT_WINDOW: u32 = 2;
const SYSTEM_CLEAR_TOASTS: u32 = 3;
const SYSTEM_EXIT: u32 = 4;

/// Message box button sets and answers, shared with the ABI and clients.
pub const ANSWER_CLOSED: u32 = 0;
pub const ANSWER_OK: u32 = 1;
pub const ANSWER_CANCEL: u32 = 2;
pub const ANSWER_YES: u32 = 3;
pub const ANSWER_NO: u32 = 4;
pub const ANSWER_BUTTONS_OK: u32 = 0;
pub const ANSWER_BUTTONS_OK_CANCEL: u32 = 1;
pub const ANSWER_BUTTONS_YES_NO: u32 = 2;
pub const ANSWER_BUTTONS_YES_NO_CANCEL: u32 = 3;
pub const SEVERITY_INFO: u32 = 0;
pub const SEVERITY_WARNING: u32 = 1;
pub const SEVERITY_ERROR: u32 = 2;
pub const SEVERITY_QUESTION: u32 = 3;

/// A dock button: a configured item, or a window to raise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Item(usize),
    Window(u32),
}
/// A pending screen capture, completed by the session once a frame is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shot {
    Screen,
    Window(u32),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}
impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}
pub struct Control {
    pub selection: TextSelection,
    pub id: u32,
    pub kind: u32,
    pub rect: Rect,
    pub text: String,
}
#[derive(Clone, Debug)]
pub struct Event {
    pub kind: u32,
    pub control: u32,
    pub text: String,
}
pub struct Window {
    pub id: u32,
    pub owner_pid: u32,
    pub title: String,
    pub color: u32,
    pub rect: Rect,
    restore: Option<Rect>,
    pub minimized: bool,
    pub content: Surface,
    pub controls: Vec<Control>,
    /// Client opted into raw keyboard and pointer events through ABI operation 12.
    pub raw_input: bool,
    /// Client allows the window to be resized by dragging its edges. Windows
    /// whose layout is fixed, such as message boxes, leave this off.
    pub resizable: bool,
    /// Client asked the window manager to route titlebar close as an event.
    pub managed_close: bool,
    pub protected_work: bool,
    pub events: VecDeque<Event>,
    active_control: Option<u32>,
    pub message: bool,
    /// Answer buttons of a message box, in layout order; the last is primary.
    /// Clicking one reports it to the client instead of closing the window.
    pub answer: Vec<u32>,
    /// Menus this window contributes to the screen-top bar while focused.
    pub menus: Vec<Menu>,
}
impl Window {
    fn event(&mut self, kind: u32, control: u32, text: String) {
        if self.events.len() == 128 {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            kind,
            control,
            text,
        });
    }
    fn resize(&mut self) {
        let w = (self.rect.w - 4) as usize;
        let h = (self.rect.h - TITLE - 2) as usize;
        if self.content.width() != w || self.content.height() != h {
            let mut next = Surface::new(w, h);
            next.pixels_mut().fill(BLACK);
            next.draw_surface(0, 0, &self.content);
            self.content = next;
            self.event(4, w as u32, h.to_string());
        }
    }
}
/// A resize in progress: which window, which edges the pointer took hold of,
/// and the frame and pointer position it started from. Working from the
/// starting frame keeps the window from creeping as the pointer is clamped.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Resize {
    window: u32,
    /// -1 for the left or top edge, 1 for the right or bottom, 0 for neither.
    horizontal: i32,
    vertical: i32,
    start: Rect,
    from: (i32, i32),
}
pub struct Desktop {
    content_scratch: Surface,
    control_scratch: Surface,
    clipboard: String,
    menu: Option<ContextMenu>,
    selecting: Option<(u32, Option<u32>)>,
    cycle: WindowCycle,
    pub windows: Vec<Window>,
    next: u32,
    pub x: i32,
    pub y: i32,
    screen_width: i32,
    screen_height: i32,
    drag: Option<(u32, i32, i32)>,
    resize: Option<Resize>,
    pressed: Option<(u32, u32)>,
    pub font: Font<'static>,
    pub quit: bool,
    pub login_session: bool,
    pub launch: Option<String>,
    raw_capture: Option<u32>,
    pub config: Config,
    /// The configured wallpaper, and the copy of it scaled to the screen. The
    /// scaling is done once, when the size it is drawn at first differs.
    wallpaper: Option<qoi::Image>,
    background: Surface,
    /// Decoded dock icons, one slot per configured dock item.
    icons: Vec<Option<qoi::Image>>,
    pub toasts: Toasts,
    /// Open bar menu: 0 is the system menu, later indices are window menus.
    open_menu: Option<usize>,
    pub screenshot: Option<Shot>,
    clock: String,
}
impl Default for Desktop {
    fn default() -> Self {
        Self::new()
    }
}
impl Desktop {
    pub fn set_screen_size(&mut self, width: usize, height: usize) {
        self.screen_width = width.max(1) as i32;
        self.screen_height = height.max(1) as i32;
        self.x = self.x.clamp(0, self.screen_width - 1);
        self.y = self.y.clamp(0, self.screen_height - 1);
    }
    pub fn cancel_control_interaction(&mut self, id: u32, cid: u32) {
        if self.selecting == Some((id, Some(cid))) {
            self.selecting = None;
        }
        if self
            .menu
            .as_ref()
            .is_some_and(|m| m.window == id && m.control == Some(cid))
        {
            self.menu = None;
        }
        if self.pressed == Some((id, cid)) {
            self.pressed = None;
        }
    }
    pub fn raw_input_focused(&self) -> bool {
        self.windows
            .iter()
            .rev()
            .find(|w| !w.minimized)
            .is_some_and(|w| w.raw_input)
    }
    pub fn end_window_cycle(&mut self) {
        self.cycle.reset();
    }
    pub fn clipboard(&self) -> &str {
        &self.clipboard
    }
    pub fn set_clipboard(&mut self, text: String) {
        self.clipboard = text;
    }
    pub fn close_client(&mut self, id: u32) {
        if let Some(w) = self.window(id) {
            w.managed_close = false;
        }
        self.close(id);
    }
    pub fn close_owner(&mut self, pid: u32) {
        let ids: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| w.owner_pid == pid)
            .map(|w| w.id)
            .collect();
        for id in ids {
            self.close_client(id);
        }
    }
    pub fn shortcut(&mut self, action: Action) -> bool {
        match action {
            // Escape backs out of whatever is in front: a menu, then the
            // open bar menu, then notifications.
            Action::Dismiss => {
                return self.menu.take().is_some()
                    || self.open_menu.take().is_some()
                    || self.toasts.clear();
            }
            Action::Screenshot => {
                self.screenshot = Some(Shot::Screen);
                self.open_menu = None;
                return true;
            }
            Action::ScreenshotWindow => {
                let focused = self
                    .windows
                    .iter()
                    .rev()
                    .find(|w| !w.minimized)
                    .map(|w| w.id);
                self.open_menu = None;
                self.screenshot = focused.map(Shot::Window);
                if focused.is_none() {
                    self.toast("No window to capture", 0xffe4c878, 0);
                }
                return true;
            }
            // The session, not the desktop, decides how to leave.
            Action::Exit => return false,
            _ => (),
        }
        if let Action::SwitchWindow(reverse) = action {
            let ids: Vec<_> = self
                .windows
                .iter()
                .rev()
                .filter(|w| !w.minimized)
                .map(|w| w.id)
                .collect();
            if let Some(id) = self.cycle.next(&ids, reverse) {
                self.focus(id);
            }
            self.menu = None;
            self.open_menu = None;
            self.selecting = None;
            self.pressed = None;
            self.drag = None;
            return true;
        }
        let Some(w) = self.windows.iter().rev().find(|w| !w.minimized) else {
            return false;
        };
        let (id, control) = (w.id, w.active_control);
        let handled = self.edit_action(id, control, action);
        if handled {
            self.menu = None;
        }
        handled
    }
    fn select_at(&mut self, id: u32, cid: Option<u32>, start: bool) {
        let (x, y) = (self.x, self.y);
        let Some(w) = self.window(id) else {
            return;
        };
        let (x, y) = (x - w.rect.x - 2, y - w.rect.y - TITLE);
        if let Some(c) = w
            .controls
            .iter_mut()
            .find(|c| Some(c.id) == cid && c.selection.selectable)
        {
            let layout =
                TextLayout::new(&c.text, c.rect.w, c.rect.h, c.kind == 1, c.selection.caret);
            let pos = layout.hit(&c.text, x - c.rect.x, y - c.rect.y);
            c.selection.caret = pos;
            if start {
                c.selection.anchor = pos;
            }
        }
    }
    pub fn right_click(&mut self) {
        self.menu = None;
        self.selecting = None;
        self.pressed = None;
        self.drag = None;
        self.cycle.reset();
        let (x, y) = (self.x, self.y);
        let Some(id) = self
            .windows
            .iter()
            .rev()
            .find(|w| !w.minimized && w.rect.contains(x, y))
            .map(|w| w.id)
        else {
            return;
        };
        self.focus(id);
        let clipboard = !self.clipboard.is_empty();
        let w = self.window(id).unwrap();
        let (cx, cy) = (x - w.rect.x - 2, y - w.rect.y - TITLE);
        if cx < 0 || cy < 0 || cx >= w.content.width() as i32 || cy >= w.content.height() as i32 {
            return;
        }
        let (control, selected, editable, paste) = {
            let Some(c) = w.controls.iter().rev().find(|c| c.rect.contains(cx, cy)) else {
                return;
            };
            if !c.selection.selectable {
                return;
            }
            let cid = c.id;
            let result = (
                Some(cid),
                !c.selection.range().is_empty(),
                c.kind == 3,
                clipboard && c.kind == 3,
            );
            w.active_control = Some(cid);
            result
        };
        self.menu = Some(ContextMenu::new(
            id,
            control,
            x,
            y,
            self.screen_width,
            self.screen_height,
            selected,
            editable,
            paste,
        ));
    }
    fn edit_action(&mut self, id: u32, cid: Option<u32>, action: Action) -> bool {
        let clipboard = self.clipboard.clone();
        let Some(w) = self.window(id) else {
            return false;
        };
        let Some(index) = w
            .controls
            .iter()
            .position(|c| Some(c.id) == cid && c.selection.selectable)
        else {
            return false;
        };
        let c = &mut w.controls[index];
        let copied = if matches!(action, Action::Copy | Action::Cut) {
            Some(c.selection.selected(&c.text).to_string())
        } else {
            None
        };
        let mut changed = false;
        match action {
            Action::SelectAll => {
                c.selection.anchor = 0;
                c.selection.caret = c.text.len();
            }
            Action::Paste if c.kind == 3 => {
                let text: String = clipboard.chars().filter(|c| !c.is_control()).collect();
                if !text.is_empty() {
                    changed = c.selection.replace(&mut c.text, &text);
                }
            }
            Action::Cut | Action::Delete if c.kind == 3 => {
                changed = c.selection.replace(&mut c.text, "")
            }
            _ => (),
        }
        if changed {
            let (cid, text) = (c.id, c.text.clone());
            w.event(2, cid, text);
        }
        if let Some(text) = copied {
            if !text.is_empty() {
                self.clipboard = text;
            }
        }
        true
    }

    pub fn remove_control(&mut self, id: u32, cid: u32) -> Result<(), String> {
        let w = self.window(id).ok_or("unknown window")?;
        let index = w
            .controls
            .iter()
            .position(|c| c.id == cid)
            .ok_or("unknown control")?;
        w.controls.remove(index);
        if w.active_control == Some(cid) {
            w.active_control = None;
        }
        w.events
            .retain(|e| !matches!(e.kind, 1..=3) || e.control != cid);
        if self.pressed == Some((id, cid)) {
            self.pressed = None;
        }
        self.cancel_control_interaction(id, cid);
        Ok(())
    }
    pub fn new() -> Self {
        Self::with_config(Config::defaults_in(crate::config::directory()))
    }
    /// Build a desktop from a configuration, decoding its dock icons. Icons
    /// that cannot be read become configuration warnings, not failures.
    pub fn with_config(mut config: Config) -> Self {
        let mut icons = Vec::with_capacity(config.dock.len());
        let mut warnings = Vec::new();
        for item in &config.dock {
            icons.push(item.icon.as_ref().and_then(|path| match qoi::load(path) {
                Ok(image) => Some(image),
                Err(e) => {
                    warnings.push(format!("{}: {e}", path.display()));
                    None
                }
            }));
        }
        // The wallpaper is optional in the ordinary way: no file means no
        // wallpaper, and only a file that cannot be read is worth a warning.
        let wallpaper = match qoi::load(&config.wallpaper) {
            Ok(image) => Some(image),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                config
                    .warnings
                    .push(format!("{}: {e}", config.wallpaper.display()));
                None
            }
        };
        config.warnings.extend(warnings);
        let toasts = Toasts::new(
            config.toast_corner,
            config.toast_ms,
            config.toast_max,
            Some(config.toast_db.clone()),
        );
        Self {
            content_scratch: Surface::new(0, 0),
            control_scratch: Surface::new(0, 0),
            clipboard: String::new(),
            menu: None,
            selecting: None,
            cycle: WindowCycle::default(),
            windows: Vec::new(),
            next: 1,
            x: 400,
            y: 300,
            screen_width: DEFAULT_SCREEN.0,
            screen_height: DEFAULT_SCREEN.1,
            drag: None,
            resize: None,
            pressed: None,
            font: Font::builtin(),
            quit: false,
            login_session: false,
            launch: None,
            raw_capture: None,
            config,
            wallpaper,
            background: Surface::new(0, 0),
            icons,
            toasts,
            open_menu: None,
            screenshot: None,
            clock: String::new(),
        }
    }
    /// Show a notification, which is logged when it leaves the screen.
    pub fn toast(&mut self, text: impl Into<String>, color: u32, milliseconds: u32) {
        self.toasts.push(text, color, milliseconds);
    }
    /// Retire expired notifications and advance the clock. True when the
    /// screen needs redrawing even though no input arrived.
    pub fn tick(&mut self) -> bool {
        let mut dirty = self.toasts.tick();
        if self.config.menubar && self.config.clock {
            let now = crate::toast::format_time(crate::toast::now_ms());
            let minutes = now.get(11..16).unwrap_or_default().to_string();
            if minutes != self.clock {
                self.clock = minutes;
                dirty = true;
            }
        }
        dirty
    }
    /// The region notifications may occupy: below the bar, above the dock.
    fn toast_area(&self) -> Rect {
        let top = top(&self.config) + 10;
        Rect {
            x: 12,
            y: top,
            w: self.screen_width - 24,
            h: self.floor() - top,
        }
    }
    fn floor(&self) -> i32 {
        self.screen_height - 70
    }
    /// Name shown at the left of the bar: the focused window, or the session.
    fn app_label(&self) -> String {
        self.windows
            .iter()
            .rev()
            .find(|w| !w.minimized)
            .map_or_else(|| "hOS".to_string(), |w| w.title.chars().take(24).collect())
    }
    /// The bar's menus: the system menu, then the focused window's menus.
    fn menus(&self) -> Vec<Menu> {
        let hint = |action| self.config.bindings.hint(action);
        let mut menus = vec![Menu::new(
            "System",
            vec![
                MenuItem::new(SYSTEM_SCREENSHOT, "Take screenshot")
                    .shortcut(hint(Action::Screenshot)),
                MenuItem::new(SYSTEM_SCREENSHOT_WINDOW, "Screenshot window")
                    .shortcut(hint(Action::ScreenshotWindow))
                    .enabled(self.windows.iter().any(|w| !w.minimized)),
                MenuItem::rule(),
                MenuItem::new(SYSTEM_CLEAR_TOASTS, "Clear notifications")
                    .enabled(!self.toasts.is_empty()),
                MenuItem::rule(),
                MenuItem::new(
                    SYSTEM_EXIT,
                    if self.login_session {
                        "Log out"
                    } else {
                        "Exit"
                    },
                )
                .shortcut(hint(Action::Exit))
                .enabled(!self.installing()),
            ],
        )];
        if let Some(window) = self.windows.iter().rev().find(|w| !w.minimized) {
            menus.extend(window.menus.iter().cloned());
        }
        menus
    }
    /// Run a bar menu item: system items act here, window items are delivered
    /// to the focused window as an event.
    fn activate_menu(&mut self, index: usize, item: MenuItem) {
        if index == 0 {
            match item.id {
                SYSTEM_SCREENSHOT => self.shortcut(Action::Screenshot),
                SYSTEM_SCREENSHOT_WINDOW => self.shortcut(Action::ScreenshotWindow),
                SYSTEM_CLEAR_TOASTS => self.toasts.clear(),
                SYSTEM_EXIT => {
                    self.quit = !self.installing();
                    self.quit
                }
                _ => false,
            };
            return;
        }
        if let Some(window) = self.windows.iter_mut().rev().find(|w| !w.minimized) {
            window.event(11, item.id, item.label);
        }
    }
    pub fn create(&mut self, title: String, w: i32, h: i32, color: u32) -> Result<u32, String> {
        if self.windows.len() >= 24 {
            return Err("window limit reached".into());
        }
        if !(180..=796).contains(&w) || !(60..=498).contains(&h) || title.len() > 128 {
            return Err("invalid window dimensions or title".into());
        }
        let offset = (self.windows.len() % 6) as i32 * 20;
        let rect = Rect {
            x: (50 + offset).min(self.screen_width - w - 4),
            y: (40 + offset)
                .min(self.floor() - h - TITLE - 2)
                .max(top(&self.config)),
            w: w + 4,
            h: h + TITLE + 2,
        };
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or("window IDs exhausted")?;
        let mut content = Surface::new(w as usize, h as usize);
        content.pixels_mut().fill(BLACK);
        self.windows.push(Window {
            id,
            owner_pid: 0,
            title,
            color: color | 0xff000000,
            rect,
            restore: None,
            minimized: false,
            content,
            controls: Vec::new(),
            raw_input: false,
            resizable: false,
            managed_close: false,
            protected_work: false,
            events: VecDeque::new(),
            active_control: None,
            message: false,
            answer: Vec::new(),
            menus: Vec::new(),
        });
        Ok(id)
    }
    /// Screen rectangle of a window, including its border and title bar.
    pub fn window_rect(&self, id: u32) -> Option<Rect> {
        self.windows.iter().find(|w| w.id == id).map(|w| w.rect)
    }
    pub fn window(&mut self, id: u32) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }
    pub fn message_box(
        &mut self,
        title: String,
        message: String,
        color: u32,
    ) -> Result<u32, String> {
        if message.len() > 2048 {
            return Err("message too long".into());
        }
        let id = self.create(title, 420, 180, color)?;
        let w = self.window(id).unwrap();
        w.message = true;
        w.controls.push(Control {
            selection: TextSelection::default(),
            id: 1,
            kind: 1,
            rect: Rect {
                x: 16,
                y: 16,
                w: 388,
                h: 112,
            },
            text: message,
        });
        w.controls.push(Control {
            selection: TextSelection::default(),
            id: 2,
            kind: 2,
            rect: Rect {
                x: 320,
                y: 140,
                w: 80,
                h: 26,
            },
            text: "OK".into(),
        });
        Ok(id)
    }
    /// A message box with answer buttons, in the manner of a system dialog.
    /// Severity picks the window color; the client closes the window when it
    /// sees the answer, so the choice is never lost with the window.
    pub fn ask(
        &mut self,
        title: String,
        message: String,
        buttons: u32,
        severity: u32,
    ) -> Result<u32, String> {
        let labels: &[(u32, &str)] = match buttons {
            ANSWER_BUTTONS_OK => &[(ANSWER_OK, "OK")],
            ANSWER_BUTTONS_OK_CANCEL => &[(ANSWER_CANCEL, "Cancel"), (ANSWER_OK, "OK")],
            ANSWER_BUTTONS_YES_NO => &[(ANSWER_NO, "No"), (ANSWER_YES, "Yes")],
            ANSWER_BUTTONS_YES_NO_CANCEL => &[
                (ANSWER_CANCEL, "Cancel"),
                (ANSWER_NO, "No"),
                (ANSWER_YES, "Yes"),
            ],
            _ => return Err("unknown message box buttons".into()),
        };
        let color = match severity {
            SEVERITY_INFO => 0xff80afff,
            SEVERITY_WARNING => 0xffe4c878,
            SEVERITY_ERROR => 0xffef6976,
            SEVERITY_QUESTION => ACCENT,
            _ => return Err("unknown message box severity".into()),
        };
        if message.len() > 2048 {
            return Err("message too long".into());
        }
        let id = self.create(title, 420, 180, color)?;
        let w = self.window(id).unwrap();
        w.message = true;
        w.answer = labels.iter().map(|(id, _)| *id).collect();
        w.controls.push(Control {
            selection: TextSelection::default(),
            id: 1,
            kind: 1,
            rect: Rect {
                x: 16,
                y: 16,
                w: 388,
                h: 112,
            },
            text: message,
        });
        // Buttons run right to left, so the primary action sits at the right.
        for (index, (button, label)) in labels.iter().rev().enumerate() {
            w.controls.push(Control {
                selection: TextSelection::default(),
                id: *button,
                kind: 2,
                rect: Rect {
                    x: 316 - index as i32 * 96,
                    y: 140,
                    w: 88,
                    h: 26,
                },
                text: (*label).into(),
            });
        }
        Ok(id)
    }
    pub fn installing(&self) -> bool {
        self.windows.iter().any(|w| w.protected_work)
    }
    pub fn close(&mut self, id: u32) {
        if self.window(id).is_some_and(|w| w.managed_close) {
            if let Some(w) = self.window(id) {
                w.event(9, 0, String::new());
            }
            return;
        }

        if self.menu.as_ref().is_some_and(|m| m.window == id) {
            self.menu = None;
        }
        if self.selecting.is_some_and(|t| t.0 == id) {
            self.selecting = None;
        }
        if self.pressed.is_some_and(|t| t.0 == id) {
            self.pressed = None;
        }
        self.windows.retain(|w| w.id != id);
        // Bar menus belong to the focused window; closing one retires them.
        self.open_menu = None;
        if self.raw_capture == Some(id) {
            self.raw_capture = None;
        }
        if self.drag.is_some_and(|d| d.0 == id) {
            self.drag = None;
        }
        if self.resize.is_some_and(|r| r.window == id) {
            self.resize = None;
        }
    }
    fn focus(&mut self, id: u32) {
        if let Some(i) = self.windows.iter().position(|w| w.id == id) {
            let mut w = self.windows.remove(i);
            w.minimized = false;
            self.windows.push(w);
            self.open_menu = None;
        }
    }
    /// Dock buttons: the configured items, then one button per window.
    fn dock_slots(&self) -> Vec<(Rect, Slot)> {
        let items = self.config.dock.len();
        let count = items + self.windows.len();
        let step = if count == 0 {
            0
        } else {
            (760 / count as i32).min(54)
        };
        if step <= 4 {
            return Vec::new();
        }
        let start = (self.screen_width - count as i32 * step) / 2;
        (0..count)
            .map(|i| {
                (
                    Rect {
                        x: start + i as i32 * step,
                        y: self.screen_height - 50,
                        w: step - 4,
                        h: 40,
                    },
                    if i < items {
                        Slot::Item(i)
                    } else {
                        Slot::Window(self.windows[i - items].id)
                    },
                )
            })
            .collect()
    }
    fn dock_hover(&self) -> Option<usize> {
        if !(self.screen_height - 50..self.screen_height - 10).contains(&self.y) {
            return None;
        }
        let count = self.config.dock.len() + self.windows.len();
        let step = if count == 0 {
            0
        } else {
            (760 / count as i32).min(54)
        };
        let x = self.x - (self.screen_width - count as i32 * step) / 2;
        if step <= 4 || x < 0 || x >= count as i32 * step || x % step >= step - 4 {
            None
        } else {
            Some((x / step) as usize)
        }
    }
    /// True when motion changes scene content as well as the cursor.
    pub fn motion(&mut self, x: i32, y: i32) -> bool {
        let old_hover = self.dock_hover();
        self.x = x.clamp(0, self.screen_width - 1);
        self.y = y.clamp(0, self.screen_height - 1);
        if let Some((id, control)) = self.selecting {
            self.select_at(id, control, false);
        }
        if let Some((id, dx, dy)) = self.drag {
            let (x, y) = (self.x - dx, self.y - dy);
            let floor = top(&self.config);
            let screen_width = self.screen_width;
            let floor_limit = self.floor();
            if let Some(w) = self.window(id) {
                w.rect.x = x.clamp(0, screen_width - w.rect.w);
                w.rect.y = y.clamp(floor, floor_limit - w.rect.h);
            }
        }
        if let Some(resize) = self.resize {
            self.apply_resize(resize);
        }
        // Dragging across the bar with a menu open follows the pointer.
        if self.open_menu.is_some() {
            let menus = self.menus();
            if let Some(index) = menu::title_at(&self.app_label(), &menus, self.x, self.y) {
                self.open_menu = Some(index);
            }
        }
        let new_hover = self.dock_hover();
        self.drag.is_some()
            || self.resize.is_some()
            || self.selecting.is_some()
            || self.menu.is_some()
            || self.open_menu.is_some()
            || old_hover != new_hover
    }
    /// Which edges of a resizable window the pointer is over, if any. The band
    /// straddles the frame, so a few pixels outside the window count too, and
    /// the corners answer for both directions at once.
    fn resize_edges(&self, w: &Window, x: i32, y: i32) -> Option<(i32, i32)> {
        if !w.resizable || w.minimized || w.restore.is_some() {
            return None;
        }
        let r = w.rect;
        if !(r.x - GRIP..r.x + r.w + GRIP).contains(&x)
            || !(r.y - GRIP..r.y + r.h + GRIP).contains(&y)
        {
            return None;
        }
        let horizontal = if x < r.x + GRIP {
            -1
        } else if x >= r.x + r.w - GRIP {
            1
        } else {
            0
        };
        // The title bar belongs to dragging, apart from the band along its top.
        let vertical = if y < r.y + GRIP {
            -1
        } else if y >= r.y + r.h - GRIP {
            1
        } else {
            0
        };
        (horizontal != 0 || vertical != 0).then_some((horizontal, vertical))
    }
    /// Move the edges a resize took hold of to follow the pointer, keeping the
    /// window on the desktop and no smaller than a window may be created.
    fn apply_resize(&mut self, resize: Resize) {
        let ceiling = top(&self.config);
        let (dx, dy) = (self.x - resize.from.0, self.y - resize.from.1);
        let start = resize.start;
        let (min_w, min_h) = (MIN_CONTENT.0 + 4, MIN_CONTENT.1 + TITLE + 2);
        let mut rect = start;
        match resize.horizontal {
            -1 => {
                // The right edge stays where it is, so the left edge may only
                // travel until the window reaches its smallest width.
                rect.x = (start.x + dx).clamp(0, start.x + start.w - min_w);
                rect.w = start.x + start.w - rect.x;
            }
            1 => rect.w = (start.w + dx).clamp(min_w, self.screen_width - start.x),
            _ => (),
        }
        match resize.vertical {
            -1 => {
                rect.y = (start.y + dy).clamp(ceiling, start.y + start.h - min_h);
                rect.h = start.y + start.h - rect.y;
            }
            1 => rect.h = (start.h + dy).clamp(min_h, self.floor() - start.y),
            _ => (),
        }
        if let Some(w) = self.window(resize.window) {
            if w.rect != rect {
                w.rect = rect;
                w.resize();
            }
        }
    }
    pub fn mouse(&mut self, down: bool) {
        if !down {
            let (x, y) = (self.x, self.y);
            if let Some(id) = self.raw_capture.take() {
                if let Some(w) = self.window(id) {
                    w.event(
                        8,
                        1,
                        format!("{} {} 0", x - w.rect.x - 2, y - w.rect.y - TITLE),
                    );
                }
                return;
            }
        }
        if down {
            self.cycle.reset();
            if let Some(menu) = self.menu.take() {
                if let Some(action) = menu.hit(self.x, self.y) {
                    self.edit_action(menu.window, menu.control, action);
                }
                return;
            }
            // The bar owns the top of the screen, whatever is behind it.
            if self.config.menubar {
                let (app, menus) = (self.app_label(), self.menus());
                if let Some(index) = menu::title_at(&app, &menus, self.x, self.y) {
                    self.open_menu = (self.open_menu != Some(index)).then_some(index);
                    self.drag = None;
                    self.selecting = None;
                    self.pressed = None;
                    return;
                }
                if let Some(open) = self.open_menu.take() {
                    if let Some(menu) = menus.get(open) {
                        let rect =
                            menu.rect(menu::title_rects(&app, &menus)[open].x, self.screen_width);
                        if let Some(index) = menu.hit(rect, self.x, self.y) {
                            self.activate_menu(open, menu.items[index].clone());
                        }
                    }
                    return;
                }
                if self.y < MENUBAR {
                    return;
                }
            }
            if self.toasts.click(self.toast_area(), self.x, self.y) {
                return;
            }
        } else if self.selecting.take().is_some() {
            return;
        }
        if !down {
            self.drag = None;
            self.resize = None;
            if let Some((id, cid)) = self.pressed.take() {
                let (x, y) = (self.x, self.y);
                let mut dismiss = false;
                if let Some(w) = self.window(id) {
                    if let Some(c) = w.controls.iter().find(|c| c.id == cid) {
                        if c.rect.contains(x - w.rect.x - 2, y - w.rect.y - TITLE) {
                            dismiss = w.message && w.answer.is_empty() && cid == 2;
                            w.event(1, cid, String::new());
                        }
                    }
                }
                if dismiss {
                    self.close(id);
                }
            }
            return;
        }
        for (r, slot) in self.dock_slots() {
            if r.contains(self.x, self.y) {
                match slot {
                    Slot::Item(index) => match &self.config.dock[index].command {
                        DockCommand::Launch(program) => self.launch = Some(program.clone()),
                        DockCommand::Exit => self.quit = !self.installing(),
                    },
                    Slot::Window(id) => self.focus(id),
                }
                return;
            }
        }
        let (x, y) = (self.x, self.y);
        // The frame of a resizable window answers before its contents do, and
        // reaches a little outside the window, so its edges stay easy to grab.
        let Some((id, edges)) = self
            .windows
            .iter()
            .rev()
            .filter(|w| !w.minimized)
            .find_map(|w| match self.resize_edges(w, x, y) {
                Some(edges) => Some((w.id, Some(edges))),
                None => w.rect.contains(x, y).then_some((w.id, None)),
            })
        else {
            return;
        };
        self.focus(id);
        if let Some((horizontal, vertical)) = edges {
            self.resize = Some(Resize {
                window: id,
                horizontal,
                vertical,
                start: self.window_rect(id).expect("the window was just focused"),
                from: (x, y),
            });
            return;
        }
        let ceiling = top(&self.config);
        let screen_width = self.screen_width;
        let floor = self.floor();
        let w = self.windows.last_mut().unwrap();
        let r = w.rect;
        if y < r.y + TITLE {
            if x >= r.x + r.w - 26 {
                self.close(id);
            } else if x >= r.x + r.w - 52 {
                if let Some(old) = w.restore.take() {
                    w.rect = old;
                } else {
                    w.restore = Some(w.rect);
                    w.rect = Rect {
                        x: 0,
                        y: ceiling,
                        w: screen_width,
                        h: floor - ceiling,
                    };
                }
                w.resize();
            } else if x >= r.x + r.w - 78 {
                w.minimized = true;
            } else if w.restore.is_none() {
                self.drag = Some((id, x - r.x, y - r.y));
            }
            return;
        }
        if w.raw_input {
            self.raw_capture = Some(id);
            w.event(8, 1, format!("{} {} 1", x - r.x - 2, y - r.y - TITLE));
            return;
        }
        w.active_control = None;
        for c in w.controls.iter().rev() {
            if c.rect.contains(x - r.x - 2, y - r.y - TITLE) {
                if c.selection.selectable {
                    let cid = c.id;
                    w.active_control = Some(cid);
                    self.select_at(id, Some(cid), true);
                    self.selecting = Some((id, Some(cid)));
                    return;
                }
                match c.kind {
                    2 => self.pressed = Some((id, c.id)),
                    3 => w.active_control = Some(c.id),
                    _ => (),
                }
                return;
            }
        }
        w.event(5, 0, format!("{} {}", x - r.x - 2, y - r.y - TITLE));
    }
    pub fn key(&mut self, bytes: &[u8]) {
        self.key_mod(bytes, 0);
    }
    pub fn key_mod(&mut self, bytes: &[u8], modifiers: u32) {
        if bytes == b"\x1b" && (self.menu.take().is_some() || self.open_menu.take().is_some()) {
            return;
        }
        let Some(w) = self.windows.iter_mut().rev().find(|w| !w.minimized) else {
            return;
        };
        if w.raw_input {
            w.event(6, modifiers, String::from_utf8_lossy(bytes).into_owned());
            return;
        }
        if w.message && (bytes == b"\r" || bytes == b"\x1b") {
            // A dialog with answers reports one; a plain message just closes.
            let answer = if bytes == b"\r" {
                w.answer.last().copied()
            } else {
                // As on other desktops, Escape answers Cancel when there is
                // one, and OK when that is the only button.
                w.answer
                    .iter()
                    .find(|a| **a == ANSWER_CANCEL)
                    .or_else(|| w.answer.first().filter(|_| w.answer.len() == 1))
                    .copied()
            };
            match answer {
                Some(answer) => w.event(1, answer, String::new()),
                None => {
                    let id = w.id;
                    self.close(id);
                }
            }
            return;
        }
        if bytes == b"\t" {
            let ids: Vec<u32> = w
                .controls
                .iter()
                .filter(|c| c.kind != 1)
                .map(|c| c.id)
                .collect();
            if !ids.is_empty() {
                let next = w
                    .active_control
                    .and_then(|id| ids.iter().position(|i| *i == id))
                    .map_or(0, |i| (i + 1) % ids.len());
                w.active_control = Some(ids[next]);
            }
            return;
        }
        if let Some(index) = w
            .controls
            .iter()
            .position(|c| Some(c.id) == w.active_control)
        {
            let c = &mut w.controls[index];
            let id = c.id;
            if c.kind == 2 && (bytes == b"\r" || bytes == b" ") {
                w.event(1, id, String::new());
                return;
            }
            if c.kind == 3 {
                let old = c.text.clone();
                if c.selection.selectable {
                    if bytes == [127] || bytes == [8] {
                        if c.selection.range().is_empty() {
                            c.selection.anchor = c.text[..c.selection.caret]
                                .char_indices()
                                .last()
                                .map_or(0, |(i, _)| i);
                        }
                        c.selection.replace(&mut c.text, "");
                    } else if bytes == b"\x1b[3~" {
                        if c.selection.range().is_empty() {
                            c.selection.anchor = c.text[c.selection.caret..]
                                .chars()
                                .next()
                                .map_or(c.selection.caret, |ch| c.selection.caret + ch.len_utf8());
                        }
                        c.selection.replace(&mut c.text, "");
                    } else if bytes == b"\x1b[D"
                        || bytes == b"\x1b[C"
                        || bytes == b"\x1b[H"
                        || bytes == b"\x1b[F"
                    {
                        let pos = match bytes {
                            b"\x1b[H" => 0,
                            b"\x1b[F" => c.text.len(),
                            b"\x1b[D" => c.text[..c.selection.caret]
                                .char_indices()
                                .last()
                                .map_or(0, |(i, _)| i),
                            _ => c.text[c.selection.caret..]
                                .chars()
                                .next()
                                .map_or(c.selection.caret, |ch| c.selection.caret + ch.len_utf8()),
                        };
                        c.selection.anchor = pos;
                        c.selection.caret = pos;
                    } else if bytes.iter().all(|b| (32..127).contains(b)) {
                        c.selection
                            .replace(&mut c.text, &String::from_utf8_lossy(bytes));
                    }
                } else {
                    if bytes == [127] || bytes == [8] {
                        c.text.pop();
                    } else if bytes.iter().all(|b| (32..127).contains(b))
                        && c.text.len() + bytes.len() <= 1024
                    {
                        c.text.push_str(&String::from_utf8_lossy(bytes));
                    }
                    c.selection.reset(&c.text);
                }
                if old != c.text {
                    let text = c.text.clone();
                    w.event(2, id, text);
                }
                if bytes == b"\r" {
                    let text = w.controls[index].text.clone();
                    w.event(3, id, text);
                }
                return;
            }
        }
        w.event(6, modifiers, String::from_utf8_lossy(bytes).into_owned());
    }
    pub fn raw_pointer_motion(&mut self, dx: i32, dy: i32) {
        let (x, y) = (self.x, self.y);
        let target = self.raw_capture.unwrap_or_else(|| {
            self.windows
                .iter()
                .rev()
                .find(|w| !w.minimized && w.raw_input && w.rect.contains(x, y))
                .map_or(0, |w| w.id)
        });
        if let Some(w) = self.windows.iter_mut().find(|w| w.id == target) {
            w.event(
                8,
                0,
                format!("{} {} 0", x - w.rect.x - 2, y - w.rect.y - TITLE),
            );
        }
        let _ = (dx, dy);
    }
    pub fn raw_pointer_right(&mut self) {
        let (x, y) = (self.x, self.y);
        if let Some(w) = self
            .windows
            .iter_mut()
            .rev()
            .find(|w| !w.minimized && w.raw_input && w.rect.contains(x, y))
        {
            w.event(
                8,
                2,
                format!("{} {} 2", x - w.rect.x - 2, y - w.rect.y - TITLE),
            );
        }
    }
    /// Deliver relative wheel motion to the window under the pointer.
    /// `axis` is 0 for vertical and 1 for horizontal; positive delta scrolls up/left.
    pub fn scroll_wheel(&mut self, delta: i32, axis: u32) {
        if delta == 0 || axis > 1 {
            return;
        }
        let (x, y) = (self.x, self.y);
        if let Some(w) = self
            .windows
            .iter_mut()
            .rev()
            .find(|w| !w.minimized && w.rect.contains(x, y))
        {
            w.event(
                10,
                delta as u32,
                format!("{} {} {axis}", x - w.rect.x - 2, y - w.rect.y - TITLE),
            );
        }
    }
    pub fn draw(&mut self, fb: &mut Surface) {
        self.draw_scene(fb);
        let mut cursor = crate::cursor::SoftwareCursor::default();
        cursor.show(fb, self.x, self.y);
    }
    /// Fill the desktop behind the windows: the configured wallpaper, scaled
    /// to cover the screen without distorting it, or the flat background.
    fn draw_background(&mut self, fb: &mut Surface) {
        let (width, height) = (fb.width(), fb.height());
        let Some(image) = &self.wallpaper else {
            fb.pixels_mut().fill(BLACK);
            return;
        };
        if self.background.width() != width || self.background.height() != height {
            self.background.reset(width, height, BLACK);
            // Cover the screen: scale by the larger of the two ratios and
            // centre the result, letting the surface clip what hangs over.
            let (iw, ih) = (image.width.max(1), image.height.max(1));
            let scale = (width * 1024)
                .div_ceil(iw)
                .max((height * 1024).div_ceil(ih));
            let w = (iw * scale / 1024).max(width) as i32;
            let h = (ih * scale / 1024).max(height) as i32;
            self.background.draw_image_smooth(
                (width as i32 - w) / 2,
                (height as i32 - h) / 2,
                w,
                h,
                image.view(),
            );
        }
        fb.draw_surface(0, 0, &self.background);
    }
    pub fn draw_scene(&mut self, fb: &mut Surface) {
        self.set_screen_size(fb.width(), fb.height());
        self.draw_background(fb);
        let focused = self
            .windows
            .iter()
            .rev()
            .find(|w| !w.minimized)
            .map(|w| w.id);
        for w in &mut self.windows {
            if w.minimized {
                continue;
            }
            let r = w.rect;
            let color = w.color;
            fb.fill_rect(r.x, r.y, r.w, r.h, color);
            fb.fill_rect(r.x + 2, r.y + 2, r.w - 4, TITLE - 4, BLACK);
            let max_chars = ((r.w - 94) / 8).max(0) as usize;
            let title: String = w.title.chars().take(max_chars).collect();
            self.font.draw(
                fb,
                r.x + 10,
                r.y + 9,
                &title,
                if focused == Some(w.id) {
                    color
                } else {
                    0xffa0a0a0
                },
            );
            let bx = r.x + r.w;
            fb.draw_line(bx - 70, r.y + 17, bx - 60, r.y + 17, 0xffe4c878);
            fb.fill_rect(bx - 45, r.y + 8, 11, 11, ACCENT);
            fb.fill_rect(bx - 44, r.y + 9, 9, 9, BLACK);
            fb.draw_line(bx - 20, r.y + 9, bx - 11, r.y + 18, 0xffef6976);
            fb.draw_line(bx - 11, r.y + 9, bx - 20, r.y + 18, 0xffef6976);
            let content = &mut self.content_scratch;
            content.reset(w.content.width(), w.content.height(), BLACK);
            content.draw_surface(0, 0, &w.content);
            for c in &w.controls {
                let cr = c.rect;
                let tile = &mut self.control_scratch;
                tile.reset(cr.w as usize, cr.h as usize, BLACK);
                if c.kind != 1 {
                    let border =
                        if w.active_control == Some(c.id) || self.pressed == Some((w.id, c.id)) {
                            0xffffffff
                        } else {
                            color
                        };
                    tile.fill_rect(0, 0, cr.w, cr.h, border);
                    tile.fill_rect(
                        1,
                        1,
                        cr.w - 2,
                        cr.h - 2,
                        if c.kind == 2 { 0xff15201d } else { BLACK },
                    );
                }
                if c.kind == 1 || c.kind == 3 {
                    TextLayout::new(&c.text, cr.w, cr.h, c.kind == 1, c.selection.caret).draw(
                        &c.text,
                        &c.selection,
                        tile,
                        &self.font,
                        c.kind == 3 && w.active_control == Some(c.id),
                    );
                } else {
                    let text: String = c
                        .text
                        .chars()
                        .take(((cr.w - 12) / 8).max(0) as usize)
                        .collect();
                    self.font.draw(
                        tile,
                        (cr.w - text.chars().count() as i32 * 8) / 2,
                        (cr.h - self.font.height() as i32) / 2,
                        &text,
                        0xffdedede,
                    );
                }
                content.draw_surface(cr.x, cr.y, tile);
            }
            fb.draw_surface(r.x + 2, r.y + TITLE, content);
            // Over the content, so the corner grip stays visible: it is what
            // says the edges of this window can be dragged.
            if w.resizable && w.restore.is_none() {
                for step in 0..3 {
                    let inset = 3 + step * 4;
                    fb.draw_line(
                        r.x + r.w - 3,
                        r.y + r.h - inset - 1,
                        r.x + r.w - inset - 1,
                        r.y + r.h - 3,
                        color,
                    );
                }
            }
        }
        let items = self.dock_slots();
        if let (Some((first, _)), Some((last, _))) = (items.first(), items.last()) {
            let (first, last) = (*first, *last);
            rounded(
                fb,
                Rect {
                    x: first.x - 9,
                    y: self.screen_height - 58,
                    w: last.x + last.w - first.x + 18,
                    h: 56,
                },
                10,
                0xff303936,
            );
            rounded(
                fb,
                Rect {
                    x: first.x - 8,
                    y: self.screen_height - 57,
                    w: last.x + last.w - first.x + 16,
                    h: 54,
                },
                9,
                0xff141a18,
            );
        }
        for (r, slot) in items {
            let hover = r.contains(self.x, self.y);
            let yy = if hover { r.y - 4 } else { r.y };
            rounded(
                fb,
                Rect {
                    x: r.x,
                    y: yy,
                    w: r.w,
                    h: r.h,
                },
                6,
                if hover { 0xff34433c } else { 0xff222d27 },
            );
            let window = match slot {
                Slot::Window(id) => self.windows.iter().find(|w| w.id == id),
                Slot::Item(_) => None,
            };
            let (label, symbol, color, icon) = match slot {
                Slot::Item(index) => {
                    let item = &self.config.dock[index];
                    (
                        item.label.as_str(),
                        item.symbol.clone(),
                        item.color,
                        self.icons[index].as_ref(),
                    )
                }
                Slot::Window(_) => (
                    window.map_or("", |w| w.title.as_str()),
                    window
                        .map(|w| w.title.chars().take(2).collect())
                        .unwrap_or_default(),
                    window.map_or(ACCENT, |w| w.color),
                    None,
                ),
            };
            match icon {
                Some(image) => {
                    fb.draw_image_scaled(r.x + 6, yy + 4, r.w - 12, r.h - 8, image.view())
                }
                None => self.font.draw(
                    fb,
                    r.x + (r.w - symbol.chars().count() as i32 * 8) / 2,
                    yy + 14,
                    &symbol,
                    color,
                ),
            }
            if let Some(window) = window {
                fb.fill_rect(
                    r.x + r.w / 2 - 2,
                    self.screen_height - 7,
                    4,
                    2,
                    if window.minimized { 0xff65726b } else { color },
                );
            }
            if hover {
                let label: String = label.chars().take(48).collect();
                let width = label.chars().count() as i32 * 8 + 16;
                let x = (r.x + r.w / 2 - width / 2).clamp(0, self.screen_width - width);
                let y = self.screen_height - 82;
                fb.fill_rect(x, y, width, 20, 0xff1e2823);
                self.font.draw(fb, x + 8, y + 5, &label, 0xffeeeeee);
            }
        }
        if let Some(menu) = &self.menu {
            menu.draw(fb, &self.font, self.x, self.y);
        }
        // The bar and its open menu sit above every window, and notifications
        // above both.
        if self.config.menubar {
            let (app, menus) = (self.app_label(), self.menus());
            menu::draw_bar(
                fb,
                &self.font,
                &app,
                self.config.accent,
                &menus,
                self.open_menu,
                (self.x, self.y),
                &self.clock,
            );
            if let Some(open) = self.open_menu.filter(|open| *open < menus.len()) {
                let rect =
                    menus[open].rect(menu::title_rects(&app, &menus)[open].x, self.screen_width);
                menus[open].draw(fb, &self.font, rect, self.x, self.y);
            }
        }
        self.toasts.draw(fb, &self.font, self.toast_area());
    }
}
fn rounded(fb: &mut Surface, r: Rect, radius: i32, color: u32) {
    for y in 0..r.h {
        for x in 0..r.w {
            let dx = if x < radius {
                radius - x
            } else if x >= r.w - radius {
                x - (r.w - radius - 1)
            } else {
                0
            };
            let dy = if y < radius {
                radius - y
            } else if y >= r.h - radius {
                y - (r.h - radius - 1)
            } else {
                0
            };
            if dx * dx + dy * dy <= radius * radius {
                fb.set_pixel(r.x + x, r.y + y, color);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn selectable_control(d: &mut Desktop, id: u32, kind: u32, text: &str, selectable: bool) {
        let mut selection = TextSelection {
            selectable,
            ..Default::default()
        };
        selection.reset(text);
        d.window(id).unwrap().controls.push(Control {
            id: 1,
            kind,
            rect: Rect {
                x: 10,
                y: 10,
                w: 180,
                h: 36,
            },
            text: text.into(),
            selection,
        });
    }
    #[test]
    fn cached_pointer_frames_match_full_redraw_including_hover() {
        let mut d = Desktop::new();
        d.create("Cached window".into(), 400, 250, ACCENT).unwrap();
        let mut cached = Surface::new(800, 600);
        let mut full = Surface::new(800, 600);
        let mut cursor = crate::cursor::SoftwareCursor::default();
        d.draw_scene(&mut cached);
        for (x, y) in [(100, 100), (799, 599), (300, 560), (302, 562), (500, 500)] {
            let dirty = d.motion(x, y);
            cursor.hide(&mut cached);
            if dirty {
                d.draw_scene(&mut cached);
            }
            cursor.show(&mut cached, d.x, d.y);
            d.draw(&mut full);
            assert_eq!(cached.pixels(), full.pixels());
        }
    }
    #[test]
    fn mouse_selection_menu_clipboard_and_editing() {
        let mut d = Desktop::new();
        let id = d.create("Edit".into(), 240, 100, ACCENT).unwrap();
        selectable_control(&mut d, id, 3, "hello world", true);
        let r = d.window(id).unwrap().rect;
        let (x, y) = (r.x + 2 + 10 + 6, r.y + TITLE + 10 + 15);
        d.motion(x, y);
        d.mouse(true);
        d.motion(x + 40, y);
        d.mouse(false);
        assert_eq!(
            d.window(id).unwrap().controls[0]
                .selection
                .selected("hello world"),
            "hello"
        );
        d.right_click();
        let menu = d.menu.as_ref().unwrap().rect;
        d.motion(menu.x + 8, menu.y + 8);
        d.mouse(true);
        d.mouse(false);
        assert_eq!(d.clipboard, "hello");
        assert!(d.menu.is_none());
        assert!(d.shortcut(Action::Cut));
        assert_eq!(d.window(id).unwrap().controls[0].text, " world");
        assert_eq!(d.window(id).unwrap().events.back().unwrap().kind, 2);
        d.shortcut(Action::Paste);
        assert_eq!(d.window(id).unwrap().controls[0].text, "hello world");
        d.shortcut(Action::SelectAll);
        d.key(b"replacement");
        assert_eq!(d.window(id).unwrap().controls[0].text, "replacement");
        d.key(b"\x1b[H");
        d.key(b"\x1b[3~");
        assert_eq!(d.window(id).unwrap().controls[0].text, "eplacement");
        d.right_click();
        d.remove_control(id, 1).unwrap();
        assert!(d.menu.is_none());
        assert!(d.selecting.is_none());
    }
    #[test]
    fn readonly_and_default_text_and_window_switching() {
        let mut d = Desktop::new();
        let first = d.create("First".into(), 240, 100, ACCENT).unwrap();
        selectable_control(&mut d, first, 1, "read only", true);
        d.window(first).unwrap().active_control = Some(1);
        d.shortcut(Action::SelectAll);
        d.shortcut(Action::Cut);
        d.shortcut(Action::Paste);
        assert_eq!(d.window(first).unwrap().controls[0].text, "read only");
        let second = d.create("Second".into(), 240, 100, ACCENT).unwrap();
        selectable_control(&mut d, second, 3, "default", false);
        d.window(second).unwrap().active_control = Some(1);
        assert!(!d.shortcut(Action::SelectAll));
        let third = d.create("Third".into(), 240, 100, ACCENT).unwrap();
        d.shortcut(Action::SwitchWindow(false));
        assert_eq!(d.windows.last().unwrap().id, second);
        d.shortcut(Action::SwitchWindow(false));
        assert_eq!(d.windows.last().unwrap().id, first);
        d.end_window_cycle();
        d.shortcut(Action::SwitchWindow(false));
        assert_eq!(d.windows.last().unwrap().id, second);
        d.window(third).unwrap().minimized = true;
        d.end_window_cycle();
        d.shortcut(Action::SwitchWindow(true));
        assert_eq!(d.windows.last().unwrap().id, first);
    }
    #[test]
    fn the_menu_bar_presents_the_focused_window_and_delivers_its_items() {
        let mut d = Desktop::new();
        let id = d.create("Editor".into(), 240, 100, ACCENT).unwrap();
        d.window(id).unwrap().menus = vec![Menu::new(
            "File",
            vec![
                MenuItem::new(7, "Save"),
                MenuItem::rule(),
                MenuItem::new(8, "Revert").enabled(false),
            ],
        )];
        assert_eq!(d.app_label(), "Editor");
        assert_eq!(d.menus().len(), 2, "the system menu plus the window menu");
        let titles = menu::title_rects(&d.app_label(), &d.menus());
        // Opening a menu, then hovering another title, moves the open menu.
        d.motion(titles[1].x + 4, 6);
        d.mouse(true);
        assert_eq!(d.open_menu, Some(1));
        assert!(
            d.motion(titles[0].x + 4, 6),
            "an open menu tracks the pointer"
        );
        assert_eq!(d.open_menu, Some(0));
        d.motion(titles[1].x + 4, 6);
        d.mouse(true);
        assert_eq!(d.open_menu, None, "clicking the open title closes it");
        d.mouse(true);
        assert_eq!(d.open_menu, Some(1));
        // Choosing an item queues a menu event for the focused window.
        let menus = d.menus();
        let rect = menus[1].rect(titles[1].x, 800);
        d.motion(rect.x + 8, rect.y + 6);
        d.mouse(true);
        assert_eq!(d.open_menu, None);
        let event = d.window(id).unwrap().events.back().unwrap().clone();
        assert_eq!(
            (event.kind, event.control, event.text.as_str()),
            (11, 7, "Save")
        );
        // Disabled items and separators queue nothing, and close the menu.
        d.motion(titles[1].x + 4, 6);
        d.mouse(true);
        assert_eq!(d.open_menu, Some(1));
        d.motion(rect.x + 8, rect.y + 4 + 22 + 7 + 4);
        d.mouse(true);
        assert_eq!(d.open_menu, None);
        assert_eq!(d.window(id).unwrap().events.back().unwrap().control, 7);
        d.motion(titles[1].x + 4, 6);
        d.mouse(true);
        assert!(d.shortcut(Action::Dismiss));
        assert_eq!(d.open_menu, None);
        // A click on the bar itself never reaches a window behind it.
        let w = d.window(id).unwrap();
        (w.rect.x, w.rect.y) = (550, MENUBAR);
        d.motion(700, 4);
        d.mouse(true);
        assert!(d.windows[0].events.iter().all(|e| e.kind != 5));
        // Without a focused window the bar still offers the system menu.
        d.close(id);
        assert_eq!(d.app_label(), "hOS");
        assert_eq!(d.menus().len(), 1);
    }
    #[test]
    fn the_system_menu_captures_screenshots_and_leaves_the_session() {
        let mut d = Desktop::new();
        assert!(!d.shortcut(Action::Exit), "the session owns exiting");
        assert!(d.shortcut(Action::ScreenshotWindow));
        assert_eq!(d.screenshot, None, "no window to capture");
        assert_eq!(d.toasts.len(), 1);
        let id = d.create("Shot".into(), 240, 100, ACCENT).unwrap();
        assert!(d.shortcut(Action::ScreenshotWindow));
        assert_eq!(d.screenshot.take(), Some(Shot::Window(id)));
        // The same items are reachable from the bar.
        let titles = menu::title_rects(&d.app_label(), &d.menus());
        d.motion(titles[0].x + 4, 6);
        d.mouse(true);
        let system = d.menus()[0].rect(titles[0].x, 800);
        d.motion(system.x + 8, system.y + 6);
        d.mouse(true);
        assert_eq!(d.screenshot.take(), Some(Shot::Screen));
        // "Clear notifications" and the exit item act immediately.
        d.toast("kept briefly", ACCENT, 1000);
        d.motion(titles[0].x + 4, 6);
        d.mouse(true);
        d.motion(system.x + 8, system.y + 4 + 22 * 2 + 7);
        d.mouse(true);
        assert!(d.toasts.is_empty());
        d.motion(titles[0].x + 4, 6);
        d.mouse(true);
        d.motion(system.x + 8, system.y + 4 + 22 * 3 + 7 * 2);
        d.mouse(true);
        assert!(d.quit);
    }
    #[test]
    fn message_boxes_report_their_answer_and_wait_for_the_client() {
        let mut d = Desktop::new();
        let id = d
            .ask(
                "Delete".into(),
                "Delete the file?".into(),
                ANSWER_BUTTONS_YES_NO,
                SEVERITY_QUESTION,
            )
            .unwrap();
        let answer = |d: &mut Desktop, id: u32| d.window(id).unwrap().events.pop_back().unwrap();
        let rect = d.window_rect(id).unwrap();
        let button = d
            .window(id)
            .unwrap()
            .controls
            .iter()
            .find(|c| c.id == ANSWER_YES)
            .unwrap()
            .rect;
        d.motion(rect.x + 2 + button.x + 4, rect.y + TITLE + button.y + 4);
        d.mouse(true);
        d.mouse(false);
        let event = answer(&mut d, id);
        assert_eq!((event.kind, event.control), (1, ANSWER_YES));
        assert!(
            d.window(id).is_some(),
            "the window stays open until the client closes it"
        );
        // Enter answers with the primary button, which sits rightmost.
        d.key(b"\r");
        assert_eq!(answer(&mut d, id).control, ANSWER_YES);
        // Escape has no answer here, so it closes the window instead.
        d.key(b"\x1b");
        assert!(d.window(id).is_none());
        let id = d
            .ask(
                "Quit".into(),
                "Save first?".into(),
                ANSWER_BUTTONS_OK_CANCEL,
                SEVERITY_WARNING,
            )
            .unwrap();
        d.key(b"\x1b");
        assert_eq!(answer(&mut d, id).control, ANSWER_CANCEL);
        d.close(id);
        // Plain message boxes keep dismissing themselves.
        let id = d.message_box("Note".into(), "Done".into(), ACCENT).unwrap();
        d.key(b"\r");
        assert!(d.window(id).is_none());
        assert!(d.ask("t".into(), "m".into(), 9, SEVERITY_INFO).is_err());
        assert!(d.ask("t".into(), "m".into(), ANSWER_BUTTONS_OK, 9).is_err());
    }
    #[test]
    fn the_dock_comes_from_the_configuration() {
        let dir = std::env::temp_dir().join(format!("hoswm-dock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let icon = dir.join("icon.qoi");
        crate::qoi::save(&icon, 2, 2, &[ACCENT; 4]).unwrap();
        let mut config = Config::parse(
            &format!(
                "[dock]
item = Files | /bin/files | F | 0xff112233 | {}
item = Quit | @exit
item = Broken icon | /bin/x | X | 0xff112233 | missing.qoi
",
                icon.display()
            ),
            &dir,
        );
        assert!(config.warnings.is_empty());
        config.toast_db = dir.join("toastdb");
        let mut d = Desktop::with_config(config);
        assert_eq!(d.config.warnings.len(), 1, "an unreadable icon warns");
        assert!(d.icons[0].is_some() && d.icons[2].is_none());
        let slots = d.dock_slots();
        assert_eq!(slots.len(), 3);
        d.motion(slots[0].0.x + 2, slots[0].0.y + 2);
        d.mouse(true);
        assert_eq!(d.launch.take(), Some("/bin/files".to_string()));
        d.motion(slots[1].0.x + 2, slots[1].0.y + 2);
        d.mouse(true);
        assert!(d.quit);
        // Window buttons follow the configured items.
        let id = d.create("Window".into(), 200, 100, ACCENT).unwrap();
        assert_eq!(d.dock_slots()[3].1, Slot::Window(id));
        let mut fb = Surface::new(800, 600);
        d.draw_scene(&mut fb);
        // An empty dock is legal and draws nothing below the desktop.
        d.config.dock.clear();
        d.icons.clear();
        d.windows.clear();
        assert!(d.dock_slots().is_empty());
        assert_eq!(d.dock_hover(), None);
        d.draw_scene(&mut fb);
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn notifications_are_shown_above_the_desktop_and_dismissed_by_clicking() {
        let dir = std::env::temp_dir().join(format!("hoswm-notify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = Config::defaults_in(&dir);
        config.toast_corner = crate::toast::Corner::TopRight;
        let mut d = Desktop::with_config(config);
        d.toast("Keyboard connected", ACCENT, 1000);
        assert_eq!(d.toasts.len(), 1);
        let area = d.toast_area();
        assert_eq!(area.y, MENUBAR + 10, "notifications clear the menu bar");
        assert_eq!(area.y + area.h, FLOOR, "and stay above the dock");
        let mut fb = Surface::new(800, 600);
        d.draw_scene(&mut fb);
        assert_ne!(fb.pixels()[(area.y as usize + 2) * 800 + 784], BLACK);
        // A click in the notification dismisses it instead of reaching the desktop.
        d.motion(784, area.y + 4);
        d.mouse(true);
        assert!(d.toasts.is_empty());
        let (records, _) = crate::toast::read(&dir.join("toastdb")).unwrap();
        assert_eq!(records[0].text, "Keyboard connected");
        assert!(records[0].shown_ms < 1000, "dismissed early");
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn window_lifecycle_and_gui() {
        let mut d = Desktop::new();
        let id = d.create("Test".into(), 240, 100, ACCENT).unwrap();
        let r = d.window(id).unwrap().rect;
        d.motion(r.x + 20, r.y + 10);
        d.mouse(true);
        d.motion(120, 100);
        d.mouse(false);
        assert_eq!(d.window(id).unwrap().rect.x, 100);
        let r = d.window(id).unwrap().rect;
        d.motion(r.x + r.w - 65, r.y + 10);
        d.mouse(true);
        assert!(d.window(id).unwrap().minimized);
        d.focus(id);
        d.motion(r.x + r.w - 40, r.y + 10);
        d.mouse(true);
        // Maximized windows fill the desktop without covering the menu bar.
        assert_eq!(
            d.window(id).unwrap().rect,
            Rect {
                x: 0,
                y: MENUBAR,
                w: 800,
                h: FLOOR - MENUBAR
            }
        );
        d.motion(760, MENUBAR + 10);
        d.mouse(true);
        assert_eq!(d.window(id).unwrap().rect, r);
        let mut fb = Surface::new(800, 600);
        d.draw(&mut fb);
        assert_ne!(fb.pixels()[0], BLACK, "the menu bar owns the top row");
        assert_eq!(fb.pixels()[MENUBAR as usize * 800], BLACK);
        d.close(id);
        assert!(d.windows.is_empty());
    }
    #[test]
    fn the_wallpaper_covers_the_desktop_when_the_file_is_there() {
        let dir = std::env::temp_dir().join(format!("hoswm-wall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut config = Config::defaults_in(&dir);
        let mut fb = Surface::new(800, 600);
        // No file: the desktop stays the flat background it has always been.
        let mut d = Desktop::with_config(config.clone());
        assert!(
            d.config.warnings.is_empty(),
            "a missing wallpaper is normal"
        );
        d.draw_scene(&mut fb);
        assert_eq!(fb.pixels()[400 * 800 + 400], BLACK);
        // An image of a different shape is scaled to cover the screen, so the
        // desktop is painted with it edge to edge rather than left blank.
        crate::qoi::save(&config.wallpaper, 64, 16, &[0xff204030; 64 * 16]).unwrap();
        d = Desktop::with_config(config.clone());
        d.draw_scene(&mut fb);
        for (x, y) in [
            (0, MENUBAR as usize),
            (799, MENUBAR as usize),
            (0, FLOOR as usize - 1),
            (799, FLOOR as usize - 1),
            (400, 300),
        ] {
            assert_eq!(fb.pixels()[y * 800 + x], 0xff204030, "{x},{y}");
        }
        // An unreadable image is a configuration warning, not a failure.
        std::fs::write(&config.wallpaper, b"not an image").unwrap();
        config.toast_db = dir.join("toastdb");
        let d = Desktop::with_config(config);
        assert_eq!(d.config.warnings.len(), 1);
        assert!(d.wallpaper.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn only_resizable_windows_follow_a_dragged_edge() {
        let mut d = Desktop::new();
        let fixed = d.create("Fixed".into(), 240, 100, ACCENT).unwrap();
        let before = d.window(fixed).unwrap().rect;
        // Without the flag the frame is inert: the press falls through to the
        // window itself and the pointer event it reports.
        d.motion(before.x + before.w - 1, before.y + before.h - 1);
        d.mouse(true);
        d.motion(before.x + before.w + 60, before.y + before.h + 40);
        assert_eq!(d.window(fixed).unwrap().rect, before);
        d.mouse(false);
        d.close(fixed);

        let id = d.create("Sized".into(), 240, 100, ACCENT).unwrap();
        d.window(id).unwrap().resizable = true;
        let start = d.window(id).unwrap().rect;
        // The grab reaches outside the frame, and the corner takes both edges.
        d.motion(start.x + start.w + 1, start.y + start.h + 1);
        d.mouse(true);
        assert!(d.resize.is_some());
        d.motion(start.x + start.w + 61, start.y + start.h + 41);
        let grown = d.window(id).unwrap().rect;
        assert_eq!((grown.w, grown.h), (start.w + 60, start.h + 40));
        assert_eq!((grown.x, grown.y), (start.x, start.y));
        assert_eq!(
            (
                d.window(id).unwrap().content.width(),
                d.window(id).unwrap().content.height()
            ),
            ((grown.w - 4) as usize, (grown.h - TITLE - 2) as usize),
            "the content follows the frame"
        );
        assert_eq!(
            d.window(id).unwrap().events.back().unwrap().kind,
            4,
            "and the client is told"
        );
        // Dragging into the corner of the screen stops at the edge of the
        // desktop: the width follows the pointer, the height meets the dock.
        d.motion(799, 599);
        let full = d.window(id).unwrap().rect;
        assert!(full.x + full.w <= 800 && full.x + full.w >= 790, "{full:?}");
        assert_eq!(full.y + full.h, FLOOR);
        d.mouse(false);
        assert!(d.resize.is_none());

        // The left edge moves the frame; the right edge stays where it is, and
        // the window never shrinks below the smallest size a client may ask for.
        let start = d.window(id).unwrap().rect;
        d.motion(start.x + 1, start.y + start.h / 2);
        d.mouse(true);
        d.motion(start.x + start.w + 200, start.y + start.h / 2);
        let squeezed = d.window(id).unwrap().rect;
        assert_eq!(squeezed.w, MIN_CONTENT.0 + 4);
        assert_eq!(squeezed.x + squeezed.w, start.x + start.w);
        d.mouse(false);
        // A maximized window has no edges to drag: that is the restore button's.
        let w = d.window(id).unwrap();
        w.restore = Some(w.rect);
        let maximized = w.rect;
        let window = d.windows.last().unwrap();
        assert!(d.resize_edges(window, maximized.x, maximized.y).is_none());
    }
    #[test]
    fn textbox_events() {
        let mut d = Desktop::new();
        let id = d.create("GUI".into(), 200, 100, ACCENT).unwrap();
        let w = d.window(id).unwrap();
        w.controls.push(Control {
            selection: TextSelection::default(),
            id: 7,
            kind: 3,
            rect: Rect {
                x: 10,
                y: 10,
                w: 180,
                h: 24,
            },
            text: String::new(),
        });
        d.key(b"\t");
        d.key(b"hi");
        d.key(&[127]);
        assert_eq!(d.window(id).unwrap().controls[0].text, "h");
        assert_eq!(d.window(id).unwrap().events.back().unwrap().kind, 2);
        // Removing a focused, pressed control must not leave input targeting it.
        d.pressed = Some((id, 7));
        d.window(id).unwrap().event(4, 7, "100".into());
        d.remove_control(id, 7).unwrap();
        assert!(d.pressed.is_none());
        let w = d.window(id).unwrap();
        assert!(w.active_control.is_none());
        assert!(w.controls.is_empty());
        assert_eq!(w.events.len(), 1);
        assert_eq!(w.events[0].kind, 4);
        d.key(b"x");
        assert_eq!(d.window(id).unwrap().events.back().unwrap().kind, 6);
        assert!(d.remove_control(id, 7).is_err());
    }
}
