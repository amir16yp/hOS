//! Hot-pluggable evdev input.
//!
//! Devices are discovered from `/dev/input`, rescanned when the kernel reports
//! a change (inotify, with a one second fallback sweep) and dropped when they
//! disappear. A session with no keyboard or mouse still runs: the devices are
//! picked up as soon as they are plugged in.
//!
//! Absolute pointers (tablets, and the QEMU `usb-tablet`) are translated to
//! desktop coordinates here, so the session only handles one motion model.
use crate::reactor::Reactor;
use std::{
    collections::HashSet,
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
    time::{Duration, Instant},
};

pub const DIRECTORY: &str = "/dev/input";
const RESCAN: Duration = Duration::from_secs(1);
/// `O_NONBLOCK`. Event reads must never stall the compositor.
const NONBLOCK: i32 = 0x800;
/// Synthetic event type for an absolute pointer position, already scaled to
/// the desktop: code 0 is x, code 1 is y.
pub const ABSOLUTE: u16 = 3;

unsafe extern "C" {
    fn inotify_init1(flags: i32) -> i32;
    fn inotify_add_watch(fd: i32, path: *const i8, mask: u32) -> i32;
}

fn has(bits: &[u8], bit: usize) -> bool {
    bits.get(bit / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0)
}

pub struct Device {
    file: File,
    pub path: String,
    pub keyboard: bool,
    pub pointer: bool,
    /// Absolute axis ranges, when this is a tablet rather than a mouse.
    absolute: Option<([i32; 2], [i32; 2])>,
    pub grabbed: bool,
}
impl Device {
    pub fn kind(&self) -> &'static str {
        match (self.keyboard, self.pointer) {
            (true, true) => "input device",
            (true, false) => "keyboard",
            (false, true) if self.absolute.is_some() => "tablet",
            _ => "mouse",
        }
    }
    /// Probe an `/dev/input/event*` node, returning `None` for devices that
    /// are neither a keyboard nor a pointer.
    fn open(path: &Path) -> io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(NONBLOCK)
            .open(path)?;
        let fd = file.as_raw_fd();
        let mut keys = [0u8; 96];
        let mut relative = [0u8; 8];
        let mut absolute = [0u8; 8];
        // EVIOCGBIT(EV_KEY, 96), EVIOCGBIT(EV_REL, 8), EVIOCGBIT(EV_ABS, 8).
        unsafe {
            crate::drm::ioctl(fd, 0x8060_4521, keys.as_mut_ptr());
            crate::drm::ioctl(fd, 0x8008_4522, relative.as_mut_ptr());
            crate::drm::ioctl(fd, 0x8008_4523, absolute.as_mut_ptr());
        }
        let keyboard = has(&keys, 30);
        let button = has(&keys, 272) || has(&keys, 330);
        let wheel = has(&relative, 0) && has(&relative, 1);
        let tablet = has(&absolute, 0) && has(&absolute, 1);
        if !keyboard && !(button && (wheel || tablet)) {
            return Ok(None);
        }
        let ranges = (button && tablet && !wheel).then(|| {
            let mut x = [0i32; 6];
            let mut y = [0i32; 6];
            // EVIOCGABS(ABS_X), EVIOCGABS(ABS_Y): struct input_absinfo.
            unsafe {
                crate::drm::ioctl(fd, 0x8018_4540, x.as_mut_ptr());
                crate::drm::ioctl(fd, 0x8018_4541, y.as_mut_ptr());
            }
            ([x[1], x[2]], [y[1], y[2]])
        });
        // EVIOCGRAB: keep key presses out of the console. Devices that refuse
        // are still used; losing exclusivity beats losing the device.
        let grabbed = unsafe { crate::drm::ioctl(fd, 0x4004_4590, 1i32) } >= 0;
        Ok(Some(Self {
            file,
            path: path.display().to_string(),
            keyboard,
            pointer: button && (wheel || tablet),
            absolute: ranges,
            grabbed,
        }))
    }
    /// Map an absolute axis value onto the 800x600 desktop.
    fn scale(&self, axis: usize, value: i32) -> Option<i32> {
        let ([x0, x1], [y0, y1]) = self.absolute?;
        let (low, high, size) = if axis == 0 {
            (x0, x1, 800)
        } else {
            (y0, y1, 600)
        };
        if high <= low {
            return None;
        }
        let span = (high - low) as i64;
        Some(((value.clamp(low, high) - low) as i64 * (size - 1) / span) as i32)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Added(String, &'static str),
    Removed(String, &'static str),
    /// The kernel dropped events; held modifiers may be stale.
    Overflow(String),
    /// The device exists but could not be opened, reported once per path.
    Unavailable(String, String),
}
impl Change {
    pub fn message(&self) -> String {
        match self {
            Self::Added(path, kind) => format!("{kind} connected ({path})"),
            Self::Removed(path, kind) => format!("{kind} disconnected ({path})"),
            Self::Overflow(path) => format!("Input overflow on {path}; state resynchronized"),
            Self::Unavailable(path, error) => format!("{path}: {error}"),
        }
    }
    /// Notification color: green for arrivals, red for losses, amber for noise.
    pub fn color(&self) -> u32 {
        match self {
            Self::Added(..) => crate::desktop::ACCENT,
            Self::Removed(..) => 0xffef6976,
            _ => 0xffe4c878,
        }
    }
}

/// The set of open input devices, kept in step with `/dev/input`.
pub struct Devices {
    devices: Vec<Device>,
    /// inotify watch on the device directory; `None` falls back to sweeping.
    watch: Option<File>,
    next_scan: Instant,
    failed: HashSet<String>,
    directory: String,
}
impl Default for Devices {
    fn default() -> Self {
        Self::new(DIRECTORY)
    }
}
impl Devices {
    pub fn new(directory: &str) -> Self {
        // IN_NONBLOCK | IN_CLOEXEC, watching creation, removal and the
        // permission changes that follow device creation.
        let watch = unsafe {
            let fd = inotify_init1(0o4000 | 0o2000000);
            if fd < 0 {
                None
            } else {
                let file = File::from_raw_fd(fd);
                let path = CString::new(directory).unwrap_or_default();
                let mask = 0x100 | 0x200 | 0x80 | 0x40 | 0x4; // create, delete, moved, attrib
                (inotify_add_watch(fd, path.as_ptr(), mask) >= 0).then_some(file)
            }
        };
        Self {
            devices: Vec::new(),
            watch,
            next_scan: Instant::now(),
            failed: HashSet::new(),
            directory: directory.to_string(),
        }
    }
    pub fn len(&self) -> usize {
        self.devices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
    pub fn has_keyboard(&self) -> bool {
        self.devices.iter().any(|d| d.keyboard)
    }
    pub fn has_pointer(&self) -> bool {
        self.devices.iter().any(|d| d.pointer)
    }
    /// Watch every device, plus the directory itself, for readability.
    pub fn watch(&self, reactor: &mut Reactor) {
        if let Some(watch) = &self.watch {
            reactor.watch(watch.as_raw_fd(), true, false);
        }
        for device in &self.devices {
            reactor.watch(device.file.as_raw_fd(), true, false);
        }
    }
    /// Rescan when the kernel reported a change or the sweep timer expired.
    pub fn poll(&mut self) -> Vec<Change> {
        let mut rescan = Instant::now() >= self.next_scan;
        if let Some(watch) = &mut self.watch {
            let mut buffer = [0u8; 4096];
            loop {
                match watch.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => rescan = true,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        if rescan { self.scan() } else { Vec::new() }
    }
    /// Open devices that appeared and forget devices that are gone.
    pub fn scan(&mut self) -> Vec<Change> {
        self.next_scan = Instant::now() + RESCAN;
        let mut changes = Vec::new();
        let mut present = Vec::new();
        let entries = match std::fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(e) => {
                let message = format!("{}: {e}", self.directory);
                if self.failed.insert(message.clone()) {
                    changes.push(Change::Unavailable(self.directory.clone(), e.to_string()));
                }
                return changes;
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("event"))
            {
                continue;
            }
            let name = path.display().to_string();
            present.push(name.clone());
            if self.devices.iter().any(|d| d.path == name) {
                continue;
            }
            match Device::open(&path) {
                Ok(Some(device)) => {
                    self.failed.remove(&name);
                    changes.push(Change::Added(name, device.kind()));
                    self.devices.push(device);
                }
                Ok(None) => (),
                Err(e) => {
                    // Report a device we cannot open once, then keep retrying
                    // quietly: permissions often settle moments after creation.
                    if self.failed.insert(name.clone()) {
                        changes.push(Change::Unavailable(name, e.to_string()));
                    }
                }
            }
        }
        self.devices.retain(|device| {
            let kept = present.contains(&device.path);
            if !kept {
                changes.push(Change::Removed(device.path.clone(), device.kind()));
            }
            kept
        });
        changes
    }
    /// Drain pending events. Devices that fail are dropped, not fatal.
    pub fn read(&mut self, events: &mut Vec<(u16, u16, i32)>) -> Vec<Change> {
        events.clear();
        let mut changes = Vec::new();
        let mut lost = Vec::new();
        for device in &mut self.devices {
            let mut bytes = [0u8; 24 * 64];
            'device: for _ in 0..8 {
                match device.file.read(&mut bytes) {
                    Ok(0) => {
                        lost.push(device.path.clone());
                        changes.push(Change::Removed(device.path.clone(), device.kind()));
                        break;
                    }
                    Ok(n) if n % 24 != 0 => {
                        lost.push(device.path.clone());
                        changes.push(Change::Removed(device.path.clone(), device.kind()));
                        break;
                    }
                    Ok(n) => {
                        for event in bytes[..n].chunks_exact(24) {
                            let kind = u16::from_ne_bytes([event[16], event[17]]);
                            let code = u16::from_ne_bytes([event[18], event[19]]);
                            let value = i32::from_ne_bytes(event[20..24].try_into().unwrap());
                            match (kind, code) {
                                // SYN_DROPPED: discard this batch and resync.
                                (0, 3) => {
                                    events.clear();
                                    changes.push(Change::Overflow(device.path.clone()));
                                    break 'device;
                                }
                                (3, 0 | 1) => {
                                    if let Some(scaled) = device.scale(code as usize, value) {
                                        events.push((ABSOLUTE, code, scaled));
                                    }
                                }
                                // BTN_TOUCH from a tablet acts as the left button.
                                (1, 330) => events.push((1, 272, value)),
                                _ => events.push((kind, code, value)),
                            }
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        lost.push(device.path.clone());
                        changes.push(Change::Removed(device.path.clone(), device.kind()));
                        break;
                    }
                }
            }
        }
        if !lost.is_empty() {
            self.devices.retain(|d| !lost.contains(&d.path));
            // A replugged device reappears on the next sweep.
            self.next_scan = Instant::now();
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capability_bits_and_absolute_scaling() {
        let mut bits = [0u8; 96];
        bits[30 / 8] |= 1 << (30 % 8);
        assert!(has(&bits, 30));
        assert!(!has(&bits, 31));
        assert!(!has(&bits, 4000), "out-of-range bits are absent, not a panic");
        let device = Device {
            file: File::open("/dev/null").unwrap(),
            path: "/dev/input/event9".into(),
            keyboard: false,
            pointer: true,
            absolute: Some(([0, 32767], [0, 32767])),
            grabbed: true,
        };
        assert_eq!(device.kind(), "tablet");
        assert_eq!(device.scale(0, 0), Some(0));
        assert_eq!(device.scale(0, 32767), Some(799));
        assert_eq!(device.scale(1, 32767), Some(599));
        assert_eq!(device.scale(1, 16384), Some(299));
        assert_eq!(device.scale(0, 99999), Some(799), "values are clamped");
        let relative = Device {
            absolute: None,
            keyboard: true,
            ..device
        };
        assert_eq!(relative.kind(), "input device");
        assert_eq!(relative.scale(0, 5), None);
    }
    #[test]
    fn a_missing_device_directory_is_reported_once_and_is_not_fatal() {
        let missing = std::env::temp_dir().join(format!("hoswm-no-input-{}", std::process::id()));
        let mut devices = Devices::new(missing.to_str().unwrap());
        let changes = devices.scan();
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0], Change::Unavailable(..)));
        assert!(devices.scan().is_empty(), "the same failure is not repeated");
        assert!(!devices.has_keyboard() && !devices.has_pointer());
        assert!(devices.is_empty());
        let mut events = Vec::new();
        assert!(devices.read(&mut events).is_empty());
        assert!(events.is_empty());
    }
    #[test]
    fn changes_describe_themselves_for_notifications() {
        let added = Change::Added("/dev/input/event3".into(), "keyboard");
        assert_eq!(added.message(), "keyboard connected (/dev/input/event3)");
        assert_eq!(added.color(), crate::desktop::ACCENT);
        let removed = Change::Removed("/dev/input/event3".into(), "mouse");
        assert_eq!(removed.message(), "mouse disconnected (/dev/input/event3)");
        assert_ne!(removed.color(), added.color());
        assert!(Change::Overflow("/dev/input/event0".into())
            .message()
            .contains("resynchronized"));
    }
    #[test]
    fn real_devices_are_discovered_when_the_host_exposes_them() {
        let mut devices = Devices::default();
        let changes = devices.scan();
        assert_eq!(changes.len(), devices.len() + changes.iter().filter(|c| matches!(c, Change::Unavailable(..))).count());
        // A second scan reports nothing new, whatever the host has.
        assert!(devices.scan().is_empty());
        let mut events = Vec::new();
        devices.read(&mut events);
    }
}
