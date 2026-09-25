//! Global and text-edit shortcuts, separate from the evdev keymap.
//!
//! Bindings are data, not code: `~/.hoswm/config.ini` may rebind every action
//! in the `[shortcuts]` section. [`Bindings::defaults`] is what an empty
//! configuration means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Copy,
    Cut,
    Paste,
    SelectAll,
    Delete,
    SwitchWindow(bool),
    Dismiss,
    /// Write the whole screen to the configured screenshot directory.
    Screenshot,
    /// Write only the focused window.
    ScreenshotWindow,
    /// Leave the session, unless an application protects critical work.
    Exit,
}
impl Action {
    /// Configuration name, and the order `[shortcuts]` keys are documented in.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::Paste => "paste",
            Self::SelectAll => "select_all",
            Self::Delete => "delete",
            Self::SwitchWindow(_) => "switch_window",
            Self::Dismiss => "dismiss",
            Self::Screenshot => "screenshot",
            Self::ScreenshotWindow => "screenshot_window",
            Self::Exit => "exit",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Copy,
            Self::Cut,
            Self::Paste,
            Self::SelectAll,
            Self::Delete,
            Self::SwitchWindow(false),
            Self::Dismiss,
            Self::Screenshot,
            Self::ScreenshotWindow,
            Self::Exit,
        ]
        .into_iter()
        .find(|a| a.name() == name.trim().to_ascii_lowercase())
    }
    fn editing(&self) -> bool {
        matches!(
            self,
            Self::Copy | Self::Cut | Self::Paste | Self::SelectAll | Self::Delete
        )
    }
}
#[derive(Default, Clone, Copy)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub code: u16,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}
impl Binding {
    pub fn new(code: u16) -> Self {
        Self {
            code,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }
    pub fn ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }
    pub fn shift(mut self) -> Self {
        self.shift = true;
        self
    }
    pub fn alt(mut self) -> Self {
        self.alt = true;
        self
    }
    /// Parse `ctrl+shift+c`, `alt+tab`, `print` or the `code:99` escape hatch.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim().to_ascii_lowercase();
        let mut binding = Self::new(0);
        let mut key = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part {
                "ctrl" | "control" => binding.ctrl = true,
                "shift" => binding.shift = true,
                "alt" | "meta" => binding.alt = true,
                name => {
                    if key.is_some() {
                        return Err(format!("{text}: more than one key"));
                    }
                    key = Some(key_code(name).ok_or_else(|| format!("unknown key {name}"))?);
                }
            }
        }
        binding.code = key.ok_or_else(|| format!("{text}: no key"))?;
        Ok(binding)
    }
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&key_name(self.code));
        out
    }
}

// Keyboard rows in evdev code order, matching the US map in `keyboard.rs`.
const ROWS: [(u16, &str); 4] = [
    (2, "1234567890-="),
    (16, "qwertyuiop[]"),
    (30, "asdfghjkl;'`"),
    (44, "zxcvbnm,./"),
];
const NAMED: [(&str, u16); 33] = [
    ("escape", 1),
    ("esc", 1),
    ("minus", 12),
    ("equal", 13),
    ("plus", 13),
    ("backspace", 14),
    ("tab", 15),
    ("leftbracket", 26),
    ("rightbracket", 27),
    ("enter", 28),
    ("return", 28),
    ("semicolon", 39),
    ("apostrophe", 40),
    ("quote", 40),
    ("grave", 41),
    ("backslash", 43),
    ("comma", 51),
    ("period", 52),
    ("slash", 53),
    ("space", 57),
    ("capslock", 58),
    ("print", 99),
    ("printscreen", 99),
    ("sysrq", 99),
    ("home", 102),
    ("up", 103),
    ("pageup", 104),
    ("left", 105),
    ("right", 106),
    ("end", 107),
    ("down", 108),
    ("insert", 110),
    ("delete", 111),
];
/// Translate a configuration key name into an evdev key code.
pub fn key_code(name: &str) -> Option<u16> {
    let name = name.trim().to_ascii_lowercase();
    if let Some(code) = name.strip_prefix("code:") {
        return code.parse().ok().filter(|c| *c < 256);
    }
    if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()) {
        return match number {
            1..=10 => Some(58 + number),
            11 | 12 => Some(76 + number),
            _ => None,
        };
    }
    if let Some((base, row)) = ROWS
        .iter()
        .find(|(_, row)| name.chars().count() == 1 && row.contains(&name))
    {
        return Some(base + row.find(&name)? as u16);
    }
    NAMED
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, code)| *code)
        .or_else(|| match name.as_str() {
            "pagedown" | "pgdn" => Some(109),
            "pageup" | "pgup" => Some(104),
            _ => None,
        })
}
/// Human-readable name for a key code, for menu shortcut hints.
pub fn key_name(code: u16) -> String {
    for (base, row) in ROWS {
        if (base..base + row.len() as u16).contains(&code) {
            return row[(code - base) as usize..][..1].to_ascii_uppercase();
        }
    }
    match code {
        1 => "Esc".into(),
        14 => "Backspace".into(),
        15 => "Tab".into(),
        28 => "Enter".into(),
        57 => "Space".into(),
        59..=68 => format!("F{}", code - 58),
        87 | 88 => format!("F{}", code - 76),
        99 => "Print".into(),
        102 => "Home".into(),
        103 => "Up".into(),
        104 => "PageUp".into(),
        105 => "Left".into(),
        106 => "Right".into(),
        107 => "End".into(),
        108 => "Down".into(),
        109 => "PageDown".into(),
        110 => "Insert".into(),
        111 => "Delete".into(),
        code => format!("code:{code}"),
    }
}

/// Ordered action bindings; the first match wins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bindings {
    entries: Vec<(Action, Binding)>,
}
impl Default for Bindings {
    fn default() -> Self {
        Self::defaults()
    }
}
impl Bindings {
    pub fn defaults() -> Self {
        Self {
            entries: vec![
                (Action::Exit, Binding::new(1).ctrl().alt()),
                (Action::SwitchWindow(false), Binding::new(15).alt()),
                (Action::Dismiss, Binding::new(1)),
                (Action::Copy, Binding::new(46).ctrl()),
                (Action::Cut, Binding::new(45).ctrl()),
                (Action::Paste, Binding::new(47).ctrl()),
                (Action::SelectAll, Binding::new(30).ctrl()),
                (Action::Screenshot, Binding::new(99)),
                (Action::ScreenshotWindow, Binding::new(99).alt()),
            ],
        }
    }
    /// Rebind an action, or unbind it with `None`. Ordering is preserved.
    pub fn set(&mut self, action: Action, binding: Option<Binding>) {
        let existing = self
            .entries
            .iter()
            .position(|(a, _)| a.name() == action.name());
        match (existing, binding) {
            (Some(index), Some(binding)) => self.entries[index] = (action, binding),
            (Some(index), None) => {
                self.entries.remove(index);
            }
            (None, Some(binding)) => self.entries.push((action, binding)),
            (None, None) => (),
        }
    }
    pub fn get(&self, action: Action) -> Option<Binding> {
        self.entries
            .iter()
            .find(|(a, _)| a.name() == action.name())
            .map(|(_, b)| *b)
    }
    /// Menu shortcut hint for an action, empty when it is unbound.
    pub fn hint(&self, action: Action) -> String {
        self.get(action).map(|b| b.describe()).unwrap_or_default()
    }
    /// Resolve a key press. `raw_input` is true while a window that consumes
    /// raw keyboard input is focused: editing shortcuts then need Shift, so
    /// programs such as the terminal keep plain Ctrl+C.
    pub fn resolve(&self, code: u16, value: i32, m: Modifiers, raw_input: bool) -> Option<Action> {
        if value == 0 {
            return None;
        }
        for (action, binding) in &self.entries {
            let reversible = matches!(action, Action::SwitchWindow(_));
            if binding.code != code || binding.ctrl != m.ctrl || binding.alt != m.alt {
                continue;
            }
            // Shift reverses window cycling, and is always tolerated on
            // editing shortcuts so Ctrl+Shift+C reaches both kinds of window.
            let shift_matches = if reversible {
                true
            } else if action.editing() {
                m.shift || !binding.shift
            } else {
                binding.shift == m.shift
            };
            if !shift_matches {
                continue;
            }
            if raw_input && action.editing() && (!m.shift || *action == Action::Cut) {
                continue;
            }
            return Some(match action {
                Action::SwitchWindow(_) => Action::SwitchWindow(m.shift),
                action => *action,
            });
        }
        None
    }
}
#[derive(Default)]
pub struct WindowCycle {
    order: Vec<u32>,
    index: usize,
}
impl WindowCycle {
    pub fn reset(&mut self) {
        self.order.clear();
        self.index = 0;
    }
    pub fn next(&mut self, current: &[u32], reverse: bool) -> Option<u32> {
        if self.order.is_empty() {
            self.order = current.to_vec();
            self.index = 0;
        }
        self.order.retain(|id| current.contains(id));
        if self.order.is_empty() {
            return None;
        }
        self.index %= self.order.len();
        self.index =
            (self.index + if reverse { self.order.len() - 1 } else { 1 }) % self.order.len();
        Some(self.order[self.index])
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_control_keys_and_window_cycle() {
        let bindings = Bindings::defaults();
        let mut m = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(bindings.resolve(46, 1, m, true), None);
        m.shift = true;
        assert_eq!(bindings.resolve(46, 1, m, true), Some(Action::Copy));
        assert_eq!(bindings.resolve(45, 1, m, true), None, "no raw-input cut");
        assert_eq!(bindings.resolve(46, 0, m, true), None);
        assert_eq!(
            bindings.resolve(46, 1, m, false),
            Some(Action::Copy),
            "GUI windows accept the shifted form too"
        );
        let mut cycle = WindowCycle::default();
        assert_eq!(cycle.next(&[3, 2, 1], false), Some(2));
        assert_eq!(cycle.next(&[2, 3, 1], false), Some(1));
        assert_eq!(cycle.next(&[1, 2, 3], true), Some(2));
        cycle.reset();
        assert_eq!(cycle.next(&[2, 1, 3], false), Some(1));
    }
    #[test]
    fn default_bindings_cover_editing_switching_and_exit() {
        let b = Bindings::defaults();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(b.resolve(46, 1, ctrl, false), Some(Action::Copy));
        assert_eq!(b.resolve(30, 1, ctrl, false), Some(Action::SelectAll));
        assert_eq!(b.resolve(15, 1, alt, false), Some(Action::SwitchWindow(false)));
        assert_eq!(
            b.resolve(
                15,
                1,
                Modifiers {
                    alt: true,
                    shift: true,
                    ..Modifiers::default()
                },
                false
            ),
            Some(Action::SwitchWindow(true)),
            "Shift reverses the cycle without a separate binding"
        );
        assert_eq!(b.resolve(1, 1, Modifiers::default(), false), Some(Action::Dismiss));
        assert_eq!(
            b.resolve(
                1,
                1,
                Modifiers {
                    ctrl: true,
                    alt: true,
                    shift: false
                },
                false
            ),
            Some(Action::Exit)
        );
        assert_eq!(b.resolve(99, 1, Modifiers::default(), false), Some(Action::Screenshot));
        assert_eq!(b.resolve(99, 1, alt, false), Some(Action::ScreenshotWindow));
        // Alt with an unbound key stays a plain key press.
        assert_eq!(b.resolve(46, 1, alt, false), None);
    }
    #[test]
    fn bindings_parse_rebind_and_unbind() {
        assert_eq!(Binding::parse("ctrl+shift+c").unwrap(), Binding::new(46).ctrl().shift());
        assert_eq!(Binding::parse(" ALT + Tab ").unwrap(), Binding::new(15).alt());
        assert_eq!(Binding::parse("code:99").unwrap(), Binding::new(99));
        assert_eq!(Binding::parse("f12").unwrap(), Binding::new(88));
        assert!(Binding::parse("ctrl+nope").is_err());
        assert!(Binding::parse("a+b").is_err());
        assert!(Binding::parse("ctrl").is_err());
        assert_eq!(Binding::new(46).ctrl().shift().describe(), "Ctrl+Shift+C");
        assert_eq!(Binding::new(99).alt().describe(), "Alt+Print");
        let mut b = Bindings::defaults();
        b.set(Action::Copy, Some(Binding::parse("f5").unwrap()));
        b.set(Action::Screenshot, None);
        assert_eq!(b.resolve(63, 1, Modifiers::default(), false), Some(Action::Copy));
        assert_eq!(b.resolve(99, 1, Modifiers::default(), false), None);
        assert_eq!(b.hint(Action::Copy), "F5");
        assert_eq!(b.hint(Action::Screenshot), "");
        assert_eq!(Action::parse("select_all"), Some(Action::SelectAll));
        assert_eq!(Action::parse("nope"), None);
    }
}
