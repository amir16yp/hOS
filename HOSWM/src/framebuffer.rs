//! Linux fbdev output. Works with firmware framebuffers and DRM fbdev emulation.
use crate::drm::ioctl;
use std::{
    fs::{File, OpenOptions},
    io,
    os::{fd::AsRawFd, unix::fs::FileExt},
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Bitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

#[repr(C)]
#[derive(Default)]
struct VarInfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: Bitfield,
    green: Bitfield,
    blue: Bitfield,
    transp: Bitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

#[repr(C)]
#[derive(Default)]
struct FixInfo {
    id: [u8; 16],
    smem_start: u64,
    smem_len: u32,
    kind: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    line_length: u32,
    mmio_start: u64,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
}

struct Layout {
    width: usize,
    height: usize,
    bytes: usize,
    pitch: usize,
    start: u64,
    fields: [Bitfield; 4],
}
impl Layout {
    fn new(v: &VarInfo, f: &FixInfo) -> Result<Self, String> {
        if f.kind != 0
            || f.visual != 2
            || v.grayscale != 0
            || v.nonstd != 0
            || !matches!(v.bits_per_pixel, 16 | 24 | 32)
            || v.rotate != 0
            || v.vmode & 256 != 0
        {
            return Err("framebuffer needs packed true-color 16/24/32-bit pixels without rotation or y-wrap".into());
        }
        let fields = [v.red, v.green, v.blue, v.transp];
        let mut used = 0u64;
        for (i, field) in fields.iter().enumerate() {
            if field.length == 0 && i == 3 {
                continue;
            }
            if field.length == 0
                || field.length > 16
                || field.msb_right != 0
                || field
                    .offset
                    .checked_add(field.length)
                    .is_none_or(|end| end > v.bits_per_pixel)
            {
                return Err("unsupported framebuffer channel layout".into());
            }
            let mask = ((1u64 << field.length) - 1) << field.offset;
            if used & mask != 0 {
                return Err("overlapping framebuffer channels".into());
            }
            used |= mask;
        }
        let bytes = v.bits_per_pixel as usize / 8;
        let width = v.xres as usize;
        let height = v.yres as usize;
        let pitch = f.line_length as usize;
        // Bound allocations and arithmetic even for malformed driver metadata.
        if width == 0
            || height == 0
            || width > 16384
            || height > 16384
            || u64::from(v.xoffset) + u64::from(v.xres) > u64::from(v.xres_virtual)
            || u64::from(v.yoffset) + u64::from(v.yres) > u64::from(v.yres_virtual)
            || (u64::from(v.xoffset) + width as u64) * bytes as u64 > pitch as u64
        {
            return Err("invalid framebuffer dimensions, offsets or stride".into());
        }
        let start = u64::from(v.yoffset) * pitch as u64 + u64::from(v.xoffset) * bytes as u64;
        let end = start + (height as u64 - 1) * pitch as u64 + (width * bytes) as u64;
        if end > u64::from(f.smem_len) || width * height * bytes > 256 * 1024 * 1024 {
            return Err(
                "framebuffer dimensions exceed accessible memory or allocation limit".into(),
            );
        }
        Ok(Self {
            width,
            height,
            bytes,
            pitch,
            start,
            fields,
        })
    }

    fn encode(&self, pixel: u32) -> [u8; 4] {
        if pixel >> 24 == 255
            && self.bytes == 4
            && self.fields[0].offset == 16
            && self.fields[0].length == 8
            && self.fields[1].offset == 8
            && self.fields[1].length == 8
            && self.fields[2].offset == 0
            && self.fields[2].length == 8
            && (self.fields[3].length == 0
                || (self.fields[3].offset == 24 && self.fields[3].length == 8))
        {
            return (if self.fields[3].length == 0 {
                pixel & 0x00ffffff
            } else {
                pixel
            })
            .to_le_bytes();
        }
        let alpha = pixel >> 24;
        let channels = [
            ((pixel >> 16) & 255) * alpha / 255,
            ((pixel >> 8) & 255) * alpha / 255,
            (pixel & 255) * alpha / 255,
            255,
        ];
        let mut packed = 0u32;
        for (field, value) in self.fields.iter().zip(channels) {
            if field.length != 0 {
                packed |= ((value * ((1 << field.length) - 1) + 127) / 255) << field.offset;
            }
        }
        packed.to_le_bytes()
    }

    fn viewport(&self, source_width: usize, source_height: usize) -> (usize, usize, usize, usize) {
        let (w, h) = if self.width * source_height <= self.height * source_width {
            (
                self.width,
                (self.width * source_height / source_width).max(1),
            )
        } else {
            (
                (self.height * source_width / source_height).max(1),
                self.height,
            )
        };
        (w, h, (self.width - w) / 2, (self.height - h) / 2)
    }

    fn update(
        &self,
        pixels: &[u32],
        frame: &mut [u8],
        rows: &[std::ops::Range<usize>],
    ) -> Vec<(usize, usize, u64)> {
        self.update_scaled(pixels, 800, 600, frame, rows)
    }

    fn update_scaled(
        &self,
        pixels: &[u32],
        source_width: usize,
        source_height: usize,
        frame: &mut [u8],
        rows: &[std::ops::Range<usize>],
    ) -> Vec<(usize, usize, u64)> {
        let row_bytes = self.width * self.bytes;
        let (w, h, left, top) = self.viewport(source_width, source_height);
        // Only encode and upload changed visible spans. Keep write(2) for
        // fbdev drivers that rely on it to notify shadow-buffer damage.
        let mut writes: Vec<(usize, usize, u64)> = Vec::new();
        for y in 0..h {
            let sy = y * source_height / h;
            let span = &rows[sy];
            if span.is_empty() {
                continue;
            }
            let x0 = (span.start * w).div_ceil(source_width);
            let x1 = (span.end * w).div_ceil(source_width).min(w);
            if x0 == x1 {
                continue;
            }
            let start = (top + y) * row_bytes + (left + x0) * self.bytes;
            let end = start + (x1 - x0) * self.bytes;
            for (x, dst) in (x0..x1).zip(frame[start..end].chunks_exact_mut(self.bytes)) {
                dst.copy_from_slice(
                    &self.encode(pixels[sy * source_width + x * source_width / w])[..self.bytes],
                );
            }
            let offset = self.start + ((top + y) * self.pitch + (left + x0) * self.bytes) as u64;
            if let Some((previous_start, previous_end, previous_offset)) = writes.last_mut() {
                if *previous_end == start
                    && *previous_offset + (*previous_end - *previous_start) as u64 == offset
                {
                    *previous_end = end;
                    continue;
                }
            }
            writes.push((start, end, offset));
        }
        writes
    }

    fn render(&self, src: &[u32], dst: &mut [u8]) {
        self.render_scaled(src, dst, 800, 600)
    }

    fn render_scaled(
        &self,
        src: &[u32],
        dst: &mut [u8],
        source_width: usize,
        source_height: usize,
    ) {
        let (w, h, left, top) = self.viewport(source_width, source_height);
        for y in 0..self.height {
            for x in 0..self.width {
                let pixel = if x >= left && x < left + w && y >= top && y < top + h {
                    src[((y - top) * source_height / h) * source_width
                        + (x - left) * source_width / w]
                } else {
                    0xff000000
                };
                let offset = (y * self.width + x) * self.bytes;
                dst[offset..offset + self.bytes].copy_from_slice(&self.encode(pixel)[..self.bytes]);
            }
        }
    }
}

pub(crate) struct Console {
    file: File,
    previous: i32,
}
impl Console {
    pub(crate) fn graphics() -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty0")
            .map_err(|e| format!("open active virtual console: {e}"))?;
        let mut previous = 0i32;
        // SAFETY: KDGETMODE writes one int; KDSETMODE takes an integer mode.
        if unsafe { ioctl(file.as_raw_fd(), 0x4b3bu64, &mut previous as *mut i32) } < 0 {
            return Err(format!("KDGETMODE: {}", io::Error::last_os_error()));
        }
        if previous != 0 {
            return Err("virtual console is already in graphics mode".into());
        }
        if unsafe { ioctl(file.as_raw_fd(), 0x4b3au64, 1i32) } < 0 {
            return Err(format!("KDSETMODE: {}", io::Error::last_os_error()));
        }
        Ok(Self { file, previous })
    }
}
impl Drop for Console {
    fn drop(&mut self) {
        // SAFETY: restore the mode obtained from this console before drawing.
        unsafe {
            ioctl(self.file.as_raw_fd(), 0x4b3au64, self.previous);
        }
    }
}

pub struct Display {
    file: File,
    layout: Layout,
    frame: Vec<u8>,
    _console: Console,
    damage: crate::damage::Damage,
    initialized: bool,
    source_width: usize,
    source_height: usize,
}
impl Display {
    pub fn open(path: &str) -> Result<Self, String> {
        let file = OpenOptions::new().read(true).write(true).open(path)
            .map_err(|e| format!("open {path}: {e}; boot with a VESA/EFI framebuffer or a driver with fbdev emulation"))?;
        let mut var = VarInfo::default();
        let mut fix = FixInfo::default();
        // SAFETY: repr(C) structures match Linux x86_64 fb.h; the kernel fills them.
        if unsafe { ioctl(file.as_raw_fd(), 0x4600u64, &mut var as *mut VarInfo) } < 0
            || unsafe { ioctl(file.as_raw_fd(), 0x4602u64, &mut fix as *mut FixInfo) } < 0
        {
            return Err(format!("framebuffer info: {}", io::Error::last_os_error()));
        }
        let layout = Layout::new(&var, &fix)?;
        let frame = vec![0; layout.width * layout.height * layout.bytes];
        let console = Console::graphics()?;
        eprintln!(
            "HOSWM framebuffer ready: {path}, {}x{} {}bpp stride={}",
            layout.width, layout.height, var.bits_per_pixel, layout.pitch
        );
        Ok(Self {
            file,
            layout,
            frame,
            _console: console,
            damage: crate::damage::Damage::new(),
            initialized: false,
            source_width: 800,
            source_height: 600,
        })
    }

    pub fn size(&self) -> (usize, usize) {
        (self.layout.width, self.layout.height)
    }

    pub fn set_source_size(&mut self, width: usize, height: usize) {
        self.source_width = width;
        self.source_height = height;
        self.damage = crate::damage::Damage::with_size(width, height);
        self.initialized = false;
    }

    pub fn present(&mut self, pixels: &[u32]) -> Result<(), String> {
        if pixels.len() != self.source_width * self.source_height {
            return Err(format!(
                "expected a {}x{} surface",
                self.source_width, self.source_height
            ));
        }
        let rows = self.damage.rows(pixels);
        let l = &self.layout;
        let row_bytes = l.width * l.bytes;
        if !self.initialized {
            l.render_scaled(
                pixels,
                &mut self.frame,
                self.source_width,
                self.source_height,
            );
            if row_bytes == l.pitch {
                self.file
                    .write_all_at(&self.frame, l.start)
                    .map_err(|e| format!("framebuffer write: {e}"))?;
            } else {
                for (y, row) in self.frame.chunks_exact(row_bytes).enumerate() {
                    self.file
                        .write_all_at(row, l.start + (y * l.pitch) as u64)
                        .map_err(|e| format!("framebuffer row {y}: {e}"))?;
                }
            }
            self.initialized = true;
        } else {
            let writes = l.update_scaled(
                pixels,
                self.source_width,
                self.source_height,
                &mut self.frame,
                &rows,
            );
            for (start, end, offset) in writes {
                self.file
                    .write_all_at(&self.frame[start..end], offset)
                    .map_err(|e| format!("framebuffer damage: {e}"))?;
            }
        }
        self.damage.commit(pixels);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn field(offset: u32, length: u32) -> Bitfield {
        Bitfield {
            offset,
            length,
            msb_right: 0,
        }
    }
    fn info() -> (VarInfo, FixInfo) {
        (
            VarInfo {
                xres: 800,
                yres: 600,
                xres_virtual: 800,
                yres_virtual: 600,
                bits_per_pixel: 32,
                red: field(16, 8),
                green: field(8, 8),
                blue: field(0, 8),
                ..Default::default()
            },
            FixInfo {
                visual: 2,
                line_length: 3216,
                smem_len: 3216 * 600,
                ..Default::default()
            },
        )
    }
    #[test]
    fn damage_matches_full_render_across_formats_scales_and_offsets() {
        for (width, height) in [(4, 4), (799, 601), (800, 600), (1024, 768), (1280, 720)] {
            for bpp in [16, 24, 32] {
                let (mut v, mut f) = info();
                v.xres = width;
                v.yres = height;
                v.xoffset = 1;
                v.yoffset = 2;
                v.xres_virtual = width + 1;
                v.yres_virtual = height + 2;
                v.bits_per_pixel = bpp;
                if bpp == 16 {
                    v.red = field(11, 5);
                    v.green = field(5, 6);
                    v.blue = field(0, 5);
                }
                f.line_length = (width + 1) * (bpp / 8) + 12;
                f.smem_len = f.line_length * (height + 2);
                let l = Layout::new(&v, &f).unwrap();
                let mut damage = crate::damage::Damage::new();
                let mut src = vec![0xff123456; 800 * 600];
                let mut partial = vec![0; l.width * l.height * l.bytes];
                l.render(&src, &mut partial);
                damage.commit(&src);
                for (i, p) in src.iter_mut().enumerate() {
                    if i % 173 == 0 || (i / 800 == 599 && i % 800 > 790) {
                        *p = 0x80703020;
                    }
                }
                let updates = l.update(&src, &mut partial, &damage.rows(&src));
                let mut full = vec![0; partial.len()];
                l.render(&src, &mut full);
                assert_eq!(partial, full, "{width}x{height} {bpp}bpp");
                for (start, end, offset) in updates {
                    assert!(end > start);
                    let relative = offset - l.start;
                    assert_eq!(
                        relative / l.pitch as u64,
                        (start / (l.width * l.bytes)) as u64
                    );
                    assert_eq!(
                        relative % l.pitch as u64,
                        (start % (l.width * l.bytes)) as u64
                    );
                }
            }
        }
    }
    #[test]
    fn full_width_damage_uploads_are_coalesced() {
        let (v, mut f) = info();
        f.line_length = 3200;
        let l = Layout::new(&v, &f).unwrap();
        let src = vec![0xff123456; 800 * 600];
        let mut dst = vec![0; 800 * 600 * 4];
        assert_eq!(
            l.update(&src, &mut dst, &vec![0..800; 600]),
            vec![(0, dst.len(), 0)]
        );
    }
    #[test]
    fn uapi_layouts() {
        assert_eq!(std::mem::size_of::<VarInfo>(), 160);
        assert_eq!(std::mem::size_of::<FixInfo>(), 80);
        assert_eq!(std::mem::offset_of!(FixInfo, line_length), 48);
    }
    #[test]
    fn formats_and_alpha() {
        let (mut v, f) = info();
        assert_eq!(
            Layout::new(&v, &f).unwrap().encode(0x80804020),
            [0x10, 0x20, 0x40, 0]
        );
        v.bits_per_pixel = 16;
        v.red = field(11, 5);
        v.green = field(5, 6);
        v.blue = field(0, 5);
        assert_eq!(
            &Layout::new(&v, &f).unwrap().encode(0xffff8000)[..2],
            &[0x00, 0xfc]
        );
        v.bits_per_pixel = 24;
        v.red = field(0, 8);
        v.green = field(8, 8);
        v.blue = field(16, 8);
        assert_eq!(
            &Layout::new(&v, &f).unwrap().encode(0xff123456)[..3],
            &[0x12, 0x34, 0x56]
        );
        v.bits_per_pixel = 32;
        v.transp = field(24, 8);
        assert_eq!(Layout::new(&v, &f).unwrap().encode(0xff123456)[3], 255);
    }
    #[test]
    fn rejects_bad_metadata_and_honors_offsets() {
        let (mut v, mut f) = info();
        f.smem_len -= 17;
        assert!(Layout::new(&v, &f).is_err());
        f.smem_len = 3216 * 602;
        v.yoffset = 2;
        v.yres_virtual = 602;
        assert_eq!(Layout::new(&v, &f).unwrap().start, 6432);
        v.red = v.blue;
        assert!(Layout::new(&v, &f).is_err());
        v.red = field(31, 8);
        assert!(Layout::new(&v, &f).is_err());
        v.red = field(16, 8);
        v.xoffset = 1;
        assert!(Layout::new(&v, &f).is_err());
        v.xoffset = 0;
        f.visual = 3;
        assert!(Layout::new(&v, &f).is_err());
    }
    #[test]
    fn scales_and_letterboxes() {
        let (mut v, mut f) = info();
        v.xres = 4;
        v.yres = 4;
        f.line_length = 20;
        let layout = Layout::new(&v, &f).unwrap();
        let mut src = vec![0xff123456; 800 * 600];
        src[400] = 0xffabcdef;
        let mut dst = vec![0; 4 * 4 * 4];
        layout.render(&src, &mut dst);
        assert_eq!(&dst[8..12], &[0xef, 0xcd, 0xab, 0]);
        assert_eq!(&dst[48..], &[0; 16]);
    }
}
