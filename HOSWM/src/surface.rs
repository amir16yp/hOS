//! Owned ARGB8888 drawing surfaces. Coordinates are clipped to each target.
/// A borrowed image view; borrowing keeps image ownership with the caller.
pub struct Image<'a> {
    pub width: usize,
    pub height: usize,
    pub pixels: &'a [u32],
}

/// Owns one ARGB8888 pixel per pixel. Drawing onto another surface borrows both
/// surfaces, so an offscreen surface is never copied just to composite it.
pub struct Surface {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

impl Surface {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![
                0;
                width
                    .checked_mul(height)
                    .expect("surface dimensions overflow")
            ],
        }
    }
    /// Reuse allocation for a scratch target, replacing all pixels with a color.
    pub fn reset(&mut self, width: usize, height: usize, color: u32) {
        self.pixels.resize(
            width
                .checked_mul(height)
                .expect("surface dimensions overflow"),
            color,
        );
        self.pixels.fill(color);
        self.width = width;
        self.height = height;
    }
    pub fn width(&self) -> usize {
        self.width
    }
    pub fn height(&self) -> usize {
        self.height
    }
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }
    pub fn set_pixel(&mut self, x: i32, y: i32, color: u32) {
        if let Some(i) = self.index(x, y) {
            self.pixels[i] = over(color, self.pixels[i]);
        }
    }
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let (x0, y0, x1, y1) = self.bounds(x, y, w, h);
        if color >> 24 == 0 {
            return;
        }
        for yy in y0..y1 {
            let row = &mut self.pixels[yy * self.width + x0..yy * self.width + x1];
            if color >> 24 == 255 {
                row.fill(color);
            } else {
                for pixel in row {
                    *pixel = over(color, *pixel);
                }
            }
        }
    }
    /// Fill a rectangle whose corners are cut to a quarter-circle of `radius`.
    pub fn fill_rounded_rect(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, color: u32) {
        let radius = radius.clamp(0, w.min(h) / 2);
        for row in 0..h {
            for column in 0..w {
                let dx = if column < radius {
                    radius - column
                } else if column >= w - radius {
                    column - (w - radius - 1)
                } else {
                    0
                };
                let dy = if row < radius {
                    radius - row
                } else if row >= h - radius {
                    row - (h - radius - 1)
                } else {
                    0
                };
                if dx * dx + dy * dy <= radius * radius {
                    self.set_pixel(x + column, y + row, color);
                }
            }
        }
    }
    pub fn draw_line(&mut self, mut x0: i32, mut y0: i32, x1: i32, y1: i32, color: u32) {
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.set_pixel(x0, y0, color);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e = 2 * err;
            if e >= dy {
                err += dy;
                x0 += sx;
            }
            if e <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }
    pub fn draw_image(&mut self, x: i32, y: i32, image: Image<'_>) {
        assert!(
            image.pixels.len()
                >= image
                    .width
                    .checked_mul(image.height)
                    .expect("image dimensions overflow")
        );
        // Clip once, before touching pixels; offscreen images cost no per-pixel work.
        let sx = (-(x as i64)).max(0).min(image.width as i64) as usize;
        let sy = (-(y as i64)).max(0).min(image.height as i64) as usize;
        let dx = x.max(0) as usize;
        let dy = y.max(0) as usize;
        let width = image
            .width
            .saturating_sub(sx)
            .min(self.width.saturating_sub(dx));
        let height = image
            .height
            .saturating_sub(sy)
            .min(self.height.saturating_sub(dy));
        if width == 0 || height == 0 {
            return;
        }
        for row in 0..height {
            let src = &image.pixels[(sy + row) * image.width + sx..][..width];
            let dst = &mut self.pixels[(dy + row) * self.width + dx..][..width];
            if src.iter().all(|p| p >> 24 == 255) {
                dst.copy_from_slice(src);
            } else {
                for (dst, &src) in dst.iter_mut().zip(src) {
                    *dst = over(src, *dst);
                }
            }
        }
    }

    /// Nearest-neighbour image scaling, used for configured icons of any size.
    pub fn draw_image_scaled(&mut self, x: i32, y: i32, w: i32, h: i32, image: Image<'_>) {
        if w <= 0 || h <= 0 || image.width == 0 || image.height == 0 {
            return;
        }
        let (x0, y0, x1, y1) = self.bounds(x, y, w, h);
        for row in y0..y1 {
            let source = (row as i32 - y) as usize * image.height / h as usize;
            let source = source.min(image.height - 1) * image.width;
            for column in x0..x1 {
                let offset = (column as i32 - x) as usize * image.width / w as usize;
                let src = image.pixels[source + offset.min(image.width - 1)];
                let dst = &mut self.pixels[row * self.width + column];
                *dst = over(src, *dst);
            }
        }
    }
    /// Filtered image scaling: an area average when the image is reduced, and
    /// bilinear interpolation when it is enlarged.
    pub fn draw_image_smooth(&mut self, x: i32, y: i32, w: i32, h: i32, image: Image<'_>) {
        if w <= 0 || h <= 0 || image.width == 0 || image.height == 0 {
            return;
        }
        let (iw, ih) = (image.width as i64, image.height as i64);
        let enlarging = w as i64 >= iw && h as i64 >= ih;
        let (x0, y0, x1, y1) = self.bounds(x, y, w, h);
        for row in y0..y1 {
            for column in x0..x1 {
                let (dx, dy) = ((column as i32 - x) as i64, (row as i32 - y) as i64);
                let src = if enlarging {
                    // Sample halfway through each destination pixel.
                    let fx = ((dx * 2 + 1) * iw * 64) / w as i64 - 64;
                    let fy = ((dy * 2 + 1) * ih * 64) / h as i64 - 64;
                    bilinear(&image, fx, fy)
                } else {
                    let sx = (dx * iw / w as i64) as usize;
                    let sy = (dy * ih / h as i64) as usize;
                    let ex = (((dx + 1) * iw / w as i64) as usize).clamp(sx + 1, image.width);
                    let ey = (((dy + 1) * ih / h as i64) as usize).clamp(sy + 1, image.height);
                    average(&image, sx, ex, sy, ey)
                };
                let dst = &mut self.pixels[row * self.width + column];
                *dst = over(src, *dst);
            }
        }
    }
    /// Copy a clipped region into a new surface, for screenshots of one window.
    pub fn crop(&self, x: i32, y: i32, w: i32, h: i32) -> Surface {
        let (x0, y0, x1, y1) = self.bounds(x, y, w, h);
        let mut out = Surface::new(x1.saturating_sub(x0), y1.saturating_sub(y0));
        for row in 0..out.height {
            out.pixels[row * out.width..][..out.width]
                .copy_from_slice(&self.pixels[(y0 + row) * self.width + x0..][..out.width]);
        }
        out
    }
    pub fn draw_surface(&mut self, x: i32, y: i32, source: &Surface) {
        self.draw_image(
            x,
            y,
            Image {
                width: source.width,
                height: source.height,
                pixels: &source.pixels,
            },
        );
    }
    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            None
        } else {
            Some(y as usize * self.width + x as usize)
        }
    }
    fn bounds(&self, x: i32, y: i32, w: i32, h: i32) -> (usize, usize, usize, usize) {
        let x0 = x.max(0).min(self.width as i32) as usize;
        let y0 = y.max(0).min(self.height as i32) as usize;
        let x1 = x.saturating_add(w.max(0)).max(0).min(self.width as i32) as usize;
        let y1 = y.saturating_add(h.max(0)).max(0).min(self.height as i32) as usize;
        (x0, y0, x1, y1)
    }
}
/// Mean of a source rectangle, one channel at a time.
fn average(image: &Image<'_>, x0: usize, x1: usize, y0: usize, y1: usize) -> u32 {
    let count = ((x1 - x0) * (y1 - y0)) as u32;
    let mut totals = [0u32; 4];
    for row in y0..y1 {
        for &pixel in &image.pixels[row * image.width + x0..row * image.width + x1] {
            for (total, shift) in totals.iter_mut().zip([0, 8, 16, 24]) {
                *total += (pixel >> shift) & 255;
            }
        }
    }
    totals
        .iter()
        .zip([0, 8, 16, 24])
        .map(|(total, shift)| ((total + count / 2) / count) << shift)
        .sum()
}
/// Bilinear sample at a position in 1/128 pixel units, clamped at the edges.
fn bilinear(image: &Image<'_>, x: i64, y: i64) -> u32 {
    let (fx, fy) = (x.max(0), y.max(0));
    let (x0, y0) = (
        (fx / 128).min(image.width as i64 - 1) as usize,
        (fy / 128).min(image.height as i64 - 1) as usize,
    );
    let (x1, y1) = ((x0 + 1).min(image.width - 1), (y0 + 1).min(image.height - 1));
    let (tx, ty) = ((fx % 128) as u32, (fy % 128) as u32);
    let at = |x: usize, y: usize| image.pixels[y * image.width + x];
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let channel = |pixel: u32| (pixel >> shift) & 255;
        let top = channel(at(x0, y0)) * (128 - tx) + channel(at(x1, y0)) * tx;
        let bottom = channel(at(x0, y1)) * (128 - tx) + channel(at(x1, y1)) * tx;
        let value = (top * (128 - ty) + bottom * ty + 8192) / 16384;
        out |= value.min(255) << shift;
    }
    out
}
/// Straight-alpha source-over compositing, rounded to nearest channel value.
fn over(src: u32, dst: u32) -> u32 {
    let a = src >> 24;
    if a == 255 {
        return src;
    }
    if a == 0 {
        return dst;
    }
    let da = dst >> 24;
    let oa = a + (da * (255 - a) + 127) / 255;
    if oa == 0 {
        return 0;
    }
    let mut out = oa << 24;
    for shift in [0, 8, 16] {
        let s = (src >> shift) & 255;
        let d = (dst >> shift) & 255;
        let prem = s * a + (d * da * (255 - a) + 127) / 255;
        out |= ((prem + oa / 2) / oa).min(255) << shift;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipped_blits_match_per_pixel_reference() {
        let pixels = [
            0xffff0000, 0x8000ff00, 0, 0xff0000ff, 0xffffffff, 0x40123456,
        ];
        for (x, y) in [(-2, -1), (1, 1), (4, 3), (i32::MIN, 0), (i32::MAX, 0)] {
            let mut actual = Surface::new(4, 3);
            let mut expected = Surface::new(4, 3);
            actual.pixels_mut().fill(0x80604020);
            expected.pixels_mut().fill(0x80604020);
            actual.draw_image(
                x,
                y,
                Image {
                    width: 3,
                    height: 2,
                    pixels: &pixels,
                },
            );
            for yy in 0..2 {
                for xx in 0..3 {
                    expected.set_pixel(
                        x.saturating_add(xx),
                        y.saturating_add(yy),
                        pixels[(yy * 3 + xx) as usize],
                    );
                }
            }
            assert_eq!(actual.pixels(), expected.pixels());
        }
    }
    #[test]
    fn clips_draws_and_blends() {
        let mut s = Surface::new(3, 2);
        s.fill_rect(-1, -1, 3, 2, 0xff0000ff);
        s.set_pixel(2, 1, 0xffff0000);
        s.draw_line(0, 0, 2, 1, 0x8000ff00);
        assert_eq!(s.pixels()[0], 0xff00807f);
        assert_eq!(s.pixels()[5], 0xff7f8000);
    }
    #[test]
    fn scales_icons_and_crops_regions() {
        let pixels = [0xffff0000, 0xff00ff00, 0xff0000ff, 0xffffffff];
        let mut s = Surface::new(4, 4);
        s.draw_image_scaled(
            0,
            0,
            4,
            4,
            Image {
                width: 2,
                height: 2,
                pixels: &pixels,
            },
        );
        assert_eq!(s.pixels()[..2], [0xffff0000, 0xffff0000]);
        assert_eq!(s.pixels()[14..], [0xffffffff, 0xffffffff]);
        // Offscreen and zero-sized destinations draw nothing rather than panic.
        let mut empty = Surface::new(2, 2);
        for (x, y, w, h) in [(-9, -9, 4, 4), (0, 0, 0, 4), (9, 9, 4, 4)] {
            empty.draw_image_scaled(
                x,
                y,
                w,
                h,
                Image {
                    width: 2,
                    height: 2,
                    pixels: &pixels,
                },
            );
        }
        assert!(empty.pixels().iter().all(|p| *p == 0));
        // Filtered scaling averages when reducing and interpolates when
        // enlarging, and matches nearest sampling at a 1:1 size.
        let mut reduced = Surface::new(1, 1);
        reduced.draw_image_smooth(
            0,
            0,
            1,
            1,
            Image {
                width: 2,
                height: 2,
                pixels: &pixels,
            },
        );
        assert_eq!(reduced.pixels()[0], 0xff_80_80_80, "mean of the four corners");
        let ramp = [0xff000000u32, 0xff000000, 0xffffffff, 0xffffffff];
        let mut enlarged = Surface::new(4, 1);
        enlarged.draw_image_smooth(
            0,
            0,
            4,
            1,
            Image {
                width: 4,
                height: 1,
                pixels: &ramp,
            },
        );
        assert_eq!(enlarged.pixels(), ramp, "1:1 leaves pixels untouched");
        let mut wide = Surface::new(8, 1);
        wide.draw_image_smooth(
            0,
            0,
            8,
            1,
            Image {
                width: 4,
                height: 1,
                pixels: &ramp,
            },
        );
        let midpoint = wide.pixels()[4] & 255;
        assert!((1..255).contains(&midpoint), "edges blend: {midpoint:#x}");
        let cropped = s.crop(2, 2, 8, 8);
        assert_eq!((cropped.width(), cropped.height()), (2, 2));
        assert_eq!(cropped.pixels(), &[0xffffffff; 4]);
        assert_eq!(s.crop(-4, 0, 2, 2).pixels().len(), 0);
    }
    #[test]
    fn composites_borrowed_surface() {
        let mut src = Surface::new(1, 1);
        src.set_pixel(0, 0, 0x80ff0000);
        let mut dst = Surface::new(2, 1);
        dst.fill_rect(0, 0, 2, 1, 0xff0000ff);
        dst.draw_surface(1, 0, &src);
        assert_eq!(dst.pixels()[1], 0xff80007f);
    }
}
