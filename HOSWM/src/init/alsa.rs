//! Minimal ALSA control UAPI: the `/dev/snd/controlC*` ioctls hOS needs.
//!
//! There is no alsa-lib in the image, so the mixer is driven through the
//! kernel's stable control interface directly. Only the four requests a mixer
//! needs are implemented: card information, the element list, element
//! information and reading and writing element values.
//!
//! The structure layouts come from `include/uapi/sound/asound.h`. Their sizes
//! are checked at compile time, because the ioctl request numbers encode them:
//! a wrong layout would be rejected by the kernel rather than misread.
use crate::init::sys;
use std::{
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

/// Where the kernel exposes sound devices.
pub const SND_DIR: &str = "/dev/snd";

/// Element interfaces; only the mixer is used here.
pub const IFACE_MIXER: i32 = 2;
/// Element value types.
pub const TYPE_BOOLEAN: i32 = 1;
pub const TYPE_INTEGER: i32 = 2;
pub const TYPE_ENUMERATED: i32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
struct ElemId {
    numid: u32,
    iface: i32,
    device: u32,
    subdevice: u32,
    name: [u8; 44],
    index: u32,
}
impl Default for ElemId {
    fn default() -> Self {
        ElemId {
            numid: 0,
            iface: IFACE_MIXER,
            device: 0,
            subdevice: 0,
            name: [0; 44],
            index: 0,
        }
    }
}
const _: () = assert!(size_of::<ElemId>() == 64);

#[repr(C)]
struct ElemList {
    offset: u32,
    space: u32,
    used: u32,
    count: u32,
    ids: *mut ElemId,
    reserved: [u8; 50],
}
const _: () = assert!(size_of::<ElemList>() == 80);

#[repr(C)]
struct ElemInfo {
    id: ElemId,
    kind: i32,
    access: u32,
    count: u32,
    owner: i32,
    value: [u8; 128],
    dimensions: [u16; 4],
    reserved: [u8; 56],
}
const _: () = assert!(size_of::<ElemInfo>() == 272);

#[repr(C)]
struct ElemValue {
    id: ElemId,
    indirect: u32,
    padding: u32,
    /// The integer union: 128 values, which is also the largest count ALSA
    /// allows for one element.
    values: [i64; 128],
    timestamp: [i64; 2],
    reserved: [u8; 112],
}
const _: () = assert!(size_of::<ElemValue>() == 1224);

#[repr(C)]
struct CardInfo {
    card: i32,
    padding: i32,
    id: [u8; 16],
    driver: [u8; 16],
    name: [u8; 32],
    longname: [u8; 80],
    reserved: [u8; 16],
    mixername: [u8; 80],
    components: [u8; 128],
}
const _: () = assert!(size_of::<CardInfo>() == 376);

/// Build an ioctl request number for the `'U'` (sound control) group.
const fn request<T>(read: bool, write: bool, number: u32) -> u64 {
    let direction = (read as u64) << 31 | (write as u64) << 30;
    direction | ((size_of::<T>() as u64) << 16) | (b'U' as u64) << 8 | number as u64
}

fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// One mixer element: a volume, a switch or a selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub numid: u32,
    pub name: String,
    pub index: u32,
    /// [`TYPE_BOOLEAN`], [`TYPE_INTEGER`] or [`TYPE_ENUMERATED`].
    pub kind: i32,
    /// Number of values, normally one per channel.
    pub count: u32,
    pub min: i64,
    pub max: i64,
}
impl Element {
    pub fn is_volume(&self) -> bool {
        self.kind == TYPE_INTEGER && self.name.contains("Volume")
    }
    pub fn is_switch(&self) -> bool {
        self.kind == TYPE_BOOLEAN && self.name.contains("Switch")
    }
}

/// An open control device for one card.
pub struct Control {
    file: File,
    pub card: u32,
}
impl Control {
    pub fn open(card: u32) -> io::Result<Control> {
        let path = PathBuf::from(SND_DIR).join(format!("controlC{card}"));
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Control { file, card })
    }
    fn call<T>(&self, request: u64, data: &mut T) -> io::Result<()> {
        // SAFETY: each caller passes the structure the request encodes.
        if unsafe { sys::ioctl(self.file.as_raw_fd(), request, data as *mut T) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// The card's short id and human-readable name.
    pub fn name(&self) -> io::Result<(String, String)> {
        let mut info: CardInfo = unsafe { std::mem::zeroed() };
        // SNDRV_CTL_IOCTL_CARD_INFO
        self.call(request::<CardInfo>(true, false, 0x01), &mut info)?;
        Ok((text(&info.id), text(&info.name)))
    }
    /// Every mixer element on the card.
    pub fn elements(&self) -> io::Result<Vec<Element>> {
        let mut list = ElemList {
            offset: 0,
            space: 0,
            used: 0,
            count: 0,
            ids: std::ptr::null_mut(),
            reserved: [0; 50],
        };
        // SNDRV_CTL_IOCTL_ELEM_LIST, first to learn how many there are.
        let number = request::<ElemList>(true, true, 0x10);
        self.call(number, &mut list)?;
        let count = list.count.min(1024) as usize;
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut ids = vec![ElemId::default(); count];
        list.offset = 0;
        list.space = count as u32;
        list.ids = ids.as_mut_ptr();
        self.call(number, &mut list)?;
        let mut elements = Vec::new();
        for id in ids.iter().take(list.used as usize) {
            let mut info: ElemInfo = unsafe { std::mem::zeroed() };
            info.id = *id;
            // SNDRV_CTL_IOCTL_ELEM_INFO
            if self
                .call(request::<ElemInfo>(true, true, 0x11), &mut info)
                .is_err()
            {
                continue;
            }
            let (min, max) = if info.kind == TYPE_INTEGER {
                (
                    i64::from_ne_bytes(info.value[..8].try_into().unwrap()),
                    i64::from_ne_bytes(info.value[8..16].try_into().unwrap()),
                )
            } else if info.kind == TYPE_ENUMERATED {
                (
                    0,
                    u32::from_ne_bytes(info.value[..4].try_into().unwrap()) as i64 - 1,
                )
            } else {
                (0, 1)
            };
            elements.push(Element {
                numid: info.id.numid,
                name: text(&info.id.name),
                index: info.id.index,
                kind: info.kind,
                count: info.count.min(128),
                min,
                max,
            });
        }
        Ok(elements)
    }
    /// Read one element's values.
    pub fn read(&self, element: &Element) -> io::Result<Vec<i64>> {
        let mut value: ElemValue = unsafe { std::mem::zeroed() };
        value.id.numid = element.numid;
        // SNDRV_CTL_IOCTL_ELEM_READ
        self.call(request::<ElemValue>(true, true, 0x12), &mut value)?;
        Ok(value.values[..element.count as usize].to_vec())
    }
    /// Write the same value to every channel of one element.
    pub fn write(&self, element: &Element, wanted: i64) -> io::Result<()> {
        let mut value: ElemValue = unsafe { std::mem::zeroed() };
        value.id.numid = element.numid;
        let wanted = wanted.clamp(element.min, element.max);
        for channel in 0..element.count as usize {
            value.values[channel] = wanted;
        }
        // SNDRV_CTL_IOCTL_ELEM_WRITE
        self.call(request::<ElemValue>(true, true, 0x13), &mut value)
    }
}

/// The cards the kernel currently exposes, by index.
pub fn cards() -> Vec<u32> {
    card_indices(Path::new(SND_DIR))
}
fn card_indices(dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut cards: Vec<u32> = entries
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            name.strip_prefix("controlC")?.parse().ok()
        })
        .collect();
    cards.sort_unstable();
    cards
}

/// Whether a card has a playback device, which is what a default needs.
pub fn has_playback(card: u32) -> bool {
    let Ok(entries) = std::fs::read_dir(SND_DIR) else {
        return false;
    };
    entries.filter_map(|entry| entry.ok()).any(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        name.starts_with(&format!("pcmC{card}D")) && name.ends_with('p')
    })
}

/// Convert a raw mixer value to a percentage of its range.
pub fn to_percent(value: i64, min: i64, max: i64) -> u32 {
    if max <= min {
        return 0;
    }
    let percent = (value - min) as f64 * 100.0 / (max - min) as f64;
    percent.round().clamp(0.0, 100.0) as u32
}
/// Convert a percentage back to a raw mixer value.
pub fn from_percent(percent: u32, min: i64, max: i64) -> i64 {
    if max <= min {
        return min;
    }
    let percent = percent.min(100) as f64 / 100.0;
    min + (percent * (max - min) as f64).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ioctl_numbers_match_the_kernel_definitions() {
        // The values in include/uapi/sound/asound.h for a 64-bit kernel.
        assert_eq!(request::<CardInfo>(true, false, 0x01), 0x8178_5501);
        assert_eq!(request::<ElemList>(true, true, 0x10), 0xc050_5510);
        assert_eq!(request::<ElemInfo>(true, true, 0x11), 0xc110_5511);
        assert_eq!(request::<ElemValue>(true, true, 0x12), 0xc4c8_5512);
        assert_eq!(request::<ElemValue>(true, true, 0x13), 0xc4c8_5513);
    }
    #[test]
    fn percentages_convert_in_both_directions() {
        assert_eq!(to_percent(0, 0, 87), 0);
        assert_eq!(to_percent(87, 0, 87), 100);
        assert_eq!(to_percent(44, 0, 87), 51);
        assert_eq!(from_percent(0, 0, 87), 0);
        assert_eq!(from_percent(100, 0, 87), 87);
        assert_eq!(from_percent(50, 0, 87), 44);
        // Ranges that start below zero, as many codecs report.
        assert_eq!(to_percent(-10240, -10240, 400), 0);
        assert_eq!(from_percent(100, -10240, 400), 400);
        // A degenerate range never divides by zero.
        assert_eq!(to_percent(5, 3, 3), 0);
        assert_eq!(from_percent(50, 3, 3), 3);
        assert_eq!(from_percent(200, 0, 87), 87, "percentages are clamped");
    }
    #[test]
    fn element_names_classify_volumes_and_switches() {
        let volume = Element {
            numid: 1,
            name: "Master Playback Volume".into(),
            index: 0,
            kind: TYPE_INTEGER,
            count: 2,
            min: 0,
            max: 87,
        };
        let switch = Element {
            name: "Master Playback Switch".into(),
            kind: TYPE_BOOLEAN,
            max: 1,
            ..volume.clone()
        };
        assert!(volume.is_volume() && !volume.is_switch());
        assert!(switch.is_switch() && !switch.is_volume());
    }
    #[test]
    fn card_indices_are_read_from_the_device_directory() {
        let dir = std::env::temp_dir().join(format!("hos-alsa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["controlC0", "controlC1", "pcmC0D0p", "seq", "timer"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(card_indices(&dir), [0, 1]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
