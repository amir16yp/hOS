//! Local account login, followed by an irreversible drop to the account's identity.
use crate::{desktop::ACCENT, font::Font, surface::Surface};
use std::{
    fs,
    io::{self, Write},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, PartialEq)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
}

fn account(passwd: &str, shadow: &str, name: &str, today: u64) -> Option<(Account, String)> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
    {
        return None;
    }
    let p: Vec<_> = passwd
        .lines()
        .find(|l| l.split(':').next() == Some(name))?
        .split(':')
        .collect();
    if p.len() != 7
        || !p[5].starts_with('/')
        || !p[6].starts_with('/')
        || matches!(p[6].rsplit('/').next(), Some("nologin" | "false"))
    {
        return None;
    }
    let hash = if p[1] == "x" {
        let s: Vec<_> = shadow
            .lines()
            .find(|l| l.split(':').next() == Some(name))?
            .split(':')
            .collect();
        if s.len() != 9 {
            return None;
        }
        let day = |v: &str| -> Option<Option<u64>> {
            if v.is_empty() || v == "-1" {
                Some(None)
            } else {
                v.parse().ok().map(Some)
            }
        };
        let changed = day(s[2])?;
        let maximum = day(s[4])?;
        let expires = day(s[7])?;
        // Password changes are handled outside this basic greeter. Deny expired
        // passwords and accounts rather than silently ignoring their policy.
        if changed == Some(0)
            || expires.is_some_and(|d| today >= d)
            || changed
                .zip(maximum)
                .is_some_and(|(d, m)| today > d.saturating_add(m))
        {
            return None;
        }
        s[1]
    } else {
        p[1]
    };
    hash_settings(hash)?; // Also rejects empty, locked and unsupported hashes.
    Some((
        Account {
            name: name.into(),
            uid: p[2].parse().ok()?,
            gid: p[3].parse().ok()?,
            home: p[5].into(),
            shell: p[6].into(),
        },
        hash.into(),
    ))
}

fn hash_settings(hash: &str) -> Option<(&str, &str)> {
    let (method, rest) = if let Some(s) = hash.strip_prefix("$6$") {
        ("sha512", s)
    } else if let Some(s) = hash.strip_prefix("$5$") {
        ("sha256", s)
    } else if let Some(s) = hash.strip_prefix("$1$") {
        ("md5", s)
    } else {
        return None;
    };
    let (salt, digest) = rest.rsplit_once('$')?;
    if salt.is_empty() || digest.is_empty() {
        return None;
    }
    Some((method, salt))
}

fn verify(helper: &str, hash: &str, password: &[u8]) -> io::Result<bool> {
    let Some(_) = hash_settings(hash) else {
        return Ok(false);
    };
    if password.is_empty() || password.contains(&b'\n') || password.contains(&0) {
        return Ok(false);
    }
    let mut child = Command::new(helper)
        .arg(hash)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let written = stdin
        .write_all(password)
        .and_then(|_| stdin.write_all(b"\n"));
    drop(stdin);
    let output = child.wait_with_output()?;
    written?;
    let actual = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let same = actual.len() == hash.len()
        && actual
            .iter()
            .zip(hash.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0;
    Ok(output.status.success() && same)
}

pub fn authenticate(name: &str, password: &[u8]) -> io::Result<Option<Account>> {
    let passwd = fs::read_to_string("/etc/passwd")?;
    let shadow = fs::read_to_string("/etc/shadow")?;
    let today = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs()
        / 86400;
    let Some((user, hash)) = account(&passwd, &shadow, name, today) else {
        return Ok(None);
    };
    Ok(verify("/bin/hos-password", &hash, password)?.then_some(user))
}

unsafe extern "C" {
    fn setgroups(size: usize, groups: *const u32) -> i32;
    fn setgid(gid: u32) -> i32;
    fn setuid(uid: u32) -> i32;
    fn umask(mask: u32) -> u32;
}

/// Call on the main thread before starting any desktop workers. Open display
/// and input descriptors are retained; applications inherit the user's identity.
pub fn enter_session(user: &Account) -> io::Result<()> {
    let mut groups = vec![user.gid];
    for line in fs::read_to_string("/etc/group")?.lines() {
        let g: Vec<_> = line.split(':').collect();
        if g.len() == 4 && g[3].split(',').any(|name| name == user.name) {
            let gid = g[2].parse().map_err(io::Error::other)?;
            if !groups.contains(&gid) {
                groups.push(gid);
            }
        }
    }
    // SAFETY: valid group array; these calls drop privileges permanently.
    if unsafe { setgroups(groups.len(), groups.as_ptr()) } != 0
        || unsafe { setgid(user.gid) } != 0
        || unsafe { setuid(user.uid) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    std::env::set_current_dir(&user.home)?;
    // No other threads exist at this stage of startup.
    unsafe {
        umask(0o022);
        for (key, value) in [
            ("HOME", user.home.as_str()),
            ("USER", &user.name),
            ("LOGNAME", &user.name),
            ("SHELL", &user.shell),
            ("PATH", "/bin:/sbin:/usr/bin:/usr/sbin"),
            ("TERM", "linux"),
        ] {
            std::env::set_var(key, value);
        }
    }
    Ok(())
}

pub struct Greeter {
    pub username: String,
    password: Vec<u8>,
    field: usize,
    pub x: i32,
    pub y: i32,
    screen_width: i32,
    screen_height: i32,
    pub submitted: bool,
    status: &'static str,
    retry_at: Instant,
}
impl Default for Greeter {
    fn default() -> Self {
        Self {
            username: String::new(),
            password: Vec::with_capacity(128),
            field: 0,
            x: 400,
            y: 300,
            screen_width: 800,
            screen_height: 600,
            submitted: false,
            status: "Enter your username and password.",
            retry_at: Instant::now(),
        }
    }
}
impl Greeter {
    pub fn set_screen_size(&mut self, width: usize, height: usize) {
        self.screen_width = width.max(1) as i32;
        self.screen_height = height.max(1) as i32;
        self.x = self.x.clamp(0, self.screen_width - 1);
        self.y = self.y.clamp(0, self.screen_height - 1);
    }
    fn card(&self) -> (i32, i32) {
        (
            (self.screen_width - 416) / 2,
            (self.screen_height - 326) / 2,
        )
    }
    fn clear_password(&mut self) {
        for byte in &mut self.password {
            // Do not leave the plaintext behind when clearing the field.
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
        self.password.clear();
    }
    pub fn key(&mut self, bytes: &[u8]) {
        match bytes {
            b"\t" => self.field = 1 - self.field,
            b"\r" => {
                if self.field == 0 {
                    self.field = 1;
                } else {
                    self.submitted = true;
                }
            }
            b"\x1b" => self.clear_password(),
            b"\x7f" | b"\x08" => {
                if self.field == 0 {
                    self.username.pop();
                } else if let Some(byte) = self.password.last_mut() {
                    unsafe {
                        std::ptr::write_volatile(byte, 0);
                    }
                    self.password.pop();
                }
            }
            _ if bytes.iter().all(|b| (32..127).contains(b)) => {
                if self.field == 0 && self.username.len() + bytes.len() <= 64 {
                    self.username.push_str(std::str::from_utf8(bytes).unwrap());
                } else if self.field == 1 && self.password.len() + bytes.len() <= 128 {
                    self.password.extend_from_slice(bytes);
                }
            }
            _ => (),
        }
    }
    pub fn click(&mut self) {
        let (left, top) = self.card();
        if (left + 32..left + 384).contains(&self.x) {
            match self.y {
                y if (top + 106..=top + 139).contains(&y) => self.field = 0,
                y if (top + 174..=top + 207).contains(&y) => self.field = 1,
                y if (top + 232..=top + 269).contains(&y) => self.submitted = true,
                _ => (),
            }
        }
    }
    pub fn login(&mut self) -> Option<Account> {
        self.submitted = false;
        if Instant::now() < self.retry_at {
            return None;
        }
        let result = authenticate(&self.username, &self.password);
        self.clear_password();
        match result {
            Ok(Some(user)) => Some(user),
            _ => {
                self.status = "Login failed. Check your details and try again.";
                self.field = 1;
                self.retry_at = Instant::now() + Duration::from_secs(2);
                None
            }
        }
    }
    pub fn draw(&self, surface: &mut Surface) {
        let font = Font::builtin();
        let (left, top) = self.card();
        surface.pixels_mut().fill(0xff101815);
        surface.fill_rect(left, top, 416, 326, 0xff080c0a);
        surface.fill_rect(left, top, 416, 3, ACCENT);
        font.draw(surface, left + 32, top + 34, "hOS", ACCENT);
        font.draw(surface, left + 32, top + 58, "Sign in", 0xffeeeeee);
        for (index, label, offset) in [(0, "Username", 106), (1, "Password", 174)] {
            let y = top + offset;
            font.draw(surface, left + 32, y - 20, label, 0xffb7c9bf);
            surface.fill_rect(
                left + 32,
                y,
                352,
                34,
                if self.field == index {
                    ACCENT
                } else {
                    0xff42564b
                },
            );
            surface.fill_rect(left + 33, y + 1, 350, 32, 0xff101815);
            let value = if index == 0 {
                self.username.clone()
            } else {
                "*".repeat(self.password.len())
            };
            let start = value.len().saturating_sub(40);
            font.draw(surface, left + 42, y + 12, &value[start..], 0xffeeeeee);
            if self.field == index {
                surface.fill_rect(
                    left + 42 + ((value.len() - start) * 8) as i32,
                    y + 11,
                    1,
                    12,
                    ACCENT,
                );
            }
        }
        surface.fill_rect(left + 32, top + 232, 352, 38, ACCENT);
        font.draw(surface, left + 148, top + 247, "Sign in", 0xff08110c);
        font.draw(
            surface,
            left + 32,
            top + 290,
            "Tab to switch fields. Enter to sign in.",
            0xffb7c9bf,
        );
        font.draw(surface, left + 8, top + 348, self.status, 0xffdddddd);
        for offset in 0..12 {
            surface.fill_rect(self.x, self.y + offset, 1 + offset / 2, 1, 0xffffffff);
        }
    }
}
impl Drop for Greeter {
    fn drop(&mut self) {
        self.clear_password();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\nalice:x:1000:100:Alice:/home/alice:/bin/sh\nservice:x:12:12::/srv:/bin/false\n";
    const SHADOW: &str = "root:$6$salt$hash:19000:0:99999:7:::\nalice:$6$salt$hash:19000:0:99999:7:::\nservice:$6$salt$hash:19000:0:99999:7:::\n";
    #[test]
    fn accounts_use_each_users_identity_and_reject_disabled_logins() {
        let (user, _) = account(PASSWD, SHADOW, "alice", 20000).unwrap();
        assert_eq!((user.uid, user.gid), (1000, 100));
        assert_eq!(user.home, "/home/alice");
        assert_eq!(account(PASSWD, SHADOW, "root", 20000).unwrap().0.uid, 0);
        for name in ["", "missing", "service", "alice\nroot"] {
            assert!(account(PASSWD, SHADOW, name, 20000).is_none());
        }
        for hash in ["", "!", "*", "!$6$salt$hash", "$y$salt$hash"] {
            assert!(account(
                PASSWD,
                &SHADOW.replace("$6$salt$hash", hash),
                "alice",
                20000
            )
            .is_none());
        }
        for dates in [
            "0:0:99999:7:::",
            "19000:0:10:7:::",
            "19000:0:99999:7::19999:",
        ] {
            assert!(account(
                PASSWD,
                &SHADOW.replace("19000:0:99999:7:::", dates),
                "alice",
                20000
            )
            .is_none());
        }
    }
    #[test]
    fn keyboard_fields_mask_password_and_do_not_assume_root() {
        let mut g = Greeter::default();
        assert!(g.username.is_empty());
        g.key(b"alice");
        g.key(b"\t");
        g.key(b"secret");
        g.key(b"\x7f");
        assert_eq!(g.username, "alice");
        assert_eq!(g.password, b"secre");
        g.key(b"\r");
        assert!(g.submitted);
        g.key(b"\x1b");
        assert!(g.password.is_empty());
        g.x = 240;
        g.y = 245;
        g.click();
        assert_eq!(g.field, 0);
        g.x = 250;
        g.y = 380;
        g.click();
        assert!(g.submitted);
    }
    #[test]
    fn salts_preserve_rounds_and_reject_locked_hashes() {
        assert_eq!(
            hash_settings("$6$rounds=5000$salt$hash"),
            Some(("sha512", "rounds=5000$salt"))
        );
        assert_eq!(hash_settings("!$6$salt$hash"), None);
    }
}
