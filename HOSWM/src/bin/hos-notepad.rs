//! A tiny keyboard-first text editor for hOS.
use hoswm::{
    client::{Client, Event, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT, WINDOW_RESIZABLE},
    font::Font,
    surface::Surface,
};
use std::{env, fs, thread, time::Duration};

const BG: u32 = 0xff101815;
const PANEL: u32 = 0xff1b2b25;
const TEXT: u32 = 0xffdfe8e2;
const DIM: u32 = 0xff8d9a93;
const ACCENT: u32 = 0xff72dbac;

struct Editor {
    lines: Vec<Vec<char>>,
    row: usize,
    col: usize,
    path: Option<String>,
    dirty: bool,
    status: String,
}

impl Editor {
    fn new(path: Option<String>) -> Self {
        let mut editor = Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            path,
            dirty: false,
            status: "Ctrl+S saves   Esc closes".into(),
        };
        if let Some(path) = editor.path.clone() {
            match fs::read_to_string(&path) {
                Ok(text) => {
                    editor.lines = text
                        .split('\n')
                        .map(|line| line.chars().collect())
                        .collect();
                    if editor.lines.is_empty() {
                        editor.lines.push(Vec::new());
                    }
                    editor.status = format!("Opened {path}");
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    editor.status = format!("New file: {path}");
                }
                Err(error) => editor.status = format!("Could not open {path}: {error}"),
            }
        }
        editor
    }

    fn save(&mut self) {
        let Some(path) = self.path.as_deref() else {
            self.status = "Start with: hos-notepad FILE".into();
            return;
        };
        let text = self
            .lines
            .iter()
            .map(|line| line.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        match fs::write(path, text) {
            Ok(()) => {
                self.dirty = false;
                self.status = format!("Saved {path}");
            }
            Err(error) => self.status = format!("Save failed: {error}"),
        }
    }

    fn insert(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            self.lines[self.row].remove(self.col);
        } else if self.row > 0 {
            let tail = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].extend(tail);
        } else {
            return;
        }
        self.dirty = true;
    }

    fn delete(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(next);
        } else {
            return;
        }
        self.dirty = true;
    }

    fn enter(&mut self) {
        let tail = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
        self.dirty = true;
    }

    fn key(&mut self, text: &str, control: u32) {
        if control & 1 != 0 {
            if text.as_bytes() == [19] {
                self.save();
            }
            return;
        }
        match text.as_bytes() {
            [27, 91, 68] => self.col = self.col.saturating_sub(1), // left
            [27, 91, 67] => self.col = (self.col + 1).min(self.lines[self.row].len()), // right
            [27, 91, 65] => {
                self.row = self.row.saturating_sub(1);
                self.col = self.col.min(self.lines[self.row].len());
            }
            [27, 91, 66] => {
                self.row = (self.row + 1).min(self.lines.len() - 1);
                self.col = self.col.min(self.lines[self.row].len());
            }
            [27, 91, 72] => self.col = 0,
            [27, 91, 70] => self.col = self.lines[self.row].len(),
            [27, 91, 51, 126] => self.delete(),
            [8] | [127] => self.backspace(),
            [13] | [10] => self.enter(),
            _ => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    self.insert(c);
                }
            }
        }
    }

    fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        let width = surface.width() as i32;
        let height = surface.height() as i32;
        surface.pixels_mut().fill(BG);
        surface.fill_rect(0, 0, width, 28, PANEL);
        font.draw(surface, 10, 8, "Notepad", ACCENT);
        font.draw(
            surface,
            90,
            8,
            self.path.as_deref().unwrap_or("untitled.txt"),
            TEXT,
        );
        let visible = ((height - 48) / 14).max(1) as usize;
        let start = self.row.saturating_sub(visible - 1);
        let max_chars = ((width - 48) / 8).max(1) as usize;
        for (screen_row, line) in self.lines.iter().skip(start).take(visible).enumerate() {
            let y = 36 + screen_row as i32 * 14;
            font.draw(
                surface,
                10,
                y,
                &format!("{:>3}", start + screen_row + 1),
                DIM,
            );
            let text: String = line.iter().skip(0).take(max_chars).collect();
            font.draw(surface, 42, y, &text, TEXT);
        }
        if self.row >= start {
            let cursor_y = 36 + (self.row - start) as i32 * 14;
            let cursor_x = 42 + self.col.min(max_chars) as i32 * 8;
            surface.fill_rect(cursor_x, cursor_y - 1, 2, 12, ACCENT);
        }
        surface.fill_rect(0, height - 20, width, 20, PANEL);
        let marker = if self.dirty { "*" } else { "" };
        font.draw(
            surface,
            10,
            height - 15,
            &format!("{marker}{}", self.status),
            DIM,
        );
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args().nth(1);
    let client = Client::connect()?;
    let window = client.create("Notepad", 620, 420, ACCENT)?;
    client.flags(
        window,
        WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE | WINDOW_RESIZABLE,
    )?;
    let font = Font::builtin();
    let mut surface = Surface::new(620, 420);
    let mut editor = Editor::new(path);
    loop {
        let (width, height, minimized) = client.size(window)?;
        if minimized {
            thread::sleep(Duration::from_millis(40));
            continue;
        }
        if (width as usize, height as usize) != (surface.width(), surface.height()) {
            surface.reset(width as usize, height as usize, BG);
        }
        editor.draw(&mut surface, &font);
        client.present(window, width, height, surface.pixels())?;
        while let Some(Event {
            kind,
            control,
            text,
        }) = client.poll(window)?
        {
            match kind {
                6 => editor.key(&text, control),
                7 | 9 => {
                    let _ = client.close(window);
                    return Ok(());
                }
                _ => {}
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("hos-notepad: {error}");
    }
}
