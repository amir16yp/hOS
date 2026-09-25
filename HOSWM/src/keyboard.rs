//! US evdev keymap with independent modifiers, repeat, and terminal key sequences.
#[derive(Default)]
pub struct Keyboard {
    left_shift: bool,
    right_shift: bool,
    left_ctrl: bool,
    right_ctrl: bool,
    left_alt: bool,
    right_alt: bool,
    caps: bool,
}
impl Keyboard {
    pub fn modifiers(&self) -> crate::shortcuts::Modifiers {
        crate::shortcuts::Modifiers {
            ctrl: self.left_ctrl || self.right_ctrl,
            shift: self.left_shift || self.right_shift,
            alt: self.left_alt || self.right_alt,
        }
    }

    /// Forget held keys after an input overflow or a device disconnecting,
    /// so a modifier released while HOSWM was not listening does not stick.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
    pub fn event(&mut self, code: u16, value: i32) -> Option<Vec<u8>> {
        match code {
            42 => self.left_shift = value != 0,
            54 => self.right_shift = value != 0,
            29 => self.left_ctrl = value != 0,
            97 => self.right_ctrl = value != 0,
            56 => self.left_alt = value != 0,
            100 => self.right_alt = value != 0,
            58 if value == 1 => self.caps = !self.caps,
            _ => (),
        }
        if value == 0 {
            return None;
        }
        let special = match code {
            1 => Some("\x1b"),
            14 => Some("\x7f"),
            15 => Some("\t"),
            28 | 96 => Some("\r"),
            103 => Some("\x1b[A"),
            108 => Some("\x1b[B"),
            106 => Some("\x1b[C"),
            105 => Some("\x1b[D"),
            102 => Some("\x1b[H"),
            107 => Some("\x1b[F"),
            110 => Some("\x1b[2~"),
            111 => Some("\x1b[3~"),
            104 => Some("\x1b[5~"),
            109 => Some("\x1b[6~"),
            _ => None,
        };
        if let Some(s) = special {
            return Some(s.as_bytes().to_vec());
        }
        let normal = match code {
            2..=13 => b"1234567890-="[(code - 2) as usize],
            16..=27 => b"qwertyuiop[]"[(code - 16) as usize],
            30..=41 => b"asdfghjkl;'`"[(code - 30) as usize],
            43 => b'\\',
            44..=53 => b"zxcvbnm,./"[(code - 44) as usize],
            57 => b' ',
            _ => return None,
        };
        let shift = self.left_shift || self.right_shift;
        let mut c = normal;
        if normal.is_ascii_alphabetic() {
            if shift ^ self.caps {
                c = c.to_ascii_uppercase();
            }
        } else if shift {
            let plain = b"1234567890-=[];'`\\,./";
            let shifted = b"!@#$%^&*()_+{}:\"~|<>?";
            if let Some(i) = plain.iter().position(|b| *b == normal) {
                c = shifted[i];
            }
        }
        if self.left_ctrl || self.right_ctrl {
            c = match c {
                b'@' | b' ' => 0,
                b'a'..=b'z' => c - b'a' + 1,
                b'A'..=b'_' => c - 64,
                b'?' => 127,
                _ => c,
            };
        }
        let mut out = Vec::new();
        if self.left_alt || self.right_alt {
            out.push(27);
        }
        out.push(c);
        Some(out)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modifiers() {
        let mut k = Keyboard::default();
        k.event(42, 1);
        assert_eq!(k.event(30, 1).unwrap(), b"A");
        k.event(54, 1);
        k.event(42, 0);
        assert_eq!(k.event(2, 2).unwrap(), b"!");
        k.event(54, 0);
        k.event(29, 1);
        assert_eq!(k.event(46, 1).unwrap(), [3]);
        k.event(56, 1);
        assert!(k.modifiers().alt && k.modifiers().ctrl);
        k.reset();
        assert!(!k.modifiers().alt && !k.modifiers().ctrl);
    }
}
