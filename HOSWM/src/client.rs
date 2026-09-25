//! Safe Rust client for the local HOSWM window ABI.
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

pub const EVENT_CLICK: u32 = 1;
pub const EVENT_CHANGE: u32 = 2;
pub const EVENT_SUBMIT: u32 = 3;
pub const EVENT_RESIZE: u32 = 4;
pub const EVENT_POINTER: u32 = 5;
pub const EVENT_KEY: u32 = 6;
pub const EVENT_CLOSED: u32 = 7;
pub const EVENT_RAW_POINTER: u32 = 8;
pub const EVENT_CLOSE_REQUEST: u32 = 9;
/// Mouse wheel event: control is signed notch delta (positive is up); text is "x y axis" in content coordinates, where axis 0 is vertical and 1 is horizontal.
pub const EVENT_WHEEL: u32 = 10;
/// Menu bar item chosen: control is the item ID, text is its label.
pub const EVENT_MENU: u32 = 11;
pub use crate::desktop::{
    ANSWER_BUTTONS_OK, ANSWER_BUTTONS_OK_CANCEL, ANSWER_BUTTONS_YES_NO,
    ANSWER_BUTTONS_YES_NO_CANCEL, ANSWER_CANCEL, ANSWER_CLOSED, ANSWER_NO, ANSWER_OK, ANSWER_YES,
    SEVERITY_ERROR, SEVERITY_INFO, SEVERITY_QUESTION, SEVERITY_WARNING,
};
pub const WINDOW_RAW_INPUT: u32 = 1;
pub const WINDOW_DEFER_CLOSE: u32 = 2;
pub const WINDOW_PROTECT_EXIT: u32 = 4;
/// The window manager may resize this window by its edges; the client learns
/// the new size from [`EVENT_RESIZE`] or [`Client::size`].
pub const WINDOW_RESIZABLE: u32 = 8;

pub use crate::menu::{Menu, MenuItem};

#[derive(Clone, Debug)]
pub struct Event {
    pub kind: u32,
    pub control: u32,
    pub text: String,
}
#[derive(Clone, Copy, Debug)]
pub struct Window(pub u32);
pub struct Client;
impl Client {
    pub fn connect() -> io::Result<Self> {
        Ok(Self)
    }
    fn call(&self, request: &[u8]) -> io::Result<Vec<u8>> {
        let path = std::env::var("HOSWM_SOCKET")
            .unwrap_or_else(|_| format!("/tmp/hoswm-{}/session.sock", unsafe { geteuid() }));
        let mut s = UnixStream::connect(path)?;
        s.set_read_timeout(Some(Duration::from_secs(3)))?;
        s.set_write_timeout(Some(Duration::from_secs(3)))?;
        s.write_all(&(request.len() as u32).to_le_bytes())?;
        s.write_all(request)?;
        let mut h = [0; 8];
        s.read_exact(&mut h)?;
        let n = u32::from_le_bytes(h[..4].try_into().unwrap()) as usize;
        let status = u32::from_le_bytes(h[4..].try_into().unwrap());
        if n < 4 || n > 800 * 530 * 4 + 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid HOSWM response length",
            ));
        }
        let mut body = vec![0; n - 4];
        s.read_exact(&mut body)?;
        if status != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                String::from_utf8_lossy(&body).into_owned(),
            ));
        }
        Ok(body)
    }
    pub fn create(&self, title: &str, width: u32, height: u32, color: u32) -> io::Result<Window> {
        let mut q = words(&[1, width, height, color, title.len() as u32]);
        q.extend(title.as_bytes());
        let b = self.call(&q)?;
        one(&b).map(Window)
    }
    pub fn close(&self, w: Window) -> io::Result<()> {
        let _ = self.call(&words(&[4, w.0]))?;
        Ok(())
    }
    /// Set every window flag at once: `WINDOW_RAW_INPUT`, `WINDOW_DEFER_CLOSE`,
    /// `WINDOW_PROTECT_EXIT` and `WINDOW_RESIZABLE`. Flags left out are cleared.
    pub fn flags(&self, w: Window, flags: u32) -> io::Result<()> {
        let _ = self.call(&words(&[12, w.0, flags]))?;
        Ok(())
    }
    pub fn present(&self, w: Window, width: u32, height: u32, pixels: &[u32]) -> io::Result<()> {
        if pixels.len() != width as usize * height as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pixel count does not match dimensions",
            ));
        }
        let mut q = words(&[3, w.0, width, height]);
        q.reserve(pixels.len() * 4);
        for p in pixels {
            q.extend(p.to_le_bytes());
        }
        let _ = self.call(&q)?;
        Ok(())
    }
    pub fn poll(&self, w: Window) -> io::Result<Option<Event>> {
        let b = self.call(&words(&[5, w.0]))?;
        if b.len() < 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short HOSWM event",
            ));
        }
        let kind = u32::from_le_bytes(b[..4].try_into().unwrap());
        if kind == 0 {
            return Ok(None);
        }
        let control = u32::from_le_bytes(b[4..8].try_into().unwrap());
        let n = u32::from_le_bytes(b[8..12].try_into().unwrap()) as usize;
        if b.len() != 12 + n {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid HOSWM event length",
            ));
        }
        Ok(Some(Event {
            kind,
            control,
            text: String::from_utf8_lossy(&b[12..]).into_owned(),
        }))
    }
    pub fn size(&self, w: Window) -> io::Result<(u32, u32, bool)> {
        let b = self.call(&words(&[8, w.0]))?;
        if b.len() != 12 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid size response",
            ));
        }
        Ok((get(&b, 0), get(&b, 4), get(&b, 8) != 0))
    }
    /// Open a message box with answer buttons. Poll it, or use [`Self::ask`].
    pub fn message(
        &self,
        title: &str,
        message: &str,
        buttons: u32,
        severity: u32,
    ) -> io::Result<Window> {
        let mut q = words(&[17, buttons, severity, title.len() as u32]);
        q.extend(title.as_bytes());
        q.extend(words(&[message.len() as u32]));
        q.extend(message.as_bytes());
        one(&self.call(&q)?).map(Window)
    }
    /// Open a message box and wait for the answer, closing it afterwards.
    /// Returns `ANSWER_CLOSED` if the user closed the window instead.
    pub fn ask(
        &self,
        title: &str,
        message: &str,
        buttons: u32,
        severity: u32,
    ) -> io::Result<u32> {
        let window = self.message(title, message, buttons, severity)?;
        loop {
            match self.poll(window)? {
                Some(Event { kind: 1, control, .. }) => {
                    let _ = self.close(window);
                    return Ok(control);
                }
                Some(Event { kind: 7, .. }) => return Ok(ANSWER_CLOSED),
                _ => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }
    /// Show a notification. Zero milliseconds uses the configured default.
    pub fn toast(&self, text: &str, color: u32, milliseconds: u32) -> io::Result<()> {
        let mut q = words(&[15, color, milliseconds, text.len() as u32]);
        q.extend(text.as_bytes());
        let _ = self.call(&q)?;
        Ok(())
    }
    /// Replace this window's menu bar menus; an empty slice removes them.
    pub fn set_menus(&self, w: Window, menus: &[Menu]) -> io::Result<()> {
        let mut q = words(&[16, w.0, menus.len() as u32]);
        for menu in menus {
            q.extend(words(&[menu.title.len() as u32]));
            q.extend(menu.title.as_bytes());
            q.extend(words(&[menu.items.len() as u32]));
            for item in &menu.items {
                q.extend(words(&[item.id, item.flags(), item.label.len() as u32]));
                q.extend(item.label.as_bytes());
                q.extend(words(&[item.shortcut.len() as u32]));
                q.extend(item.shortcut.as_bytes());
            }
        }
        let _ = self.call(&q)?;
        Ok(())
    }
    pub fn clipboard(&self) -> io::Result<String> {
        let b = self.call(&words(&[14]))?;
        if b.len() < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short clipboard response",
            ));
        }
        let n = get(&b, 0) as usize;
        if b.len() != n + 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid clipboard response",
            ));
        }
        Ok(String::from_utf8_lossy(&b[4..]).into_owned())
    }
    pub fn set_clipboard(&self, text: &str) -> io::Result<()> {
        let mut q = words(&[13, text.len() as u32]);
        q.extend(text.as_bytes());
        let _ = self.call(&q)?;
        Ok(())
    }
}
fn words(w: &[u32]) -> Vec<u8> {
    w.iter().flat_map(|x| x.to_le_bytes()).collect()
}
fn get(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
fn one(b: &[u8]) -> io::Result<u32> {
    if b.len() != 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid HOSWM window ID",
        ));
    }
    Ok(get(b, 0))
}
unsafe extern "C" {
    fn geteuid() -> u32;
}
