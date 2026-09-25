use hoswm::{
    client::{Client, Event, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT},
    font::Font,
    surface::Surface,
};
use std::os::unix::process::CommandExt;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn valid_user(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && !s.starts_with('-')
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}
fn hash(password: &str) -> io::Result<String> {
    if password.is_empty() || password.contains(['\n', '\0', ':']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid password",
        ));
    }
    let mut child = Command::new("/bin/hos-password")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{password}\n").as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(io::Error::other("password hashing failed"));
    }
    let h = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !h.starts_with("$6$") || h.contains(':') {
        return Err(io::Error::other("unsupported password hash"));
    }
    Ok(h)
}
fn authenticate(password: &str) -> io::Result<bool> {
    let uid = unsafe { getuid() };
    let passwd = fs::read_to_string("/etc/passwd")?;
    let name = passwd
        .lines()
        .find(|l| l.split(':').nth(2) == Some(&uid.to_string()))
        .and_then(|l| l.split(':').next())
        .unwrap_or("")
        .to_owned();
    let line = fs::read_to_string("/etc/shadow")?
        .lines()
        .find(|l| l.split(':').next() == Some(&name))
        .map(str::to_owned);
    let Some(line) = line else { return Ok(false) };
    let Some(h) = line.split(':').nth(1) else {
        return Ok(false);
    };
    if !h.starts_with("$6$") {
        return Ok(false);
    }
    let mut child = Command::new("/bin/hos-password")
        .arg(h)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{password}\n").as_bytes())?;
    let out = child.wait_with_output()?;
    Ok(out.status.success()
        && out.stdout.strip_suffix(b"\n").unwrap_or(&out.stdout) == h.as_bytes())
}
fn prepare_gui() {
    let uid = unsafe { getuid() };
    unsafe {
        std::env::set_var("HOSWM_SOCKET", format!("/tmp/hoswm-{uid}/session.sock"));
    }
}
fn require_root(password: &str) -> io::Result<()> {
    if unsafe { geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "privileged launcher is not installed",
        ));
    }
    if !authenticate(password)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "authentication failed",
        ));
    }
    let uid = unsafe { getuid() };
    let passwd = fs::read_to_string("/etc/passwd")?;
    let name = passwd
        .lines()
        .find(|l| l.split(':').nth(2) == Some(&uid.to_string()))
        .and_then(|l| l.split(':').next())
        .unwrap_or("");
    let sudo = fs::read_to_string("/etc/group")?.lines().any(|l| {
        let p: Vec<_> = l.split(':').collect();
        p.len() == 4 && p[0] == "sudo" && p[3].split(',').any(|u| u == name)
    });
    if uid != 0 && !sudo {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "user is not in the sudo group",
        ));
    }
    Ok(())
}
fn main() {
    let mode = std::env::args()
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or("useradd")
        .to_owned();
    if mode == "sudo" {
        if let Err(e) = sudo_gui(std::env::args_os().skip(1).collect()) {
            eprintln!("sudo: {e}");
            std::process::exit(1);
        }
    } else if let Err(e) = account_gui(&mode) {
        eprintln!("{mode}: {e}");
    }
}
unsafe extern "C" {
    fn geteuid() -> u32;
    fn getuid() -> u32;
    fn setgroups(n: usize, groups: *const u32) -> i32;
    fn setgid(g: u32) -> i32;
    fn setuid(u: u32) -> i32;
}
fn sudo_gui(command: Vec<OsString>) -> Result<(), Box<dyn std::error::Error>> {
    prepare_gui();
    let c = Client::connect()?;
    let password_only = !command.is_empty();
    let w = c.create("hOS sudo", 520, 300, 0xff72dbac)?;
    c.flags(w, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)?;
    let f = Font::builtin();
    let mut s = Surface::new(520, 300);
    let mut typed_command = String::new();
    let mut password = String::new();
    let mut active = 0;
    let mut status = if password_only {
        "Enter your password to run this command.".to_owned()
    } else {
        "Enter a command and your password.".to_owned()
    };
    loop {
        let (ww, hh, min) = c.size(w)?;
        if min {
            thread::sleep(Duration::from_millis(40));
            continue;
        }
        s.pixels_mut().fill(0xff101815);
        f.draw(&mut s, 24, 24, "Run command as root", 0xff72dbac);
        let fields: &[(&str, &str)] = if password_only {
            &[("Password", "")]
        } else {
            &[("Command", &typed_command), ("Password", "")]
        };
        for (i, (label, value)) in fields.iter().enumerate() {
            let y = 78 + i as i32 * 72;
            f.draw(&mut s, 24, y, label, 0xffc3cdc7);
            s.fill_rect(
                24,
                y + 20,
                472,
                34,
                if active == i { 0xff72dbac } else { 0xff42564b },
            );
            s.fill_rect(25, y + 21, 470, 32, 0xff080b0a);
            let text = if *label == "Password" {
                "*".repeat(password.len())
            } else {
                (*value).to_owned()
            };
            f.draw(&mut s, 34, y + 32, &text, 0xffffffff);
        }
        f.draw(&mut s, 24, 220, &status, 0xffe4c878);
        s.fill_rect(24, 252, 120, 34, 0xff263e34);
        f.draw(&mut s, 48, 263, "Run", 0xffffffff);
        f.draw(
            &mut s,
            180,
            263,
            if password_only {
                "Enter: authenticate"
            } else {
                "Tab: switch  Enter: run"
            },
            0xffc3cdc7,
        );
        c.present(w, ww, hh, s.pixels())?;
        let Some(Event { kind, text, .. }) = c.poll(w)? else {
            thread::sleep(Duration::from_millis(25));
            continue;
        };
        match kind {
            7 | 9 => {
                let _ = c.close(w);
                break;
            }
            6 => match text.as_str() {
                "\x1b" => {
                    let _ = c.close(w);
                    return Ok(());
                }
                "\t" if !password_only => active = 1 - active,
                "\r" => {
                    let selected: Vec<OsString> = if password_only {
                        command.clone()
                    } else {
                        typed_command
                            .split_whitespace()
                            .map(OsString::from)
                            .collect()
                    };
                    if selected.is_empty() {
                        status = "Enter a command path.".into();
                        continue;
                    }
                    match require_root(&password) {
                        Ok(()) => {
                            let _ = c.close(w);
                            exec_root_command(&selected)?;
                            return Ok(());
                        }
                        Err(e) => {
                            status = e.to_string();
                            password.clear();
                        }
                    }
                }
                "\x7f" | "\x08" => {
                    if !password_only && active == 0 {
                        typed_command.pop();
                    } else {
                        password.pop();
                    }
                }
                _ if text.bytes().all(|b| (32..127).contains(&b)) => {
                    if !password_only && active == 0 {
                        if typed_command.len() + text.len() < 256 {
                            typed_command.push_str(&text);
                        }
                    } else if password.len() + text.len() < 128 {
                        password.push_str(&text);
                    }
                }
                _ => (),
            },
            8 => {
                let mut p = text.split_whitespace();
                let x = p.next().and_then(|x| x.parse::<i32>().ok()).unwrap_or(0);
                let y = p.next().and_then(|x| x.parse::<i32>().ok()).unwrap_or(0);
                if x < 150 && y > 245 {
                    let selected: Vec<OsString> = if password_only {
                        command.clone()
                    } else {
                        typed_command
                            .split_whitespace()
                            .map(OsString::from)
                            .collect()
                    };
                    if !selected.is_empty() {
                        match require_root(&password) {
                            Ok(()) => {
                                let _ = c.close(w);
                                exec_root_command(&selected)?;
                                return Ok(());
                            }
                            Err(e) => {
                                status = e.to_string();
                                password.clear();
                            }
                        }
                    } else {
                        status = "Enter a command path.".into();
                    }
                }
            }
            _ => (),
        }
    }
    Ok(())
}
fn exec_root_command(args: &[OsString]) -> io::Result<()> {
    let exe = args
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let requested = Path::new(exe);
    let program = if requested.components().count() == 1 {
        ["/bin", "/sbin", "/usr/bin", "/usr/sbin"]
            .iter()
            .map(|dir| Path::new(dir).join(exe))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "command not found in root PATH")
            })?
    } else {
        requested.to_path_buf()
    };
    // Authentication has completed. Set real as well as effective IDs so shells
    // do not drop privileges, and discard the caller's supplementary groups.
    if unsafe { setgroups(0, std::ptr::null()) } != 0
        || unsafe { setgid(0) } != 0
        || unsafe { setuid(0) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // Keep sudo's PID, foreground process group and inherited terminal FDs.
    // Spawning and returning lets the parent shell reclaim the terminal early.
    Err(Command::new(program)
        .args(&args[1..])
        .env("PATH", "/bin:/sbin:/usr/bin:/usr/sbin")
        .env("HOME", "/root")
        .env("USER", "root")
        .env("LOGNAME", "root")
        .exec())
}

#[cfg(test)]
mod sudo_tests {
    use super::*;

    #[test]
    fn rejects_empty_command() {
        assert_eq!(
            exec_root_command(&[]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    #[ignore = "requires root; run in a disposable VM"]
    fn replaces_launcher_with_root_command() {
        const CHILD: &str = "HOS_SUDO_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert_eq!(unsafe { getuid() }, 1000);
            assert_eq!(unsafe { geteuid() }, 0);
            // PID continuity verifies that the launcher cannot return early and
            // give the terminal back to its parent. Also exercise inherited I/O.
            let script = format!(
                "test \"$$\" = {} && read value && test \"$value\" = input && \
                 test \"$(id -u)\" = 0 && test \"$(id -ru)\" = 0 && \
                 test \"$(id -g)\" = 0 && test \"$(id -rg)\" = 0 && \
                 test \"$(id -G)\" = 0 && test \"$HOME:$USER:$LOGNAME\" = /root:root:root && \
                 test \"$PATH\" = /bin:/sbin:/usr/bin:/usr/sbin || exit 99; \
                 echo stdout-ok; echo stderr-ok >&2; exit 37",
                std::process::id()
            );
            exec_root_command(&["sh".into(), "-c".into(), script.into()]).unwrap();
            panic!("exec returned successfully");
        }
        assert_eq!(unsafe { geteuid() }, 0, "run this test in a root VM");
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "sudo_tests::replaces_launcher_with_root_command",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        unsafe {
            child.pre_exec(|| {
                unsafe extern "C" {
                    fn setresuid(real: u32, effective: u32, saved: u32) -> i32;
                }
                // Model a setuid-root launcher invoked by an ordinary user.
                let groups = [1000, 27];
                if setgroups(groups.len(), groups.as_ptr()) != 0
                    || setgid(1000) != 0
                    || setresuid(1000, 0, 0) != 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = child.spawn().unwrap();
        child.stdin.take().unwrap().write_all(b"input\n").unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(37), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("stdout-ok"));
        assert!(String::from_utf8_lossy(&out.stderr).contains("stderr-ok"));
    }
}

fn account_gui(mode: &str) -> Result<(), Box<dyn std::error::Error>> {
    let _ = geteuid;
    prepare_gui();
    let c = Client::connect()?;
    let w = c.create(&format!("hOS {mode}"), 560, 440, 0xff72dbac)?;
    c.flags(w, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)?;
    let f = Font::builtin();
    let mut s = Surface::new(560, 440);
    let mut fields = [String::new(), String::new(), String::new(), String::new()];
    let mut active = 0usize;
    let mut status = String::from("Enter your password to authorize this change.");
    loop {
        let (ww, hh, min) = c.size(w)?;
        if min {
            thread::sleep(Duration::from_millis(40));
            continue;
        }
        s.pixels_mut().fill(0xff101815);
        f.draw(&mut s, 24, 22, &format!("hOS {mode}"), 0xff72dbac);
        let labels = match mode {
            "useradd" => ["Username", "Full name", "Password", "Root password"],
            "usermod" => [
                "Username",
                "New full name",
                "New password (blank keeps)",
                "Root password",
            ],
            _ => ["Username", "Remove home too? (yes/no)", "Root password", ""],
        };
        let n = if mode == "userdel" { 3 } else { 4 };
        for i in 0..n {
            let y = 66 + i as i32 * 68;
            f.draw(&mut s, 24, y, labels[i], 0xffc3cdc7);
            s.fill_rect(
                24,
                y + 18,
                512,
                32,
                if active == i { 0xff72dbac } else { 0xff42564b },
            );
            s.fill_rect(25, y + 19, 510, 30, 0xff080b0a);
            let v = if (mode == "useradd" && i == 2)
                || (mode == "usermod" && i == 2)
                || (mode == "userdel" && i == 2)
            {
                "*".repeat(fields[i].len())
            } else {
                fields[i].clone()
            };
            f.draw(&mut s, 32, y + 28, &v, 0xffffffff)
        }
        f.draw(&mut s, 24, 350, &status, 0xffe4c878);
        s.fill_rect(24, 382, 120, 34, 0xff263e34);
        f.draw(&mut s, 56, 393, "Apply", 0xffffffff);
        c.present(w, ww, hh, s.pixels())?;
        let Some(Event { kind, text, .. }) = c.poll(w)? else {
            thread::sleep(Duration::from_millis(25));
            continue;
        };
        match kind {
            7 | 9 => {
                let _ = c.close(w);
                break;
            }
            6 => match text.as_str() {
                "\t" => active = (active + 1) % n,
                "\r" => {
                    status = match apply_account(mode, &fields) {
                        Ok(()) => "Account database updated.".into(),
                        Err(e) => e.to_string(),
                    };
                    fields[2].clear();
                }
                "\x7f" | "\x08" => {
                    fields[active].pop();
                }
                _ if text.bytes().all(|b| (32..127).contains(&b)) => {
                    if fields[active].len() + text.len() < 128 {
                        fields[active].push_str(&text)
                    }
                }
                _ => (),
            },
            8 => {
                let mut p = text.split_whitespace();
                let x = p.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                let y = p.next().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
                if x < 150 && y > 375 {
                    status = match apply_account(mode, &fields) {
                        Ok(()) => "Account database updated.".into(),
                        Err(e) => e.to_string(),
                    };
                }
            }
            _ => (),
        }
    }
    Ok(())
}
fn apply_account(mode: &str, v: &[String; 4]) -> io::Result<()> {
    if !valid_user(&v[0]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid username",
        ));
    }
    require_root(if mode == "userdel" { &v[2] } else { &v[3] })?;
    let mut passwd = fs::read_to_string("/etc/passwd")?;
    let mut shadow = fs::read_to_string("/etc/shadow")?;
    let mut group = fs::read_to_string("/etc/group")?;
    if mode == "useradd" {
        if passwd
            .lines()
            .any(|l| l.split(':').next() == Some(v[0].as_str()))
        {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "user exists"));
        }
        let uid = passwd
            .lines()
            .filter_map(|l| l.split(':').nth(2)?.parse::<u32>().ok())
            .max()
            .unwrap_or(999)
            .max(999)
            + 1;
        let h = hash(&v[2])?;
        passwd.push_str(&format!(
            "{}:x:{uid}:{uid}:{}:/home/{}:/bin/bash\n",
            v[0], v[1], v[0]
        ));
        shadow.push_str(&format!("{}:{}:19000:0:99999:7:::\n", v[0], h));
        group.push_str(&format!(
            "{}:x:{uid}:\nusers:x:100:{}\nsudo:x:27:{}\n",
            v[0], v[0], v[0]
        ));
        let home = format!("/home/{}", v[0]);
        fs::create_dir_all(&home)?;
        let status = Command::new("/bin/chown")
            .args(["-R", &format!("{uid}:{uid}"), &home])
            .status()?;
        if !status.success() {
            return Err(io::Error::other("failed to set home ownership"));
        }
    } else if mode == "usermod" {
        let mut found = false;
        let mut out = String::new();
        for line in passwd.lines() {
            let mut p: Vec<&str> = line.split(':').collect();
            if p.first() == Some(&v[0].as_str()) {
                if p.len() != 7 {
                    return Err(io::Error::other("malformed passwd entry"));
                }
                if !v[1].is_empty() {
                    p[4] = &v[1];
                }
                out.push_str(&p.join(":"));
                out.push('\n');
                found = true;
            } else {
                out.push_str(line);
                out.push('\n')
            }
        }
        if !found {
            return Err(io::Error::new(io::ErrorKind::NotFound, "user not found"));
        }
        passwd = out;
        if !v[2].is_empty() {
            let h = hash(&v[2])?;
            shadow = shadow
                .lines()
                .map(|l| {
                    if l.split(':').next() == Some(v[0].as_str()) {
                        format!("{}:{}", v[0], h)
                            + l.splitn(3, ':')
                                .nth(2)
                                .map(|x| format!(":{x}"))
                                .unwrap_or_default()
                                .as_str()
                    } else {
                        l.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
        }
    } else if mode == "userdel" {
        let uid = passwd
            .lines()
            .find(|l| l.split(':').next() == Some(v[0].as_str()))
            .and_then(|l| l.split(':').nth(2))
            .unwrap_or("0")
            .parse::<u32>()
            .unwrap_or(0);
        if uid < 1000 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to delete a system account",
            ));
        }
        let old = passwd.len();
        passwd = passwd
            .lines()
            .filter(|l| l.split(':').next() != Some(v[0].as_str()))
            .map(|l| format!("{l}\n"))
            .collect();
        if old == passwd.len() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "user not found"));
        }
        shadow = shadow
            .lines()
            .filter(|l| l.split(':').next() != Some(v[0].as_str()))
            .map(|l| format!("{l}\n"))
            .collect();
        group = group
            .lines()
            .map(|l| {
                let mut p: Vec<String> = l.split(':').map(str::to_owned).collect();
                if p.len() == 4 {
                    p[3] = p[3]
                        .split(',')
                        .filter(|n| *n != v[0])
                        .collect::<Vec<_>>()
                        .join(",");
                }
                format!("{}\n", p.join(":"))
            })
            .collect();
        if v[1] == "yes" {
            fs::remove_dir_all(format!("/home/{}", v[0]))?;
        }
    }
    atomic_write("/etc/passwd", &passwd, 0o644)?;
    atomic_write("/etc/group", &group, 0o644)?;
    atomic_write("/etc/shadow", &shadow, 0o600)?;
    Ok(())
}
fn atomic_write(path: &str, data: &str, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let p = Path::new(path);
    let tmp = p.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp, data)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::rename(tmp, p)
}
