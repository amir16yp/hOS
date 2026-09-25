use hoswm::{
    client::{
        Client, Event, Menu, MenuItem, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT, WINDOW_RESIZABLE,
    },
    font::Font,
    surface::Surface,
};
use std::{thread, time::Duration};
mod terminal {
    //! PTY-backed shell and ANSI/VT screen (16/256/true-color SGR, cursor, erase, scroll).
    use hoswm::{font::Font, surface::Surface};
    use std::{
        collections::VecDeque,
        fs::{File, OpenOptions},
        io::{self, Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::{fs::OpenOptionsExt, process::CommandExt},
        },
        process::{Child, Command, Stdio},
    };
    unsafe extern "C" {
        fn posix_openpt(flags: i32) -> i32;
        fn grantpt(fd: i32) -> i32;
        fn unlockpt(fd: i32) -> i32;
        fn ptsname_r(fd: i32, buf: *mut u8, len: usize) -> i32;
        fn setsid() -> i32;
        fn ioctl(fd: i32, request: u64, ...) -> i32;
        fn kill(pid: i32, sig: i32) -> i32;
    }
    const COLORS: [u32; 8] = [
        0xff000000, 0xffef6976, 0xff73d8a0, 0xffe4c878, 0xff7aabef, 0xffbb8cde, 0xff77cdd0,
        0xffdedede,
    ];
    fn ansi_color(n: usize) -> u32 {
        const BRIGHT: [u32; 8] = [
            0xff555555, 0xffff5555, 0xff55ff55, 0xffffff55, 0xff5555ff, 0xffff55ff, 0xff55ffff,
            0xffffffff,
        ];
        match n {
            0..=7 => COLORS[n],
            8..=15 => BRIGHT[n - 8],
            16..=231 => {
                let v = n - 16;
                let level = |c: usize| if c == 0 { 0 } else { 55 + c as u32 * 40 };
                0xff000000 | level(v / 36) << 16 | level(v / 6 % 6) << 8 | level(v % 6)
            }
            _ => {
                let gray = 8 + (n.min(255) - 232) as u32 * 10;
                0xff000000 | gray << 16 | gray << 8 | gray
            }
        }
    }
    #[derive(Clone, Copy)]
    struct Cell {
        c: char,
        fg: u32,
        bg: u32,
    }
    pub struct Screen {
        pub selection: hoswm::text::TextSelection,
        cols: usize,
        rows: usize,
        cells: Vec<Cell>,
        x: usize,
        y: usize,
        saved: (usize, usize),
        fg: u32,
        bg: u32,
        bold: bool,
        inverse: bool,
        escape: Vec<u8>,
        state: u8,
        wrap: bool,
        pub cursor: bool,
        history: VecDeque<Vec<Cell>>,
        offset: usize,
        bracketed_paste: bool,
    }
    impl Screen {
        pub fn new(cols: usize, rows: usize) -> Self {
            let mut s = Self {
                selection: hoswm::text::TextSelection {
                    selectable: true,
                    ..Default::default()
                },
                cols: cols.max(1),
                rows: rows.max(1),
                cells: Vec::new(),
                x: 0,
                y: 0,
                saved: (0, 0),
                fg: COLORS[7],
                bg: COLORS[0],
                bold: false,
                inverse: false,
                escape: Vec::new(),
                state: 0,
                wrap: false,
                cursor: true,
                history: VecDeque::new(),
                offset: 0,
                bracketed_paste: false,
            };
            s.cells = vec![s.blank(); s.cols * s.rows];
            s
        }
        fn blank(&self) -> Cell {
            Cell {
                c: ' ',
                fg: self.fg,
                bg: self.bg,
            }
        }
        fn newline(&mut self) {
            self.y += 1;
            if self.y >= self.rows {
                self.history.push_back(self.cells[..self.cols].to_vec());
                if self.history.len() > 2000 {
                    self.history.pop_front();
                }
                if self.offset > 0 {
                    self.offset = (self.offset + 1).min(self.history.len());
                }
                self.cells.copy_within(self.cols.., 0);
                let blank = self.blank();
                self.cells[(self.rows - 1) * self.cols..].fill(blank);
                self.y = self.rows - 1;
            }
        }
        fn visible_cell(&self, index: usize) -> Cell {
            let row = self.history.len() - self.offset + index / self.cols;
            let col = index % self.cols;
            if row < self.history.len() {
                self.history[row]
                    .get(col)
                    .copied()
                    .unwrap_or_else(|| self.blank())
            } else {
                self.cells[(row - self.history.len()) * self.cols + col]
            }
        }
        pub fn scroll(&mut self, up: bool) {
            let step = self.rows.saturating_sub(1).max(1);
            self.offset = if up {
                (self.offset + step).min(self.history.len())
            } else {
                self.offset.saturating_sub(step)
            };
            self.selection.anchor = 0;
            self.selection.caret = 0;
        }
        fn paste_bytes(&self, value: &str) -> Vec<u8> {
            // Strip control sequences, including forged bracketed-paste endings.
            let clean: String = value
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .chars()
                .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
                .collect();
            if self.bracketed_paste {
                format!("\x1b[200~{clean}\x1b[201~").into_bytes()
            } else {
                clean.replace('\n', " ").into_bytes()
            }
        }
        pub fn select_at(&mut self, x: i32, y: i32, start: bool) {
            let col = ((x - 6 + 4).max(0) / 8).min(self.cols as i32) as usize;
            let row = ((y - 6).max(0) / 12).min(self.rows as i32 - 1) as usize;
            self.selection.caret = row * self.cols + col;
            if start {
                self.selection.anchor = self.selection.caret;
            }
        }
        /// Return to live output after scrolling back through history.
        pub fn scroll_to_bottom(&mut self) {
            self.offset = 0;
        }
        pub fn select_all(&mut self) {
            self.selection.anchor = 0;
            self.selection.caret = self.cells.len();
        }
        pub fn selected_text(&self) -> String {
            let range = self.selection.range();
            let mut lines = Vec::new();
            for row in 0..self.rows {
                let start = range.start.max(row * self.cols);
                let end = range.end.min((row + 1) * self.cols);
                if start < end {
                    lines.push(
                        (start..end)
                            .map(|i| self.visible_cell(i).c)
                            .collect::<String>()
                            .trim_end()
                            .to_string(),
                    );
                }
            }
            lines.join("\n")
        }
        pub fn feed(&mut self, data: &[u8]) {
            // Output can scroll or overwrite cells: do not retain a stale selection.
            if !data.is_empty() {
                self.selection.anchor = 0;
                self.selection.caret = 0;
            }

            for &b in data {
                if self.state == 3 {
                    if b == 7 {
                        self.state = 0;
                    } else if b == 27 {
                        self.state = 4;
                    }
                    continue;
                }
                if self.state == 4 {
                    self.state = if b == b'\\' { 0 } else { 3 };
                    continue;
                }
                if self.state == 1 {
                    self.state = 0;
                    match b {
                        b'[' => {
                            self.state = 2;
                            self.escape.clear();
                        }
                        b']' => self.state = 3,
                        b'7' => self.saved = (self.x, self.y),
                        b'8' => {
                            self.x = self.saved.0.min(self.cols - 1);
                            self.y = self.saved.1.min(self.rows - 1);
                        }
                        b'c' => *self = Self::new(self.cols, self.rows),
                        _ => (),
                    }
                    continue;
                }
                if self.state == 2 {
                    if (0x40..=0x7e).contains(&b) {
                        self.csi(b);
                        self.state = 0;
                    } else if self.escape.len() < 128 {
                        self.escape.push(b);
                    } else {
                        self.state = 0;
                    }
                    continue;
                }
                match b {
                    27 => self.state = 1,
                    b'\r' => {
                        self.x = 0;
                        self.wrap = false;
                    }
                    b'\n' => {
                        self.wrap = false;
                        self.newline();
                    }
                    8 => {
                        self.x = self.x.saturating_sub(1);
                        self.wrap = false;
                    }
                    b'\t' => {
                        self.x = ((self.x / 8 + 1) * 8).min(self.cols - 1);
                        self.wrap = false;
                    }
                    32..=126 | 128..=255 => {
                        if self.wrap {
                            self.x = 0;
                            self.newline();
                            self.wrap = false;
                        }
                        let fg = if self.bold {
                            COLORS
                                .iter()
                                .position(|color| *color == self.fg)
                                .map_or(self.fg, |n| ansi_color(n + 8))
                        } else {
                            self.fg
                        };
                        self.cells[self.y * self.cols + self.x] = Cell {
                            c: if b < 128 { b as char } else { '?' },
                            fg: if self.inverse { self.bg } else { fg },
                            bg: if self.inverse { fg } else { self.bg },
                        };
                        if self.x + 1 == self.cols {
                            self.wrap = true;
                        } else {
                            self.x += 1;
                        }
                    }
                    _ => (),
                }
            }
        }
        fn csi(&mut self, b: u8) {
            let text = String::from_utf8_lossy(&self.escape);
            let private = text.starts_with('?');
            let p: Vec<usize> = text
                .trim_start_matches('?')
                .split(';')
                .map(|s| s.parse::<usize>().unwrap_or(0).min(65535))
                .collect();
            let n = p.first().copied().unwrap_or(0);
            let a = n.max(1);
            let blank = self.blank();
            match b {
                b'A' => self.y = self.y.saturating_sub(a),
                b'B' => self.y = (self.y + a).min(self.rows - 1),
                b'C' => self.x = (self.x + a).min(self.cols - 1),
                b'D' => self.x = self.x.saturating_sub(a),
                b'H' | b'f' => {
                    self.y = a.saturating_sub(1).min(self.rows - 1);
                    self.x = p
                        .get(1)
                        .copied()
                        .unwrap_or(1)
                        .max(1)
                        .saturating_sub(1)
                        .min(self.cols - 1);
                }
                b'G' => self.x = (a - 1).min(self.cols - 1),
                b'd' => self.y = (a - 1).min(self.rows - 1),
                b'J' => {
                    let i = self.y * self.cols + self.x;
                    match n {
                        0 => self.cells[i..].fill(blank),
                        1 => self.cells[..=i].fill(blank),
                        2 => self.cells.fill(blank),
                        3 => {
                            self.history.clear();
                            self.offset = 0;
                        }
                        _ => (),
                    }
                }
                b'K' => {
                    let row = self.y * self.cols;
                    match n {
                        0 => self.cells[row + self.x..row + self.cols].fill(blank),
                        1 => self.cells[row..=row + self.x].fill(blank),
                        2 => self.cells[row..row + self.cols].fill(blank),
                        _ => (),
                    }
                }
                b'm' => {
                    let mut params = p.into_iter();
                    while let Some(n) = params.next() {
                        match n {
                            0 => {
                                self.fg = COLORS[7];
                                self.bg = COLORS[0];
                                self.bold = false;
                                self.inverse = false;
                            }
                            30..=37 => self.fg = COLORS[n - 30],
                            40..=47 => self.bg = COLORS[n - 40],
                            90..=97 => self.fg = ansi_color(n - 90 + 8),
                            100..=107 => self.bg = ansi_color(n - 100 + 8),
                            38 | 48 => {
                                let color = match params.next() {
                                    Some(5) => params.next().filter(|v| *v < 256).map(ansi_color),
                                    Some(2) => {
                                        let rgb = (params.next(), params.next(), params.next());
                                        match rgb {
                                            (Some(r), Some(g), Some(b))
                                                if r < 256 && g < 256 && b < 256 =>
                                            {
                                                Some(
                                                    0xff000000
                                                        | (r as u32) << 16
                                                        | (g as u32) << 8
                                                        | b as u32,
                                                )
                                            }
                                            _ => None,
                                        }
                                    }
                                    _ => None,
                                };
                                if let Some(color) = color {
                                    if n == 38 {
                                        self.fg = color;
                                    } else {
                                        self.bg = color;
                                    }
                                }
                            }
                            39 => self.fg = COLORS[7],
                            49 => self.bg = COLORS[0],
                            1 => self.bold = true,
                            22 => self.bold = false,
                            7 => self.inverse = true,
                            27 => self.inverse = false,
                            _ => (),
                        }
                    }
                }
                b's' => self.saved = (self.x, self.y),
                b'u' => {
                    self.x = self.saved.0.min(self.cols - 1);
                    self.y = self.saved.1.min(self.rows - 1);
                }
                b'h' | b'l' if private => {
                    for mode in p {
                        match mode {
                            25 => self.cursor = b == b'h',
                            2004 => self.bracketed_paste = b == b'h',
                            _ => (),
                        }
                    }
                }
                _ => (),
            }
            if b != b'm' {
                self.wrap = false;
            }
        }
        pub fn resize(&mut self, cols: usize, rows: usize) {
            let (cols, rows) = (cols.max(1), rows.max(1));
            if (cols, rows) == (self.cols, self.rows) {
                return;
            }
            let mut next = Self::new(cols, rows);
            for y in 0..rows.min(self.rows) {
                for x in 0..cols.min(self.cols) {
                    next.cells[y * cols + x] = self.cells[y * self.cols + x];
                }
            }
            next.x = self.x.min(cols - 1);
            next.y = self.y.min(rows - 1);
            next.fg = self.fg;
            next.bg = self.bg;
            next.bold = self.bold;
            next.inverse = self.inverse;
            next.history = std::mem::take(&mut self.history);
            next.offset = self.offset;
            next.bracketed_paste = self.bracketed_paste;
            next.cursor = self.cursor;
            next.saved = (self.saved.0.min(cols - 1), self.saved.1.min(rows - 1));
            next.escape = std::mem::take(&mut self.escape);
            next.state = self.state;
            *self = next;
        }
        pub fn draw(&self, s: &mut Surface, f: &Font<'_>) {
            s.pixels_mut().fill(COLORS[0]);
            for y in 0..self.rows {
                for x in 0..self.cols {
                    let c = self.visible_cell(y * self.cols + x);
                    let selected = self.selection.range().contains(&(y * self.cols + x));
                    s.fill_rect(
                        (x * 8 + 6) as i32,
                        (y * 12 + 6) as i32,
                        8,
                        12,
                        if selected {
                            hoswm::text::SELECTION_COLOR
                        } else {
                            c.bg
                        },
                    );
                    f.draw(
                        s,
                        (x * 8 + 6) as i32,
                        (y * 12 + 6) as i32,
                        &c.c.to_string(),
                        if selected { 0xffffffff } else { c.fg },
                    );
                }
            }
            if self.cursor && self.offset == 0 {
                s.fill_rect(
                    (self.x * 8 + 6) as i32,
                    (self.y * 12 + 16) as i32,
                    8,
                    2,
                    COLORS[2],
                );
            }
        }
    }
    pub struct Terminal {
        master: File,
        child: Child,
        pending: VecDeque<u8>,
        pub screen: Screen,
        pub exited: bool,
    }
    impl Terminal {
        pub fn spawn(w: usize, h: usize) -> io::Result<Self> {
            let fd = unsafe { posix_openpt(2 | 0x100 | 0x800 | 0x80000) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let master = unsafe { File::from_raw_fd(fd) };
            if unsafe { grantpt(fd) } < 0 || unsafe { unlockpt(fd) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut name = [0u8; 256];
            let error = unsafe { ptsname_r(fd, name.as_mut_ptr(), name.len()) };
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
            let path = std::ffi::CStr::from_bytes_until_nul(&name)
                .map_err(io::Error::other)?
                .to_str()
                .map_err(io::Error::other)?;
            let slave = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(0x100)
                .open(path)?;
            let mut command = Command::new("/bin/bash");
            command
                .args(["--rcfile", "/etc/bash.bashrc", "-i"])
                .env("TERM", "ansi")
                .env("HOME", "/root")
                .env("SHELL", "/bin/bash")
                .stdin(Stdio::from(slave.try_clone()?))
                .stdout(Stdio::from(slave.try_clone()?))
                .stderr(Stdio::from(slave));
            unsafe {
                command.pre_exec(|| {
                    if setsid() < 0 || ioctl(0, 0x540e, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let size = [
                h.saturating_sub(12).div_euclid(12).max(1) as u16,
                w.saturating_sub(12).div_euclid(8).max(1) as u16,
                0,
                0,
            ];
            if unsafe { ioctl(fd, 0x5414, size.as_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            let child = command.spawn()?;
            Ok(Self {
                master,
                child,
                pending: VecDeque::new(),
                screen: Screen::new(size[1] as usize, size[0] as usize),
                exited: false,
            })
        }
        pub fn resize(&mut self, w: usize, h: usize) {
            let cols = w.saturating_sub(12).div_euclid(8).max(1);
            let rows = h.saturating_sub(12).div_euclid(12).max(1);
            if (cols, rows) == (self.screen.cols, self.screen.rows) {
                return;
            }
            self.screen.resize(cols, rows);
            let size = [rows as u16, cols as u16, 0, 0];
            unsafe {
                ioctl(self.master.as_raw_fd(), 0x5414, size.as_ptr());
            }
        }
        pub fn paste(&mut self, value: &str) {
            self.input(&self.screen.paste_bytes(value));
        }
        pub fn input(&mut self, bytes: &[u8]) {
            self.screen.offset = 0;
            if !self.exited && self.pending.len() + bytes.len() < 65536 {
                self.pending.extend(bytes);
            }
        }
        pub fn tick(&mut self) -> bool {
            if self.exited {
                return false;
            }
            let mut dirty = false;
            while !self.pending.is_empty() {
                match self.master.write(self.pending.make_contiguous()) {
                    Ok(0) => break,
                    Ok(n) => {
                        self.pending.drain(..n);
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let mut buf = [0u8; 4096];
            for _ in 0..16 {
                match self.master.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        self.screen.feed(&buf[..n]);
                        dirty = true;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                self.exited = true;
                self.screen
                    .feed(format!("\r\n[Shell exited: {status}]\r\n").as_bytes());
                dirty = true;
            }
            dirty
        }
    }
    impl Drop for Terminal {
        fn drop(&mut self) {
            if !self.exited {
                unsafe {
                    kill(-(self.child.id() as i32), 1);
                }
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn extended_colors_and_selection() {
            let mut s = Screen::new(20, 2);
            s.feed(b"\x1b[38;5;196mR\x1b[48;5;232mB\x1b[38;2;1;2;");
            s.feed(b"3mT\x1b[48;2;4;5;6mU\x1b[0mD");
            assert_eq!(s.cells[0].fg, 0xffff0000);
            assert_eq!(s.cells[1].bg, 0xff080808);
            assert_eq!(s.cells[2].fg, 0xff010203);
            assert_eq!(s.cells[3].bg, 0xff040506);
            assert_eq!(s.cells[4].fg, COLORS[7]);
            assert_eq!(s.cells[4].bg, COLORS[0]);
            s.feed(b"\x1b[1;31mB\x1b[22;7;7mI\x1b[27mN");
            assert_eq!(s.cells[5].fg, ansi_color(9));
            assert_eq!(s.cells[6].bg, COLORS[1]);
            assert_eq!(s.cells[7].fg, COLORS[1]);
            s.select_at(6, 6, true);
            s.select_at(30, 6, false);
            assert_eq!(s.selected_text(), "RBT");
            let before = s.cells[0].fg;
            let mut surface = Surface::new(180, 40);
            s.draw(&mut surface, &Font::builtin());
            assert!(surface.pixels().contains(&hoswm::text::SELECTION_COLOR));
            assert_eq!(s.cells[0].fg, before);
            s.feed(b"!");
            assert_eq!(s.selected_text(), "");
        }
        #[test]
        fn scrollback_selection_resize_and_limit() {
            let mut s = Screen::new(4, 2);
            s.feed(b"one\r\ntwo\r\ntri");
            s.scroll(true);
            s.select_all();
            assert_eq!(s.selected_text(), "one\ntwo");
            s.resize(6, 3);
            s.select_all();
            assert_eq!(s.selected_text(), "one\ntwo\ntri");
            s.feed(b"\x1b[3J");
            assert!(s.history.is_empty());
            for _ in 0..2010 {
                s.feed(b"\r\nx");
            }
            assert_eq!(s.history.len(), 2000);
        }
        #[test]
        fn paste_modes_survive_resize_and_filter_controls() {
            let mut s = Screen::new(10, 2);
            assert_eq!(s.paste_bytes("a\r\nb\x03\x1b"), b"a b");
            s.feed(b"\x1b[?25;2004h");
            s.resize(12, 3);
            assert_eq!(s.paste_bytes("a\nb"), b"\x1b[200~a\nb\x1b[201~");
            s.feed(b"\x1b[?2004l");
            assert_eq!(s.paste_bytes("a\nb"), b"a b");
        }
        #[test]
        fn real_shell_pty() {
            let mut terminal = Terminal::spawn(644, 372).expect("host needs /dev/pts for PTY test");
            terminal.input(b"printf '\\033[32mPTY-%s-OK\\033[0m\\n' shell; exit\r");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !terminal.exited && std::time::Instant::now() < deadline {
                terminal.tick();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(terminal.exited, "shell did not exit");
            let output: String = terminal.screen.cells.iter().map(|c| c.c).collect();
            assert!(output.contains("PTY-shell-OK"), "{output}");
        }
        #[test]
        fn clear_homes_cursor_and_cancels_pending_wrap() {
            let mut s = Screen::new(4, 2);
            s.feed(b"1234\r\n5678");
            // clear sends home followed by erase-display.
            s.feed(b"\x1b[H\x1b[J");
            assert!(s.cells.iter().all(|c| c.c == ' '));
            assert_eq!((s.x, s.y), (0, 0));
            s.feed(b"hos$");
            assert_eq!(s.cells[0].c, 'h');
            assert_eq!(s.cells[4].c, ' ');
        }
        #[test]
        fn ansi_scroll_resize() {
            let mut s = Screen::new(4, 2);
            s.feed(b"1234\r\n5678\r\nX");
            assert_eq!(s.cells[0].c, '5');
            assert_eq!(s.cells[4].c, 'X');
            s.feed(b"\x1b[2J\x1b[H\x1b[31mR");
            assert_eq!(s.cells[0].fg, COLORS[1]);
            assert_eq!(s.cells[1].c, ' ');
            s.resize(2, 1);
            assert_eq!(s.cells[0].c, 'R');
        }
    }
}
use terminal::Terminal;

const MENU_INTERRUPT: u32 = 1;
const MENU_CLEAR: u32 = 2;
const MENU_CLEAR_SCROLLBACK: u32 = 3;
const MENU_COPY: u32 = 4;
const MENU_PASTE: u32 = 5;
const MENU_SELECT_ALL: u32 = 6;
const MENU_PAGE_UP: u32 = 7;
const MENU_PAGE_DOWN: u32 = 8;
const MENU_BOTTOM: u32 = 9;

/// The same actions as the keyboard shortcuts, in the screen-top menu bar.
fn menus() -> Vec<Menu> {
    vec![
        Menu::new(
            "Terminal",
            vec![
                MenuItem::new(MENU_INTERRUPT, "Send interrupt").shortcut("Ctrl+C"),
                MenuItem::rule(),
                MenuItem::new(MENU_CLEAR, "Clear screen"),
                MenuItem::new(MENU_CLEAR_SCROLLBACK, "Clear scrollback"),
            ],
        ),
        Menu::new(
            "Edit",
            vec![
                MenuItem::new(MENU_COPY, "Copy").shortcut("Ctrl+Shift+C"),
                MenuItem::new(MENU_PASTE, "Paste").shortcut("Ctrl+Shift+V"),
                MenuItem::rule(),
                MenuItem::new(MENU_SELECT_ALL, "Select all").shortcut("Ctrl+Shift+A"),
            ],
        ),
        Menu::new(
            "View",
            vec![
                MenuItem::new(MENU_PAGE_UP, "Scroll up").shortcut("Shift+PageUp"),
                MenuItem::new(MENU_PAGE_DOWN, "Scroll down").shortcut("Shift+PageDown"),
                MenuItem::new(MENU_BOTTOM, "Scroll to bottom"),
            ],
        ),
    ]
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let c = Client::connect()?;
    let w = c.create("Terminal", 644, 372, 0xff72dbac)?;
    c.flags(w, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE | WINDOW_RESIZABLE)?;
    c.set_menus(w, &menus())?;
    let (mut width, mut height, _) = c.size(w)?;
    let mut s = Surface::new(width as usize, height as usize);
    let mut t = Terminal::spawn(s.width(), s.height())?;
    let f = Font::builtin();
    let mut selecting = false;
    let mut dirty = true;
    loop {
        let (nw, nh, min) = c.size(w)?;
        if nw != width || nh != height {
            width = nw;
            height = nh;
            s.reset(width as usize, height as usize, 0xff000000);
            t.resize(s.width(), s.height());
            dirty = true;
        }
        for _ in 0..32 {
            let Some(Event {
                kind,
                control,
                text,
            }) = c.poll(w)?
            else {
                break;
            };
            dirty = true;
            match kind {
                6 => {
                    if control & 2 != 0 && (text == "\x1b[5~" || text == "\x1b[6~") {
                        t.screen.scroll(text == "\x1b[5~");
                    } else if control & 3 == 3 {
                        match text.as_bytes() {
                            [3] => {
                                let selected = t.screen.selected_text();
                                if !selected.is_empty() {
                                    c.set_clipboard(&selected)?;
                                }
                            }
                            [22] => {
                                let value = c.clipboard()?;
                                t.paste(&value);
                            }
                            [1] => t.screen.select_all(),
                            _ => t.input(text.as_bytes()),
                        }
                    } else {
                        t.input(text.as_bytes());
                    }
                }
                8 => {
                    let mut p = text.split_whitespace();
                    let x = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let action = p.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    if control == 1 {
                        match action {
                            1 => {
                                selecting = true;
                                t.screen.select_at(x, y, true)
                            }
                            0 => selecting = false,
                            _ => (),
                        }
                    } else if control == 0 && selecting {
                        t.screen.select_at(x, y, false);
                    } else if control == 2 {
                        let selected = t.screen.selected_text();
                        if !selected.is_empty() {
                            c.set_clipboard(&selected)?
                        }
                    }
                }
                10 => {
                    let mut p = text.split_whitespace();
                    let _x = p.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                    let _y = p.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                    let axis = p.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    let delta = control as i32;
                    if axis == 0 {
                        for _ in 0..delta.unsigned_abs().min(32) {
                            t.screen.scroll(delta > 0);
                        }
                    }
                }
                11 => match control {
                    MENU_INTERRUPT => t.input(&[3]),
                    MENU_CLEAR => t.screen.feed(b"\x1b[H\x1b[J"),
                    MENU_CLEAR_SCROLLBACK => t.screen.feed(b"\x1b[3J"),
                    MENU_COPY => {
                        let selected = t.screen.selected_text();
                        if !selected.is_empty() {
                            c.set_clipboard(&selected)?;
                        }
                    }
                    MENU_PASTE => {
                        let value = c.clipboard()?;
                        t.paste(&value);
                    }
                    MENU_SELECT_ALL => t.screen.select_all(),
                    MENU_PAGE_UP => t.screen.scroll(true),
                    MENU_PAGE_DOWN => t.screen.scroll(false),
                    MENU_BOTTOM => t.screen.scroll_to_bottom(),
                    _ => (),
                },
                9 => {
                    let _ = c.close(w);
                    return Ok(());
                }
                _ => (),
            }
        }
        dirty |= t.tick();
        if dirty {
            dirty = false;
            s.pixels_mut().fill(0xff000000);
            t.screen.draw(&mut s, &f);
            c.present(w, width, height, s.pixels())?;
        }
        if t.exited {
            thread::sleep(Duration::from_millis(150));
            let _ = c.close(w);
            break;
        }
        if min {
            thread::sleep(Duration::from_millis(40))
        } else {
            thread::sleep(Duration::from_millis(12))
        }
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-terminal: {e}");
    }
}
