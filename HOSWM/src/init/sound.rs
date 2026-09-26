//! `hos-soundd`: default device, volume, mute and what is playing.
//!
//! Mixing is left to ALSA. The service writes an `/etc/asound.conf` whose
//! default device is a `dmix` plug, so several applications can play at once
//! without a sound server in between; capture goes through `dsnoop` the same
//! way. Volume and mute are the card's own mixer controls, driven through the
//! control device in [`crate::init::alsa`].
//!
//! What is playing is read from `/proc/asound`, so applications appear
//! whether or not they know this service exists.
//!
//! ```text
//! [sound]
//! card = 0            # default card index, or the card id such as PCH
//! volume = 60         # applied at startup when there is no saved state
//! rate = 48000        # mixing rate written into asound.conf
//! ```
use crate::init::{
    Settings, alsa, config_path, ipc,
    ipc::{Fields, Peer, Request, Response, Service},
    log,
    mixer::{self, Mixer},
    state_path, sys,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Where ALSA reads its system-wide configuration.
pub const ASOUND_CONF: &str = "/etc/asound.conf";
const PROC_ASOUND: &str = "/proc/asound";
/// Written to `/etc/hos` the first time the service runs.
const SOUNDD_DEFAULT: &str = include_str!("config/soundd.conf");

const STATE: &str = "soundd.state";
/// Mixer controls are re-read this often, so changes made elsewhere show up.
const POLL: Duration = Duration::from_secs(1);
/// What `SET` may change in `soundd.conf`.
const SETTABLE: &[(&str, &str)] = &[("sound", "card"), ("sound", "volume"), ("sound", "rate")];

/// Mixer element names worth using as the main volume, best first.
const VOLUME_NAMES: [&str; 6] = [
    "Master Playback Volume",
    "PCM Playback Volume",
    "Speaker Playback Volume",
    "Headphone Playback Volume",
    "Digital Playback Volume",
    "Front Playback Volume",
];

/// One sound card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Card {
    pub index: u32,
    pub id: String,
    pub name: String,
    pub playback: bool,
}

/// One application's playback or capture stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    pub card: u32,
    pub device: u32,
    pub subdevice: u32,
    /// `playback` or `capture`.
    pub direction: String,
    /// `RUNNING`, `PREPARED`, `SETUP` or `DRAINING`.
    pub state: String,
    pub pid: i32,
    pub program: String,
    pub rate: u32,
    pub channels: u32,
    pub format: String,
}
impl Stream {
    pub fn running(&self) -> bool {
        self.state == "RUNNING"
    }
    fn fields(&self) -> Fields {
        Fields::new()
            .number("card", self.card)
            .number("device", self.device)
            .number("subdevice", self.subdevice)
            .text("direction", &self.direction)
            .text("state", &self.state)
            .number("pid", self.pid)
            .text("program", &self.program)
            .number("rate", self.rate)
            .number("channels", self.channels)
            .text("format", &self.format)
    }
}

/// Parse one `/proc/asound/cardN/pcmMp/subS/status` file.
pub fn parse_status(text: &str) -> Option<(String, i32)> {
    let mut state = None;
    let mut pid = 0;
    for line in text.lines() {
        let (key, value) = match line.split_once(':') {
            Some((key, value)) => (key.trim(), value.trim()),
            None => continue,
        };
        match key {
            "state" => state = Some(value.to_string()),
            "owner_pid" => pid = value.parse().unwrap_or(0),
            _ => (),
        }
    }
    // A closed stream holds the single line "closed".
    match state {
        Some(state) if state != "closed" => Some((state, pid)),
        _ => None,
    }
}
/// Parse the `hw_params` beside a stream's status file.
pub fn parse_hw_params(text: &str) -> (u32, u32, String) {
    let mut rate = 0;
    let mut channels = 0;
    let mut format = String::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "rate" => rate = value.split_whitespace().next().unwrap_or("0").parse().unwrap_or(0),
            "channels" => channels = value.parse().unwrap_or(0),
            "format" => format = value.to_string(),
            _ => (),
        }
    }
    (rate, channels, format)
}

/// Read every open stream from `/proc/asound`.
fn read_streams(root: &Path) -> Vec<Stream> {
    let mut streams = Vec::new();
    let Ok(cards) = fs::read_dir(root) else {
        return streams;
    };
    let mut card_dirs: Vec<PathBuf> = cards
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("card"))
        })
        .collect();
    card_dirs.sort();
    for card_dir in card_dirs {
        let card: u32 = card_dir
            .file_name()
            .and_then(|name| name.to_string_lossy().strip_prefix("card").map(str::parse))
            .and_then(Result::ok)
            .unwrap_or(0);
        let Ok(devices) = fs::read_dir(&card_dir) else {
            continue;
        };
        let mut device_dirs: Vec<PathBuf> = devices
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("pcm"))
            })
            .collect();
        device_dirs.sort();
        for device_dir in device_dirs {
            let name = device_dir.file_name().unwrap_or_default().to_string_lossy();
            let Some(rest) = name.strip_prefix("pcm") else {
                continue;
            };
            let direction = if rest.ends_with('c') { "capture" } else { "playback" };
            let device: u32 = rest[..rest.len() - 1].parse().unwrap_or(0);
            let Ok(subdevices) = fs::read_dir(&device_dir) else {
                continue;
            };
            let mut sub_dirs: Vec<PathBuf> = subdevices
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("sub"))
                })
                .collect();
            sub_dirs.sort();
            for sub_dir in sub_dirs {
                let Some(status) = sys::read_text(sub_dir.join("status")) else {
                    continue;
                };
                let Some((state, pid)) = parse_status(&status) else {
                    continue;
                };
                let (rate, channels, format) = sys::read_text(sub_dir.join("hw_params"))
                    .map(|text| parse_hw_params(&text))
                    .unwrap_or_default();
                let subdevice: u32 = sub_dir
                    .file_name()
                    .and_then(|name| name.to_string_lossy().strip_prefix("sub").map(str::parse))
                    .and_then(Result::ok)
                    .unwrap_or(0);
                streams.push(Stream {
                    card,
                    device,
                    subdevice,
                    direction: direction.to_string(),
                    state,
                    pid,
                    program: program_name(pid),
                    rate,
                    channels,
                    format,
                });
            }
        }
    }
    streams
}
/// The command name behind a process id, for showing which app is playing.
fn program_name(pid: i32) -> String {
    if pid <= 0 {
        return String::new();
    }
    sys::read_text(format!("/proc/{pid}/comm")).unwrap_or_default()
}

/// The `asound.conf` that gives every application the same mixed device.
///
/// `dmix` does the mixing in the applications themselves through shared
/// memory, which is why no sound server is needed for several programs to
/// play at once. `plug` in front of it converts rates and formats.
pub fn asound_conf(card: u32, rate: u32) -> String {
    let key = 2048 + card * 2;
    format!(
        "# Written by hos-soundd. Changes are overwritten when the default \
         device changes.\n\
         # Several applications share one card through dmix and dsnoop.\n\
         pcm.hos_dmix {{\n\
         \x20   type dmix\n\
         \x20   ipc_key {key}\n\
         \x20   ipc_perm 0666\n\
         \x20   slave {{\n\
         \x20       pcm \"hw:{card},0\"\n\
         \x20       rate {rate}\n\
         \x20       channels 2\n\
         \x20       period_size 1024\n\
         \x20       buffer_size 8192\n\
         \x20   }}\n\
         }}\n\
         pcm.hos_dsnoop {{\n\
         \x20   type dsnoop\n\
         \x20   ipc_key {}\n\
         \x20   ipc_perm 0666\n\
         \x20   slave {{\n\
         \x20       pcm \"hw:{card},0\"\n\
         \x20       rate {rate}\n\
         \x20       channels 2\n\
         \x20   }}\n\
         }}\n\
         pcm.hos {{\n\
         \x20   type asym\n\
         \x20   playback.pcm \"hos_dmix\"\n\
         \x20   capture.pcm \"hos_dsnoop\"\n\
         }}\n\
         pcm.!default {{\n\
         \x20   type plug\n\
         \x20   slave.pcm \"hos\"\n\
         }}\n\
         ctl.!default {{\n\
         \x20   type hw\n\
         \x20   card {card}\n\
         }}\n",
        key + 1
    )
}

/// The hardware mixer of one card: the elements the service drives.
struct CardMixer {
    control: alsa::Control,
    volume: Option<alsa::Element>,
    switch: Option<alsa::Element>,
}
impl CardMixer {
    fn open(card: u32) -> io::Result<CardMixer> {
        let control = alsa::Control::open(card)?;
        let elements = control.elements()?;
        // Prefer the well-known names, then any playback volume at all.
        let volume = VOLUME_NAMES
            .iter()
            .find_map(|name| elements.iter().find(|e| e.name == *name && e.is_volume()))
            .or_else(|| elements.iter().find(|e| e.is_volume()))
            .cloned();
        let switch = volume.as_ref().and_then(|volume| {
            let wanted = volume.name.replace("Volume", "Switch");
            elements
                .iter()
                .find(|e| e.name == wanted && e.is_switch())
                .or_else(|| elements.iter().find(|e| e.is_switch()))
                .cloned()
        });
        Ok(CardMixer {
            control,
            volume,
            switch,
        })
    }
    fn percent(&self) -> Option<u32> {
        let volume = self.volume.as_ref()?;
        let values = self.control.read(volume).ok()?;
        Some(alsa::to_percent(
            *values.first()?,
            volume.min,
            volume.max,
        ))
    }
    fn set_percent(&self, percent: u32) -> io::Result<()> {
        let volume = self
            .volume
            .as_ref()
            .ok_or_else(|| io::Error::other("this card has no volume control"))?;
        self.control
            .write(volume, alsa::from_percent(percent, volume.min, volume.max))
    }
    fn muted(&self) -> Option<bool> {
        let switch = self.switch.as_ref()?;
        // A playback switch is on when sound is allowed through.
        Some(self.control.read(switch).ok()?.first()? == &0)
    }
    fn set_muted(&self, muted: bool) -> io::Result<()> {
        let switch = self
            .switch
            .as_ref()
            .ok_or_else(|| io::Error::other("this card has no mute switch"))?;
        self.control.write(switch, i64::from(!muted))
    }
}

/// Saved volume, mute and default card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Saved {
    card: Option<u32>,
    volume: Option<u32>,
    muted: Option<bool>,
}
fn read_saved() -> Saved {
    let settings = Settings::read(&state_path(STATE));
    Saved {
        card: settings.top("card").and_then(|value| value.parse().ok()),
        volume: settings.top("volume").and_then(|value| value.parse().ok()),
        muted: settings.boolean("", "muted"),
    }
}

pub struct Soundd {
    settings: Settings,
    cards: Vec<Card>,
    default: Option<u32>,
    mixer: Option<CardMixer>,
    /// The software mixer applications play through.
    audio: Option<Mixer>,
    streams: Vec<Stream>,
    volume: u32,
    muted: bool,
    rate: u32,
    events: Vec<String>,
    polled: Instant,
}
impl Soundd {
    pub fn new() -> Soundd {
        let settings = Settings::install("soundd.conf", SOUNDD_DEFAULT);
        for warning in &settings.warnings {
            log("hos-soundd", &format!("soundd.conf: {warning}"));
        }
        let rate = settings
            .number::<u32>("sound", "rate")
            .unwrap_or(48000)
            .clamp(8000, 192_000);
        let mut soundd = Soundd {
            settings,
            cards: Vec::new(),
            default: None,
            mixer: None,
            audio: None,
            streams: Vec::new(),
            volume: 0,
            muted: false,
            rate,
            events: Vec::new(),
            polled: Instant::now(),
        };
        soundd.scan();
        soundd.restore();
        // The software mixer lets several windows play at the same time.
        match Mixer::start(soundd.default.unwrap_or(0), soundd.rate, 2) {
            Ok(audio) => {
                soundd.audio = Some(audio);
                soundd.sync_mixer();
                log(
                    "hos-soundd",
                    &format!("applications may play through /run/hos/{}", mixer::SOCKET),
                );
            }
            Err(e) => log("hos-soundd", &format!("software mixer unavailable: {e}")),
        }
        soundd
    }
    /// Re-read the card list and keep the default device pointing at a card
    /// that still exists.
    fn scan(&mut self) {
        let mut cards = Vec::new();
        for index in alsa::cards() {
            let (id, name) = alsa::Control::open(index)
                .and_then(|control| control.name())
                .unwrap_or_else(|_| (format!("card{index}"), format!("card {index}")));
            cards.push(Card {
                index,
                id,
                name,
                playback: alsa::has_playback(index),
            });
        }
        let changed = cards != self.cards;
        self.cards = cards;
        if !changed {
            return;
        }
        let wanted = self.configured_card();
        if self.default.is_none_or(|current| {
            !self.cards.iter().any(|card| card.index == current) || Some(current) != wanted
        }) {
            let chosen = wanted
                .filter(|index| self.cards.iter().any(|card| card.index == *index))
                .or_else(|| {
                    self.cards
                        .iter()
                        .find(|card| card.playback)
                        .or_else(|| self.cards.first())
                        .map(|card| card.index)
                });
            if let Some(card) = chosen {
                self.use_card(card);
            }
        }
    }
    /// The card named in configuration or saved state, by index or by id.
    fn configured_card(&self) -> Option<u32> {
        let wanted = self
            .settings
            .get("sound", "card")
            .map(str::to_string)
            .or_else(|| read_saved().card.map(|card| card.to_string()))?;
        if let Ok(index) = wanted.parse::<u32>() {
            return Some(index);
        }
        self.cards
            .iter()
            .find(|card| card.id == wanted)
            .map(|card| card.index)
    }
    /// Make one card the default: open its mixer and write `asound.conf`.
    fn use_card(&mut self, index: u32) {
        match CardMixer::open(index) {
            Ok(mixer) => {
                self.volume = mixer.percent().unwrap_or(self.volume);
                self.muted = mixer.muted().unwrap_or(false);
                self.mixer = Some(mixer);
            }
            Err(e) => {
                log("hos-soundd", &format!("card {index}: {e}"));
                self.mixer = None;
            }
        }
        self.default = Some(index);
        if let Some(audio) = &self.audio {
            audio.use_card(index);
        }
        self.sync_mixer();
        let name = self
            .cards
            .iter()
            .find(|card| card.index == index)
            .map(|card| card.name.clone())
            .unwrap_or_default();
        let contents = asound_conf(index, self.rate);
        match sys::write_atomic(Path::new(ASOUND_CONF), &contents) {
            Ok(()) => log(
                "hos-soundd",
                &format!("default device is card {index} ({name}), mixing through dmix"),
            ),
            Err(e) => log("hos-soundd", &format!("{ASOUND_CONF}: {e}")),
        }
        self.events.push(
            Fields::new()
                .text("event", "default")
                .number("card", index)
                .text("name", &name)
                .line(),
        );
    }
    /// Apply the saved or configured volume at startup.
    fn restore(&mut self) {
        let saved = read_saved();
        let volume = saved
            .volume
            .or_else(|| self.settings.number::<u32>("sound", "volume"));
        if let (Some(mixer), Some(volume)) = (self.mixer.as_ref(), volume) {
            if mixer.set_percent(volume).is_ok() {
                self.volume = volume.min(100);
            }
        }
        if let (Some(mixer), Some(muted)) = (self.mixer.as_ref(), saved.muted) {
            if mixer.set_muted(muted).is_ok() {
                self.muted = muted;
            }
        }
        log(
            "hos-soundd",
            &format!(
                "{} card(s), volume {}%{}",
                self.cards.len(),
                self.volume,
                if self.muted { ", muted" } else { "" }
            ),
        );
    }
    fn save(&self) {
        let contents = format!(
            "# Written by hos-soundd.\ncard = {}\nvolume = {}\nmuted = {}\n",
            self.default.unwrap_or(0),
            self.volume,
            if self.muted { "yes" } else { "no" }
        );
        if let Err(e) = sys::write_atomic(&state_path(STATE), &contents) {
            log("hos-soundd", &format!("could not save the volume: {e}"));
        }
    }
    fn mixer(&self) -> Result<&CardMixer, String> {
        self.mixer
            .as_ref()
            .ok_or_else(|| "no sound card is available".to_string())
    }
    /// Parse `70`, `+5` or `-5` against the current volume.
    fn wanted_volume(&self, value: &str) -> Result<u32, String> {
        let parse = |text: &str| -> Result<i64, String> {
            text.parse::<i64>()
                .map_err(|_| format!("{value}: expected a percentage, +n or -n"))
        };
        let wanted = match value.as_bytes().first() {
            Some(b'+') => self.volume as i64 + parse(&value[1..])?,
            Some(b'-') => self.volume as i64 - parse(&value[1..])?,
            _ => parse(value)?,
        };
        Ok(wanted.clamp(0, 100) as u32)
    }
    /// Whether the card has no volume control, so the software mixer applies
    /// the volume to what it plays.
    fn software_volume(&self) -> bool {
        self.mixer.as_ref().is_none_or(|mixer| mixer.volume.is_none())
    }
    /// Tell the software mixer what the current volume is.
    fn sync_mixer(&self) {
        if let Some(audio) = &self.audio {
            audio.set_volume(self.volume, self.muted, self.software_volume());
        }
    }
    fn set_volume(&mut self, percent: u32) -> Result<u32, String> {
        let percent = percent.min(100);
        match self.mixer().and_then(|mixer| {
            mixer.set_percent(percent).map_err(|e| e.to_string())?;
            Ok(mixer.percent().unwrap_or(percent))
        }) {
            Ok(volume) => self.volume = volume,
            // Without a hardware control the software mixer carries the
            // volume, but only for what this service plays.
            Err(e) if self.audio.is_none() => return Err(e),
            Err(_) => self.volume = percent,
        }
        self.sync_mixer();
        self.publish_volume();
        self.save();
        Ok(self.volume)
    }
    fn set_muted(&mut self, muted: bool) -> Result<bool, String> {
        match self.mixer().and_then(|mixer| {
            mixer.set_muted(muted).map_err(|e| e.to_string())?;
            Ok(mixer.muted().unwrap_or(muted))
        }) {
            Ok(muted) => self.muted = muted,
            Err(e) if self.audio.is_none() => return Err(e),
            Err(_) => self.muted = muted,
        }
        self.sync_mixer();
        self.publish_volume();
        self.save();
        Ok(self.muted)
    }
    fn publish_volume(&mut self) {
        self.events.push(
            Fields::new()
                .text("event", "volume")
                .number("volume", self.volume)
                .flag("muted", self.muted)
                .line(),
        );
    }
    fn summary(&self) -> Fields {
        let card = self
            .cards
            .iter()
            .find(|card| Some(card.index) == self.default);
        Fields::new()
            .number("card", self.default.unwrap_or(0))
            .text("id", card.map(|card| card.id.as_str()).unwrap_or(""))
            .text("name", card.map(|card| card.name.as_str()).unwrap_or(""))
            .number("volume", self.volume)
            .flag("muted", self.muted)
            .flag(
                "mixer",
                self.mixer.as_ref().is_some_and(|m| m.volume.is_some()),
            )
            .number("cards", self.cards.len())
            .number("streams", self.streams.iter().filter(|s| s.running()).count())
            .number(
                "mixed",
                self.audio.as_ref().map(|audio| audio.streams().len()).unwrap_or(0),
            )
            .flag("software_mixer", self.audio.is_some())
            .text(
                "error",
                &self
                    .audio
                    .as_ref()
                    .and_then(Mixer::error)
                    .unwrap_or_default(),
            )
            .number("rate", self.rate)
            .text("config", ASOUND_CONF)
    }
}

impl Default for Soundd {
    fn default() -> Self {
        Soundd::new()
    }
}

impl Service for Soundd {
    fn handle(&mut self, request: &Request, peer: &Peer) -> Result<Response, String> {
        match request.verb.as_str() {
            "STATUS" => Ok(Response::ok()
                .record(self.summary())
                .records(self.cards.iter().map(|card| {
                    Fields::new()
                        .number("card", card.index)
                        .text("id", &card.id)
                        .text("name", &card.name)
                        .flag("playback", card.playback)
                        .flag("default", Some(card.index) == self.default)
                }))),
            "CARDS" => Ok(Response::ok().records(self.cards.iter().map(|card| {
                Fields::new()
                    .number("card", card.index)
                    .text("id", &card.id)
                    .text("name", &card.name)
                    .flag("playback", card.playback)
                    .flag("default", Some(card.index) == self.default)
            }))),
            "DEFAULT" => match request.arg(0) {
                None => Ok(Response::ok().record(self.summary())),
                Some(_) if !peer.root() => Err("changing the default card requires root".into()),
                Some(value) => {
                    let index = value
                        .parse::<u32>()
                        .ok()
                        .or_else(|| {
                            self.cards
                                .iter()
                                .find(|card| card.id == value)
                                .map(|card| card.index)
                        })
                        .ok_or_else(|| format!("{value}: no such card"))?;
                    if !self.cards.iter().any(|card| card.index == index) {
                        return Err(format!("{value}: no such card"));
                    }
                    self.use_card(index);
                    self.save();
                    Ok(Response::ok().record(self.summary()))
                }
            },
            "VOLUME" => match request.arg(0) {
                None => Ok(Response::ok().record(
                    Fields::new()
                        .number("volume", self.volume)
                        .flag("muted", self.muted),
                )),
                Some(value) => {
                    let wanted = self.wanted_volume(value)?;
                    let volume = self.set_volume(wanted)?;
                    Ok(Response::ok()
                        .record(Fields::new().number("volume", volume).flag("muted", self.muted)))
                }
            },
            "MUTE" => {
                let muted = match request.keyword(0).as_str() {
                    "" | "TOGGLE" => !self.muted,
                    "ON" | "YES" | "TRUE" | "1" => true,
                    "OFF" | "NO" | "FALSE" | "0" => false,
                    other => return Err(format!("{other}: expected ON, OFF or TOGGLE")),
                };
                let muted = self.set_muted(muted)?;
                Ok(Response::ok()
                    .record(Fields::new().number("volume", self.volume).flag("muted", muted)))
            }
            "STREAMS" => {
                // What applications are playing through this service, and
                // what the kernel sees on the card itself.
                let mixed = self
                    .audio
                    .as_ref()
                    .map(Mixer::streams)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|stream| {
                        Fields::new()
                            .number("stream", stream.id)
                            .text("name", &stream.name)
                            .text("source", stream.kind)
                            .number("pid", stream.pid)
                            .number("rate", stream.rate)
                            .number("channels", stream.channels)
                            .number("frames", stream.frames)
                    });
                Ok(Response::ok()
                    .records(mixed)
                    .records(self.streams.iter().map(Stream::fields)))
            }
            "PLAY" => {
                let path = request.need(0, "a path to a WAV file")?.to_string();
                let audio = self
                    .audio
                    .as_ref()
                    .ok_or("the software mixer is not running")?;
                let length = fs::metadata(&path)
                    .map_err(|e| format!("{path}: {e}"))?
                    .len();
                if length > mixer::MAX_FILE {
                    return Err(format!(
                        "{path} is larger than {} MiB",
                        mixer::MAX_FILE / 1024 / 1024
                    ));
                }
                let bytes = fs::read(&path).map_err(|e| format!("{path}: {e}"))?;
                let (samples, rate, channels) =
                    mixer::decode_wav(&bytes).map_err(|e| format!("{path}: {e}"))?;
                let frames = samples.len() as u32 / channels.max(1);
                let name = Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                audio.play(&name, samples, rate, channels);
                Ok(Response::message(format!(
                    "playing {name} ({:.1}s)",
                    frames as f64 / rate.max(1) as f64
                )))
            }
            "STOP" => {
                let audio = self
                    .audio
                    .as_ref()
                    .ok_or("the software mixer is not running")?;
                audio.stop_all();
                Ok(Response::message("stopped"))
            }
            "CONTROLS" => {
                let mixer = self.mixer()?;
                let elements = mixer.control.elements().map_err(|e| e.to_string())?;
                Ok(Response::ok().records(elements.iter().map(|element| {
                    Fields::new()
                        .number("numid", element.numid)
                        .text("name", &element.name)
                        .number("index", element.index)
                        .number("type", element.kind)
                        .number("count", element.count)
                        .number("min", element.min)
                        .number("max", element.max)
                })))
            }
            "SET" => {
                let response = ipc::setting("soundd.conf", SETTABLE, request)?;
                self.reload();
                Ok(response)
            }
            verb => Err(format!("{verb} is not a sound command")),
        }
    }
    fn tick(&mut self) -> Duration {
        if self.polled.elapsed() < POLL {
            return POLL - self.polled.elapsed();
        }
        self.polled = Instant::now();
        self.scan();
        let streams = read_streams(Path::new(PROC_ASOUND));
        if streams != self.streams {
            // Report only what an application would notice: a stream that
            // started, stopped or changed state.
            for stream in &streams {
                let before = self.streams.iter().find(|s| {
                    (s.card, s.device, s.subdevice) == (stream.card, stream.device, stream.subdevice)
                });
                if before.is_none_or(|before| before.state != stream.state) {
                    let fields = stream.fields();
                    self.events
                        .push(Fields::new().text("event", "stream").line() + " " + &fields.line());
                }
            }
            for stream in &self.streams {
                let gone = !streams.iter().any(|s| {
                    (s.card, s.device, s.subdevice) == (stream.card, stream.device, stream.subdevice)
                });
                if gone {
                    self.events.push(
                        Fields::new()
                            .text("event", "stream")
                            .number("card", stream.card)
                            .number("device", stream.device)
                            .number("subdevice", stream.subdevice)
                            .text("state", "closed")
                            .text("program", &stream.program)
                            .line(),
                    );
                }
            }
            self.streams = streams;
        }
        // Something else may have moved the mixer; keep the reported state true.
        if let Some(mixer) = self.mixer.as_ref() {
            let volume = mixer.percent().unwrap_or(self.volume);
            let muted = mixer.muted().unwrap_or(self.muted);
            if (volume, muted) != (self.volume, self.muted) {
                self.volume = volume;
                self.muted = muted;
                self.publish_volume();
            }
        }
        POLL
    }
    fn events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }
    fn public(&self) -> &'static [&'static str] {
        &[
            "STATUS", "CARDS", "VOLUME", "MUTE", "STREAMS", "CONTROLS", "DEFAULT", "PLAY", "STOP",
        ]
    }
    fn help(&self) -> &'static [&'static str] {
        &[
            "STATUS - default card, volume and every card",
            "CARDS - one record per sound card",
            "DEFAULT [card] - show or change the default card",
            "VOLUME [percent|+n|-n] - show or change the volume",
            "MUTE [ON|OFF|TOGGLE] - show or change mute",
            "STREAMS - what is playing right now, mixed and on the card",
            "PLAY path.wav - play a sound file through the mixer",
            "STOP - stop everything the mixer is playing",
            "CONTROLS - the mixer elements of the default card",
            "SET sound key value - change soundd.conf",
        ]
    }
    fn reload(&mut self) {
        self.settings = Settings::install("soundd.conf", SOUNDD_DEFAULT);
        if let Some(rate) = self.settings.number::<u32>("sound", "rate") {
            self.rate = rate.clamp(8000, 192_000);
        }
        if let Some(card) = self.configured_card() {
            self.use_card(card);
        }
        log("hos-soundd", "reloaded soundd.conf");
    }
    fn stop(&mut self) {
        self.save();
    }
}

/// Run the service.
pub fn main() -> io::Result<()> {
    log("hos-soundd", "starting");
    // The configuration directory is where the settings application writes.
    let _ = fs::create_dir_all(config_path("").parent().unwrap_or(Path::new("/etc")));
    let mut soundd = Soundd::new();
    ipc::serve("soundd", 0o666, &mut soundd)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_generated_asound_conf_mixes_through_dmix() {
        let text = asound_conf(1, 44100);
        assert!(text.contains("type dmix"));
        assert!(text.contains("type dsnoop"));
        assert!(text.contains("pcm \"hw:1,0\""));
        assert!(text.contains("rate 44100"));
        assert!(text.contains("pcm.!default"));
        assert!(text.contains("ctl.!default"));
        // dmix and dsnoop must not share one shared-memory key.
        assert!(text.contains("ipc_key 2050") && text.contains("ipc_key 2051"));
        assert!(asound_conf(0, 48000).contains("ipc_key 2048"));
    }
    #[test]
    fn stream_status_and_parameters_parse() {
        let running = "state: RUNNING\nowner_pid   : 412\ntrigger_time: 55.1\n";
        assert_eq!(parse_status(running), Some(("RUNNING".into(), 412)));
        assert_eq!(parse_status("closed\n"), None, "a closed stream is not one");
        assert_eq!(parse_status(""), None);
        let params = "access: RW_INTERLEAVED\nformat: S16_LE\nchannels: 2\nrate: 48000 (48000/1)\nperiod_size: 1024\n";
        assert_eq!(parse_hw_params(params), (48000, 2, "S16_LE".into()));
        assert_eq!(parse_hw_params("closed\n"), (0, 0, String::new()));
    }
    #[test]
    fn streams_are_read_out_of_proc_asound() {
        let root = std::env::temp_dir().join(format!("hos-sound-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sub = root.join("card0/pcm0p/sub0");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("status"), "state: RUNNING\nowner_pid   : 1\n").unwrap();
        fs::write(sub.join("hw_params"), "format: S16_LE\nchannels: 2\nrate: 48000\n").unwrap();
        let idle = root.join("card0/pcm0c/sub0");
        fs::create_dir_all(&idle).unwrap();
        fs::write(idle.join("status"), "closed\n").unwrap();
        let streams = read_streams(&root);
        assert_eq!(streams.len(), 1, "only open streams are reported");
        assert_eq!(streams[0].direction, "playback");
        assert!(streams[0].running());
        assert_eq!((streams[0].rate, streams[0].channels), (48000, 2));
        assert_eq!(streams[0].card, 0);
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn relative_volume_changes_are_clamped() {
        let mut soundd = Soundd {
            settings: Settings::default(),
            cards: Vec::new(),
            default: None,
            mixer: None,
            audio: None,
            streams: Vec::new(),
            volume: 60,
            muted: false,
            rate: 48000,
            events: Vec::new(),
            polled: Instant::now(),
        };
        assert_eq!(soundd.wanted_volume("70").unwrap(), 70);
        assert_eq!(soundd.wanted_volume("+5").unwrap(), 65);
        assert_eq!(soundd.wanted_volume("-70").unwrap(), 0);
        assert_eq!(soundd.wanted_volume("+50").unwrap(), 100);
        assert!(soundd.wanted_volume("loud").is_err());
        // Without a card, changing the volume explains itself.
        soundd.volume = 60;
        assert_eq!(
            soundd.set_volume(70).unwrap_err(),
            "no sound card is available"
        );
    }
}
