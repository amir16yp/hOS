//! Validated PSF1 bitmap fonts, including Unicode glyph maps.
use crate::surface::Surface;
use std::collections::HashMap;
pub struct Font<'a> {
    data: &'a [u8],
    height: usize,
    count: usize,
    unicode: HashMap<char, usize>,
}
impl<'a> Font<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, &'static str> {
        if data.len() < 4 || data[..2] != [0x36, 0x04] || data[2] > 5 || data[3] == 0 {
            return Err("invalid PSF1 header");
        }
        let count = if data[2] & 1 != 0 { 512 } else { 256 };
        let height = data[3] as usize;
        let end = 4 + count * height;
        if data.len() < end {
            return Err("truncated PSF1 glyphs");
        }
        let mut unicode = HashMap::new();
        if data[2] & 6 != 0 {
            let mut glyph = 0;
            let mut sequence = false;
            if (data.len() - end) % 2 != 0 {
                return Err("truncated PSF1 Unicode table");
            }
            for pair in data[end..].chunks_exact(2) {
                let v = u16::from_le_bytes([pair[0], pair[1]]);
                if glyph >= count {
                    return Err("extra PSF1 Unicode entries");
                }
                match v {
                    0xffff => {
                        glyph += 1;
                        sequence = false;
                    }
                    0xfffe => sequence = true,
                    _ => {
                        if !sequence {
                            if let Some(c) = char::from_u32(v as u32) {
                                unicode.entry(c).or_insert(glyph);
                            }
                        }
                    }
                }
            }
            if glyph != count {
                return Err("incomplete PSF1 Unicode table");
            }
        }
        Ok(Self {
            data,
            height,
            count,
            unicode,
        })
    }
    pub fn builtin() -> Font<'static> {
        Font::parse(include_bytes!("default8x9.psf")).expect("bundled PSF1 font")
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn draw(&self, dst: &mut Surface, x: i32, y: i32, text: &str, color: u32) {
        let mut xx = x;
        let mut yy = y;
        for c in text.chars() {
            if c == '\n' {
                xx = x;
                yy += self.height as i32 + 3;
                continue;
            }
            let glyph = self
                .unicode
                .get(&c)
                .copied()
                .or_else(|| {
                    if self.unicode.is_empty() && (c as usize) < self.count {
                        Some(c as usize)
                    } else {
                        None
                    }
                })
                .unwrap_or(63);
            for row in 0..self.height {
                let bits = self.data[4 + glyph * self.height + row];
                for col in 0..8 {
                    if bits & (0x80 >> col) != 0 {
                        dst.set_pixel(xx + col, yy + row as i32, color);
                    }
                }
            }
            xx += 8;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_and_renders() {
        assert!(Font::parse(&[0x36, 4, 0, 9]).is_err());
        let f = Font::builtin();
        let mut s = Surface::new(16, 12);
        f.draw(&mut s, -2, 0, "Hi", 0xffffffff);
        assert!(s.pixels().iter().any(|p| *p != 0));
    }
}
