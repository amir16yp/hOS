//! Version 1 local window ABI. See include/hoswm.h and ../docs/applications.md.
use crate::desktop::{Control, Desktop, Rect};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    time::{Duration, Instant},
};
unsafe extern "C" {
    fn geteuid() -> u32;
    fn getsockopt(fd: i32, level: i32, name: i32, value: *mut u8, length: *mut u32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}
#[repr(C)]
struct PeerCred {
    pid: i32,
    uid: u32,
    gid: u32,
}
fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred = PeerCred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<PeerCred>() as u32;
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            1,
            17,
            &mut cred as *mut _ as *mut u8,
            &mut len,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    if len as usize != std::mem::size_of::<PeerCred>() || cred.pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid peer credentials",
        ));
    }
    Ok(cred.pid as u32)
}
const MAX: usize = 800 * 530 * 4 + 4096;
struct Client {
    stream: UnixStream,
    pid: u32,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    started: Instant,
    done: bool,
}
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    clients: Vec<Client>,
}
impl Server {
    pub fn bind() -> io::Result<Self> {
        let uid = unsafe { geteuid() };
        let directory = PathBuf::from(format!("/tmp/hoswm-{uid}"));
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e),
        }
        let meta = fs::symlink_metadata(&directory)?;
        if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "HOSWM socket directory must be owned by this user with mode 0700",
            ));
        }
        let path = directory.join("session.sock");
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "HOSWM session already running",
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(&path)?,
                Err(e) => return Err(e),
            }
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path,
            clients: Vec::new(),
        })
    }
    pub fn watch(&self, reactor: &mut crate::reactor::Reactor) {
        use std::os::fd::AsRawFd;
        reactor.watch(self.listener.as_raw_fd(), true, false);
        for client in &self.clients {
            reactor.watch(
                client.stream.as_raw_fd(),
                !client.done,
                client.done && client.written < client.output.len(),
            );
        }
    }
    pub fn tick(&mut self, desktop: &mut Desktop) -> bool {
        let owners: Vec<u32> = desktop
            .windows
            .iter()
            .map(|w| w.owner_pid)
            .filter(|pid| *pid != 0)
            .collect();
        let mut dead = Vec::new();
        for pid in owners {
            if !dead.contains(&pid)
                && unsafe { kill(pid as i32, 0) } < 0
                && io::Error::last_os_error().raw_os_error() == Some(3)
            {
                dead.push(pid);
            }
        }
        let mut dirty = false;
        for pid in dead {
            desktop.close_owner(pid);
            dirty = true;
        }
        for _ in 0..8 {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.clients.len() < 32 && stream.set_nonblocking(true).is_ok() {
                        let pid = peer_pid(&stream).unwrap_or(0);
                        self.clients.push(Client {
                            stream,
                            pid,
                            input: Vec::new(),
                            output: Vec::new(),
                            written: 0,
                            started: Instant::now(),
                            done: false,
                        });
                    }
                }
                Err(_) => break,
            }
        }
        self.clients.retain_mut(|c| {
            if c.started.elapsed() > Duration::from_secs(3) {
                return false;
            }
            let mut closed = false;
            if !c.done {
                let mut buf = [0u8; 16384];
                for _ in 0..16 {
                    match c.stream.read(&mut buf) {
                        Ok(0) => {
                            closed = true;
                            break;
                        }
                        Ok(n) => {
                            if c.input.len() + n > MAX + 4 {
                                return false;
                            }
                            c.input.extend_from_slice(&buf[..n]);
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return false,
                    }
                }
                if c.input.len() >= 4 {
                    let len = u32::from_le_bytes(c.input[..4].try_into().unwrap()) as usize;
                    if !(4..=MAX).contains(&len) {
                        return false;
                    }
                    if c.input.len() >= len + 4 {
                        let result = dispatch_as(desktop, &c.input[4..len + 4], c.pid);
                        let (status, body) = match result {
                            Ok(b) => (0u32, b),
                            Err(e) => (1, e.into_bytes()),
                        };
                        c.output
                            .extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
                        c.output.extend_from_slice(&status.to_le_bytes());
                        c.output.extend(body);
                        c.done = true;
                        let op = u32::from_le_bytes(c.input[4..8].try_into().unwrap());
                        dirty |= matches!(op, 1..=4 | 6 | 7 | 10..=13 | 15..=20);
                    }
                }
            }
            if closed && !c.done {
                return false;
            }
            if c.done {
                while c.written < c.output.len() {
                    match c.stream.write(&c.output[c.written..]) {
                        Ok(0) => return false,
                        Ok(n) => c.written += n,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return false,
                    }
                }
                return false;
            }
            true
        });
        dirty
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn u32(&mut self) -> Result<u32, String> {
        if self.0.len() < 4 {
            return Err("short request".into());
        }
        let n = u32::from_le_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        Ok(n)
    }
    fn text(&mut self, max: usize) -> Result<String, String> {
        let n = self.u32()? as usize;
        if n > max || self.0.len() < n {
            return Err("invalid string length".into());
        }
        let s = std::str::from_utf8(&self.0[..n])
            .map_err(|_| "invalid UTF-8")?
            .to_owned();
        self.0 = &self.0[n..];
        Ok(s)
    }
    fn end(&self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("extra request data".into())
        }
    }
}
fn ints(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|n| n.to_le_bytes()).collect()
}
pub fn dispatch(d: &mut Desktop, bytes: &[u8]) -> Result<Vec<u8>, String> {
    dispatch_as(d, bytes, 0)
}
fn dispatch_as(d: &mut Desktop, bytes: &[u8], owner_pid: u32) -> Result<Vec<u8>, String> {
    let mut r = Reader(bytes);
    let op = r.u32()?;
    if op == 0 {
        r.end()?;
        return Ok(ints(&[1, 800, 600]));
    }
    if op == 1 {
        let w = r.u32()? as i32;
        let h = r.u32()? as i32;
        let color = r.u32()?;
        let title = r.text(128)?;
        r.end()?;
        let id = d.create(title, w, h, color)?;
        d.window(id).unwrap().owner_pid = owner_pid;
        return Ok(ints(&[id]));
    }
    if op == 2 {
        let color = r.u32()?;
        let title = r.text(128)?;
        let message = r.text(2048)?;
        r.end()?;
        let id = d.message_box(title, message, color)?;
        d.window(id).unwrap().owner_pid = owner_pid;
        return Ok(ints(&[id]));
    }
    if op == 15 {
        let color = r.u32()?;
        let milliseconds = r.u32()?;
        let text = r.text(crate::toast::MAX_TEXT)?;
        r.end()?;
        if text.trim().is_empty() {
            return Err("notification text required".into());
        }
        d.toast(text, color, milliseconds);
        return Ok(Vec::new());
    }
    if op == 17 {
        let buttons = r.u32()?;
        let severity = r.u32()?;
        let title = r.text(128)?;
        let message = r.text(2048)?;
        r.end()?;
        let id = d.ask(title, message, buttons, severity)?;
        d.window(id).unwrap().owner_pid = owner_pid;
        return Ok(ints(&[id]));
    }
    if op == 18 {
        let internal_w = r.u32()? as i32;
        let internal_h = r.u32()? as i32;
        let final_w = r.u32()? as i32;
        let final_h = r.u32()? as i32;
        let color = r.u32()?;
        let title = r.text(128)?;
        r.end()?;
        let id = d.create_sized(title, internal_w, internal_h, final_w, final_h, color)?;
        d.window(id).unwrap().owner_pid = owner_pid;
        return Ok(ints(&[id]));
    }
    let id = r.u32()?;
    if op == 10 {
        let cid = r.u32()?;
        r.end()?;
        d.remove_control(id, cid)?;
        return Ok(Vec::new());
    }
    if op == 4 {
        r.end()?;
        d.close_client(id);
        return Ok(Vec::new());
    }
    if op == 5 && d.window(id).is_none() {
        r.end()?;
        return Ok(ints(&[7, 0, 0]));
    }
    let w = d.window(id).ok_or("unknown window")?;
    match op {
        3 => {
            let width = r.u32()? as usize;
            let height = r.u32()? as usize;
            if (width, height) != (w.content.width(), w.content.height())
                || r.0.len() != width * height * 4
            {
                return Err("pixel dimensions must match current content size".into());
            }
            for (pixel, bytes) in w.content.pixels_mut().iter_mut().zip(r.0.chunks_exact(4)) {
                *pixel = u32::from_le_bytes(bytes.try_into().unwrap()) | 0xff000000;
            }
        }
        12 => {
            let flags = r.u32()?;
            r.end()?;
            if flags & !15 != 0 {
                return Err("invalid window flags".into());
            }
            let w = d.window(id).unwrap();
            w.raw_input = flags & 1 != 0;
            w.managed_close = flags & 2 != 0;
            w.protected_work = flags & 4 != 0;
            w.resizable = flags & 8 != 0;
        }
        13 => {
            let text = r.text(65536)?;
            r.end()?;
            d.set_clipboard(text);
        }
        14 => {
            r.end()?;
            let text = d.clipboard();
            let mut out = ints(&[text.len() as u32]);
            out.extend(text.as_bytes());
            return Ok(out);
        }
        5 => {
            r.end()?;
            if let Some(e) = w.events.pop_front() {
                let mut out = ints(&[e.kind, e.control, e.text.len() as u32]);
                out.extend(e.text.as_bytes());
                return Ok(out);
            }
            return Ok(ints(&[0, 0, 0]));
        }
        6 | 11 => {
            let cid = r.u32()?;
            let kind = r.u32()?;
            let rect = Rect {
                x: r.u32()? as i32,
                y: r.u32()? as i32,
                w: r.u32()? as i32,
                h: r.u32()? as i32,
            };
            let selectable = if op == 11 {
                match r.u32()? {
                    0 => false,
                    1 => true,
                    _ => return Err("invalid selectable boolean".into()),
                }
            } else {
                false
            };
            let text = r.text(1024)?;
            r.end()?;
            if selectable && kind == 2 {
                return Err("buttons do not support text selection".into());
            }
            if cid == 0
                || !(1..=3).contains(&kind)
                || rect.x < 0
                || rect.y < 0
                || rect.w < 16
                || rect.h < 12
                || rect.w > 796
                || rect.h > 498
                || rect.x > 796 - rect.w
                || rect.y > 498 - rect.h
            {
                return Err("invalid control".into());
            }
            let mut selection = crate::text::TextSelection::default();
            selection.reset(&text);
            selection.selectable = selectable;
            let control = Control {
                selection,
                id: cid,
                kind,
                rect,
                text,
            };
            if let Some(old) = w.controls.iter_mut().find(|c| c.id == cid) {
                *old = control;
            } else {
                if w.controls.len() >= 128 {
                    return Err("control limit reached".into());
                }
                w.controls.push(control);
            }
            d.cancel_control_interaction(id, cid);
        }
        7 => {
            let cid = r.u32()?;
            let text = r.text(1024)?;
            r.end()?;
            let c = w
                .controls
                .iter_mut()
                .find(|c| c.id == cid)
                .ok_or("unknown control")?;
            c.text = text;
            c.selection.reset(&c.text);
            d.cancel_control_interaction(id, cid);
        }
        8 => {
            r.end()?;
            return Ok(ints(&[
                w.content.width() as u32,
                w.content.height() as u32,
                w.minimized as u32,
            ]));
        }
        19 => {
            r.end()?;
            return Ok(ints(&[
                w.content.width() as u32,
                w.content.height() as u32,
                (w.rect.w - 4).max(0) as u32,
                (w.rect.h - crate::desktop::TITLE - 2).max(0) as u32,
                w.minimized as u32,
                w.fps,
            ]));
        }
        20 => {
            let fps = r.u32()?;
            r.end()?;
            if !(1..=240).contains(&fps) {
                return Err("invalid window FPS".into());
            }
            w.fps = fps;
        }
        9 => {
            let cid = r.u32()?;
            r.end()?;
            let text = &w
                .controls
                .iter()
                .find(|c| c.id == cid)
                .ok_or("unknown control")?
                .text;
            let mut out = ints(&[text.len() as u32]);
            out.extend(text.as_bytes());
            return Ok(out);
        }
        16 => {
            let count = r.u32()? as usize;
            if count > 8 {
                return Err("menu limit reached".into());
            }
            let mut menus = Vec::with_capacity(count);
            for _ in 0..count {
                let title = r.text(32)?;
                if title.trim().is_empty() {
                    return Err("menu title required".into());
                }
                let items = r.u32()? as usize;
                if items > 32 {
                    return Err("menu item limit reached".into());
                }
                let mut entries = Vec::with_capacity(items);
                for _ in 0..items {
                    let item = r.u32()?;
                    let flags = r.u32()?;
                    if flags & !7 != 0 {
                        return Err("invalid menu item flags".into());
                    }
                    let label = r.text(48)?;
                    let shortcut = r.text(16)?;
                    if item == 0 && flags & crate::menu::ITEM_SEPARATOR == 0 {
                        return Err("menu item IDs must be nonzero".into());
                    }
                    entries.push(crate::menu::MenuItem::from_flags(
                        item, flags, label, shortcut,
                    ));
                }
                menus.push(crate::menu::Menu::new(title, entries));
            }
            r.end()?;
            w.menus = menus;
        }
        _ => return Err("unknown ABI operation".into()),
    }
    Ok(Vec::new())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn c_client_roundtrip() {
        use std::process::Command;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = std::env::temp_dir().join(format!("hoswm-abi-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let binary = dir.join("client");
        assert!(
            Command::new("cc")
                .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
                .arg(root.join("include"))
                .arg(root.join("tests/client_roundtrip.c"))
                .arg(root.join("src/client.c"))
                .arg("-o")
                .arg(&binary)
                .status()
                .unwrap()
                .success()
        );
        let path = dir.join("session.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut server = Server {
            listener,
            path: path.clone(),
            clients: Vec::new(),
        };
        // Keep notifications from this test inside its own directory.
        let mut desktop = Desktop::with_config(crate::config::Config::defaults_in(&dir));
        let mut client = Command::new(&binary)
            .env("HOSWM_SOCKET", &path)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            server.tick(&mut desktop);
            if let Some(status) = client.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() > deadline {
                let _ = client.kill();
                let _ = client.wait();
                panic!("C client timed out");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(desktop.windows.is_empty());
        drop(server);
        // The notification the C client posted is logged when the session ends.
        drop(desktop);
        let (records, damage) = crate::toast::read(&dir.join("toastdb")).unwrap();
        assert_eq!(damage, None);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].text, "C client notification");
        assert_eq!(records[0].requested_ms, 1500);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn window_flags_carry_the_resizable_bit() {
        let mut d = Desktop::new();
        let mut request = ints(&[1, 320, 120, 0xffabcdef, 4]);
        request.extend(b"Test");
        let id = u32::from_le_bytes(dispatch(&mut d, &request).unwrap().try_into().unwrap());
        assert!(!d.window(id).unwrap().resizable);
        dispatch(&mut d, &ints(&[12, id, 1 | 8])).unwrap();
        let w = d.window(id).unwrap();
        assert!(w.resizable && w.raw_input && !w.managed_close);
        // Flags are set as a whole, so an operation without the bit clears it.
        dispatch(&mut d, &ints(&[12, id, 1])).unwrap();
        assert!(!d.window(id).unwrap().resizable);
        assert!(dispatch(&mut d, &ints(&[12, id, 16])).is_err());
    }
    #[test]
    fn protocol_create_status_close_invalid() {
        let mut d = Desktop::new();
        let mut request = ints(&[1, 320, 120, 0xffabcdef, 4]);
        request.extend(b"Test");
        let id = u32::from_le_bytes(dispatch(&mut d, &request).unwrap().try_into().unwrap());
        assert_eq!(
            dispatch(&mut d, &ints(&[8, id])).unwrap(),
            ints(&[320, 120, 0])
        );
        assert!(dispatch(&mut d, &ints(&[3, id, 800, 600])).is_err());
        dispatch(&mut d, &ints(&[4, id])).unwrap();
        assert_eq!(dispatch(&mut d, &ints(&[5, id])).unwrap(), ints(&[7, 0, 0]));
        for n in 0..request.len() {
            assert!(dispatch(&mut d, &request[..n]).is_err());
        }
    }
}
