//! The software mixer behind `hos-soundd`, so several windows can play at once.
//!
//! An application connects to `/run/hos/sound.pcm`, sends a small header and
//! then writes interleaved 16-bit samples. A mixing thread reads every open
//! stream, resamples it to the card's rate, sums the streams and writes one
//! period at a time through [`crate::init::pcm`]. Writing to the card blocks
//! until it has room, which is what paces the whole loop.
//!
//! Applications use this through [`crate::audio`] in Rust or `hoswm_sound_*`
//! in C; nothing needs alsa-lib.
use crate::init::{log, pcm::Playback, run_dir};
use std::{
    fs,
    io::{self, Read},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, TryRecvError, channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The socket applications write audio to.
pub const SOCKET: &str = "sound.pcm";
/// Header magic; the version follows it.
pub const MAGIC: [u8; 8] = *b"HOSPCM\0\0";
pub const VERSION: u32 = 1;
/// Interleaved signed 16-bit little-endian, the only format accepted.
pub const FORMAT_S16_LE: u32 = 0;
/// Fixed part of the header, before the stream name.
pub const HEADER: usize = 32;
/// Longest stream name an application may send.
pub const MAX_NAME: usize = 64;
/// About a second of stereo audio at 48 kHz, per stream.
const MAX_BUFFERED: usize = 48_000 * 2 * 2;
/// How long the card stays open after the last stream ended.
const IDLE: Duration = Duration::from_secs(3);
/// Largest sound file the service will load for `PLAY`.
pub const MAX_FILE: u64 = 16 * 1024 * 1024;

/// One playing stream, as reported by `hos-soundd`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamInfo {
    pub id: u32,
    pub name: String,
    pub pid: i32,
    pub rate: u32,
    pub channels: u32,
    /// Frames of this stream already mixed.
    pub frames: u64,
    /// `app` for a socket stream, `file` for one this service is playing.
    pub kind: &'static str,
}

/// What the mixing thread and the service share.
#[derive(Debug, Default)]
pub struct Shared {
    pub streams: Vec<StreamInfo>,
    /// Volume applied in software, used when the card has no volume control.
    pub volume: u32,
    pub muted: bool,
    pub software_volume: bool,
    /// The most recent device error, shown in `STATUS`.
    pub error: Option<String>,
    pub playing: bool,
}

/// Messages the service sends the mixing thread.
enum Command {
    /// Play decoded samples, such as a notification sound.
    Play {
        name: String,
        samples: Vec<i16>,
        rate: u32,
        channels: u32,
    },
    /// Use another card, after the default device changed.
    Card(u32),
    /// Stop every stream at once.
    StopAll,
}

/// Where one stream's samples come from.
enum Source {
    /// An application, with the bytes read from its socket so far.
    Socket(UnixStream, Vec<u8>, bool),
    /// A sound this service decoded itself.
    Memory(Vec<i16>, usize),
}

struct Stream {
    info: StreamInfo,
    source: Source,
    /// Fractional read position, for resampling to the card's rate.
    position: f64,
    /// The last frame taken, held for interpolation across reads.
    pending: Vec<i16>,
}
impl Stream {
    /// One frame of this stream, resampled to `rate` and `channels`.
    ///
    /// Returns `None` when the stream has run out of samples for now.
    fn frame(&mut self, rate: u32, channels: u32, out: &mut [i32]) -> bool {
        let step = self.info.rate as f64 / rate as f64;
        let stride = self.info.channels.max(1) as usize;
        // Advance to the frame this output frame needs.
        while self.position >= 1.0 {
            if !self.take(stride) {
                return false;
            }
            self.position -= 1.0;
        }
        if self.pending.len() < stride && !self.take(stride) {
            return false;
        }
        for (channel, sample) in out.iter_mut().enumerate().take(channels as usize) {
            // Mono plays on every channel; extra channels are dropped.
            let source = if stride == 1 { 0 } else { channel.min(stride - 1) };
            *sample += self.pending.get(source).copied().unwrap_or(0) as i32;
        }
        self.position += step;
        self.info.frames += 1;
        true
    }
    /// Pull one frame of `stride` samples out of the source.
    fn take(&mut self, stride: usize) -> bool {
        self.pending.clear();
        match &mut self.source {
            Source::Memory(samples, at) => {
                if *at + stride > samples.len() {
                    return false;
                }
                self.pending.extend_from_slice(&samples[*at..*at + stride]);
                *at += stride;
                true
            }
            Source::Socket(_, buffer, _) => {
                let bytes = stride * 2;
                if buffer.len() < bytes {
                    return false;
                }
                for sample in buffer[..bytes].chunks_exact(2) {
                    self.pending
                        .push(i16::from_le_bytes([sample[0], sample[1]]));
                }
                buffer.drain(..bytes);
                true
            }
        }
    }
    /// Read whatever the application has sent since the last period.
    fn fill(&mut self) {
        let Source::Socket(socket, buffer, open) = &mut self.source else {
            return;
        };
        let mut chunk = [0u8; 16384];
        while buffer.len() < MAX_BUFFERED {
            match socket.read(&mut chunk) {
                Ok(0) => {
                    *open = false;
                    break;
                }
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    *open = false;
                    break;
                }
            }
        }
    }
    /// Whether this stream is finished and can be forgotten.
    fn finished(&self) -> bool {
        match &self.source {
            Source::Memory(samples, at) => *at >= samples.len(),
            Source::Socket(_, buffer, open) => !*open && buffer.len() < 2,
        }
    }
}

/// The mixer: a socket, a thread and the state the service reads.
pub struct Mixer {
    pub shared: Arc<Mutex<Shared>>,
    commands: Sender<Command>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    path: PathBuf,
}
impl Mixer {
    /// Bind the audio socket and start mixing.
    pub fn start(card: u32, rate: u32, channels: u32) -> io::Result<Mixer> {
        let path = run_dir().join(SOCKET);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if path.exists() && UnixStream::connect(&path).is_err() {
            fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666))?;
        let shared = Arc::new(Mutex::new(Shared {
            volume: 100,
            ..Shared::default()
        }));
        let running = Arc::new(AtomicBool::new(true));
        let (commands, inbox) = channel();
        let handle = {
            let shared = Arc::clone(&shared);
            let running = Arc::clone(&running);
            thread::Builder::new()
                .name("hos-mixer".into())
                .spawn(move || mix(listener, inbox, shared, running, card, rate, channels))?
        };
        Ok(Mixer {
            shared,
            commands,
            running,
            handle: Some(handle),
            path,
        })
    }
    /// Play decoded samples, such as a notification sound.
    pub fn play(&self, name: &str, samples: Vec<i16>, rate: u32, channels: u32) {
        let _ = self.commands.send(Command::Play {
            name: name.to_string(),
            samples,
            rate,
            channels,
        });
    }
    /// Move playback to another card.
    pub fn use_card(&self, card: u32) {
        let _ = self.commands.send(Command::Card(card));
    }
    /// Stop every stream immediately.
    pub fn stop_all(&self) {
        let _ = self.commands.send(Command::StopAll);
    }
    /// The streams currently being mixed.
    pub fn streams(&self) -> Vec<StreamInfo> {
        self.shared
            .lock()
            .map(|shared| shared.streams.clone())
            .unwrap_or_default()
    }
    /// Tell the mixer how to apply volume when the card cannot.
    pub fn set_volume(&self, volume: u32, muted: bool, software: bool) {
        if let Ok(mut shared) = self.shared.lock() {
            shared.volume = volume.min(100);
            shared.muted = muted;
            shared.software_volume = software;
        }
    }
    pub fn error(&self) -> Option<String> {
        self.shared.lock().ok().and_then(|shared| shared.error.clone())
    }
}
impl Drop for Mixer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        // Wake the thread if it is waiting on the card rather than the socket.
        let _ = self.commands.send(Command::StopAll);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

/// Read and check one stream header. Returns the stream's format and name.
pub fn parse_header(bytes: &[u8]) -> Result<(u32, u32, usize), String> {
    if bytes.len() < HEADER {
        return Err("the header is too short".into());
    }
    if bytes[..8] != MAGIC {
        return Err("this is not an hOS audio stream".into());
    }
    let number = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    if number(8) != VERSION {
        return Err(format!("unsupported audio protocol version {}", number(8)));
    }
    let rate = number(12);
    let channels = number(16);
    let format = number(20);
    let name = number(28) as usize;
    if !(4000..=192_000).contains(&rate) {
        return Err(format!("{rate} Hz is outside the supported range"));
    }
    if !(1..=8).contains(&channels) {
        return Err(format!("{channels} channels is outside the supported range"));
    }
    if format != FORMAT_S16_LE {
        return Err("only signed 16-bit little-endian samples are supported".into());
    }
    if name > MAX_NAME {
        return Err("the stream name is too long".into());
    }
    Ok((rate, channels, name))
}

/// Build the header an application sends. Used by the client and the tests.
pub fn header(rate: u32, channels: u32, name: &str) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(MAX_NAME)];
    let mut bytes = Vec::with_capacity(HEADER + name.len());
    bytes.extend_from_slice(&MAGIC);
    for value in [VERSION, rate, channels, FORMAT_S16_LE, 0, name.len() as u32] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(name);
    bytes
}

/// Mix `streams` into one period of interleaved samples.
///
/// Streams that have nothing to give contribute silence rather than holding
/// everyone else up; a stream that has ended is left for the caller to drop.
fn mix_period(streams: &mut [Stream], rate: u32, channels: u32, out: &mut [i16], gain: f64) {
    let channels = channels.max(1) as usize;
    let frames = out.len() / channels;
    let mut accumulator = vec![0i32; channels];
    for frame in 0..frames {
        accumulator.iter_mut().for_each(|sample| *sample = 0);
        for stream in streams.iter_mut() {
            stream.frame(rate, channels as u32, &mut accumulator);
        }
        for (channel, sample) in accumulator.iter().enumerate() {
            let value = (*sample as f64 * gain).round();
            out[frame * channels + channel] = value.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
    }
}

/// The mixing thread.
fn mix(
    listener: UnixListener,
    inbox: Receiver<Command>,
    shared: Arc<Mutex<Shared>>,
    running: Arc<AtomicBool>,
    mut card: u32,
    rate: u32,
    channels: u32,
) {
    let mut streams: Vec<Stream> = Vec::new();
    let mut device: Option<Playback> = None;
    let mut next_id = 1u32;
    let mut idle_since = Instant::now();
    let mut period = vec![0i16; 1024 * channels as usize];
    while running.load(Ordering::Relaxed) {
        loop {
            match inbox.try_recv() {
                Ok(Command::Play {
                    name,
                    samples,
                    rate,
                    channels,
                }) => {
                    let id = next_id;
                    next_id += 1;
                    streams.push(Stream {
                        info: StreamInfo {
                            id,
                            name,
                            pid: 0,
                            rate,
                            channels,
                            frames: 0,
                            kind: "file",
                        },
                        source: Source::Memory(samples, 0),
                        position: 0.0,
                        pending: Vec::new(),
                    });
                }
                Ok(Command::Card(index)) => {
                    card = index;
                    device = None;
                }
                Ok(Command::StopAll) => {
                    streams.clear();
                    if let Some(device) = device.as_mut() {
                        device.drop_audio();
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // Accept applications that want to play something.
        while let Ok((socket, _)) = listener.accept() {
            match accept(socket, next_id) {
                Ok(stream) => {
                    next_id += 1;
                    log(
                        "hos-soundd",
                        &format!(
                            "{} is playing ({} Hz, {} channels)",
                            stream.info.name, stream.info.rate, stream.info.channels
                        ),
                    );
                    streams.push(stream);
                }
                // A client that sent nonsense is simply not played.
                Err(e) => log("hos-soundd", &format!("audio client refused: {e}")),
            }
        }
        for stream in &mut streams {
            stream.fill();
        }
        streams.retain(|stream| !stream.finished());
        let (volume, muted, software) = shared
            .lock()
            .map(|shared| (shared.volume, shared.muted, shared.software_volume))
            .unwrap_or((100, false, false));
        if streams.is_empty() {
            if device.is_some() && idle_since.elapsed() > IDLE {
                device = None;
            }
            if let Ok(mut shared) = shared.lock() {
                shared.streams.clear();
                shared.playing = false;
            }
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        idle_since = Instant::now();
        if device.is_none() {
            match Playback::open(card, 0, rate, channels) {
                Ok(playback) => {
                    period = vec![0i16; playback.period as usize * playback.channels as usize];
                    if let Ok(mut shared) = shared.lock() {
                        shared.error = None;
                    }
                    device = Some(playback);
                }
                Err(e) => {
                    if let Ok(mut shared) = shared.lock() {
                        shared.error = Some(e.to_string());
                    }
                    log("hos-soundd", &format!("card {card}: {e}"));
                    streams.clear();
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            }
        }
        let playback = device.as_mut().expect("the device was just opened");
        // The card owns the volume unless it has no control of its own.
        let gain = if muted {
            0.0
        } else if software {
            volume as f64 / 100.0
        } else {
            1.0
        };
        mix_period(
            &mut streams,
            playback.rate,
            playback.channels,
            &mut period,
            gain,
        );
        if let Ok(mut shared) = shared.lock() {
            shared.streams = streams.iter().map(|stream| stream.info.clone()).collect();
            shared.playing = true;
        }
        if let Err(e) = playback.write(&period) {
            log("hos-soundd", &format!("playback: {e}"));
            if let Ok(mut shared) = shared.lock() {
                shared.error = Some(e.to_string());
            }
            device = None;
            thread::sleep(Duration::from_millis(100));
        }
    }
    if let Some(mut device) = device {
        device.drain();
    }
}

/// Read a new client's header and turn the connection into a stream.
fn accept(mut socket: UnixStream, id: u32) -> Result<Stream, String> {
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut bytes = [0u8; HEADER];
    socket
        .read_exact(&mut bytes)
        .map_err(|e| format!("header: {e}"))?;
    let (rate, channels, name_length) = parse_header(&bytes)?;
    let mut name = vec![0u8; name_length];
    if name_length > 0 {
        socket
            .read_exact(&mut name)
            .map_err(|e| format!("name: {e}"))?;
    }
    socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    let name = String::from_utf8_lossy(&name).trim().to_string();
    Ok(Stream {
        info: StreamInfo {
            id,
            name: if name.is_empty() {
                "application".into()
            } else {
                name
            },
            pid: peer_pid(&socket),
            rate,
            channels,
            frames: 0,
            kind: "app",
        },
        source: Source::Socket(socket, Vec::new(), true),
        position: 0.0,
        pending: Vec::new(),
    })
}

fn peer_pid(socket: &UnixStream) -> i32 {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }
    let mut cred = Ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = size_of::<Ucred>() as u32;
    // SAFETY: SO_PEERCRED fills the live structure above.
    let rc = unsafe {
        crate::init::sys::getsockopt(
            socket.as_raw_fd(),
            1,
            17,
            &mut cred as *mut Ucred as *mut u8,
            &mut length,
        )
    };
    if rc < 0 { 0 } else { cred.pid }
}

/// Decode a 16-bit or 8-bit PCM WAV file.
///
/// Returns the samples, the sample rate and the channel count. Compressed
/// formats are refused: this is for short sounds the desktop ships with.
pub fn decode_wav(bytes: &[u8]) -> Result<(Vec<i16>, u32, u32), String> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("this is not a WAV file".into());
    }
    let mut rest = &bytes[12..];
    let mut format = None;
    while rest.len() >= 8 {
        let id = &rest[..4];
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let body = rest.get(8..8 + size).ok_or("a chunk runs past the end")?;
        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err("the format chunk is too short".into());
                }
                let kind = u16::from_le_bytes(body[..2].try_into().unwrap());
                let channels = u16::from_le_bytes(body[2..4].try_into().unwrap()) as u32;
                let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                if kind != 1 {
                    return Err("only uncompressed PCM WAV files are supported".into());
                }
                if !(1..=8).contains(&channels) || !(4000..=192_000).contains(&rate) {
                    return Err("unsupported channel count or sample rate".into());
                }
                if bits != 16 && bits != 8 {
                    return Err(format!("{bits}-bit samples are not supported"));
                }
                format = Some((rate, channels, bits));
            }
            b"data" => {
                let (rate, channels, bits) = format.ok_or("the data chunk came first")?;
                let samples = if bits == 16 {
                    body.chunks_exact(2)
                        .map(|s| i16::from_le_bytes([s[0], s[1]]))
                        .collect()
                } else {
                    // 8-bit WAV samples are unsigned.
                    body.iter()
                        .map(|s| ((*s as i16) - 128) * 256)
                        .collect::<Vec<i16>>()
                };
                return Ok((samples, rate, channels));
            }
            _ => (),
        }
        // Chunks are padded to an even length.
        rest = rest.get(8 + size + (size & 1)..).unwrap_or(&[]);
    }
    Err("the file has no audio data".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(samples: Vec<i16>, rate: u32, channels: u32) -> Stream {
        Stream {
            info: StreamInfo {
                id: 1,
                name: "test".into(),
                pid: 0,
                rate,
                channels,
                frames: 0,
                kind: "file",
            },
            source: Source::Memory(samples, 0),
            position: 0.0,
            pending: Vec::new(),
        }
    }

    #[test]
    fn headers_round_trip_and_bad_ones_are_refused() {
        let bytes = header(48000, 2, "hos-terminal");
        assert_eq!(parse_header(&bytes), Ok((48000, 2, 12)));
        assert_eq!(&bytes[HEADER..], b"hos-terminal");
        assert!(parse_header(&bytes[..16]).is_err());
        let mut wrong = header(48000, 2, "");
        wrong[0] = b'X';
        assert!(parse_header(&wrong).is_err());
        assert!(parse_header(&header(1000, 2, "")).is_err(), "rate too low");
        assert!(parse_header(&header(48000, 99, "")).is_err(), "channels");
        let mut version = header(48000, 2, "");
        version[8] = 9;
        assert!(parse_header(&version).is_err());
    }
    #[test]
    fn two_streams_are_summed_and_clipped() {
        let mut streams = vec![
            memory(vec![10_000, 10_000, 10_000, 10_000], 48000, 2),
            memory(vec![25_000, 25_000, 25_000, 25_000], 48000, 2),
        ];
        let mut out = vec![0i16; 4];
        mix_period(&mut streams, 48000, 2, &mut out, 1.0);
        assert_eq!(out, [32767, 32767, 32767, 32767], "the sum is clipped");
        let mut quiet = vec![memory(vec![1000; 8], 48000, 2)];
        let mut out = vec![0i16; 4];
        mix_period(&mut quiet, 48000, 2, &mut out, 0.5);
        assert_eq!(out, [500, 500, 500, 500], "software volume halves it");
    }
    #[test]
    fn mono_is_played_on_both_channels_and_rates_are_converted() {
        let mut streams = vec![memory(vec![100, 200, 300, 400], 24000, 1)];
        let mut out = vec![0i16; 8];
        // 24 kHz mono into 48 kHz stereo: every frame is used twice.
        mix_period(&mut streams, 48000, 2, &mut out, 1.0);
        assert_eq!(out[0], out[1], "mono reaches both channels");
        assert_eq!(&out[..4], &[100, 100, 100, 100]);
        assert_eq!(&out[4..8], &[200, 200, 200, 200]);
    }
    #[test]
    fn a_stream_that_runs_dry_becomes_silence_rather_than_a_stall() {
        let mut streams = vec![memory(vec![500, 500], 48000, 2)];
        let mut out = vec![0i16; 8];
        mix_period(&mut streams, 48000, 2, &mut out, 1.0);
        assert_eq!(&out[..2], &[500, 500]);
        assert_eq!(&out[2..], &[0; 6], "the rest of the period is silent");
        assert!(streams[0].finished());
    }
    #[test]
    fn a_client_handshake_turns_a_socket_into_a_stream() {
        use std::io::Write;
        let (mut client, server) = UnixStream::pair().unwrap();
        let sent = std::thread::spawn(move || {
            client.write_all(&header(44100, 1, "hos-terminal")).unwrap();
            client.write_all(&[0x10, 0x27, 0x10, 0x27]).unwrap(); // two samples
            client
        });
        let mut stream = accept(server, 7).expect("a valid header is accepted");
        let client = sent.join().unwrap();
        assert_eq!(stream.info.name, "hos-terminal");
        assert_eq!((stream.info.rate, stream.info.channels), (44100, 1));
        assert_eq!(stream.info.kind, "app");
        stream.fill();
        let mut out = vec![0i16; 4];
        mix_period(&mut [stream], 44100, 2, &mut out, 1.0);
        assert_eq!(&out[..4], &[10000, 10000, 10000, 10000]);
        drop(client);
        // A client that sends something else is refused, not played.
        let (mut bad, server) = UnixStream::pair().unwrap();
        let sent = std::thread::spawn(move || bad.write_all(&[0u8; HEADER]));
        assert!(accept(server, 8).is_err());
        let _ = sent.join();
    }
    #[test]
    fn wav_files_decode_and_damaged_ones_are_refused() {
        let mut wav = Vec::new();
        let samples: Vec<i16> = vec![0, 1000, -1000, 32767];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&2u16.to_le_bytes()); // channels
        wav.extend_from_slice(&44100u32.to_le_bytes());
        wav.extend_from_slice(&(44100u32 * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        assert_eq!(decode_wav(&wav), Ok((samples, 44100, 2)));
        assert!(decode_wav(b"not a wav file at all, not even close!!!!!!").is_err());
        let mut compressed = wav.clone();
        compressed[20] = 3; // a float format
        assert!(decode_wav(&compressed).is_err());
        assert!(decode_wav(&wav[..30]).is_err());
    }
}
