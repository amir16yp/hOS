//! Minimal ALSA PCM playback: the `/dev/snd/pcmC*D*p` ioctls.
//!
//! There is no alsa-lib in the image, so `dmix` is only available to programs
//! that link against it. hOS applications instead send audio to `hos-soundd`,
//! which mixes them in [`crate::init::mixer`] and writes the result to one
//! device through this module.
//!
//! Only interleaved 16-bit little-endian playback is implemented, which is
//! what every hOS application uses and what every card supports.
use crate::init::sys;
use std::{
    fs::OpenOptions,
    io,
    os::fd::{AsRawFd, RawFd},
    time::Duration,
};

/// Interleaved read/write access.
const ACCESS_RW_INTERLEAVED: usize = 3;
/// Signed 16-bit little-endian samples.
const FORMAT_S16_LE: usize = 2;
const SUBFORMAT_STD: usize = 0;

/// Interval parameters, as offsets into the interval array.
const CHANNELS: usize = 10 - 8;
const RATE: usize = 11 - 8;
const PERIOD_SIZE: usize = 13 - 8;
const PERIODS: usize = 15 - 8;
const BUFFER_SIZE: usize = 17 - 8;

#[repr(C)]
#[derive(Clone, Copy)]
struct Mask {
    bits: [u32; 8],
}
#[repr(C)]
#[derive(Clone, Copy)]
struct Interval {
    min: u32,
    max: u32,
    /// `openmin`, `openmax`, `integer` and `empty`, one bit each.
    flags: u32,
}
#[repr(C)]
struct HwParams {
    flags: u32,
    masks: [Mask; 3],
    reserved_masks: [Mask; 5],
    intervals: [Interval; 12],
    reserved_intervals: [Interval; 9],
    rmask: u32,
    cmask: u32,
    info: u32,
    msbits: u32,
    rate_num: u32,
    rate_den: u32,
    fifo_size: u64,
    reserved: [u8; 64],
}
const _: () = assert!(size_of::<HwParams>() == 608);

#[repr(C)]
struct SwParams {
    tstamp_mode: i32,
    period_step: u32,
    sleep_min: u32,
    padding: u32,
    avail_min: u64,
    xfer_align: u64,
    start_threshold: u64,
    stop_threshold: u64,
    silence_threshold: u64,
    silence_size: u64,
    boundary: u64,
    proto: u32,
    tstamp_type: u32,
    reserved: [u8; 56],
}
const _: () = assert!(size_of::<SwParams>() == 136);

#[repr(C)]
struct Transfer {
    result: i64,
    buffer: *const u8,
    frames: u64,
}
const _: () = assert!(size_of::<Transfer>() == 24);

const HW_PARAMS: u64 = 0xc260_4111;
const SW_PARAMS: u64 = 0xc088_4113;
const PREPARE: u64 = 0x0000_4140;
const DROP: u64 = 0x0000_4143;
const DRAIN: u64 = 0x0000_4144;
const WRITEI: u64 = 0x4018_4150;

impl HwParams {
    /// Start with every parameter open, as alsa-lib and tinyalsa do.
    fn any() -> HwParams {
        // SAFETY: the structure is plain data with no invalid bit patterns.
        let mut params: HwParams = unsafe { std::mem::zeroed() };
        for mask in &mut params.masks {
            mask.bits = [u32::MAX; 8];
        }
        for interval in &mut params.intervals {
            interval.min = 0;
            interval.max = u32::MAX;
        }
        params.rmask = u32::MAX;
        params
    }
    fn set_mask(&mut self, index: usize, bit: usize) {
        self.masks[index].bits = [0; 8];
        self.masks[index].bits[bit / 32] = 1 << (bit % 32);
    }
    fn set_interval(&mut self, index: usize, value: u32) {
        self.intervals[index] = Interval {
            min: value,
            max: value,
            flags: 0,
        };
    }
    fn interval(&self, index: usize) -> u32 {
        self.intervals[index].min
    }
}

/// One open playback device.
pub struct Playback {
    file: std::fs::File,
    pub card: u32,
    pub device: u32,
    pub rate: u32,
    pub channels: u32,
    /// Frames in one period: the unit the mixer writes.
    pub period: u32,
    pub buffer: u32,
    prepared: bool,
}
impl Playback {
    /// Open a card's playback device and configure it.
    ///
    /// The requested rate is a preference: the driver may choose another,
    /// which is reported in [`Playback::rate`].
    pub fn open(card: u32, device: u32, rate: u32, channels: u32) -> io::Result<Playback> {
        let path = format!("{}/pcmC{card}D{device}p", crate::init::alsa::SND_DIR);
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let mut playback = Playback {
            file,
            card,
            device,
            rate,
            channels,
            period: 1024,
            buffer: 4096,
            prepared: false,
        };
        playback.configure(rate, channels)?;
        Ok(playback)
    }
    fn call<T>(&self, request: u64, data: *mut T) -> io::Result<()> {
        // SAFETY: each caller passes the structure the request encodes.
        if unsafe { sys::ioctl(self.file.as_raw_fd(), request, data) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn configure(&mut self, rate: u32, channels: u32) -> io::Result<()> {
        let mut params = HwParams::any();
        params.set_mask(0, ACCESS_RW_INTERLEAVED);
        params.set_mask(1, FORMAT_S16_LE);
        params.set_mask(2, SUBFORMAT_STD);
        params.set_interval(CHANNELS, channels);
        params.set_interval(RATE, rate);
        params.set_interval(PERIOD_SIZE, self.period);
        params.set_interval(PERIODS, 4);
        self.call(HW_PARAMS, &mut params)?;
        self.rate = params.interval(RATE).max(1);
        self.channels = params.interval(CHANNELS).max(1);
        self.period = params.interval(PERIOD_SIZE).max(1);
        self.buffer = params.interval(BUFFER_SIZE).max(self.period);

        // SAFETY: plain data; every field is set below or left at zero.
        let mut software: SwParams = unsafe { std::mem::zeroed() };
        software.avail_min = self.period as u64;
        // Start as soon as one period is queued, and stop on an underrun so
        // the next write reports it instead of playing stale audio.
        software.start_threshold = self.period as u64;
        software.stop_threshold = self.buffer as u64;
        let mut boundary = self.buffer as u64;
        while boundary * 2 < (i64::MAX as u64) / 2 {
            boundary *= 2;
        }
        software.boundary = boundary;
        self.call(SW_PARAMS, &mut software)?;
        self.prepare()
    }
    pub fn prepare(&mut self) -> io::Result<()> {
        self.call(PREPARE, std::ptr::null_mut::<u8>())?;
        self.prepared = true;
        Ok(())
    }
    /// How long one period of audio lasts.
    pub fn period_duration(&self) -> Duration {
        Duration::from_micros(self.period as u64 * 1_000_000 / self.rate.max(1) as u64)
    }
    /// Write interleaved samples, recovering once from an underrun.
    ///
    /// The call blocks until the card has room, which is what paces the
    /// mixer: one period is written per period of audio played.
    pub fn write(&mut self, samples: &[i16]) -> io::Result<u64> {
        if self.channels == 0 || samples.is_empty() {
            return Ok(0);
        }
        let frames = samples.len() as u64 / self.channels as u64;
        if frames == 0 {
            return Ok(0);
        }
        for attempt in 0..2 {
            if !self.prepared {
                self.prepare()?;
            }
            let mut transfer = Transfer {
                result: 0,
                buffer: samples.as_ptr() as *const u8,
                frames,
            };
            match self.call(WRITEI, &mut transfer) {
                Ok(()) => return Ok(transfer.result.max(0) as u64),
                Err(e) => {
                    // EPIPE is an underrun: the device stopped because the
                    // mixer was late. Prepare it and write the period again.
                    let recoverable = matches!(e.raw_os_error(), Some(32) | Some(77) | Some(86));
                    if !recoverable || attempt == 1 {
                        return Err(e);
                    }
                    self.prepared = false;
                }
            }
        }
        Ok(0)
    }
    /// Stop playback and discard whatever the card has not played yet.
    pub fn drop_audio(&mut self) {
        let _ = self.call(DROP, std::ptr::null_mut::<u8>());
        self.prepared = false;
    }
    /// Let the card finish what is already queued.
    pub fn drain(&mut self) {
        let _ = self.call(DRAIN, std::ptr::null_mut::<u8>());
        self.prepared = false;
    }
    pub fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}
impl Drop for Playback {
    fn drop(&mut self) {
        self.drop_audio();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ioctl_numbers_match_the_kernel_definitions() {
        // The values in include/uapi/sound/asound.h for a 64-bit kernel.
        assert_eq!(
            0xc000_0000 | ((size_of::<HwParams>() as u64) << 16) | 0x4111,
            HW_PARAMS
        );
        assert_eq!(
            0xc000_0000 | ((size_of::<SwParams>() as u64) << 16) | 0x4113,
            SW_PARAMS
        );
        assert_eq!(
            0x4000_0000 | ((size_of::<Transfer>() as u64) << 16) | 0x4150,
            WRITEI
        );
        assert_eq!(PREPARE, 0x4140);
        assert_eq!(DROP, 0x4143);
        assert_eq!(DRAIN, 0x4144);
    }
    #[test]
    fn open_parameters_become_exact_ones() {
        let mut params = HwParams::any();
        assert_eq!(params.masks[0].bits[0], u32::MAX);
        assert_eq!(params.intervals[RATE].max, u32::MAX);
        params.set_mask(1, FORMAT_S16_LE);
        params.set_interval(RATE, 48000);
        params.set_interval(CHANNELS, 2);
        assert_eq!(params.masks[1].bits[0], 1 << FORMAT_S16_LE);
        assert_eq!(params.masks[1].bits[1], 0);
        assert_eq!(params.interval(RATE), 48000);
        assert_eq!(params.intervals[RATE].max, 48000);
        assert_eq!(params.interval(CHANNELS), 2);
        assert_eq!(params.rmask, u32::MAX, "every parameter is refined");
    }
}
