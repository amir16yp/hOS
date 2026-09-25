//! Minimal Linux x86_64 DRM UAPI. No libdrm or graphics runtime dependency.
use std::ffi::{c_int, c_ulong, c_void};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    mem::size_of,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
};

unsafe extern "C" {
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> c_int;
}
fn request<T>(nr: u8) -> c_ulong {
    0xc000_6400 | ((size_of::<T>() as c_ulong) << 16) | nr as c_ulong
}
fn call<T>(f: &File, nr: u8, data: &mut T) -> io::Result<()> {
    // SAFETY: all callers use the repr(C) UAPI structure associated with nr.
    if unsafe { ioctl(f.as_raw_fd(), request::<T>(nr), data as *mut T) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[repr(C)]
#[derive(Default)]
struct Resources {
    fbs: u64,
    crtcs: u64,
    connectors: u64,
    encoders: u64,
    nfb: u32,
    ncrtc: u32,
    nconn: u32,
    nenc: u32,
    minw: u32,
    maxw: u32,
    minh: u32,
    maxh: u32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Mode {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    kind: u32,
    name: [u8; 32],
}
#[repr(C)]
#[derive(Default)]
struct Connector {
    encoders: u64,
    modes: u64,
    props: u64,
    prop_values: u64,
    nmodes: u32,
    nprops: u32,
    nenc: u32,
    encoder: u32,
    id: u32,
    kind: u32,
    kind_id: u32,
    connection: u32,
    mmw: u32,
    mmh: u32,
    subpixel: u32,
    pad: u32,
}
#[repr(C)]
#[derive(Default)]
struct Encoder {
    id: u32,
    kind: u32,
    crtc: u32,
    possible_crtcs: u32,
    clones: u32,
}
#[repr(C)]
#[derive(Default)]
struct Crtc {
    connectors: u64,
    count: u32,
    id: u32,
    fb: u32,
    x: u32,
    y: u32,
    gamma: u32,
    valid: u32,
    mode: Mode,
}
#[repr(C)]
#[derive(Default)]
struct Dumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}
#[repr(C)]
#[derive(Default)]
struct Map {
    handle: u32,
    pad: u32,
    offset: u64,
}
#[repr(C)]
#[derive(Default)]
struct Fb {
    id: u32,
    width: u32,
    height: u32,
    format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifiers: [u64; 4],
}
#[repr(C)]
#[derive(Default)]
struct Dirty {
    id: u32,
    flags: u32,
    color: u32,
    count: u32,
    clips: u64,
}

#[repr(C)]
#[derive(Default)]
struct Cursor {
    flags: u32,
    crtc: u32,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    handle: u32,
}
#[repr(C)]
#[derive(Default)]
struct Cap {
    capability: u64,
    value: u64,
}
#[repr(C)]
#[derive(Debug, PartialEq)]
struct Clip {
    x1: u16,
    y1: u16,
    x2: u16,
    y2: u16,
}

struct Scanout {
    file: File,
    ptr: *mut u8,
    len: usize,
    pitch: usize,
    handle: u32,
    fb: u32,
    damage: crate::damage::Damage,
}
impl Scanout {
    fn new(file: &File) -> Result<Self, String> {
        let mut d = Self {
            file: file.try_clone().map_err(|e| e.to_string())?,
            ptr: std::ptr::null_mut(),
            len: 0,
            pitch: 0,
            handle: 0,
            fb: 0,
            damage: crate::damage::Damage::new(),
        };
        let mut dumb = Dumb {
            width: 800,
            height: 600,
            bpp: 32,
            ..Default::default()
        };
        call(&d.file, 0xb2, &mut dumb).map_err(|e| format!("DRM dumb buffer: {e}"))?;
        d.handle = dumb.handle;
        d.pitch = dumb.pitch as usize;
        d.len = dumb.size as usize;
        if d.pitch < 800 * 4 || d.len < d.pitch * 600 {
            return Err("DRM returned invalid pitch/size".into());
        }
        // DRM_FORMAT_XRGB8888 = fourcc('X','R','2','4'). Explicitly require this format.
        let mut fb = Fb {
            width: 800,
            height: 600,
            format: u32::from_le_bytes(*b"XR24"),
            ..Default::default()
        };
        fb.handles[0] = d.handle;
        fb.pitches[0] = dumb.pitch;
        call(&d.file, 0xb8, &mut fb)
            .map_err(|e| format!("DRM XRGB8888 ADDFB2: {e}; unsupported buffer format"))?;
        d.fb = fb.id;
        let mut map = Map {
            handle: d.handle,
            ..Default::default()
        };
        call(&d.file, 0xb3, &mut map).map_err(|e| format!("DRM MAP_DUMB: {e}"))?;
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                d.len,
                3,
                1,
                d.file.as_raw_fd(),
                map.offset as i64,
            )
        };
        if ptr as isize == -1 {
            return Err(format!("DRM mmap: {}", io::Error::last_os_error()));
        }
        d.ptr = ptr.cast();
        Ok(d)
    }
}
impl Drop for Scanout {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                munmap(self.ptr.cast(), self.len);
            }
        }
        if self.fb != 0 {
            let _ = call(&self.file, 0xaf, &mut self.fb);
        }
        if self.handle != 0 {
            let _ = call(&self.file, 0xb4, &mut self.handle);
        }
    }
}
#[repr(C)]
#[derive(Default)]
struct PageFlip {
    crtc: u32,
    fb: u32,
    flags: u32,
    reserved: u32,
    user_data: u64,
}

pub struct Display {
    file: File,
    front: Scanout,
    back: Option<Scanout>,
    pending: bool,
    flips_verified: bool,
    crtc: u32,
    cursor_handle: u32,
    cursor_attempted: bool,
    dirty_supported: bool,
    previous: Crtc,
    connector: u32,
    active: bool,
    _console: crate::framebuffer::Console,
}
impl Display {
    pub fn open(path: &str) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x800)
            .open(path)
            .map_err(|e| {
                format!("open {path}: {e}; use -device virtio-vga and CONFIG_DRM_VIRTIO_GPU=y")
            })?;
        // SET_MASTER has no payload and therefore is DRM_IO, not DRM_IOWR.
        if unsafe { ioctl(file.as_raw_fd(), 0x641e as c_ulong) } < 0 {
            return Err(format!(
                "DRM master: {}; run as root with no other display server",
                io::Error::last_os_error()
            ));
        }
        let setup = || -> io::Result<(u32, u32, Mode)> {
            let mut r = Resources::default();
            call(&file, 0xa0, &mut r)?;
            let mut conns = vec![0u32; r.nconn as usize];
            let mut crtcs = vec![0u32; r.ncrtc as usize];
            r.connectors = conns.as_mut_ptr() as u64;
            r.crtcs = crtcs.as_mut_ptr() as u64;
            // We do not request FB/encoder arrays on the second call.
            r.nfb = 0;
            r.nenc = 0;
            call(&file, 0xa0, &mut r)?;
            for id in conns {
                let mut c = Connector {
                    id,
                    ..Default::default()
                };
                call(&file, 0xa7, &mut c)?;
                if c.connection != 1 {
                    continue;
                }
                let mut modes = vec![Mode::default(); c.nmodes as usize];
                let mut encoders = vec![0u32; c.nenc as usize];
                c.modes = modes.as_mut_ptr() as u64;
                c.encoders = encoders.as_mut_ptr() as u64;
                c.nprops = 0;
                call(&file, 0xa7, &mut c)?;
                modes.sort_by_key(|mode| mode.vrefresh.abs_diff(60));
                for mode in modes
                    .into_iter()
                    .filter(|m| m.hdisplay == 800 && m.vdisplay == 600)
                {
                    for &enc in &encoders {
                        let mut e = Encoder {
                            id: enc,
                            ..Default::default()
                        };
                        call(&file, 0xa6, &mut e)?;
                        if let Some((_, &crtc)) = crtcs
                            .iter()
                            .enumerate()
                            .find(|(i, _)| *i < 32 && e.possible_crtcs & (1 << i) != 0)
                        {
                            return Ok((id, crtc, mode));
                        }
                    }
                }
            }
            Err(io::Error::other(
                "no connected output with an 800x600 mode and usable CRTC; use virtio-vga, video=Virtual-1:800x600@60",
            ))
        };
        let (mut connector, crtc, mode) = setup().map_err(|e| format!("DRM resources: {e}"))?;
        let mut previous = Crtc {
            id: crtc,
            ..Default::default()
        };
        call(&file, 0xa1, &mut previous).map_err(|e| format!("DRM GETCRTC: {e}"))?;
        let console = crate::framebuffer::Console::graphics()?;
        let front = Scanout::new(&file)?;
        let back = match Scanout::new(&file) {
            Ok(buffer) => Some(buffer),
            Err(error) => {
                eprintln!("HOSWM DRM single-buffer fallback: {error}");
                None
            }
        };
        let mut d = Self {
            file,
            front,
            back,
            pending: false,
            flips_verified: false,
            crtc,
            cursor_handle: 0,
            cursor_attempted: false,
            dirty_supported: true,
            previous,
            connector,
            active: false,
            _console: console,
        };
        let mut c = Crtc {
            connectors: &mut connector as *mut u32 as u64,
            count: 1,
            id: crtc,
            fb: d.front.fb,
            valid: 1,
            mode,
            ..Default::default()
        };
        call(&d.file, 0xa2, &mut c).map_err(|e| format!("DRM SETCRTC 800x600: {e}"))?;
        d.active = true;
        eprintln!(
            "HOSWM DRM ready: {path}, connector={connector}, crtc={crtc}, 800x600 XRGB8888 pitch={}",
            d.front.pitch
        );
        Ok(d)
    }
    /// Try a hardware cursor once, falling back permanently if unsupported.
    pub fn cursor(&mut self, x: i32, y: i32) -> bool {
        if !self.cursor_attempted {
            self.cursor_attempted = true;
            if let Err(error) = self.create_cursor(x, y) {
                eprintln!("HOSWM DRM software cursor: {error}");
            }
        }
        if self.cursor_handle == 0 {
            return false;
        }
        if call(
            &self.file,
            0xa3,
            &mut Cursor {
                flags: 2,
                crtc: self.crtc,
                x,
                y,
                ..Default::default()
            },
        )
        .is_ok()
        {
            return true;
        }
        let _ = call(
            &self.file,
            0xa3,
            &mut Cursor {
                flags: 1,
                crtc: self.crtc,
                ..Default::default()
            },
        );
        let _ = call(&self.file, 0xb4, &mut self.cursor_handle);
        self.cursor_handle = 0;
        false
    }
    fn create_cursor(&mut self, x: i32, y: i32) -> io::Result<()> {
        let mut width = Cap {
            capability: 8,
            value: 64,
        };
        let mut height = Cap {
            capability: 9,
            value: 64,
        };
        let _ = call(&self.file, 0x0c, &mut width);
        let _ = call(&self.file, 0x0c, &mut height);
        if !(9..=512).contains(&width.value) || !(18..=512).contains(&height.value) {
            return Err(io::Error::other("unsupported cursor dimensions"));
        }
        let mut dumb = Dumb {
            width: width.value as u32,
            height: height.value as u32,
            bpp: 32,
            ..Default::default()
        };
        call(&self.file, 0xb2, &mut dumb)?;
        let result = (|| {
            if dumb.pitch < dumb.width * 4
                || dumb.size < u64::from(dumb.pitch) * u64::from(dumb.height)
                || dumb.size > 16 * 1024 * 1024
            {
                return Err(io::Error::other("invalid cursor buffer layout"));
            }
            let mut map = Map {
                handle: dumb.handle,
                ..Default::default()
            };
            call(&self.file, 0xb3, &mut map)?;
            // SAFETY: validated buffer size and kernel-provided mmap offset.
            let ptr = unsafe {
                mmap(
                    std::ptr::null_mut(),
                    dumb.size as usize,
                    3,
                    1,
                    self.file.as_raw_fd(),
                    map.offset as i64,
                )
            };
            if ptr as isize == -1 {
                return Err(io::Error::last_os_error());
            }
            let pixels =
                unsafe { std::slice::from_raw_parts_mut(ptr.cast::<u8>(), dumb.size as usize) };
            pixels.fill(0);
            for y in 0..18 {
                for x in 0..9 {
                    let offset = y * dumb.pitch as usize + x * 4;
                    pixels[offset..offset + 4]
                        .copy_from_slice(&crate::cursor::pixel(x, y).to_le_bytes());
                }
            }
            unsafe {
                munmap(ptr, dumb.size as usize);
            }
            call(
                &self.file,
                0xa3,
                &mut Cursor {
                    flags: 3,
                    crtc: self.crtc,
                    x,
                    y,
                    width: dumb.width,
                    height: dumb.height,
                    handle: dumb.handle,
                },
            )
        })();
        if result.is_ok() {
            self.cursor_handle = dumb.handle;
            eprintln!("HOSWM DRM hardware cursor enabled");
        } else {
            let _ = call(&self.file, 0xb4, &mut dumb.handle);
        }
        result
    }
    pub fn watch(&self, reactor: &mut crate::reactor::Reactor) {
        if self.pending {
            reactor.watch(self.file.as_raw_fd(), true, false);
        }
    }
    pub fn ready(&mut self) -> Result<bool, String> {
        if !self.pending {
            return Ok(true);
        }
        let mut bytes = [0u8; 4096];
        match self.file.read(&mut bytes) {
            Ok(0) => return Err("DRM event stream closed".into()),
            Ok(n) => {
                let mut offset = 0;
                while offset + 8 <= n {
                    let kind = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
                    let length =
                        u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap())
                            as usize;
                    if length < 8 || length > n - offset {
                        return Err("invalid DRM event".into());
                    }
                    if kind == 2 && self.pending {
                        std::mem::swap(&mut self.front, self.back.as_mut().unwrap());
                        self.pending = false;
                        if !self.flips_verified {
                            self.flips_verified = true;
                            eprintln!("HOSWM DRM page flips active");
                        }
                    }
                    offset += length;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                ()
            }
            Err(error) => return Err(format!("DRM page-flip event: {error}")),
        }
        Ok(!self.pending)
    }
    pub fn present(&mut self, pixels: &[u32]) -> Result<(), String> {
        if pixels.len() != 800 * 600 {
            return Err("expected an 800x600 surface".into());
        }
        if self.pending {
            return Err("DRM page flip still pending".into());
        }
        let buffer = self.back.as_mut().unwrap_or(&mut self.front);
        let rows = buffer.damage.rows(pixels);
        // SAFETY: mmap succeeded; Scanout validated the buffer length and pitch.
        let dst = unsafe { std::slice::from_raw_parts_mut(buffer.ptr, buffer.len) };
        let clips = damage_clips(&rows);
        for (y, span) in rows.iter().enumerate() {
            if span.is_empty() {
                continue;
            }
            for x in span.clone() {
                let offset = y * buffer.pitch + x * 4;
                dst[offset..offset + 4].copy_from_slice(&xrgb(pixels[y * 800 + x]).to_le_bytes());
            }
        }
        if !clips.is_empty() && self.dirty_supported {
            if let Err(error) = call(
                &self.file,
                0xb1,
                &mut Dirty {
                    id: buffer.fb,
                    count: clips.len() as u32,
                    clips: clips.as_ptr() as u64,
                    ..Default::default()
                },
            ) {
                // Native scanout drivers need no shadow-buffer damage notification.
                if matches!(error.raw_os_error(), Some(38 | 95)) {
                    self.dirty_supported = false;
                } else {
                    return Err(format!("DRM DIRTYFB: {error}"));
                }
            }
        }
        buffer.damage.commit(pixels);
        if let Some(back) = &self.back {
            match call(
                &self.file,
                0xb0,
                &mut PageFlip {
                    crtc: self.crtc,
                    fb: back.fb,
                    flags: 1,
                    ..Default::default()
                },
            ) {
                Ok(()) => {
                    self.pending = true;
                }
                Err(error) if matches!(error.raw_os_error(), Some(22 | 38 | 95)) => {
                    eprintln!("HOSWM DRM page flips unavailable, using damage updates: {error}");
                    self.back = None;
                    return self.present(pixels);
                }
                Err(error) => return Err(format!("DRM page flip: {error}")),
            }
        }
        Ok(())
    }
}
impl Drop for Display {
    fn drop(&mut self) {
        if self.cursor_handle != 0 {
            let _ = call(
                &self.file,
                0xa3,
                &mut Cursor {
                    flags: 1,
                    crtc: self.crtc,
                    ..Default::default()
                },
            );
            let _ = call(&self.file, 0xb4, &mut self.cursor_handle);
        }
        if self.active {
            if self.previous.valid != 0 {
                self.previous.connectors = &mut self.connector as *mut u32 as u64;
                self.previous.count = 1;
            }
            let _ = call(&self.file, 0xa2, &mut self.previous);
        }
    }
}
/// Composite ARGB8888 over black, then write little-endian XRGB8888 B,G,R,X bytes.
/// Padding belongs to the driver and is never interpreted as framebuffer pixels.
pub fn convert(src: &[u32], dst: &mut [u8], pitch: usize) {
    assert_eq!(src.len(), 800 * 600);
    for (y, row) in src.chunks_exact(800).enumerate() {
        for (x, &p) in row.iter().enumerate() {
            let o = y * pitch + x * 4;
            dst[o..o + 4].copy_from_slice(&xrgb(p).to_le_bytes());
        }
    }
}
fn damage_clips(rows: &[std::ops::Range<usize>]) -> Vec<Clip> {
    let mut clips: Vec<Clip> = Vec::new();
    for (y, span) in rows.iter().enumerate() {
        if span.is_empty() {
            continue;
        }
        if let Some(last) = clips.last_mut()
            && last.x1 == span.start as u16
            && last.x2 == span.end as u16
            && last.y2 == y as u16
        {
            last.y2 += 1;
        } else {
            clips.push(Clip {
                x1: span.start as u16,
                x2: span.end as u16,
                y1: y as u16,
                y2: y as u16 + 1,
            });
        }
    }
    // Linux DRM_MODE_FB_DIRTY_MAX_CLIPS is 256. Fall back to a bounding
    // rectangle for fragmented damage, while still copying only changed spans.
    if clips.len() > 256 {
        let bound = Clip {
            x1: clips.iter().map(|c| c.x1).min().unwrap(),
            x2: clips.iter().map(|c| c.x2).max().unwrap(),
            y1: clips.first().unwrap().y1,
            y2: clips.last().unwrap().y2,
        };
        clips.clear();
        clips.push(bound);
    }
    clips
}
fn xrgb(p: u32) -> u32 {
    let a = p >> 24;
    if a == 255 {
        return p & 0x00ffffff;
    }
    ((p & 255) * a / 255)
        | ((((p >> 8) & 255) * a / 255) << 8)
        | ((((p >> 16) & 255) * a / 255) << 16)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn damage_is_merged_and_bounded_to_kernel_limit() {
        assert!(damage_clips(&vec![0..0; 600]).is_empty());
        assert_eq!(
            damage_clips(&vec![0..800; 600]),
            vec![Clip {
                x1: 0,
                x2: 800,
                y1: 0,
                y2: 600
            }]
        );
        let rows: Vec<_> = (0..600).map(|y| (y % 2)..800).collect();
        assert_eq!(
            damage_clips(&rows),
            vec![Clip {
                x1: 0,
                x2: 800,
                y1: 0,
                y2: 600
            }]
        );
    }
    #[test]
    fn format_and_stride() {
        let mut src = vec![0xff123456; 800 * 600];
        src[800] = 0x80804020;
        let mut dst = vec![0xaa; 3216 * 600];
        convert(&src, &mut dst, 3216);
        assert_eq!(&dst[..4], &[0x56, 0x34, 0x12, 0]);
        assert_eq!(&dst[3200..3216], &[0xaa; 16]);
        assert_eq!(&dst[3216..3220], &[0x10, 0x20, 0x40, 0]);
    }
    #[test]
    fn uapi_layouts() {
        assert_eq!(size_of::<Mode>(), 68);
        assert_eq!(size_of::<Connector>(), 80);
        assert_eq!(size_of::<Crtc>(), 104);
        assert_eq!(size_of::<Fb>(), 104);
        assert_eq!(size_of::<Resources>(), 64);
        assert_eq!(size_of::<Dumb>(), 32);
        assert_eq!(size_of::<Cursor>(), 28);
        assert_eq!(size_of::<PageFlip>(), 24);
        assert_eq!(size_of::<Cap>(), 16);
        assert_eq!(size_of::<Clip>(), 8);
    }
}
