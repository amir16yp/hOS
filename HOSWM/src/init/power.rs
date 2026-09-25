//! `hos-power`: battery state, the power button, suspend and shutdown.
//!
//! The service reads `/sys/class/power_supply` for batteries and mains, opens
//! the input devices that carry a power button or a lid switch, and applies
//! the policy in `/etc/hos/power.conf`:
//!
//! ```text
//! [power]
//! button = shutdown      # shutdown, suspend, ask or ignore
//! lid = suspend          # suspend or ignore
//! low = 15               # percent that raises a warning event
//! critical = 5           # percent that triggers critical-action
//! critical-action = shutdown
//! allow-users = yes      # may a desktop user ask for suspend or shutdown
//! ```
//!
//! `ask` publishes a `button` event and does nothing else, leaving the choice
//! to the desktop. Shutdown and reboot are requests: they are forwarded to
//! `hos-init`, which owns the actual `reboot(2)` call.
use crate::init::{
    Settings, ipc,
    ipc::{Fields, Peer, Request, Response, Service},
    log,
    supervisor::Shutdown,
    sys,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::OpenOptionsExt,
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SUPPLY_DIR: &str = "/sys/class/power_supply";
/// Written to `/etc/hos` the first time the service runs.
const POWER_DEFAULT: &str = include_str!("config/power.conf");

const INPUT_DIR: &str = "/dev/input";
const SLEEP_STATE: &str = "/sys/power/state";
const KEY_POWER: usize = 116;
const KEY_SLEEP: usize = 142;
const KEY_SUSPEND: usize = 205;
const SW_LID: usize = 0;
/// How often batteries are re-read while running on battery.
const POLL_DISCHARGING: Duration = Duration::from_secs(10);
const POLL_CHARGING: Duration = Duration::from_secs(30);
/// What `SET` may change in `power.conf`.
const SETTABLE: &[(&str, &str)] = &[
    ("power", "button"),
    ("power", "lid"),
    ("power", "low"),
    ("power", "critical"),
    ("power", "critical-action"),
    ("power", "allow-users"),
];

/// What to do when the power button is pressed, or a battery runs out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Shutdown,
    Suspend,
    /// Publish an event and let the desktop decide.
    Ask,
    Ignore,
}
impl Policy {
    pub fn parse(value: &str) -> Option<Policy> {
        match value.trim().to_ascii_lowercase().as_str() {
            "shutdown" | "poweroff" => Some(Policy::Shutdown),
            "suspend" | "sleep" => Some(Policy::Suspend),
            "ask" | "prompt" => Some(Policy::Ask),
            "ignore" | "nothing" | "off" => Some(Policy::Ignore),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Policy::Shutdown => "shutdown",
            Policy::Suspend => "suspend",
            Policy::Ask => "ask",
            Policy::Ignore => "ignore",
        }
    }
}

/// The policy this service applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerPolicy {
    pub button: Policy,
    pub lid: Policy,
    pub low: u32,
    pub critical: u32,
    pub critical_action: Policy,
    pub allow_users: bool,
}
impl Default for PowerPolicy {
    fn default() -> Self {
        PowerPolicy {
            button: Policy::Shutdown,
            lid: Policy::Suspend,
            low: 15,
            critical: 5,
            critical_action: Policy::Shutdown,
            allow_users: true,
        }
    }
}
impl PowerPolicy {
    pub fn read(settings: &Settings) -> PowerPolicy {
        let mut policy = PowerPolicy::default();
        let read = |key: &str, current: &mut Policy| {
            if let Some(value) = settings.get("power", key) {
                match Policy::parse(value) {
                    Some(parsed) => *current = parsed,
                    None => log("hos-power", &format!("{key} = {value}: unknown action")),
                }
            }
        };
        read("button", &mut policy.button);
        read("lid", &mut policy.lid);
        read("critical-action", &mut policy.critical_action);
        if let Some(low) = settings.number::<u32>("power", "low") {
            policy.low = low.min(100);
        }
        if let Some(critical) = settings.number::<u32>("power", "critical") {
            policy.critical = critical.min(100);
        }
        if let Some(allow) = settings.boolean("power", "allow-users") {
            policy.allow_users = allow;
        }
        policy
    }
    fn fields(&self) -> Fields {
        Fields::new()
            .text("button", self.button.name())
            .text("lid", self.lid.name())
            .number("low", self.low)
            .number("critical", self.critical)
            .text("critical_action", self.critical_action.name())
            .flag("allow_users", self.allow_users)
    }
}

/// One battery, as the kernel reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Battery {
    pub name: String,
    /// Charge level in percent.
    pub capacity: u32,
    /// `charging`, `discharging`, `full`, `idle` or `unknown`.
    pub status: String,
    /// Minutes until empty or full, when the kernel reports enough to tell.
    pub minutes: Option<u32>,
}
impl Battery {
    pub fn charging(&self) -> bool {
        self.status == "charging"
    }
    pub fn discharging(&self) -> bool {
        self.status == "discharging"
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new()
            .text("battery", &self.name)
            .number("capacity", self.capacity)
            .text("status", &self.status);
        if let Some(minutes) = self.minutes {
            fields = fields.number("minutes", minutes);
        }
        fields
    }
}

/// Batteries and mains, read together so their states agree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Supplies {
    pub batteries: Vec<Battery>,
    /// Whether a mains supply is connected; `None` when there is none at all.
    pub ac: Option<bool>,
}
impl Supplies {
    pub fn read() -> Supplies {
        read_supplies(Path::new(SUPPLY_DIR))
    }
    /// The overall charge level across batteries.
    pub fn capacity(&self) -> Option<u32> {
        if self.batteries.is_empty() {
            return None;
        }
        let total: u32 = self.batteries.iter().map(|b| b.capacity).sum();
        Some(total / self.batteries.len() as u32)
    }
    pub fn discharging(&self) -> bool {
        self.ac != Some(true) && self.batteries.iter().any(Battery::discharging)
    }
    fn summary(&self) -> Fields {
        Fields::new()
            .text(
                "supply",
                match self.ac {
                    Some(true) => "ac",
                    Some(false) => "battery",
                    None => "unknown",
                },
            )
            .number("capacity", self.capacity().unwrap_or(0))
            .number("batteries", self.batteries.len())
            .flag("discharging", self.discharging())
    }
}

fn read_supplies(root: &Path) -> Supplies {
    let mut supplies = Supplies::default();
    let Ok(entries) = fs::read_dir(root) else {
        return supplies;
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let kind = sys::read_text(path.join("type")).unwrap_or_default();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match kind.as_str() {
            "Battery" => supplies.batteries.push(read_battery(&path, name)),
            "Mains" | "USB" | "UPS" => {
                let online = sys::read_number::<u32>(path.join("online")) == Some(1);
                // Any connected supply counts as running on mains.
                supplies.ac = Some(supplies.ac.unwrap_or(false) || online);
            }
            _ => (),
        }
    }
    supplies
}
fn read_battery(path: &Path, name: String) -> Battery {
    let capacity = sys::read_number::<u32>(path.join("capacity")).or_else(|| {
        let now = charge(path, "now")?;
        let full = charge(path, "full")?;
        (full > 0).then(|| (now * 100 / full) as u32)
    });
    let status = sys::read_text(path.join("status"))
        .unwrap_or_else(|| "Unknown".into())
        .to_ascii_lowercase()
        .replace("not charging", "idle");
    // Time left needs a rate; not every battery reports one.
    let minutes = (|| {
        let now = charge(path, "now")?;
        let rate = sys::read_number::<u64>(path.join("power_now"))
            .or_else(|| sys::read_number::<u64>(path.join("current_now")))
            .filter(|rate| *rate > 0)?;
        let remaining = if status == "charging" {
            charge(path, "full")?.saturating_sub(now)
        } else {
            now
        };
        Some((remaining * 60 / rate) as u32)
    })();
    Battery {
        name,
        capacity: capacity.unwrap_or(0).min(100),
        status,
        minutes: minutes.filter(|minutes| *minutes > 0 && *minutes < 60 * 48),
    }
}
/// Energy or charge, whichever the battery exposes.
fn charge(path: &Path, suffix: &str) -> Option<u64> {
    sys::read_number(path.join(format!("energy_{suffix}")))
        .or_else(|| sys::read_number(path.join(format!("charge_{suffix}"))))
}

/// An input device that reports a power button, sleep key or lid switch.
struct Switch {
    path: String,
    file: File,
    lid: bool,
}
impl Switch {
    /// Open an event device if it carries one of the keys this service wants.
    fn open(path: &Path) -> io::Result<Option<Switch>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(0o4000) // O_NONBLOCK
            .open(path)?;
        let fd = file.as_raw_fd();
        let mut keys = [0u8; 96];
        let mut switches = [0u8; 8];
        // EVIOCGBIT(EV_KEY, 96) and EVIOCGBIT(EV_SW, 8).
        // SAFETY: both buffers match the length encoded in the request.
        unsafe {
            sys::ioctl(fd, 0x8060_4521, keys.as_mut_ptr());
            sys::ioctl(fd, 0x8008_4525, switches.as_mut_ptr());
        }
        let power = [KEY_POWER, KEY_SLEEP, KEY_SUSPEND]
            .iter()
            .any(|key| has(&keys, *key));
        let lid = has(&switches, SW_LID);
        if !power && !lid {
            return Ok(None);
        }
        Ok(Some(Switch {
            path: path.display().to_string(),
            file,
            lid,
        }))
    }
    /// Read pending events. Returns the presses and lid changes seen.
    fn read(&mut self) -> Vec<(u16, u16, i32)> {
        let mut events = Vec::new();
        let mut buffer = [0u8; 24 * 32];
        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    for event in buffer[..n].chunks_exact(24) {
                        let kind = u16::from_ne_bytes([event[16], event[17]]);
                        let code = u16::from_ne_bytes([event[18], event[19]]);
                        let value = i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
                        events.push((kind, code, value));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        events
    }
}
fn has(bits: &[u8], bit: usize) -> bool {
    bits.get(bit / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0)
}

/// A reason not to suspend or shut down right now, held by one process.
#[derive(Clone, Debug)]
pub struct Inhibitor {
    pub id: u32,
    /// `sleep`, `shutdown` or `all`.
    pub what: String,
    pub who: String,
    pub pid: i32,
}
impl Inhibitor {
    fn blocks(&self, what: &str) -> bool {
        self.what == "all" || self.what == what
    }
}

/// Ask `hos-init` to bring the system down, falling back to a signal.
fn request_shutdown(how: Shutdown) -> Result<(), String> {
    let verb = how.name().to_ascii_uppercase();
    if let Ok(mut client) = ipc::Client::connect("init") {
        if client.call(&verb).is_ok() {
            return Ok(());
        }
    }
    // SAFETY: signalling PID 1, which handles these as shutdown requests.
    if unsafe { sys::kill(1, how.signal()) } < 0 {
        return Err(format!(
            "could not reach hos-init: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Write `mem` to `/sys/power/state`, falling back to `freeze`.
///
/// The write returns when the machine wakes up again.
fn enter_sleep() -> io::Result<&'static str> {
    let supported = sys::read_text(SLEEP_STATE).unwrap_or_default();
    let states: Vec<&str> = ["mem", "freeze"]
        .into_iter()
        .filter(|state| supported.split_whitespace().any(|s| s == *state))
        .collect();
    let mut last = io::Error::new(
        io::ErrorKind::Unsupported,
        "the kernel supports no sleep state",
    );
    for state in states {
        match fs::write(SLEEP_STATE, state) {
            Ok(()) => return Ok(if state == "mem" { "mem" } else { "freeze" }),
            Err(e) => last = e,
        }
    }
    Err(last)
}

pub struct Power {
    policy: PowerPolicy,
    supplies: Supplies,
    switches: Vec<Switch>,
    inhibitors: Vec<Inhibitor>,
    events: Vec<String>,
    next_id: u32,
    polled: Instant,
    scanned: Instant,
    /// Set once per discharge, so one warning is not repeated every poll.
    warned_low: bool,
    warned_critical: bool,
}
impl Power {
    pub fn new() -> Power {
        let settings = Settings::install("power.conf", POWER_DEFAULT);
        for warning in &settings.warnings {
            log("hos-power", &format!("power.conf: {warning}"));
        }
        let mut power = Power {
            policy: PowerPolicy::read(&settings),
            supplies: Supplies::read(),
            switches: Vec::new(),
            inhibitors: Vec::new(),
            events: Vec::new(),
            next_id: 1,
            polled: Instant::now(),
            scanned: Instant::now(),
            warned_low: false,
            warned_critical: false,
        };
        power.scan();
        log(
            "hos-power",
            &format!(
                "{} batteries, power button policy: {}",
                power.supplies.batteries.len(),
                power.policy.button.name()
            ),
        );
        power
    }
    /// Find the input devices carrying a power button or lid switch.
    fn scan(&mut self) {
        self.scanned = Instant::now();
        let Ok(entries) = fs::read_dir(INPUT_DIR) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("event"))
            })
            .collect();
        paths.sort();
        self.switches
            .retain(|switch| paths.iter().any(|path| path.display().to_string() == switch.path));
        for path in paths {
            let known = self
                .switches
                .iter()
                .any(|switch| switch.path == path.display().to_string());
            if known {
                continue;
            }
            match Switch::open(&path) {
                Ok(Some(switch)) => {
                    log(
                        "hos-power",
                        &format!(
                            "watching {} for the {}",
                            switch.path,
                            if switch.lid { "lid switch" } else { "power button" }
                        ),
                    );
                    self.switches.push(switch);
                }
                Ok(None) => (),
                // Devices that cannot be opened are simply not watched.
                Err(_) => (),
            }
        }
    }
    /// Whether something asked to block `what`, dropping dead inhibitors.
    fn inhibited(&mut self, what: &str) -> Option<String> {
        self.inhibitors.retain(|inhibitor| {
            // SAFETY: signal 0 only checks that the process still exists.
            let alive = unsafe { sys::kill(inhibitor.pid, 0) };
            inhibitor.pid <= 0 || alive == 0
        });
        self.inhibitors
            .iter()
            .find(|inhibitor| inhibitor.blocks(what))
            .map(|inhibitor| inhibitor.who.clone())
    }
    /// Suspend the machine, unless something is holding it awake.
    pub fn suspend(&mut self, force: bool) -> Result<String, String> {
        if let Some(who) = self.inhibited("sleep").filter(|_| !force) {
            return Err(format!("{who} is keeping the system awake"));
        }
        self.publish("sleep", Fields::new().text("state", "begin"));
        log("hos-power", "suspending");
        let result = enter_sleep();
        match result {
            Ok(state) => {
                log("hos-power", "resumed");
                self.publish("sleep", Fields::new().text("state", "end"));
                // The battery may look very different after a long sleep.
                self.supplies = Supplies::read();
                Ok(format!("resumed from {state}"))
            }
            Err(e) => {
                self.publish(
                    "sleep",
                    Fields::new().text("state", "failed").text("error", &e.to_string()),
                );
                Err(format!("suspend failed: {e}"))
            }
        }
    }
    /// Apply one policy action, for the button, the lid or a flat battery.
    fn act(&mut self, policy: Policy, reason: &str) {
        match policy {
            Policy::Ignore => (),
            Policy::Ask => self.publish(
                "ask",
                Fields::new().text("reason", reason).text("choices", "suspend,shutdown,cancel"),
            ),
            Policy::Suspend => {
                if let Err(e) = self.suspend(false) {
                    log("hos-power", &e);
                }
            }
            Policy::Shutdown => {
                if let Some(who) = self.inhibited("shutdown") {
                    log("hos-power", &format!("{who} is blocking shutdown"));
                    self.publish("blocked", Fields::new().text("who", &who).text("reason", reason));
                    return;
                }
                self.publish("shutdown", Fields::new().text("reason", reason));
                if let Err(e) = request_shutdown(Shutdown::PowerOff) {
                    log("hos-power", &e);
                }
            }
        }
    }
    fn publish(&mut self, event: &str, fields: Fields) {
        self.events
            .push(Fields::new().text("event", event).line() + " " + &fields.line());
    }
    /// Read the batteries and report what changed.
    fn poll_supplies(&mut self) {
        self.polled = Instant::now();
        let supplies = Supplies::read();
        if supplies == self.supplies {
            return;
        }
        let was_ac = self.supplies.ac;
        self.supplies = supplies;
        let capacity = self.supplies.capacity().unwrap_or(100);
        if was_ac != self.supplies.ac {
            self.publish(
                "ac",
                Fields::new().flag("online", self.supplies.ac == Some(true)),
            );
            if self.supplies.ac == Some(true) {
                self.warned_low = false;
                self.warned_critical = false;
            }
        }
        let summary = self.supplies.summary();
        self.publish("battery", summary);
        if !self.supplies.discharging() {
            return;
        }
        if capacity <= self.policy.critical && !self.warned_critical {
            self.warned_critical = true;
            log(
                "hos-power",
                &format!("battery critical at {capacity}%; {}", self.policy.critical_action.name()),
            );
            self.publish("critical", Fields::new().number("capacity", capacity));
            let action = self.policy.critical_action;
            self.act(action, "battery critical");
        } else if capacity <= self.policy.low && !self.warned_low {
            self.warned_low = true;
            log("hos-power", &format!("battery low at {capacity}%"));
            self.publish("low", Fields::new().number("capacity", capacity));
        }
    }
    /// Read the button and lid devices.
    fn poll_switches(&mut self) {
        let mut actions: Vec<(Policy, String)> = Vec::new();
        for switch in &mut self.switches {
            for (kind, code, value) in switch.read() {
                match (kind, code as usize, value) {
                    // A key press, not a release or an auto-repeat.
                    (1, KEY_POWER, 1) => actions.push((self.policy.button, "power button".into())),
                    (1, KEY_SLEEP | KEY_SUSPEND, 1) => {
                        actions.push((Policy::Suspend, "sleep key".into()))
                    }
                    (5, SW_LID, state) => {
                        if state == 1 {
                            actions.push((self.policy.lid, "lid closed".into()));
                        } else {
                            actions.push((Policy::Ignore, "lid opened".into()));
                        }
                    }
                    _ => (),
                }
            }
        }
        for (policy, reason) in actions {
            log("hos-power", &format!("{reason} ({})", policy.name()));
            self.publish("button", Fields::new().text("reason", &reason).text("action", policy.name()));
            self.act(policy, &reason);
        }
    }
}

impl Default for Power {
    fn default() -> Self {
        Power::new()
    }
}

impl Service for Power {
    fn handle(&mut self, request: &Request, peer: &Peer) -> Result<Response, String> {
        let allowed = peer.root() || self.policy.allow_users;
        match request.verb.as_str() {
            "STATUS" => Ok(Response::ok()
                .record(self.supplies.summary())
                .record(self.policy.fields())
                .records(self.supplies.batteries.iter().map(Battery::fields))),
            "BATTERIES" => Ok(Response::ok().records(self.supplies.batteries.iter().map(Battery::fields))),
            "SUSPEND" => {
                if !allowed {
                    return Err("SUSPEND requires root".into());
                }
                let force = request.keyword(0) == "FORCE";
                self.suspend(force).map(Response::message)
            }
            "SHUTDOWN" | "POWEROFF" | "REBOOT" | "HALT" => {
                if !allowed {
                    return Err(format!("{} requires root", request.verb));
                }
                let how = Shutdown::parse(&request.verb).expect("the verb matched above");
                if let Some(who) = self.inhibited("shutdown") {
                    return Err(format!("{who} is blocking {}", how.name()));
                }
                self.publish("shutdown", Fields::new().text("reason", "requested"));
                request_shutdown(how)?;
                Ok(Response::message(format!("{} requested", how.name())))
            }
            "INHIBIT" => {
                let what = request.keyword(0).to_ascii_lowercase();
                let what = match what.as_str() {
                    "" => "all".to_string(),
                    "sleep" | "shutdown" | "all" => what,
                    other => return Err(format!("{other}: inhibit sleep, shutdown or all")),
                };
                if self.inhibitors.len() >= 32 {
                    return Err("too many inhibitors".into());
                }
                let id = self.next_id;
                self.next_id += 1;
                let who = request
                    .args
                    .get(1..)
                    .map(|rest| rest.join(" "))
                    .filter(|who| !who.is_empty())
                    .unwrap_or_else(|| format!("pid {}", peer.pid));
                self.inhibitors.push(Inhibitor {
                    id,
                    what: what.clone(),
                    who: who.clone(),
                    pid: peer.pid,
                });
                log("hos-power", &format!("{who} is inhibiting {what}"));
                Ok(Response::ok().record(Fields::new().number("inhibitor", id).text("what", &what)))
            }
            "RELEASE" => {
                let id: u32 = request
                    .need(0, "an inhibitor ID")?
                    .parse()
                    .map_err(|_| "RELEASE needs an inhibitor ID".to_string())?;
                let before = self.inhibitors.len();
                self.inhibitors
                    .retain(|inhibitor| inhibitor.id != id || (!peer.root() && inhibitor.pid != peer.pid));
                if self.inhibitors.len() == before {
                    return Err(format!("no inhibitor {id}"));
                }
                Ok(Response::message(format!("inhibitor {id} released")))
            }
            "INHIBITORS" => Ok(Response::ok().records(self.inhibitors.iter().map(|inhibitor| {
                Fields::new()
                    .number("inhibitor", inhibitor.id)
                    .text("what", &inhibitor.what)
                    .text("who", &inhibitor.who)
                    .number("pid", inhibitor.pid)
            }))),
            "POLICY" => {
                let key = request.keyword(0).to_ascii_lowercase();
                if key.is_empty() {
                    return Ok(Response::ok().record(self.policy.fields()));
                }
                if !peer.root() {
                    return Err("changing the policy requires root".into());
                }
                let value = request.need(1, "a value")?.to_string();
                let policy = Policy::parse(&value);
                match (key.as_str(), policy) {
                    ("button", Some(policy)) => self.policy.button = policy,
                    ("lid", Some(policy)) => self.policy.lid = policy,
                    ("critical-action", Some(policy)) => self.policy.critical_action = policy,
                    ("low", _) => {
                        self.policy.low = value.parse().map_err(|_| "low takes a percent")?
                    }
                    ("critical", _) => {
                        self.policy.critical =
                            value.parse().map_err(|_| "critical takes a percent")?
                    }
                    ("allow-users", _) => {
                        self.policy.allow_users = matches!(value.as_str(), "yes" | "true" | "on" | "1")
                    }
                    (other, _) => return Err(format!("{other}: not a policy, or not a valid value")),
                }
                // The change lasts until the service restarts or reloads.
                Ok(Response::ok().record(self.policy.fields()))
            }
            "SET" => {
                let response = ipc::setting("power.conf", SETTABLE, request)?;
                self.reload();
                Ok(response)
            }
            verb => Err(format!("{verb} is not a power command")),
        }
    }
    fn tick(&mut self) -> Duration {
        self.poll_switches();
        let interval = if self.supplies.discharging() {
            POLL_DISCHARGING
        } else {
            POLL_CHARGING
        };
        if self.polled.elapsed() >= interval {
            self.poll_supplies();
        }
        if self.scanned.elapsed() >= Duration::from_secs(5) {
            self.scan();
        }
        interval
            .saturating_sub(self.polled.elapsed())
            .min(Duration::from_secs(5))
            .max(Duration::from_millis(100))
    }
    fn sources(&mut self) -> Vec<RawFd> {
        self.switches
            .iter()
            .map(|switch| switch.file.as_raw_fd())
            .collect()
    }
    fn events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }
    fn public(&self) -> &'static [&'static str] {
        &[
            "STATUS",
            "BATTERIES",
            "INHIBITORS",
            "POLICY",
            "INHIBIT",
            "RELEASE",
            "SUSPEND",
            "SHUTDOWN",
            "POWEROFF",
            "REBOOT",
            "HALT",
        ]
    }
    fn help(&self) -> &'static [&'static str] {
        &[
            "STATUS - supply, batteries and the current policy",
            "BATTERIES - one record per battery",
            "SUSPEND [FORCE] - sleep unless something inhibits it",
            "SHUTDOWN | REBOOT | HALT - ask hos-init to bring the system down",
            "INHIBIT [sleep|shutdown|all] [who] - hold off sleep or shutdown",
            "RELEASE id - drop an inhibitor",
            "INHIBITORS - what is currently held",
            "POLICY [key value] - show or change the policy until restart",
            "SET power key value - change the policy in power.conf",
        ]
    }
    fn reload(&mut self) {
        self.policy = PowerPolicy::read(&Settings::install("power.conf", POWER_DEFAULT));
        log("hos-power", "reloaded power.conf");
    }
}

/// Run the service.
pub fn main() -> io::Result<()> {
    log("hos-power", "starting");
    let mut power = Power::new();
    ipc::serve("power", 0o666, &mut power)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        for (file, value) in files {
            fs::write(dir.join(file), format!("{value}\n")).unwrap();
        }
    }
    #[test]
    fn batteries_and_mains_are_read_out_of_sysfs() {
        let root = std::env::temp_dir().join(format!("hos-power-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        supply(
            &root,
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "42"),
                ("status", "Discharging"),
                ("energy_now", "21000000"),
                ("energy_full", "50000000"),
                ("power_now", "10500000"),
            ],
        );
        supply(&root, "AC", &[("type", "Mains"), ("online", "0")]);
        let supplies = read_supplies(&root);
        assert_eq!(supplies.ac, Some(false));
        assert_eq!(supplies.batteries.len(), 1);
        let battery = &supplies.batteries[0];
        assert_eq!(battery.name, "BAT0");
        assert_eq!(battery.capacity, 42);
        assert!(battery.discharging());
        assert_eq!(battery.minutes, Some(120), "21 Wh left at 10.5 W");
        assert!(supplies.discharging());
        assert_eq!(supplies.capacity(), Some(42));
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn a_battery_without_a_capacity_file_is_computed_from_its_charge() {
        let root = std::env::temp_dir().join(format!("hos-power-charge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        supply(
            &root,
            "BAT1",
            &[
                ("type", "Battery"),
                ("status", "Not charging"),
                ("charge_now", "1500"),
                ("charge_full", "3000"),
            ],
        );
        let supplies = read_supplies(&root);
        assert_eq!(supplies.batteries[0].capacity, 50);
        assert_eq!(supplies.batteries[0].status, "idle");
        assert_eq!(supplies.batteries[0].minutes, None, "no rate was reported");
        assert!(!supplies.discharging());
        assert_eq!(supplies.ac, None, "this machine reported no mains supply");
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn the_policy_comes_from_configuration_with_documented_defaults() {
        let defaults = PowerPolicy::read(&Settings::default());
        assert_eq!(defaults.button, Policy::Shutdown);
        assert_eq!(defaults.lid, Policy::Suspend);
        assert_eq!((defaults.low, defaults.critical), (15, 5));
        let settings = Settings::parse(
            "[power]\nbutton = ask\nlid = ignore\nlow = 25\ncritical = 200\nallow-users = no\nbutton-x = nonsense\n",
        );
        let policy = PowerPolicy::read(&settings);
        assert_eq!(policy.button, Policy::Ask);
        assert_eq!(policy.lid, Policy::Ignore);
        assert_eq!(policy.low, 25);
        assert_eq!(policy.critical, 100, "percentages are clamped");
        assert!(!policy.allow_users);
        assert_eq!(Policy::parse("shut down"), None);
    }
    #[test]
    fn inhibitors_block_and_are_released() {
        let mut power = Power {
            policy: PowerPolicy::default(),
            supplies: Supplies::default(),
            switches: Vec::new(),
            inhibitors: Vec::new(),
            events: Vec::new(),
            next_id: 1,
            polled: Instant::now(),
            scanned: Instant::now(),
            warned_low: false,
            warned_critical: false,
        };
        let peer = Peer {
            pid: std::process::id() as i32,
            uid: 0,
        };
        let reply = power
            .handle(&Request::parse("INHIBIT shutdown installing\\shOS"), &peer)
            .unwrap();
        assert!(reply.records[0].starts_with("inhibitor=1"));
        assert_eq!(power.inhibited("shutdown").as_deref(), Some("installing hOS"));
        assert_eq!(power.inhibited("sleep"), None);
        assert!(
            power
                .handle(&Request::parse("SHUTDOWN"), &peer)
                .unwrap_err()
                .contains("blocking")
        );
        power.handle(&Request::parse("RELEASE 1"), &peer).unwrap();
        assert_eq!(power.inhibited("shutdown"), None);
        assert!(power.handle(&Request::parse("RELEASE 1"), &peer).is_err());
        // An inhibitor whose process is gone stops counting.
        power.inhibitors.push(Inhibitor {
            id: 2,
            what: "all".into(),
            who: "ghost".into(),
            pid: i32::MAX,
        });
        assert_eq!(power.inhibited("sleep"), None);
    }
}
