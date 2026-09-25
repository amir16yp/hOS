//! hOS init: the PID 1 supervisor, its service API and the system services.
//!
//! `hos-init` is a small supervisor. It keeps the desktop session and the
//! system services running and answers a control socket. Every service speaks
//! the same line protocol ([`ipc`]) over its own Unix socket in `/run/hos`, so
//! the desktop, `hosctl` and the services themselves share one client.
//!
//! | Service | Socket | Responsibility |
//! | --- | --- | --- |
//! | `hos-init` | `/run/hos/init.sock` | supervision, service control, shutdown |
//! | `hos-netd` | `/run/hos/netd.sock` | link state, DHCP, DNS, Wi-Fi |
//! | `hos-power` | `/run/hos/power.sock` | battery, power button, suspend |
//! | `hos-ntpd` | `/run/hos/ntpd.sock` | time synchronization, parked offline |
//! | `hos-soundd` | `/run/hos/soundd.sock` | default device, volume, mute, streams |
pub mod alsa;
pub mod dhcp;
pub mod ipc;
pub mod mixer;
pub mod net;
pub mod ntp;
pub mod pcm;
pub mod power;
pub mod sound;
pub mod supervisor;
pub mod sys;
pub mod unit;
pub mod wifi;

use std::path::{Path, PathBuf};

/// Runtime directory holding one socket per service.
pub const RUN: &str = "/run/hos";
/// Service configuration: `netd.conf`, `power.conf`, `ntpd.conf`, `soundd.conf`.
pub const CONFIG: &str = "/etc/hos";
/// State that should survive a restart, such as the saved volume.
pub const STATE: &str = "/var/lib/hos";

fn directory(variable: &str, default: &str) -> PathBuf {
    std::env::var_os(variable)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}
/// Runtime directory; `HOS_RUN_DIR` relocates it for tests and nested runs.
pub fn run_dir() -> PathBuf {
    directory("HOS_RUN_DIR", RUN)
}
/// Configuration directory; `HOS_CONFIG_DIR` relocates it.
pub fn config_dir() -> PathBuf {
    directory("HOS_CONFIG_DIR", CONFIG)
}
/// State directory; `HOS_STATE_DIR` relocates it.
pub fn state_dir() -> PathBuf {
    directory("HOS_STATE_DIR", STATE)
}
/// The socket a service listens on: `netd` becomes `/run/hos/netd.sock`.
pub fn socket_path(service: &str) -> PathBuf {
    run_dir().join(format!("{}.sock", service.trim_start_matches("hos-")))
}
/// `/etc/hos/NAME`, the configuration file for one service.
pub fn config_path(name: &str) -> PathBuf {
    config_dir().join(name)
}
/// `/var/lib/hos/NAME`, saved state for one service.
pub fn state_path(name: &str) -> PathBuf {
    state_dir().join(name)
}

/// One `key = value` line from a configuration file, with its `[section]`.
///
/// Section names are free-form so interfaces and networks can carry their own
/// settings: `[interface wlan0]` parses as the section `interface wlan0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub section: String,
    pub key: String,
    pub value: String,
}

/// A parsed configuration file. An unreadable file parses as empty: a service
/// starts with its defaults instead of refusing to run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub entries: Vec<Setting>,
    /// Lines that could not be understood; services report these once.
    pub warnings: Vec<String>,
}

impl Settings {
    pub fn parse(text: &str) -> Self {
        let mut settings = Settings::default();
        let mut section = String::new();
        for (number, line) in text.lines().enumerate() {
            let line = match line.split_once('#') {
                Some((before, _)) => before.trim(),
                None => line.trim(),
            };
            if line.is_empty() {
                continue;
            }
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_string();
                continue;
            }
            match line.split_once('=') {
                Some((key, value)) if !key.trim().is_empty() => settings.entries.push(Setting {
                    section: section.clone(),
                    key: key.trim().to_ascii_lowercase(),
                    value: value.trim().to_string(),
                }),
                _ => settings
                    .warnings
                    .push(format!("line {}: expected key = value", number + 1)),
            }
        }
        settings
    }
    /// Read a configuration file. A missing file is not an error.
    pub fn read(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Settings::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => Settings {
                entries: Vec::new(),
                warnings: vec![format!("{}: {e}", path.display())],
            },
        }
    }
    /// Read the configuration file of one service by file name.
    pub fn load(name: &str) -> Self {
        Settings::read(&config_path(name))
    }
    /// Read a service's configuration, writing the annotated defaults the
    /// first time, so `/etc/hos` documents itself the way `config.ini` does.
    pub fn install(name: &str, template: &str) -> Self {
        let path = config_path(name);
        if !path.exists() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&path, template) {
                log("hos-init", &format!("{}: {e}", path.display()));
            }
        }
        Settings::read(&path)
    }
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|e| e.section == section && e.key == key)
            .map(|e| e.value.as_str())
    }
    /// A value outside any section.
    pub fn top(&self, key: &str) -> Option<&str> {
        self.get("", key)
    }
    pub fn number<T: std::str::FromStr>(&self, section: &str, key: &str) -> Option<T> {
        self.get(section, key)?.parse().ok()
    }
    pub fn boolean(&self, section: &str, key: &str) -> Option<bool> {
        match self.get(section, key)?.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Some(true),
            "false" | "no" | "off" | "0" => Some(false),
            _ => None,
        }
    }
    /// Every section named `prefix NAME`, in file order, without duplicates.
    pub fn sections(&self, prefix: &str) -> Vec<String> {
        let mut names = Vec::new();
        for entry in &self.entries {
            if let Some(name) = entry.section.strip_prefix(&format!("{prefix} ")) {
                let name = name.trim().to_string();
                if !name.is_empty() && !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }
}

/// Set one `key = value` in a configuration file's text.
///
/// Comments, ordering and unrelated settings are kept, so a file a person
/// edited by hand still reads like their file after the settings application
/// changes one line in it. An empty `section` means the top of the file.
pub fn apply_setting(text: &str, section: &str, key: &str, value: &str) -> String {
    let key = key.trim().to_ascii_lowercase();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut current = String::new();
    let mut section_end = None;
    for index in 0..lines.len() {
        let line = match lines[index].split_once('#') {
            Some((before, _)) => before.trim(),
            None => lines[index].trim(),
        };
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if current == section {
                section_end = Some(index);
            }
            current = name.trim().to_string();
            continue;
        }
        if current != section || line.is_empty() {
            continue;
        }
        if line
            .split_once('=')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(&key))
        {
            lines[index] = format!("{key} = {value}");
            return finish(lines);
        }
    }
    match section_end.or(if current == section {
        Some(lines.len())
    } else {
        None
    }) {
        // The section exists: add the setting at its end.
        Some(index) => lines.insert(index, format!("{key} = {value}")),
        None => {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.push(format!("[{section}]"));
            lines.push(format!("{key} = {value}"));
        }
    }
    finish(lines)
}
fn finish(lines: Vec<String>) -> String {
    let mut text = lines.join("\n");
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// Change one setting in a service's configuration file.
pub fn store_setting(file: &str, section: &str, key: &str, value: &str) -> std::io::Result<()> {
    let path = config_path(file);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    sys::write_atomic(&path, &apply_setting(&text, section, key, value))
}

/// Service log line. Services run under the supervisor, which leaves their
/// output on the console, so every line names its sender.
pub fn log(service: &str, message: &str) {
    eprintln!("{service}: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_parse_sections_comments_and_bad_lines() {
        let settings = Settings::parse(concat!(
            "# comment\n",
            "hostname = hos\n",
            "[interface eth0]\n",
            "Method = dhcp   # trailing comment\n",
            "[interface wlan0]\n",
            "method = off\n",
            "nonsense\n",
        ));
        assert_eq!(settings.top("hostname"), Some("hos"));
        assert_eq!(settings.get("interface eth0", "method"), Some("dhcp"));
        assert_eq!(settings.get("interface wlan0", "method"), Some("off"));
        assert_eq!(settings.sections("interface"), ["eth0", "wlan0"]);
        assert_eq!(settings.warnings.len(), 1);
        assert!(settings.warnings[0].starts_with("line 7"));
    }
    #[test]
    fn settings_read_numbers_and_booleans() {
        let settings = Settings::parse("[power]\nlow = 15\nlid = yes\nbutton = ask\n");
        assert_eq!(settings.number::<u32>("power", "low"), Some(15));
        assert_eq!(settings.boolean("power", "lid"), Some(true));
        assert_eq!(settings.boolean("power", "button"), None);
        assert_eq!(settings.number::<u32>("power", "missing"), None);
    }
    #[test]
    fn one_setting_changes_without_disturbing_the_rest_of_the_file() {
        let original = "# power policy\n\
                        [power]\n\
                        button = shutdown   # what the button does\n\
                        low = 15\n\
                        \n\
                        [other]\n\
                        key = value\n";
        let changed = apply_setting(original, "power", "button", "ask");
        assert!(changed.starts_with("# power policy\n"), "{changed}");
        assert!(changed.contains("button = ask\n"));
        assert!(changed.contains("low = 15\n"));
        assert!(changed.contains("[other]\nkey = value\n"));
        assert_eq!(Settings::parse(&changed).get("power", "button"), Some("ask"));
        // A key the file does not have yet joins its section.
        let added = apply_setting(original, "power", "lid", "ignore");
        let settings = Settings::parse(&added);
        assert_eq!(settings.get("power", "lid"), Some("ignore"));
        assert_eq!(settings.get("power", "button"), Some("shutdown"));
        assert_eq!(settings.get("other", "key"), Some("value"));
        // A section the file does not have yet is appended.
        let new_section = apply_setting(original, "interface eth0", "method", "static");
        assert!(new_section.contains("[interface eth0]\nmethod = static\n"));
        // An empty file becomes a file with one section.
        assert_eq!(
            apply_setting("", "sound", "volume", "40"),
            "[sound]\nvolume = 40\n"
        );
        // Keys are matched without regard to case, and rewritten in lower case.
        assert!(apply_setting("[power]\nBUTTON = ask\n", "power", "button", "ignore")
            .contains("button = ignore"));
    }
    #[test]
    fn socket_paths_follow_the_service_name() {
        assert_eq!(socket_path("hos-netd").file_name().unwrap(), "netd.sock");
        assert_eq!(socket_path("power").file_name().unwrap(), "power.sock");
    }
}
