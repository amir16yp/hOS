//! `~/.hoswm/config.ini`: dock contents, icons, toast placement and shortcuts.
//!
//! The file is optional. A missing file is written once with the documented
//! defaults, and any line that cannot be understood is reported as a warning
//! and skipped, so a typo never keeps the session from starting.
use crate::{
    shortcuts::{Action, Binding, Bindings},
    toast::Corner,
};
use std::path::{Path, PathBuf};

pub const FILE: &str = "config.ini";
/// Written the first time a session starts without a configuration file.
pub const TEMPLATE: &str = include_str!("config.default.ini");

/// A dock button: an application to start, or a built-in session action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockCommand {
    Launch(String),
    /// `@exit`: leave the session, as the dock exit button always has.
    Exit,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockItem {
    pub label: String,
    pub command: DockCommand,
    /// Two-character fallback drawn when no icon is configured or readable.
    pub symbol: String,
    pub color: u32,
    /// QOI image, resolved against the configuration directory.
    pub icon: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub dir: PathBuf,
    pub accent: u32,
    pub menubar: bool,
    pub clock: bool,
    pub toast_corner: Corner,
    pub toast_ms: u32,
    pub toast_max: usize,
    pub toast_db: PathBuf,
    pub screenshots: PathBuf,
    /// QOI image drawn behind the windows. A missing file is not a problem:
    /// the desktop is then the flat background colour it has always been.
    pub wallpaper: PathBuf,
    pub dock: Vec<DockItem>,
    pub bindings: Bindings,
    /// Problems found while reading the file; the session shows these.
    pub warnings: Vec<String>,
}

/// The user's home directory, falling back to a writable per-user location.
pub fn home() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return PathBuf::from(home);
    }
    let uid = unsafe { geteuid() };
    if uid == 0 {
        PathBuf::from("/root")
    } else {
        PathBuf::from(format!("/tmp/hoswm-{uid}"))
    }
}
/// The configuration directory, overridable with `HOSWM_HOME`.
pub fn directory() -> PathBuf {
    std::env::var_os("HOSWM_HOME")
        .filter(|d| !d.is_empty())
        .map_or_else(|| home().join(".hoswm"), PathBuf::from)
}
fn resolve(dir: &Path, value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        return home().join(rest);
    }
    let path = PathBuf::from(value);
    if path.is_absolute() { path } else { dir.join(path) }
}
fn boolean(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}
/// Accept `0xAARRGGBB`, `#AARRGGBB` and `#RRGGBB`.
pub fn color(value: &str) -> Option<u32> {
    let text = value.trim();
    let digits = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .or_else(|| text.strip_prefix('#'))?;
    match digits.len() {
        6 => u32::from_str_radix(digits, 16).ok().map(|c| c | 0xff00_0000),
        8 => u32::from_str_radix(digits, 16).ok(),
        _ => None,
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults_in(directory())
    }
}
impl Config {
    pub fn defaults_in(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            accent: crate::desktop::ACCENT,
            menubar: true,
            clock: true,
            toast_corner: Corner::TopRight,
            toast_ms: 4000,
            toast_max: 4,
            toast_db: dir.join("toastdb"),
            screenshots: dir.join("screenshots"),
            wallpaper: dir.join("wallpaper.qoi"),
            dock: vec![
                DockItem {
                    label: "Terminal".into(),
                    command: DockCommand::Launch("/bin/hos-terminal".into()),
                    symbol: ">_".into(),
                    color: crate::desktop::ACCENT,
                    icon: None,
                },
                DockItem {
                    label: "Files".into(),
                    command: DockCommand::Launch("/bin/hos-files".into()),
                    symbol: "Fs".into(),
                    color: 0xff9ccfd8,
                    icon: None,
                },
                DockItem {
                    label: "About hOS".into(),
                    command: DockCommand::Launch("/bin/hos-about".into()),
                    symbol: "i".into(),
                    color: 0xff80afff,
                    icon: None,
                },
                DockItem {
                    label: "Notifications".into(),
                    command: DockCommand::Launch("/bin/hos-notifications".into()),
                    symbol: "!".into(),
                    color: 0xffe4c878,
                    icon: None,
                },
                DockItem {
                    label: "Settings".into(),
                    command: DockCommand::Launch("/bin/hos-settings".into()),
                    symbol: "St".into(),
                    color: 0xff9ccfd8,
                    icon: None,
                },
                DockItem {
                    label: "Install hOS".into(),
                    command: DockCommand::Launch("/bin/hos-installer".into()),
                    symbol: "HD".into(),
                    color: crate::desktop::ACCENT,
                    icon: None,
                },
                DockItem {
                    label: "Exit".into(),
                    command: DockCommand::Exit,
                    symbol: "X".into(),
                    color: 0xffef6976,
                    icon: None,
                },
            ],
            bindings: Bindings::defaults(),
            warnings: Vec::new(),
            dir,
        }
    }
    /// Read the configuration, writing the annotated template when absent.
    pub fn load() -> Self {
        let dir = directory();
        let path = dir.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text, dir),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut config = Self::defaults_in(&dir);
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| {
                    std::fs::write(&path, TEMPLATE)
                }) {
                    config.warnings.push(format!("{}: {e}", path.display()));
                }
                config
            }
            Err(e) => {
                let mut config = Self::defaults_in(&dir);
                config.warnings.push(format!("{}: {e}", path.display()));
                config
            }
        }
    }
    pub fn parse(text: &str, dir: impl Into<PathBuf>) -> Self {
        let mut config = Self::defaults_in(dir);
        let mut section = String::new();
        let mut dock: Option<Vec<DockItem>> = None;
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            let mut warn = |message: String| {
                config.warnings.push(format!("line {}: {message}", number + 1));
            };
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_ascii_lowercase();
                if section == "dock" {
                    dock.get_or_insert_default();
                }
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                warn(format!("{line}: expected key = value"));
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match (section.as_str(), key.as_str()) {
                ("session", "accent") => match color(value) {
                    Some(c) => config.accent = c,
                    None => warn(format!("{value}: expected a 0xAARRGGBB color")),
                },
                ("menubar", "enabled") | ("menubar", "clock") => match boolean(value) {
                    Some(on) if key == "clock" => config.clock = on,
                    Some(on) => config.menubar = on,
                    None => warn(format!("{value}: expected true or false")),
                },
                ("toasts", "corner") => match Corner::parse(value) {
                    Some(corner) => config.toast_corner = corner,
                    None => warn(format!("{value}: expected top-left, top-right, bottom-left or bottom-right")),
                },
                ("toasts", "duration_ms") => match value.parse::<u32>() {
                    Ok(ms) => config.toast_ms = ms.clamp(crate::toast::MIN_MS, crate::toast::MAX_MS),
                    Err(e) => warn(format!("{value}: {e}")),
                },
                ("toasts", "max_visible") => match value.parse::<usize>() {
                    Ok(max) => config.toast_max = max.clamp(1, 8),
                    Err(e) => warn(format!("{value}: {e}")),
                },
                ("session", "wallpaper") => config.wallpaper = resolve(&config.dir, value),
                ("toasts", "database") => config.toast_db = resolve(&config.dir, value),
                ("screenshots", "directory") => config.screenshots = resolve(&config.dir, value),
                ("dock", "item") => match parse_dock_item(value, &config.dir, config.accent) {
                    Ok(item) => dock.get_or_insert_default().push(item),
                    Err(e) => warn(e),
                },
                ("shortcuts", name) => match Action::parse(name) {
                    Some(action) if matches!(value.to_ascii_lowercase().as_str(), "" | "none" | "off") => {
                        config.bindings.set(action, None)
                    }
                    Some(action) => match Binding::parse(value) {
                        Ok(binding) => config.bindings.set(action, Some(binding)),
                        Err(e) => warn(e),
                    },
                    None => warn(format!("{name}: unknown action")),
                },
                ("", _) => warn(format!("{key}: outside any section")),
                (section, key) => warn(format!("{section}: unknown setting {key}")),
            }
        }
        if let Some(dock) = dock {
            config.dock = dock;
        }
        config
    }
}
// `label | command | symbol | color | icon`, with everything after the
// command optional.
fn parse_dock_item(value: &str, dir: &Path, accent: u32) -> Result<DockItem, String> {
    let mut fields = value.split('|').map(str::trim);
    let label = fields.next().unwrap_or_default();
    let command = fields.next().unwrap_or_default();
    if label.is_empty() || command.is_empty() {
        return Err(format!("{value}: expected label | command"));
    }
    let symbol = fields.next().filter(|s| !s.is_empty());
    let color = fields.next().filter(|s| !s.is_empty());
    let icon = fields.next().filter(|s| !s.is_empty());
    Ok(DockItem {
        label: label.chars().take(48).collect(),
        command: if command == "@exit" {
            DockCommand::Exit
        } else if let Some(rest) = command.strip_prefix('@') {
            return Err(format!("{rest}: unknown built-in dock action"));
        } else {
            DockCommand::Launch(command.into())
        },
        symbol: symbol
            .map(|s| s.chars().take(2).collect())
            .unwrap_or_else(|| label.chars().take(2).collect()),
        color: match color {
            Some(text) => self::color(text).ok_or(format!("{text}: expected a 0xAARRGGBB color"))?,
            None => accent,
        },
        icon: icon.map(|path| resolve(dir, path)),
    })
}
unsafe extern "C" {
    fn geteuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_written_template_reproduces_the_built_in_defaults() {
        let dir = PathBuf::from("/home/test/.hoswm");
        let parsed = Config::parse(TEMPLATE, &dir);
        assert_eq!(parsed.warnings, Vec::<String>::new());
        assert_eq!(parsed, Config::defaults_in(&dir));
    }
    #[test]
    fn settings_override_defaults() {
        let dir = PathBuf::from("/cfg");
        let config = Config::parse(
            "\
[session]
accent = #ff8800

[menubar]
enabled = false
clock = off

[toasts]
corner = bottom-left
duration_ms = 100
max_visible = 99
database = /var/log/toastdb

[screenshots]
directory = shots

[dock]
item = Files | /bin/files | F | 0xff112233 | icons/files.qoi
item = Quit | @exit

[shortcuts]
copy = ctrl+insert
screenshot = none
",
            &dir,
        );
        assert_eq!(config.warnings, Vec::<String>::new());
        assert_eq!(config.accent, 0xffff8800);
        assert!(!config.menubar && !config.clock);
        assert_eq!(config.toast_corner, Corner::BottomLeft);
        assert_eq!(config.toast_ms, crate::toast::MIN_MS, "clamped");
        assert_eq!(config.toast_max, 8, "clamped");
        assert_eq!(config.toast_db, PathBuf::from("/var/log/toastdb"));
        assert_eq!(config.screenshots, dir.join("shots"));
        assert_eq!(config.dock.len(), 2);
        assert_eq!(config.dock[0].icon, Some(dir.join("icons/files.qoi")));
        assert_eq!(config.dock[0].symbol, "F");
        assert_eq!(config.dock[1].command, DockCommand::Exit);
        assert_eq!(config.dock[1].symbol, "Qu", "the label supplies a symbol");
        assert_eq!(config.dock[1].color, config.accent);
        assert_eq!(config.bindings.get(Action::Copy), Some(Binding::new(110).ctrl()));
        assert_eq!(config.bindings.get(Action::Screenshot), None);
    }
    #[test]
    fn bad_lines_warn_without_losing_the_rest_of_the_file() {
        let config = Config::parse(
            "\
accent = 0xff000000
[session]
accent = blue
color = 0xff000000
[toasts]
corner = middle
duration_ms = soon
[dock]
item = Broken
item = Bad color | /bin/x | x | purple
item = Missing | @reboot
item = Good | /bin/good
[shortcuts]
copy = ctrl+nope
fly = ctrl+f
missing-equals
",
            "/cfg",
        );
        assert_eq!(config.warnings.len(), 11, "{:#?}", config.warnings);
        assert!(config.warnings[0].starts_with("line 1:"));
        assert_eq!(config.accent, crate::desktop::ACCENT);
        assert_eq!(config.toast_corner, Corner::TopRight);
        assert_eq!(config.dock.len(), 1, "only valid dock items are kept");
        assert_eq!(config.dock[0].label, "Good");
        assert_eq!(config.bindings.get(Action::Copy), Bindings::defaults().get(Action::Copy));
    }
    #[test]
    fn an_empty_dock_section_empties_the_dock() {
        assert!(Config::parse("[dock]\n", "/cfg").dock.is_empty());
        assert_eq!(Config::parse("", "/cfg").dock.len(), 7);
    }
    #[test]
    fn paths_resolve_against_the_configuration_directory() {
        assert_eq!(resolve(Path::new("/cfg"), "shots"), PathBuf::from("/cfg/shots"));
        assert_eq!(resolve(Path::new("/cfg"), "/tmp/a"), PathBuf::from("/tmp/a"));
        assert_eq!(resolve(Path::new("/cfg"), "~/a"), home().join("a"));
        assert_eq!(color("0xff72dbac"), Some(0xff72dbac));
        assert_eq!(color("#72dbac"), Some(0xff72dbac));
        assert_eq!(color("72dbac"), None);
        assert_eq!(boolean("Yes"), Some(true));
        assert_eq!(boolean("maybe"), None);
    }
}
