//! Shared cursor image and allocation-free software cursor save/restore.
use crate::surface::Surface;
pub fn pixel(x: usize, y: usize) -> u32 {
    if y >= 18 || x > y / 2 {
        0
    } else if x == 0 || x == y / 2 || y == 17 {
        0xff000000
    } else {
        0xffffffff
    }
}
#[derive(Default)]
pub struct SoftwareCursor {
    saved: Vec<(usize, u32)>,
}
impl SoftwareCursor {
    pub fn hide(&mut self, surface: &mut Surface) {
        for (index, color) in self.saved.drain(..) {
            surface.pixels_mut()[index] = color;
        }
    }
    pub fn show(&mut self, surface: &mut Surface, x: i32, y: i32) {
        self.hide(surface);
        for dy in 0..18 {
            for dx in 0..9 {
                let color = pixel(dx, dy);
                let (px, py) = (x + dx as i32, y + dy as i32);
                if color != 0
                    && px >= 0
                    && py >= 0
                    && (px as usize) < surface.width()
                    && (py as usize) < surface.height()
                {
                    let index = py as usize * surface.width() + px as usize;
                    self.saved.push((index, surface.pixels()[index]));
                    surface.pixels_mut()[index] = color;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_restores_background_at_edges_and_after_redraw() {
        let mut surface = Surface::new(800, 600);
        for (i, p) in surface.pixels_mut().iter_mut().enumerate() {
            *p = 0xff000000 | i as u32;
        }
        let original = surface.pixels().to_vec();
        let mut cursor = SoftwareCursor::default();
        for (x, y) in [(0, 0), (799, 599), (-4, -7), (400, 300)] {
            cursor.show(&mut surface, x, y);
            cursor.hide(&mut surface);
            assert_eq!(surface.pixels(), &original);
        }
        surface.pixels_mut().fill(0xff123456);
        cursor.show(&mut surface, 10, 10);
        cursor.hide(&mut surface);
        assert!(surface.pixels().iter().all(|p| *p == 0xff123456));
    }
}
