//! `hos-settings`: one place to change the system and the desktop.
//!
//! The left column chooses a page; the right column shows that page's
//! settings. System settings are read and written through the service API
//! ([`hoswm::init::ipc`]), so the running service applies the change
//! immediately and writes it to `/etc/hos/*.conf` for the next boot. Desktop
//! settings are the session's own `~/.hoswm/config.ini`, which is edited in
//! place, keeping its comments.
//!
//! Anything that needs root is shown but refused with an explanation when the
//! session is not running as root, rather than hidden.
use hoswm::{
    audio,
    client::{Client, Event, WINDOW_RAW_INPUT},
    config,
    font::Font,
    init::{apply_setting, ipc},
    surface::Surface,
};
use std::{
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

const BACKGROUND: u32 = 0xff0b0f0e;
const PANEL: u32 = 0xff111714;
const ACCENT: u32 = 0xff72dbac;
const TEXT: u32 = 0xffdfe4e1;
const DIM: u32 = 0xff8d9a93;
const LINE: u32 = 0xff26302b;
const WARN: u32 = 0xffe4c878;
const ERROR: u32 = 0xffef6976;

const WIDTH: u32 = 720;
const HEIGHT: u32 = 460;
const SIDEBAR: i32 = 150;
const TOP: i32 = 34;
const ROW: i32 = 30;
/// One character of the bundled font.
const GLYPH: i32 = 8;

/// The pages in the sidebar, in order.
const PAGES: [(&str, &str); 7] = [
    ("Network", "netd"),
    ("Wi-Fi", "netd"),
    ("Sound", "soundd"),
    ("Power", "power"),
    ("Date & time", "ntpd"),
    ("Desktop", ""),
    ("Services", "init"),
];

/// What a row lets the user do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Control {
    /// Read-only text.
    Info(String),
    Toggle(bool),
    /// One of several values; clicking moves to the next.
    Choice {
        options: Vec<String>,
        index: usize,
    },
    /// A percentage, set by clicking along the track.
    Slider(u32),
    Text {
        value: String,
        secret: bool,
    },
    Button(String),
    /// A list of choices, one per line.
    List {
        items: Vec<String>,
        selected: usize,
    },
}

/// One line of a page: a label, a control and the action it performs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    label: String,
    control: Control,
    /// The action name this row applies, such as `sound.volume`.
    id: String,
    /// Shown under the label in grey.
    hint: String,
    height: i32,
}
impl Row {
    fn new(label: &str, id: &str, control: Control) -> Row {
        let height = match &control {
            Control::List { items, .. } => (items.len().max(1) as i32 * 14) + 10,
            _ => ROW,
        };
        Row {
            label: label.to_string(),
            control,
            id: id.to_string(),
            hint: String::new(),
            height,
        }
    }
    fn hint(mut self, hint: &str) -> Row {
        self.hint = hint.to_string();
        self.height = self.height.max(ROW + 12);
        self
    }
    fn info(label: &str, value: &str) -> Row {
        Row::new(label, "", Control::Info(value.to_string()))
    }
}

/// Ask one service for something. Missing services are reported, not fatal.
fn call(service: &str, request: &str) -> Result<ipc::Reply, String> {
    let mut client = ipc::Client::connect(service)
        .map_err(|e| format!("hos-{service}: {e}; is the service running?"))?;
    client.call(request).map_err(|e| format!("{e}"))
}
/// A request whose reply is only interesting when it fails.
fn command(service: &str, request: &str) -> Result<String, String> {
    call(service, request).map(|reply| reply.message)
}

/// The desktop configuration file this application edits.
fn desktop_config() -> PathBuf {
    config::directory().join(config::FILE)
}
fn desktop_text() -> String {
    let path = desktop_config();
    match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // The session writes the template on first start; before that, show
        // the same defaults it would write.
        Err(_) => config::TEMPLATE.to_string(),
    }
}
fn desktop_value(text: &str, section: &str, key: &str, fallback: &str) -> String {
    hoswm::init::Settings::parse(text)
        .get(section, key)
        .unwrap_or(fallback)
        .to_string()
}
fn set_desktop(section: &str, key: &str, value: &str) -> Result<String, String> {
    let path = desktop_config();
    let text = desktop_text();
    let updated = apply_setting(&text, section, key, value);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, updated).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(format!("{key} = {value}; restart the session to apply it"))
}

/// Everything one page needs, read from the services.
#[derive(Default)]
struct Data {
    records: Vec<ipc::Record>,
    /// The Wi-Fi networks of the most recent scan.
    networks: Vec<ipc::Record>,
    error: String,
}

struct App {
    page: usize,
    rows: Vec<Row>,
    data: Data,
    status: String,
    status_color: u32,
    /// The row being typed into.
    focus: Option<usize>,
    selected_network: usize,
    selected_unit: usize,
    password: String,
    refreshed: Instant,
    root: bool,
    scroll: i32,
    dirty: bool,
}
impl App {
    fn new() -> App {
        let mut app = App {
            page: 0,
            rows: Vec::new(),
            data: Data::default(),
            status: String::new(),
            status_color: DIM,
            focus: None,
            selected_network: 0,
            selected_unit: 0,
            password: String::new(),
            refreshed: Instant::now(),
            // SAFETY: geteuid only reads this process's effective user.
            root: unsafe { hoswm::init::sys::geteuid() } == 0,
            scroll: 0,
            dirty: true,
        };
        app.refresh();
        app
    }
    fn say(&mut self, message: impl Into<String>, color: u32) {
        self.status = message.into();
        self.status_color = color;
    }
    /// Read the current page's data and rebuild its rows.
    fn refresh(&mut self) {
        self.refreshed = Instant::now();
        self.data = Data::default();
        let request = match self.page {
            0 => Some(("netd", "STATUS")),
            1 => Some(("netd", "WIFI STATUS")),
            2 => Some(("soundd", "STATUS")),
            3 => Some(("power", "STATUS")),
            4 => Some(("ntpd", "STATUS")),
            6 => Some(("init", "LIST")),
            _ => None,
        };
        if let Some((service, verb)) = request {
            match call(service, verb) {
                Ok(reply) => self.data.records = reply.records,
                Err(e) => self.data.error = e,
            }
        }
        if self.page == 1 && self.data.error.is_empty() {
            match call("netd", "WIFI LIST") {
                Ok(reply) => self.data.networks = reply.records,
                Err(e) => self.data.error = e,
            }
        }
        self.build();
        self.dirty = true;
    }
    fn build(&mut self) {
        let text = desktop_text();
        let records = std::mem::take(&mut self.data.records);
        let networks = std::mem::take(&mut self.data.networks);
        let mut rows = Vec::new();
        match self.page {
            0 => {
                let summary = records.first().cloned().unwrap_or_default();
                rows.push(Row::info(
                    "Status",
                    if summary.flag("online") {
                        "Online"
                    } else {
                        "Offline"
                    },
                ));
                let gateway = summary.get("gateway").unwrap_or("");
                if !gateway.is_empty() {
                    rows.push(Row::info("Gateway", gateway));
                }
                rows.push(
                    Row::new(
                        "Name servers",
                        "net.dns",
                        Control::Text {
                            value: summary.get("dns").unwrap_or("").replace(',', " "),
                            secret: false,
                        },
                    )
                    .hint("Written to /etc/resolv.conf; DHCP replaces it on renewal"),
                );
                for link in records.iter().skip(1) {
                    let name = link.get("iface").unwrap_or("?").to_string();
                    let address = link.get("address").unwrap_or("");
                    let state = match (link.flag("carrier"), address.is_empty()) {
                        (false, _) => "no carrier".to_string(),
                        (true, true) => format!("up, {}", link.get("dhcp").unwrap_or("no address")),
                        (true, false) => address.to_string(),
                    };
                    rows.push(Row::info(&format!("{name} ({})", link.get("kind").unwrap_or("")), &state));
                    let methods = ["dhcp", "static", "off"];
                    let current = link.get("method").unwrap_or("dhcp");
                    rows.push(
                        Row::new(
                            &format!("{name} configuration"),
                            &format!("net.method:{name}"),
                            Control::Choice {
                                options: methods.iter().map(|m| m.to_string()).collect(),
                                index: methods.iter().position(|m| *m == current).unwrap_or(0),
                            },
                        )
                        .hint("Static addresses are set in /etc/hos/netd.conf"),
                    );
                    rows.push(Row::new(
                        &format!("{name} enabled"),
                        &format!("net.up:{name}"),
                        Control::Toggle(link.flag("up")),
                    ));
                }
                rows.push(Row::new(
                    "Renew leases",
                    "net.renew",
                    Control::Button("Renew".into()),
                ));
            }
            1 => {
                let status = records.first().cloned().unwrap_or_default();
                rows.push(Row::info(
                    "Connection",
                    &match status.get("ssid").unwrap_or("") {
                        "" => format!("{} (not connected)", status.get("state").unwrap_or("-")),
                        ssid => format!("{ssid} ({})", status.get("state").unwrap_or("")),
                    },
                ));
                let items: Vec<String> = networks
                    .iter()
                    .map(|network| {
                        format!(
                            "{:<24} {:>4} dBm  {}{}",
                            truncate(network.get("ssid").unwrap_or(""), 24),
                            network.get("signal").unwrap_or("?"),
                            network.get("security").unwrap_or(""),
                            if network.flag("saved") { "  saved" } else { "" }
                        )
                    })
                    .collect();
                self.selected_network = self.selected_network.min(items.len().saturating_sub(1));
                rows.push(Row::new(
                    "Networks",
                    "wifi.list",
                    Control::List {
                        items: if items.is_empty() {
                            vec!["No networks found yet. Choose Scan.".into()]
                        } else {
                            items
                        },
                        selected: self.selected_network,
                    },
                ));
                rows.push(
                    Row::new(
                        "Password",
                        "wifi.password",
                        Control::Text {
                            value: self.password.clone(),
                            secret: true,
                        },
                    )
                    .hint("Leave empty for an open network"),
                );
                rows.push(Row::new(
                    "Join the selected network",
                    "wifi.connect",
                    Control::Button("Connect".into()),
                ));
                rows.push(Row::new("Scan again", "wifi.scan", Control::Button("Scan".into())));
                rows.push(Row::new(
                    "Forget the selected network",
                    "wifi.forget",
                    Control::Button("Forget".into()),
                ));
                rows.push(Row::new(
                    "Disconnect",
                    "wifi.disconnect",
                    Control::Button("Disconnect".into()),
                ));
            }
            2 => {
                let summary = records.first().cloned().unwrap_or_default();
                rows.push(Row::new(
                    "Volume",
                    "sound.volume",
                    Control::Slider(summary.number("volume").unwrap_or(0)),
                ));
                rows.push(Row::new(
                    "Muted",
                    "sound.mute",
                    Control::Toggle(summary.flag("muted")),
                ));
                let cards: Vec<String> = records
                    .iter()
                    .skip(1)
                    .map(|card| {
                        format!(
                            "{} {}",
                            card.get("card").unwrap_or("?"),
                            card.get("name").unwrap_or("")
                        )
                    })
                    .collect();
                let current = summary.get("card").unwrap_or("0").to_string();
                if !cards.is_empty() {
                    rows.push(
                        Row::new(
                            "Output device",
                            "sound.card",
                            Control::Choice {
                                index: cards
                                    .iter()
                                    .position(|card| card.starts_with(&format!("{current} ")))
                                    .unwrap_or(0),
                                options: cards,
                            },
                        )
                        .hint("Applications are mixed onto this card"),
                    );
                }
                rows.push(
                    Row::new("Test sound", "sound.test", Control::Button("Play".into()))
                        .hint("Plays a short tone through the mixer"),
                );
                rows.push(Row::info(
                    "Playing now",
                    &format!(
                        "{} application stream(s), {} on the card",
                        summary.get("mixed").unwrap_or("0"),
                        summary.get("streams").unwrap_or("0")
                    ),
                ));
                if let Some(error) = summary.get("error").filter(|e| !e.is_empty()) {
                    rows.push(Row::info("Sound error", error));
                }
            }
            3 => {
                let summary = records.first().cloned().unwrap_or_default();
                let policy = records.get(1).cloned().unwrap_or_default();
                rows.push(Row::info(
                    "Power source",
                    match summary.get("supply").unwrap_or("unknown") {
                        "ac" => "Mains",
                        "battery" => "Battery",
                        _ => "Unknown",
                    },
                ));
                for battery in records.iter().skip(2) {
                    rows.push(Row::info(
                        battery.get("battery").unwrap_or("Battery"),
                        &format!(
                            "{}%, {}{}",
                            battery.get("capacity").unwrap_or("?"),
                            battery.get("status").unwrap_or(""),
                            match battery.get("minutes") {
                                Some(minutes) => format!(", {minutes} minutes left"),
                                None => String::new(),
                            }
                        ),
                    ));
                }
                let actions = ["shutdown", "suspend", "ask", "ignore"];
                rows.push(
                    Row::new(
                        "Power button",
                        "power.button",
                        Control::Choice {
                            options: actions.iter().map(|a| a.to_string()).collect(),
                            index: actions
                                .iter()
                                .position(|a| Some(*a) == policy.get("button"))
                                .unwrap_or(0),
                        },
                    )
                    .hint("\"ask\" lets the desktop decide"),
                );
                let lid = ["suspend", "shutdown", "ignore"];
                rows.push(Row::new(
                    "Closing the lid",
                    "power.lid",
                    Control::Choice {
                        options: lid.iter().map(|a| a.to_string()).collect(),
                        index: lid
                            .iter()
                            .position(|a| Some(*a) == policy.get("lid"))
                            .unwrap_or(0),
                    },
                ));
                rows.push(Row::new(
                    "Warn at battery percent",
                    "power.low",
                    Control::Text {
                        value: policy.get("low").unwrap_or("15").to_string(),
                        secret: false,
                    },
                ));
                rows.push(Row::new(
                    "Act at battery percent",
                    "power.critical",
                    Control::Text {
                        value: policy.get("critical").unwrap_or("5").to_string(),
                        secret: false,
                    },
                ));
                rows.push(
                    Row::new(
                        "Users may suspend or shut down",
                        "power.allow-users",
                        Control::Toggle(policy.flag("allow_users")),
                    )
                    .hint("Applies to sessions that are not running as root"),
                );
                rows.push(Row::new("Suspend now", "power.suspend", Control::Button("Suspend".into())));
                rows.push(Row::new("Restart", "power.reboot", Control::Button("Reboot".into())));
                rows.push(Row::new("Shut down", "power.shutdown", Control::Button("Shut down".into())));
            }
            4 => {
                let status = records.first().cloned().unwrap_or_default();
                rows.push(Row::info(
                    "Clock",
                    &format!(
                        "{}{}",
                        status.get("state").unwrap_or("unknown"),
                        match status.get("offset") {
                            Some(offset) => format!(", last offset {offset}s"),
                            None => String::new(),
                        }
                    ),
                ));
                rows.push(Row::info("Now", &clock()));
                rows.push(
                    Row::new(
                        "Time servers",
                        "time.servers",
                        Control::Text {
                            value: servers(),
                            secret: false,
                        },
                    )
                    .hint("Queried only while the system is online"),
                );
                rows.push(Row::new(
                    "Synchronize now",
                    "time.sync",
                    Control::Button("Sync".into()),
                ));
            }
            5 => {
                rows.push(
                    Row::new(
                        "Accent color",
                        "desktop.accent",
                        Control::Text {
                            value: desktop_value(&text, "session", "accent", "0xff72dbac"),
                            secret: false,
                        },
                    )
                    .hint("0xAARRGGBB or #RRGGBB"),
                );
                rows.push(Row::new(
                    "Menu bar",
                    "desktop.menubar",
                    Control::Toggle(desktop_value(&text, "menubar", "enabled", "true") != "false"),
                ));
                rows.push(Row::new(
                    "Menu bar clock",
                    "desktop.clock",
                    Control::Toggle(desktop_value(&text, "menubar", "clock", "true") != "false"),
                ));
                let corners = ["top-left", "top-right", "bottom-left", "bottom-right"];
                let corner = desktop_value(&text, "toasts", "corner", "top-right");
                rows.push(Row::new(
                    "Notification corner",
                    "desktop.corner",
                    Control::Choice {
                        options: corners.iter().map(|c| c.to_string()).collect(),
                        index: corners.iter().position(|c| *c == corner).unwrap_or(1),
                    },
                ));
                rows.push(Row::new(
                    "Notification time (ms)",
                    "desktop.duration",
                    Control::Text {
                        value: desktop_value(&text, "toasts", "duration_ms", "4000"),
                        secret: false,
                    },
                ));
                rows.push(Row::new(
                    "Screenshot directory",
                    "desktop.screenshots",
                    Control::Text {
                        value: desktop_value(&text, "screenshots", "directory", "screenshots"),
                        secret: false,
                    },
                ));
                rows.push(
                    Row::new(
                        "Restart the session",
                        "desktop.restart",
                        Control::Button("Restart".into()),
                    )
                    .hint("Desktop settings are read when the session starts"),
                );
                rows.push(Row::info("Settings file", &desktop_config().display().to_string()));
            }
            _ => {
                let items: Vec<String> = records
                    .iter()
                    .map(|unit| {
                        format!(
                            "{:<12} {:<9} {}",
                            unit.get("unit").unwrap_or("?"),
                            unit.get("state").unwrap_or("?"),
                            unit.get("description").unwrap_or("")
                        )
                    })
                    .collect();
                self.selected_unit = self.selected_unit.min(items.len().saturating_sub(1));
                rows.push(Row::new(
                    "Services",
                    "services.list",
                    Control::List {
                        items: if items.is_empty() {
                            vec!["hos-init is not answering".into()]
                        } else {
                            items
                        },
                        selected: self.selected_unit,
                    },
                ));
                rows.push(Row::new("Restart the selected service", "services.restart", Control::Button("Restart".into())));
                rows.push(Row::new("Stop the selected service", "services.stop", Control::Button("Stop".into())));
                rows.push(Row::new("Start the selected service", "services.start", Control::Button("Start".into())));
            }
        }
        if !self.data.error.is_empty() {
            let error = self.data.error.clone();
            rows.insert(0, Row::info("Service", &error));
        }
        self.rows = rows;
        self.data.records = records;
        self.data.networks = networks;
    }
    /// The unit or network name the list rows have selected.
    fn selected(&self, key: &str) -> Option<String> {
        let records = if self.page == 1 {
            &self.data.networks
        } else {
            &self.data.records
        };
        let index = if self.page == 1 {
            self.selected_network
        } else {
            self.selected_unit
        };
        records.get(index)?.get(key).map(str::to_string)
    }
    /// Carry out the action of one row.
    fn apply(&mut self, id: &str, value: String) {
        let result = self.run(id, &value);
        match result {
            Ok(message) if message.is_empty() => (),
            Ok(message) => self.say(message, ACCENT),
            Err(e) => self.say(e, ERROR),
        }
        self.refresh();
    }
    fn run(&mut self, id: &str, value: &str) -> Result<String, String> {
        let (action, argument) = match id.split_once(':') {
            Some((action, argument)) => (action, argument),
            None => (id, ""),
        };
        match action {
            "net.dns" => command("netd", &format!("DNS SET {value}")),
            "net.method" => {
                command(
                    "netd",
                    &format!("SET interface\\s{argument} method {value}"),
                )?;
                command("netd", &format!("RENEW {argument}"))
            }
            "net.up" => command(
                "netd",
                &format!("{} {argument}", if value == "true" { "UP" } else { "DOWN" }),
            ),
            "net.renew" => command("netd", "RENEW"),
            "wifi.scan" => command("netd", "WIFI SCAN"),
            "wifi.connect" => {
                let ssid = self
                    .selected("ssid")
                    .ok_or("Select a network in the list first")?;
                let password = self.password.clone();
                let request = if password.is_empty() {
                    format!("WIFI CONNECT {}", ipc::escape(&ssid))
                } else {
                    format!(
                        "WIFI CONNECT {} {}",
                        ipc::escape(&ssid),
                        ipc::escape(&password)
                    )
                };
                let result = command("netd", &request);
                self.password.clear();
                result
            }
            "wifi.forget" => {
                let ssid = self.selected("ssid").ok_or("Select a network first")?;
                command("netd", &format!("WIFI FORGET {}", ipc::escape(&ssid)))
            }
            "wifi.disconnect" => command("netd", "WIFI DISCONNECT"),
            "wifi.password" => {
                self.password = value.to_string();
                Ok(String::new())
            }
            "sound.volume" => command("soundd", &format!("VOLUME {value}")),
            "sound.mute" => command(
                "soundd",
                if value == "true" { "MUTE ON" } else { "MUTE OFF" },
            ),
            "sound.card" => {
                let card = value.split_whitespace().next().unwrap_or("0").to_string();
                command("soundd", &format!("DEFAULT {card}"))?;
                command("soundd", &format!("SET sound card {card}"))
            }
            "sound.test" => {
                test_tone();
                Ok("Playing a test tone".into())
            }
            "power.button" | "power.lid" | "power.low" | "power.critical"
            | "power.allow-users" => {
                let key = action.trim_start_matches("power.");
                let value = if key == "allow-users" {
                    if value == "true" { "yes" } else { "no" }
                } else {
                    value
                };
                command("power", &format!("SET power {key} {value}"))
            }
            "power.suspend" => command("power", "SUSPEND"),
            "power.reboot" => command("power", "REBOOT"),
            "power.shutdown" => command("power", "SHUTDOWN"),
            "time.servers" => command("ntpd", &format!("SET ntp servers {value}")),
            "time.sync" => command("ntpd", "SYNC"),
            "desktop.accent" => set_desktop("session", "accent", value),
            "desktop.menubar" => set_desktop("menubar", "enabled", value),
            "desktop.clock" => set_desktop("menubar", "clock", value),
            "desktop.corner" => set_desktop("toasts", "corner", value),
            "desktop.duration" => set_desktop("toasts", "duration_ms", value),
            "desktop.screenshots" => set_desktop("screenshots", "directory", value),
            "desktop.restart" => command("init", "SESSION RESTART"),
            "services.restart" | "services.stop" | "services.start" => {
                let unit = self.selected("unit").ok_or("Select a service first")?;
                let verb = action.trim_start_matches("services.").to_ascii_uppercase();
                command("init", &format!("{verb} {unit}"))
            }
            "wifi.list" | "services.list" | "" => Ok(String::new()),
            other => Err(format!("{other} is not a setting")),
        }
    }

    // Layout and input.

    /// The top of row `index` in content coordinates.
    fn row_top(&self, index: usize) -> i32 {
        TOP + 8 - self.scroll
            + self.rows[..index]
                .iter()
                .map(|row| row.height)
                .sum::<i32>()
    }
    fn content_height(&self) -> i32 {
        self.rows.iter().map(|row| row.height).sum::<i32>() + 16
    }
    fn click(&mut self, x: i32, y: i32, width: i32) {
        self.dirty = true;
        if x < SIDEBAR {
            let index = ((y - TOP) / ROW).clamp(0, PAGES.len() as i32 - 1) as usize;
            if y >= TOP && index != self.page {
                self.page = index;
                self.scroll = 0;
                self.focus = None;
                self.status.clear();
                self.refresh();
            }
            return;
        }
        for index in 0..self.rows.len() {
            let top = self.row_top(index);
            let height = self.rows[index].height;
            if y < top || y >= top + height {
                continue;
            }
            let control = self.rows[index].control.clone();
            let id = self.rows[index].id.clone();
            self.focus = None;
            match control {
                Control::Info(_) => (),
                Control::Toggle(on) => self.apply(&id, (!on).to_string()),
                Control::Choice { options, index: at } => {
                    let next = options
                        .get((at + 1) % options.len().max(1))
                        .cloned()
                        .unwrap_or_default();
                    self.apply(&id, next);
                }
                Control::Slider(_) => {
                    let track = control_x(width);
                    let percent = ((x - track) * 100 / slider_width(width)).clamp(0, 100);
                    self.apply(&id, percent.to_string());
                }
                Control::Text { .. } => {
                    self.focus = Some(index);
                    self.say("Type, then press Enter to apply", DIM);
                }
                Control::Button(_) => {
                    if id == "power.shutdown" || id == "power.reboot" {
                        self.say("Hold on...", WARN);
                    }
                    self.apply(&id, String::new());
                }
                Control::List { items, .. } => {
                    let line = ((y - top - 4) / 14).clamp(0, items.len() as i32 - 1) as usize;
                    if self.page == 1 {
                        self.selected_network = line;
                    } else {
                        self.selected_unit = line;
                    }
                    self.build();
                }
            }
            return;
        }
    }
    fn key(&mut self, bytes: &[u8]) {
        self.dirty = true;
        match bytes {
            b"\x1b" => (),
            b"\t" => {
                self.page = (self.page + 1) % PAGES.len();
                self.scroll = 0;
                self.refresh();
            }
            b"\x1b[B" | b"\x1b[A" => {
                let down = bytes == b"\x1b[B";
                let selected = if self.page == 1 {
                    &mut self.selected_network
                } else {
                    &mut self.selected_unit
                };
                *selected = if down {
                    selected.saturating_add(1)
                } else {
                    selected.saturating_sub(1)
                };
                self.build();
            }
            b"\r" => {
                if let Some(index) = self.focus.take() {
                    if let Control::Text { value, .. } = self.rows[index].control.clone() {
                        let id = self.rows[index].id.clone();
                        self.apply(&id, value);
                    }
                }
            }
            _ => {
                let Some(index) = self.focus else { return };
                let Control::Text { value, secret } = &mut self.rows[index].control else {
                    return;
                };
                if bytes == [127] || bytes == [8] {
                    value.pop();
                } else if value.len() + bytes.len() <= 128
                    && bytes.iter().all(|byte| (32..127).contains(byte))
                {
                    value.push_str(&String::from_utf8_lossy(bytes));
                }
                // The password field lives outside the row list, so the value
                // survives a refresh.
                if *secret {
                    self.password = value.clone();
                }
            }
        }
    }
    fn wheel(&mut self, notches: i32, height: i32) {
        let room = (self.content_height() - (height - TOP - 24)).max(0);
        self.scroll = (self.scroll - notches * 3 * 14).clamp(0, room);
        self.dirty = true;
    }
    fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        let width = surface.width() as i32;
        let height = surface.height() as i32;
        surface.pixels_mut().fill(BACKGROUND);
        surface.fill_rect(0, 0, SIDEBAR, height, PANEL);
        font.draw(surface, 12, 12, "Settings", ACCENT);
        for (index, (name, _)) in PAGES.iter().enumerate() {
            let y = TOP + index as i32 * ROW;
            if index == self.page {
                surface.fill_rect(0, y, SIDEBAR, ROW, 0xff1b2620);
                surface.fill_rect(0, y, 3, ROW, ACCENT);
            }
            font.draw(
                surface,
                14,
                y + 10,
                name,
                if index == self.page { TEXT } else { DIM },
            );
        }
        if !self.root {
            font.draw(surface, 12, height - 34, "not root:", WARN);
            font.draw(surface, 12, height - 24, "some settings", DIM);
            font.draw(surface, 12, height - 14, "are refused", DIM);
        }
        font.draw(surface, SIDEBAR + 16, 12, PAGES[self.page].0, ACCENT);
        surface.fill_rect(SIDEBAR + 16, TOP - 6, width - SIDEBAR - 32, 1, LINE);
        for (index, row) in self.rows.iter().enumerate() {
            let top = self.row_top(index);
            if top + row.height < TOP || top > height - 24 {
                continue;
            }
            self.draw_row(surface, font, row, top, width);
        }
        surface.fill_rect(0, height - 22, width, 1, LINE);
        let status = if self.status.is_empty() {
            "Click a setting to change it. Tab moves between pages."
        } else {
            &self.status
        };
        font.draw(
            surface,
            12,
            height - 15,
            &truncate(status, ((width - 24) / GLYPH).max(8) as usize),
            if self.status.is_empty() {
                DIM
            } else {
                self.status_color
            },
        );
    }
    fn draw_row(&self, surface: &mut Surface, font: &Font<'_>, row: &Row, top: i32, width: i32) {
        let label_room = ((control_x(width) - SIDEBAR - 24) / GLYPH).max(8) as usize;
        font.draw(
            surface,
            SIDEBAR + 16,
            top + 8,
            &truncate(&row.label, label_room),
            TEXT,
        );
        if !row.hint.is_empty() {
            font.draw(
                surface,
                SIDEBAR + 16,
                top + 20,
                &truncate(&row.hint, ((width - SIDEBAR - 32) / GLYPH).max(8) as usize),
                DIM,
            );
        }
        let x = control_x(width);
        match &row.control {
            Control::Info(value) => {
                let room = ((width - x - 16) / GLYPH).max(4) as usize;
                font.draw(surface, x, top + 8, &truncate(value, room), DIM);
            }
            Control::Toggle(on) => {
                let (label, color) = if *on { ("ON", ACCENT) } else { ("OFF", DIM) };
                surface.fill_rect(x, top + 4, 48, 18, LINE);
                surface.fill_rect(if *on { x + 26 } else { x + 2 }, top + 6, 20, 14, color);
                font.draw(surface, x + 56, top + 8, label, color);
            }
            Control::Choice { options, index } => {
                let value = options.get(*index).cloned().unwrap_or_default();
                surface.fill_rect(x, top + 4, 160, 18, LINE);
                font.draw(surface, x + 6, top + 8, &truncate(&value, 17), TEXT);
                font.draw(surface, x + 148, top + 8, ">", ACCENT);
            }
            Control::Slider(percent) => {
                let track = slider_width(width);
                surface.fill_rect(x, top + 11, track, 4, LINE);
                surface.fill_rect(x, top + 11, track * *percent as i32 / 100, 4, ACCENT);
                let knob = x + track * *percent as i32 / 100;
                surface.fill_rect(knob - 3, top + 5, 6, 16, ACCENT);
                font.draw(surface, x + track + 10, top + 8, &format!("{percent}%"), TEXT);
            }
            Control::Text { value, secret } => {
                let focused = self
                    .focus
                    .and_then(|index| self.rows.get(index))
                    .is_some_and(|focused| focused.id == row.id);
                let field = (width - x - 16).max(64);
                surface.fill_rect(x, top + 4, field, 18, if focused { ACCENT } else { LINE });
                surface.fill_rect(x + 1, top + 5, field - 2, 16, BACKGROUND);
                let shown = if *secret {
                    "*".repeat(value.chars().count())
                } else {
                    value.clone()
                };
                let room = ((field - 12) / GLYPH).max(4) as usize;
                let tail: String = shown
                    .chars()
                    .skip(shown.chars().count().saturating_sub(room))
                    .collect();
                font.draw(surface, x + 6, top + 8, &tail, TEXT);
                if focused {
                    surface.fill_rect(
                        x + 6 + tail.chars().count() as i32 * GLYPH,
                        top + 7,
                        2,
                        11,
                        ACCENT,
                    );
                }
            }
            Control::Button(label) => {
                let button = (label.chars().count() as i32 * GLYPH + 24).max(72);
                surface.fill_rect(x, top + 3, button, 20, LINE);
                surface.fill_rect(x, top + 3, 2, 20, ACCENT);
                font.draw(surface, x + 12, top + 8, label, TEXT);
            }
            Control::List { items, selected } => {
                let left = SIDEBAR + 16;
                let room = ((width - left - 24) / GLYPH).max(8) as usize;
                for (line, item) in items.iter().enumerate() {
                    let y = top + 4 + line as i32 * 14;
                    if line == *selected {
                        surface.fill_rect(left - 4, y - 2, width - left - 12, 14, 0xff1b2620);
                    }
                    font.draw(
                        surface,
                        left,
                        y,
                        &truncate(item, room),
                        if line == *selected { TEXT } else { DIM },
                    );
                }
            }
        }
    }
}

/// Where controls start, leaving room for labels.
fn control_x(width: i32) -> i32 {
    (width * 6 / 10).max(SIDEBAR + 180)
}
fn slider_width(width: i32) -> i32 {
    (width - control_x(width) - 70).clamp(60, 200)
}
/// Shorten text to `room` characters, marking what was cut.
fn truncate(text: &str, room: usize) -> String {
    if text.chars().count() <= room {
        return text.to_string();
    }
    let kept: String = text.chars().take(room.saturating_sub(1)).collect();
    format!("{kept}~")
}
/// The current UTC time, which is all the clock in the menu bar shows too.
fn clock() -> String {
    let (seconds, _) = hoswm::init::sys::realtime();
    let days = seconds.div_euclid(86400);
    let time = seconds.rem_euclid(86400);
    // Days since the epoch to a civil date, by the usual integer method.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}
/// The configured time servers, read from the service.
fn servers() -> String {
    call("ntpd", "SERVERS")
        .map(|reply| {
            reply
                .records
                .iter()
                .filter_map(|record| record.get("server"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}
/// Play a short tone, so the sound page can prove the card works.
fn test_tone() {
    thread::spawn(|| {
        let Ok(mut playback) = audio::Playback::open(48000, 2, "hos-settings") else {
            return;
        };
        let mut samples = vec![0i16; 48000 * 2 / 2]; // half a second, stereo
        for (index, frame) in samples.chunks_mut(2).enumerate() {
            // A 440 Hz sine with a short fade, so it does not click.
            let time = index as f32 / 48000.0;
            let fade = (1.0 - time * 2.0).clamp(0.0, 1.0);
            let value = ((time * 440.0 * std::f32::consts::TAU).sin() * 6000.0 * fade) as i16;
            frame[0] = value;
            frame[1] = value;
        }
        let _ = playback.write(&samples);
        // Let the mixer finish before the stream closes.
        thread::sleep(Duration::from_millis(700));
    });
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::connect()?;
    let window = client.create("Settings", WIDTH, HEIGHT, ACCENT)?;
    client.flags(window, WINDOW_RAW_INPUT)?;
    let font = Font::builtin();
    let mut surface = Surface::new(WIDTH as usize, HEIGHT as usize);
    let mut app = App::new();
    loop {
        let (width, height, minimized) = client.size(window)?;
        if width as usize != surface.width() || height as usize != surface.height() {
            surface.reset(width as usize, height as usize, BACKGROUND);
            app.dirty = true;
        }
        for _ in 0..32 {
            let Some(Event {
                kind,
                control,
                text,
            }) = client.poll(window)?
            else {
                break;
            };
            match kind {
                6 => app.key(text.as_bytes()),
                8 => {
                    let mut parts = text.split_whitespace();
                    let x = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let action = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    if action == 1 {
                        app.click(x, y, width as i32);
                    }
                }
                10 => app.wheel(control as i32, height as i32),
                7 | 9 => {
                    let _ = client.close(window);
                    return Ok(());
                }
                _ => (),
            }
        }
        // Service state changes on its own; keep the page current.
        if app.focus.is_none() && app.refreshed.elapsed() > Duration::from_secs(3) {
            app.refresh();
        }
        if app.dirty && !minimized {
            app.draw(&mut surface, &font);
            client.present(window, width, height, surface.pixels())?;
            app.dirty = false;
        }
        thread::sleep(Duration::from_millis(if minimized { 120 } else { 30 }));
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("hos-settings: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App {
            page: 0,
            rows: Vec::new(),
            data: Data::default(),
            status: String::new(),
            status_color: DIM,
            focus: None,
            selected_network: 0,
            selected_unit: 0,
            password: String::new(),
            refreshed: Instant::now(),
            root: true,
            scroll: 0,
            dirty: true,
        }
    }

    #[test]
    fn the_network_page_turns_service_records_into_rows() {
        let mut app = app();
        app.data.records = vec![
            ipc::Record::parse("online=yes gateway=10.0.2.2 dns=10.0.2.3,1.1.1.1"),
            ipc::Record::parse("iface=eth0 kind=wired method=dhcp carrier=yes up=yes address=10.0.2.15/24"),
        ];
        app.build();
        assert_eq!(app.rows[0].control, Control::Info("Online".into()));
        assert!(app.rows.iter().any(|row| row.id == "net.method:eth0"));
        let dns = app.rows.iter().find(|row| row.id == "net.dns").unwrap();
        assert_eq!(
            dns.control,
            Control::Text {
                value: "10.0.2.3 1.1.1.1".into(),
                secret: false
            }
        );
        let method = app
            .rows
            .iter()
            .find(|row| row.id == "net.method:eth0")
            .unwrap();
        assert_eq!(
            method.control,
            Control::Choice {
                options: vec!["dhcp".into(), "static".into(), "off".into()],
                index: 0
            }
        );
    }
    #[test]
    fn the_sound_page_shows_the_volume_and_the_cards() {
        let mut app = app();
        app.page = 2;
        app.data.records = vec![
            ipc::Record::parse("card=1 volume=42 muted=yes mixed=2 streams=1"),
            ipc::Record::parse("card=0 id=PCH name=Built-in default=no"),
            ipc::Record::parse("card=1 id=USB name=Headset default=yes"),
        ];
        app.build();
        assert_eq!(app.rows[0].control, Control::Slider(42));
        assert_eq!(app.rows[1].control, Control::Toggle(true));
        let card = app.rows.iter().find(|row| row.id == "sound.card").unwrap();
        assert_eq!(
            card.control,
            Control::Choice {
                options: vec!["0 Built-in".into(), "1 Headset".into()],
                index: 1
            }
        );
        assert!(app.rows.iter().any(|row| row.id == "sound.test"));
    }
    #[test]
    fn the_power_page_reflects_the_policy_and_the_battery() {
        let mut app = app();
        app.page = 3;
        app.data.records = vec![
            ipc::Record::parse("supply=battery capacity=42 batteries=1 discharging=yes"),
            ipc::Record::parse("button=ask lid=ignore low=25 critical=5 allow_users=no"),
            ipc::Record::parse("battery=BAT0 capacity=42 status=discharging minutes=90"),
        ];
        app.build();
        assert_eq!(app.rows[0].control, Control::Info("Battery".into()));
        assert_eq!(
            app.rows[1].control,
            Control::Info("42%, discharging, 90 minutes left".into())
        );
        let button = app.rows.iter().find(|row| row.id == "power.button").unwrap();
        assert_eq!(
            button.control,
            Control::Choice {
                options: vec![
                    "shutdown".into(),
                    "suspend".into(),
                    "ask".into(),
                    "ignore".into()
                ],
                index: 2
            }
        );
        let users = app
            .rows
            .iter()
            .find(|row| row.id == "power.allow-users")
            .unwrap();
        assert_eq!(users.control, Control::Toggle(false));
    }
    #[test]
    fn desktop_settings_are_read_from_the_configuration_text() {
        let text = "[menubar]\nenabled = false\nclock = true\n[toasts]\ncorner = bottom-left\n";
        assert_eq!(desktop_value(text, "menubar", "enabled", "true"), "false");
        assert_eq!(desktop_value(text, "toasts", "corner", "top-right"), "bottom-left");
        assert_eq!(desktop_value(text, "toasts", "duration_ms", "4000"), "4000");
        // Editing keeps everything else in place.
        let changed = apply_setting(text, "toasts", "corner", "top-left");
        assert!(changed.contains("[menubar]\nenabled = false"));
        assert!(changed.contains("corner = top-left"));
    }
    #[test]
    fn typing_edits_the_focused_field_and_enter_applies_it() {
        let mut app = app();
        app.page = 5;
        app.build();
        let index = app
            .rows
            .iter()
            .position(|row| row.id == "desktop.duration")
            .unwrap();
        app.focus = Some(index);
        app.key(&[127]);
        app.key(b"9");
        let Control::Text { value, .. } = &app.rows[index].control else {
            panic!("the duration is a text field");
        };
        assert!(value.ends_with('9'));
        // Keys without focus do not change anything.
        app.focus = None;
        let before = app.rows[index].control.clone();
        app.key(b"x");
        assert_eq!(app.rows[index].control, before);
    }
    #[test]
    fn list_pages_track_the_selected_row() {
        let mut app = app();
        app.page = 6;
        app.data.records = vec![
            ipc::Record::parse("unit=hos-netd state=running description=network"),
            ipc::Record::parse("unit=hos-power state=running description=power"),
        ];
        app.build();
        assert_eq!(app.selected("unit").as_deref(), Some("hos-netd"));
        app.key(b"\x1b[B");
        assert_eq!(app.selected_unit, 1);
        assert_eq!(app.selected("unit").as_deref(), Some("hos-power"));
        // The selection never runs past the end of the list.
        app.key(b"\x1b[B");
        app.build();
        assert_eq!(app.selected_unit, 1);
    }
    #[test]
    fn the_clock_reads_like_a_date() {
        let text = clock();
        assert!(text.ends_with(" UTC"), "{text}");
        assert_eq!(text.len(), "2026-09-25 12:34:56 UTC".len());
        assert_eq!(&text[4..5], "-");
    }
    #[test]
    fn long_values_are_shortened_to_fit() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("0123456789", 5), "0123~");
        assert_eq!(truncate("", 5), "");
    }
    #[test]
    fn the_whole_window_draws_without_a_session() {
        let mut app = app();
        app.data.records = vec![ipc::Record::parse("online=no")];
        app.build();
        let mut surface = Surface::new(WIDTH as usize, HEIGHT as usize);
        app.draw(&mut surface, &Font::builtin());
        assert!(surface.pixels().contains(&ACCENT), "the sidebar is drawn");
        // Every page draws, including ones with no service answering.
        for page in 0..PAGES.len() {
            app.page = page;
            app.build();
            app.draw(&mut surface, &Font::builtin());
        }
    }
}
