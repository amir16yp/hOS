//! Sound for HOSWM applications: play a file, or write PCM frame by frame.
//!
//! Audio does not go through the window server. An application opens a stream
//! to `hos-soundd`, which mixes every stream and writes the result to the
//! sound card, so several windows can play at once:
//!
//! ```no_run
//! use hoswm::audio::{Playback, play_file};
//! // A short sound, played by the service: an uncompressed WAV file.
//! play_file("/root/notify.wav")?;
//!
//! // Or generate samples: 16-bit interleaved, at whatever rate suits.
//! let mut sound = Playback::open(48000, 2, "hos-hello")?;
//! let mut frames = [0i16; 960];
//! for (index, frame) in frames.chunks_mut(2).enumerate() {
//!     let value = ((index as f32 * 0.1).sin() * 8000.0) as i16;
//!     frame[0] = value;
//!     frame[1] = value;
//! }
//! sound.write(&frames)?;
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! The volume, mute and default device belong to the system, not to one
//! window: [`volume`], [`set_volume`] and [`set_muted`] go through the same
//! service, and the settings application shows the result.
use crate::init::{ipc, mixer};
use std::{
    io::{self, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

/// An open playback stream.
///
/// Samples are interleaved and 16-bit; the service resamples to whatever the
/// card is running at. Dropping the stream ends it once the service has
/// played what it already has.
#[derive(Debug)]
pub struct Playback {
    socket: UnixStream,
    pub rate: u32,
    pub channels: u32,
}
impl Playback {
    /// Open a stream. `name` appears in `hosctl sound streams`.
    pub fn open(rate: u32, channels: u32, name: &str) -> io::Result<Playback> {
        let path = crate::init::run_dir().join(mixer::SOCKET);
        let mut socket = UnixStream::connect(&path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("{}: {e}; is hos-soundd running?", path.display()),
            )
        })?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        socket.write_all(&mixer::header(rate, channels, name))?;
        Ok(Playback {
            socket,
            rate,
            channels,
        })
    }
    /// Write interleaved samples. The call blocks while the service catches
    /// up, so an application can write in a loop at its own pace.
    pub fn write(&mut self, samples: &[i16]) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.socket.write_all(&bytes)
    }
    /// Write one buffer of already-encoded little-endian samples.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.socket.write_all(bytes)
    }
    /// How long `frames` of audio last, for pacing a generator.
    pub fn duration(&self, frames: usize) -> Duration {
        Duration::from_micros(frames as u64 * 1_000_000 / self.rate.max(1) as u64)
    }
}

fn service() -> io::Result<ipc::Client> {
    ipc::Client::connect("soundd").map_err(|e| {
        io::Error::new(e.kind(), format!("hos-soundd: {e}; is the sound service running?"))
    })
}

/// Ask the sound service to play a WAV file.
///
/// The file is read and decoded by the service, so this returns as soon as
/// playback starts rather than when the sound ends.
pub fn play_file(path: impl AsRef<Path>) -> io::Result<()> {
    let path = path.as_ref().display().to_string();
    service()?
        .call(&format!("PLAY {}", ipc::escape(&path)))
        .map(|_| ())
}

/// The system volume in percent, and whether output is muted.
pub fn volume() -> io::Result<(u32, bool)> {
    let reply = service()?.call("VOLUME")?;
    let record = reply.first();
    Ok((record.number("volume").unwrap_or(0), record.flag("muted")))
}
/// Set the system volume. `+5` and `-5` style changes use [`change_volume`].
pub fn set_volume(percent: u32) -> io::Result<u32> {
    let reply = service()?.call(&format!("VOLUME {}", percent.min(100)))?;
    Ok(reply.first().number("volume").unwrap_or(percent))
}
/// Change the volume by a number of percentage points.
pub fn change_volume(delta: i32) -> io::Result<u32> {
    let reply = service()?.call(&format!("VOLUME {delta:+}"))?;
    Ok(reply.first().number("volume").unwrap_or(0))
}
/// Mute or unmute the default device.
pub fn set_muted(muted: bool) -> io::Result<bool> {
    let reply = service()?.call(if muted { "MUTE ON" } else { "MUTE OFF" })?;
    Ok(reply.first().flag("muted"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stream_header_describes_the_format_the_service_expects() {
        let header = mixer::header(44100, 1, "hos-terminal");
        let (rate, channels, name) = mixer::parse_header(&header).unwrap();
        assert_eq!((rate, channels), (44100, 1));
        assert_eq!(&header[mixer::HEADER..], b"hos-terminal");
        assert_eq!(name, "hos-terminal".len());
    }
    #[test]
    fn opening_a_stream_without_the_service_says_so() {
        // SAFETY: the test only redirects its own view of the run directory.
        unsafe { std::env::set_var("HOS_RUN_DIR", "/nonexistent-hos-run") };
        let error = Playback::open(48000, 2, "test").unwrap_err().to_string();
        assert!(error.contains("hos-soundd"), "{error}");
        unsafe { std::env::remove_var("HOS_RUN_DIR") };
    }
}
