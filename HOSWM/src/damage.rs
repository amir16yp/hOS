//! Exact scanline damage against the last successfully presented logical frame.
pub(crate) struct Damage {
    previous: Vec<u32>,
    valid: bool,
}
impl Damage {
    pub fn new() -> Self {
        Self {
            previous: vec![0; 800 * 600],
            valid: false,
        }
    }
    pub fn rows(&self, pixels: &[u32]) -> Vec<std::ops::Range<usize>> {
        assert_eq!(pixels.len(), self.previous.len());
        pixels
            .chunks_exact(800)
            .zip(self.previous.chunks_exact(800))
            .map(|(new, old)| {
                if !self.valid {
                    return 0..800;
                }
                // Slice equality uses the platform's optimized memory comparison.
                if new == old {
                    return 0..0;
                }
                let first = new.iter().zip(old).position(|(a, b)| a != b).unwrap();
                let last = new.iter().zip(old).rposition(|(a, b)| a != b).unwrap();
                first..last + 1
            })
            .collect()
    }
    pub fn commit(&mut self, pixels: &[u32]) {
        self.previous.copy_from_slice(pixels);
        self.valid = true;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_unchanged_and_disjoint_damage() {
        let mut d = Damage::new();
        let mut p = vec![0; 800 * 600];
        assert!(d.rows(&p).iter().all(|r| *r == (0..800)));
        d.commit(&p);
        assert!(d.rows(&p).iter().all(|r| r.is_empty()));
        p[803] = 7;
        p[809] = 9;
        p[479999] = 1;
        let rows = d.rows(&p);
        assert_eq!(rows[1], 3..10);
        assert_eq!(rows[599], 799..800);
        assert!(rows[0].is_empty());
        d.commit(&p);
        assert!(d.rows(&p).iter().all(|r| r.is_empty()));
    }
}
