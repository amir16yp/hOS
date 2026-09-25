//! QOI ("Quite OK Image") decoding and encoding for ARGB8888 surfaces.
//!
//! This is the published QOI format: a 14-byte header, a chunk stream and an
//! 8-byte end marker. Pixels are exchanged as `0xAARRGGBB` words, so images
//! interoperate with [`Surface`] without a separate conversion pass.
use crate::surface::Surface;
use std::{fs, io, path::Path};

/// Refuse absurd allocations from a corrupt or hostile header.
const MAX_PIXELS: usize = 1 << 24;

pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}
impl Image {
    /// Borrow the pixels for [`Surface::draw_image`].
    pub fn view(&self) -> crate::surface::Image<'_> {
        crate::surface::Image {
            width: self.width,
            height: self.height,
            pixels: &self.pixels,
        }
    }
}

fn channels(pixel: u32) -> (u8, u8, u8, u8) {
    (
        (pixel >> 16) as u8,
        (pixel >> 8) as u8,
        pixel as u8,
        (pixel >> 24) as u8,
    )
}
fn pixel(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}
fn slot(value: u32) -> usize {
    let (r, g, b, a) = channels(value);
    (r as usize * 3 + g as usize * 5 + b as usize * 7 + a as usize * 11) % 64
}

/// Encode ARGB8888 pixels. Fully opaque images are written as three channels.
pub fn encode(width: usize, height: usize, pixels: &[u32]) -> Vec<u8> {
    assert_eq!(width * height, pixels.len(), "pixel count must match size");
    let opaque = pixels.iter().all(|p| p >> 24 == 255);
    let mut out = Vec::with_capacity(pixels.len() + 32);
    out.extend(b"qoif");
    out.extend((width as u32).to_be_bytes());
    out.extend((height as u32).to_be_bytes());
    out.push(if opaque { 3 } else { 4 });
    out.push(0);
    let mut index = [0u32; 64];
    let mut previous = 0xff000000u32;
    let mut run = 0u8;
    for &next in pixels {
        if next == previous {
            run += 1;
            if run == 62 {
                out.push(0xc0 | (run - 1));
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(0xc0 | (run - 1));
            run = 0;
        }
        let hash = slot(next);
        if index[hash] == next {
            out.push(hash as u8);
            previous = next;
            continue;
        }
        index[hash] = next;
        let (r, g, b, a) = channels(next);
        let (pr, pg, pb, pa) = channels(previous);
        previous = next;
        if a != pa {
            out.extend([0xff, r, g, b, a]);
            continue;
        }
        let dr = r.wrapping_sub(pr) as i8 as i32;
        let dg = g.wrapping_sub(pg) as i8 as i32;
        let db = b.wrapping_sub(pb) as i8 as i32;
        let (dr_g, db_g) = (dr - dg, db - dg);
        if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
            out.push(0x40 | ((dr + 2) as u8) << 4 | ((dg + 2) as u8) << 2 | (db + 2) as u8);
        } else if (-32..=31).contains(&dg) && (-8..=7).contains(&dr_g) && (-8..=7).contains(&db_g) {
            out.push(0x80 | (dg + 32) as u8);
            out.push(((dr_g + 8) as u8) << 4 | (db_g + 8) as u8);
        } else {
            out.extend([0xfe, r, g, b]);
        }
    }
    if run > 0 {
        out.push(0xc0 | (run - 1));
    }
    out.extend([0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

/// Decode a QOI byte stream into ARGB8888 pixels.
pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() < 22 || &bytes[..4] != b"qoif" {
        return Err("not a QOI image".into());
    }
    let width = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let height = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if !matches!(bytes[12], 3 | 4) || bytes[13] > 1 {
        return Err("unsupported QOI channel count or colorspace".into());
    }
    let count = width
        .checked_mul(height)
        .filter(|n| *n > 0 && *n <= MAX_PIXELS)
        .ok_or("invalid QOI image dimensions")?;
    let mut pixels: Vec<u32> = Vec::with_capacity(count);
    let mut index = [0u32; 64];
    let mut current = 0xff000000u32;
    let mut data = &bytes[14..];
    while pixels.len() < count {
        let (tag, rest) = data.split_first().ok_or("truncated QOI chunk")?;
        data = rest;
        let byte = |data: &mut &[u8]| -> Result<u8, String> {
            let (value, rest) = data.split_first().ok_or("truncated QOI chunk")?;
            *data = rest;
            Ok(*value)
        };
        match *tag {
            0xfe => {
                let (r, g, b) = (byte(&mut data)?, byte(&mut data)?, byte(&mut data)?);
                current = pixel(r, g, b, (current >> 24) as u8);
            }
            0xff => {
                let (r, g, b) = (byte(&mut data)?, byte(&mut data)?, byte(&mut data)?);
                current = pixel(r, g, b, byte(&mut data)?);
            }
            tag => match tag >> 6 {
                0 => current = index[(tag & 0x3f) as usize],
                1 => {
                    let (r, g, b, a) = channels(current);
                    current = pixel(
                        r.wrapping_add(tag >> 4 & 3).wrapping_sub(2),
                        g.wrapping_add(tag >> 2 & 3).wrapping_sub(2),
                        b.wrapping_add(tag & 3).wrapping_sub(2),
                        a,
                    );
                }
                2 => {
                    let second = byte(&mut data)?;
                    let green = (tag & 0x3f).wrapping_sub(32);
                    let (r, g, b, a) = channels(current);
                    current = pixel(
                        r.wrapping_add(green).wrapping_add(second >> 4).wrapping_sub(8),
                        g.wrapping_add(green),
                        b.wrapping_add(green).wrapping_add(second & 15).wrapping_sub(8),
                        a,
                    );
                }
                _ => {
                    let run = (tag & 0x3f) as usize + 1;
                    if pixels.len() + run > count {
                        return Err("QOI run exceeds image size".into());
                    }
                    pixels.resize(pixels.len() + run, current);
                    continue;
                }
            },
        }
        index[slot(current)] = current;
        pixels.push(current);
    }
    if data.len() < 8 || data[..8] != [0, 0, 0, 0, 0, 0, 0, 1] {
        return Err("missing QOI end marker".into());
    }
    Ok(Image {
        width,
        height,
        pixels,
    })
}

pub fn load(path: impl AsRef<Path>) -> io::Result<Image> {
    decode(&fs::read(path)?).map_err(io::Error::other)
}
pub fn save(path: impl AsRef<Path>, width: usize, height: usize, pixels: &[u32]) -> io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, encode(width, height, pixels))
}
/// Save any drawing target: window contents, a scratch tile or the whole screen.
pub fn save_surface(path: impl AsRef<Path>, surface: &Surface) -> io::Result<()> {
    save(path, surface.width(), surface.height(), surface.pixels())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> (usize, usize, Vec<u32>) {
        let (w, h) = (17usize, 11usize);
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                pixels.push(match (x + y * w) % 23 {
                    0..=7 => 0xff203040,                      // repeated: runs
                    8 => 0xff213141,                          // one step: diff
                    9 => 0xff2a3a4a,                          // larger step: luma
                    10 => 0xff203040,                         // seen before: index
                    11 => 0x80ff0000,                         // alpha change: RGBA
                    n => 0xff000000 | (n as u32 * 0x010307) ^ ((x * y) as u32 & 0xffffff),
                });
            }
        }
        (w, h, pixels)
    }
    #[test]
    fn roundtrips_every_chunk_kind() {
        let (w, h, pixels) = sample();
        let bytes = encode(w, h, &pixels);
        assert_eq!(&bytes[..4], b"qoif");
        assert_eq!(bytes[12], 4, "the sample contains alpha");
        let image = decode(&bytes).unwrap();
        assert_eq!((image.width, image.height), (w, h));
        assert_eq!(image.pixels, pixels);
    }
    #[test]
    fn opaque_images_use_three_channels_and_long_runs() {
        let pixels = vec![0xff72dbac; 500];
        let bytes = encode(100, 5, &pixels);
        assert_eq!(bytes[12], 3);
        assert!(bytes.len() < 40, "runs should compress: {}", bytes.len());
        assert_eq!(decode(&bytes).unwrap().pixels, pixels);
    }
    #[test]
    fn rejects_corrupt_streams() {
        let bytes = encode(2, 2, &[0xff010203, 0xff040506, 0x80070809, 0xff0a0b0c]);
        assert!(decode(b"nope").is_err());
        assert!(decode(&bytes[..bytes.len() - 1]).is_err());
        let mut truncated = bytes.clone();
        truncated[7] = 200; // width beyond the encoded pixel stream
        assert!(decode(&truncated).is_err());
        let mut huge = bytes.clone();
        huge[4] = 0xff;
        assert!(decode(&huge).is_err());
        let mut channels = bytes.clone();
        channels[12] = 2;
        assert!(decode(&channels).is_err());
    }
    #[test]
    fn saves_and_loads_a_surface() {
        let mut s = Surface::new(6, 4);
        s.fill_rect(1, 1, 3, 2, 0xff72dbac);
        let path = std::env::temp_dir().join(format!("hoswm-qoi-{}.qoi", std::process::id()));
        save_surface(&path, &s).unwrap();
        let image = load(&path).unwrap();
        assert_eq!((image.width, image.height), (6, 4));
        assert_eq!(image.pixels, s.pixels());
        fs::remove_file(path).unwrap();
    }
}
