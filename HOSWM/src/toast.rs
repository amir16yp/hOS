//! Transient on-screen notifications and the notification log they leave behind.
//!
//! Every toast that finishes is appended to a small append-only binary log
//! (`~/.hoswm/toastdb` by default), recording when it appeared, how long it
//! actually stayed on screen, its color and its text. `hos-notifications`
//! reads the same file.
//!
//! # File format
//!
//! All integers are little-endian. A 16-byte header is followed by records:
//!
//! ```text
//! header: "HOSTOAST" magic (8) | u16 version | u16 header length | u32 reserved
//! record: u32 length (whole record, including this field)
//!         u64 unix milliseconds when the toast appeared
//!         u32 requested milliseconds
//!         u32 milliseconds actually shown
//!         u32 color (0xAARRGGBB)
//!         u32 text length in bytes
//!         UTF-8 text, zero-padded to a 4-byte boundary
//!         u32 CRC-32 of the record between the length and this field
//! ```
//!
//! Records are self-delimiting and checksummed, so a log truncated by a power
//! loss is read up to the last intact record instead of being discarded.
use crate::{desktop::Rect, font::Font, surface::Surface};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAGIC: [u8; 8] = *b"HOSTOAST";
pub const VERSION: u16 = 1;
pub const HEADER: usize = 16;
/// Shortest and longest display time a caller can ask for.
pub const MIN_MS: u32 = 500;
pub const MAX_MS: u32 = 60_000;
pub const MAX_TEXT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Corner {
    TopLeft,
    #[default]
    TopRight,
    BottomLeft,
    BottomRight,
}
impl Corner {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().replace([' ', '_'], "-").as_str() {
            "top-left" => Some(Self::TopLeft),
            "top-right" => Some(Self::TopRight),
            "bottom-left" => Some(Self::BottomLeft),
            "bottom-right" => Some(Self::BottomRight),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }
    fn top(&self) -> bool {
        matches!(self, Self::TopLeft | Self::TopRight)
    }
    fn left(&self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }
}

/// One finished notification, as stored in the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub timestamp_ms: u64,
    pub requested_ms: u32,
    pub shown_ms: u32,
    pub color: u32,
    pub text: String,
}
impl Record {
    pub fn encode(&self) -> Vec<u8> {
        let text = self.text.as_bytes();
        let padding = (4 - text.len() % 4) % 4;
        let mut body = Vec::with_capacity(28 + text.len() + padding);
        body.extend(self.timestamp_ms.to_le_bytes());
        body.extend(self.requested_ms.to_le_bytes());
        body.extend(self.shown_ms.to_le_bytes());
        body.extend(self.color.to_le_bytes());
        body.extend((text.len() as u32).to_le_bytes());
        body.extend(text);
        body.extend(std::iter::repeat_n(0u8, padding));
        let mut out = Vec::with_capacity(body.len() + 8);
        out.extend(((body.len() + 8) as u32).to_le_bytes());
        out.extend(&body);
        out.extend(crc32(&body).to_le_bytes());
        out
    }
    /// Decode one record, returning it with the number of bytes consumed.
    pub fn decode(bytes: &[u8]) -> Result<(Self, usize), String> {
        if bytes.len() < 4 {
            return Err("truncated record length".into());
        }
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        if length < 32 || length % 4 != 0 {
            return Err("invalid record length".into());
        }
        if bytes.len() < length {
            return Err("truncated record".into());
        }
        let body = &bytes[4..length - 4];
        let stored = u32::from_le_bytes(bytes[length - 4..length].try_into().unwrap());
        if crc32(body) != stored {
            return Err("record checksum mismatch".into());
        }
        let word = |i: usize| u32::from_le_bytes(body[i..i + 4].try_into().unwrap());
        let text_len = word(20) as usize;
        if text_len > body.len() - 24 || body.len() - 24 - text_len >= 4 {
            return Err("invalid record text length".into());
        }
        Ok((
            Self {
                timestamp_ms: u64::from_le_bytes(body[..8].try_into().unwrap()),
                requested_ms: word(8),
                shown_ms: word(12),
                color: word(16),
                text: String::from_utf8_lossy(&body[24..24 + text_len]).into_owned(),
            },
            length,
        ))
    }
}

/// CRC-32 (IEEE 802.3, reflected), computed without a lookup table.
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

pub fn header() -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER);
    out.extend(MAGIC);
    out.extend(VERSION.to_le_bytes());
    out.extend((HEADER as u16).to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out
}

/// Append one record, creating the log and its directory when missing.
pub fn append(path: &Path, record: &Record) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    let mut bytes = if file.metadata()?.len() == 0 {
        header()
    } else {
        Vec::new()
    };
    bytes.extend(record.encode());
    file.write_all(&bytes)
}

/// Read every intact record. A damaged tail stops parsing and is reported.
pub fn read(path: &Path) -> io::Result<(Vec<Record>, Option<String>)> {
    let bytes = fs::read(path)?;
    if bytes.len() < HEADER || bytes[..8] != MAGIC {
        return Err(io::Error::other("not a HOSWM toast log"));
    }
    let version = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let start = u16::from_le_bytes(bytes[10..12].try_into().unwrap()) as usize;
    if version != VERSION || start < HEADER || start > bytes.len() {
        return Err(io::Error::other(format!(
            "unsupported toast log version {version}"
        )));
    }
    let mut records = Vec::new();
    let mut rest = &bytes[start..];
    while !rest.is_empty() {
        match Record::decode(rest) {
            Ok((record, used)) => {
                records.push(record);
                rest = &rest[used..];
            }
            Err(e) => {
                return Ok((
                    records,
                    Some(format!("{e}; {} trailing bytes ignored", rest.len())),
                ));
            }
        }
    }
    Ok((records, None))
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Format unix milliseconds as `YYYY-MM-DD HH:MM:SS` in UTC.
pub fn format_time(ms: u64) -> String {
    let seconds = (ms / 1000) as i64;
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        time / 3600,
        time / 60 % 60,
        time % 60
    )
}
// Howard Hinnant's civil_from_days, for a proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (
        year_of_era as i64 + era * 400 + i64::from(month <= 2),
        month,
        day,
    )
}

struct Live {
    text: String,
    color: u32,
    requested_ms: u32,
    timestamp_ms: u64,
    shown_at: Instant,
}

/// The live notification stack drawn over the desktop.
pub struct Toasts {
    live: Vec<Live>,
    pub corner: Corner,
    pub default_ms: u32,
    pub max: usize,
    pub db: Option<PathBuf>,
    /// Last logging failure, reported by the caller and then cleared.
    pub error: Option<String>,
}
impl Default for Toasts {
    fn default() -> Self {
        Self {
            live: Vec::new(),
            corner: Corner::default(),
            default_ms: 4000,
            max: 4,
            db: None,
            error: None,
        }
    }
}
impl Toasts {
    pub fn new(corner: Corner, default_ms: u32, max: usize, db: Option<PathBuf>) -> Self {
        Self {
            live: Vec::new(),
            corner,
            default_ms,
            max: max.max(1),
            db,
            error: None,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
    pub fn len(&self) -> usize {
        self.live.len()
    }
    /// Show a notification. `ms` of zero uses the configured default.
    pub fn push(&mut self, text: impl Into<String>, color: u32, ms: u32) {
        let mut text: String = text.into();
        if text.len() > MAX_TEXT {
            let end = (0..=MAX_TEXT).rev().find(|n| text.is_char_boundary(*n));
            text.truncate(end.unwrap_or(0));
        }
        while self.live.len() >= self.max {
            self.retire(0);
        }
        self.live.push(Live {
            text,
            color: color | 0xff00_0000,
            requested_ms: if ms == 0 { self.default_ms } else { ms }.clamp(MIN_MS, MAX_MS),
            timestamp_ms: now_ms(),
            shown_at: Instant::now(),
        });
    }
    /// Retire expired notifications. Returns true when the screen changed.
    pub fn tick(&mut self) -> bool {
        let expired: Vec<usize> = self
            .live
            .iter()
            .enumerate()
            .filter(|(_, l)| l.shown_at.elapsed() >= Duration::from_millis(l.requested_ms as u64))
            .map(|(i, _)| i)
            .collect();
        for index in expired.iter().rev() {
            self.retire(*index);
        }
        !expired.is_empty()
    }
    /// Time until the next notification expires, for the frame scheduler.
    pub fn next_deadline(&self) -> Option<Duration> {
        self.live
            .iter()
            .map(|l| {
                Duration::from_millis(l.requested_ms as u64).saturating_sub(l.shown_at.elapsed())
            })
            .min()
    }
    /// Dismiss the notification under a point, logging the time it was shown.
    pub fn click(&mut self, area: Rect, x: i32, y: i32) -> bool {
        let Some(index) = self
            .rects(area)
            .into_iter()
            .position(|r| r.contains(x, y))
        else {
            return false;
        };
        self.retire(index);
        true
    }
    pub fn clear(&mut self) -> bool {
        let any = !self.live.is_empty();
        while !self.live.is_empty() {
            self.retire(0);
        }
        any
    }
    fn retire(&mut self, index: usize) {
        let live = self.live.remove(index);
        let record = Record {
            timestamp_ms: live.timestamp_ms,
            requested_ms: live.requested_ms,
            shown_ms: live.shown_at.elapsed().as_millis().min(u32::MAX as u128) as u32,
            color: live.color,
            text: live.text,
        };
        if let Some(path) = &self.db {
            if let Err(e) = append(path, &record) {
                self.error = Some(format!("{}: {e}", path.display()));
            }
        }
    }
    fn lines(text: &str, columns: usize) -> Vec<String> {
        let mut lines = Vec::new();
        for paragraph in text.split('\n') {
            let mut line = String::new();
            for word in paragraph.split(' ') {
                for part in wrap_word(word, columns) {
                    let width = line.chars().count();
                    if width > 0 && width + 1 + part.chars().count() > columns {
                        lines.push(std::mem::take(&mut line));
                    } else if width > 0 {
                        line.push(' ');
                    }
                    line.push_str(&part);
                }
            }
            lines.push(line);
        }
        lines.truncate(3);
        lines
    }
    fn boxes(&self, area: Rect) -> Vec<(Rect, Vec<String>)> {
        let columns = ((area.w - 34) / 8).clamp(8, 40) as usize;
        let mut out: Vec<(Rect, Vec<String>)> = Vec::with_capacity(self.live.len());
        let mut offset = 0;
        // The newest notification sits closest to the configured corner.
        for live in self.live.iter().rev() {
            let lines = Self::lines(&live.text, columns);
            let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
            let w = (widest as i32 * 8 + 34).clamp(160, area.w);
            let h = lines.len() as i32 * 13 + 17;
            let x = if self.corner.left() {
                area.x
            } else {
                area.x + area.w - w
            };
            let y = if self.corner.top() {
                area.y + offset
            } else {
                area.y + area.h - offset - h
            };
            offset += h + 8;
            out.push((Rect { x, y, w, h }, lines));
        }
        out
    }
    fn rects(&self, area: Rect) -> Vec<Rect> {
        // `boxes` is newest first; report oldest first to match `self.live`.
        let mut rects: Vec<Rect> = self.boxes(area).into_iter().map(|(r, _)| r).collect();
        rects.reverse();
        rects
    }
    pub fn draw(&self, fb: &mut Surface, font: &Font<'_>, area: Rect) {
        for ((rect, lines), live) in self.boxes(area).into_iter().zip(self.live.iter().rev()) {
            fb.fill_rounded_rect(rect.x, rect.y, rect.w, rect.h, 6, 0xf01e2823);
            fb.fill_rect(rect.x + 3, rect.y + 5, 4, rect.h - 10, live.color);
            for (index, line) in lines.iter().enumerate() {
                font.draw(
                    fb,
                    rect.x + 16,
                    rect.y + 9 + index as i32 * 13,
                    line,
                    0xffe8ece9,
                );
            }
        }
    }
}
impl Drop for Toasts {
    /// Log notifications that were still on screen when the session ended.
    fn drop(&mut self) {
        while !self.live.is_empty() {
            self.retire(0);
        }
    }
}
fn wrap_word(word: &str, columns: usize) -> Vec<String> {
    if word.chars().count() <= columns {
        return vec![word.to_string()];
    }
    let mut parts = Vec::new();
    let mut part = String::new();
    for c in word.chars() {
        if part.chars().count() == columns {
            parts.push(std::mem::take(&mut part));
        }
        part.push(c);
    }
    parts.push(part);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hoswm-{name}-{}", std::process::id()))
    }
    #[test]
    fn records_round_trip_through_the_log() {
        let path = temp("toastdb").join("toastdb");
        let _ = fs::remove_dir_all(path.parent().unwrap());
        let records = [
            Record {
                timestamp_ms: 1_700_000_000_123,
                requested_ms: 4000,
                shown_ms: 4012,
                color: 0xff72dbac,
                text: "Keyboard connected".into(),
            },
            Record {
                timestamp_ms: 1_700_000_005_000,
                requested_ms: 2500,
                shown_ms: 900,
                color: 0xffef6976,
                text: String::new(),
            },
            Record {
                timestamp_ms: 1_700_000_009_000,
                requested_ms: 1000,
                shown_ms: 1000,
                color: 0xff80afff,
                text: "unicode: ÅÄÖ – one two".into(),
            },
        ];
        for record in &records {
            append(&path, record).unwrap();
        }
        let (read_back, damage) = read(&path).unwrap();
        assert_eq!(read_back, records);
        assert_eq!(damage, None);
        assert_eq!(fs::read(&path).unwrap()[..8], MAGIC);
        // A torn tail keeps every intact record and reports the damage.
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 6);
        fs::write(&path, &bytes).unwrap();
        let (partial, damage) = read(&path).unwrap();
        assert_eq!(partial, records[..2]);
        assert!(damage.is_some());
        // A flipped bit inside a record is caught by its checksum.
        let mut bytes = fs::read(&path).unwrap();
        bytes[HEADER + 20] ^= 0x40;
        fs::write(&path, &bytes).unwrap();
        assert!(read(&path).unwrap().1.unwrap().contains("checksum"));
        fs::write(&path, b"not a log").unwrap();
        assert!(read(&path).is_err());
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn expiry_and_dismissal_log_the_time_actually_shown() {
        let path = temp("toast-live").join("toastdb");
        let _ = fs::remove_dir_all(path.parent().unwrap());
        let area = Rect {
            x: 12,
            y: 34,
            w: 776,
            h: 484,
        };
        let mut toasts = Toasts::new(Corner::BottomLeft, 4000, 2, Some(path.clone()));
        toasts.push("first", 0x72dbac, 0);
        toasts.push("second", 0xef6976, MIN_MS);
        assert_eq!(toasts.len(), 2);
        // A third notification retires the oldest immediately.
        toasts.push("third", 0x80afff, 30);
        assert_eq!(toasts.len(), 2);
        let (records, _) = read(&path).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].text, "first");
        assert_eq!(records[0].requested_ms, 4000, "zero means the default");
        // The clamped 30 ms request has already elapsed by the next tick.
        std::thread::sleep(Duration::from_millis(5));
        assert!(!toasts.tick(), "nothing has reached 500 ms yet");
        assert!(toasts.next_deadline().unwrap() <= Duration::from_millis(MIN_MS as u64));
        let rects = toasts.rects(area);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].x, area.x, "bottom-left hugs the left edge");
        assert!(rects[1].y > rects[0].y, "the newest sits closest to the bottom corner");
        assert!(!toasts.click(area, 0, 0));
        assert!(toasts.click(area, rects[1].x + 4, rects[1].y + 4));
        assert_eq!(toasts.len(), 1);
        let (records, _) = read(&path).unwrap();
        assert_eq!(records[1].text, "third");
        assert!(records[1].shown_ms < 500, "dismissed before expiry");
        drop(toasts);
        let (records, _) = read(&path).unwrap();
        assert_eq!(records.len(), 3, "the session flushes live notifications");
        assert_eq!(records[2].text, "second");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn long_text_is_truncated_wrapped_and_drawn_inside_its_corner() {
        let area = Rect {
            x: 12,
            y: 34,
            w: 776,
            h: 484,
        };
        let mut toasts = Toasts::new(Corner::TopRight, 4000, 4, None);
        toasts.push("x".repeat(MAX_TEXT + 40), 0xffffff, 1000);
        toasts.push("a short one", 0x72dbac, 1000);
        let mut fb = Surface::new(800, 600);
        toasts.draw(&mut fb, &Font::builtin(), area);
        let rects = toasts.rects(area);
        assert_eq!(rects[0].x + rects[0].w, area.x + area.w);
        assert!(rects.iter().all(|r| r.y >= area.y && r.h <= area.h));
        assert!(fb.pixels().iter().any(|p| *p != 0));
        assert_eq!(Toasts::lines(&"x".repeat(200), 40).len(), 3);
        assert_eq!(
            Toasts::lines("one two three", 7),
            ["one two", "three"],
            "words wrap on spaces"
        );
    }
    #[test]
    fn corners_and_timestamps_parse_and_format() {
        assert_eq!(Corner::parse(" Bottom_Left "), Some(Corner::BottomLeft));
        assert_eq!(Corner::parse("middle"), None);
        assert_eq!(Corner::parse(Corner::TopLeft.name()), Some(Corner::TopLeft));
        assert_eq!(format_time(0), "1970-01-01 00:00:00");
        assert_eq!(format_time(1_700_000_000_123), "2023-11-14 22:13:20");
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926, "CRC-32 check value");
    }
}
