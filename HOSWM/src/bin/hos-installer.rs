use hoswm::{
    client::{Client, Event, WINDOW_DEFER_CLOSE, WINDOW_PROTECT_EXIT, WINDOW_RAW_INPUT},
    font::Font,
    surface::Surface,
};
use std::{thread, time::Duration};
mod installer {
    //! Native live installer. Commands receive separate arguments, never shell source.
    use hoswm::{
        desktop::{ACCENT, BLACK, Rect},
        font::Font,
        surface::Surface,
    };
    use std::{
        fs,
        io::Write,
        os::unix::fs::{FileTypeExt, PermissionsExt},
        path::Path,
        process::{Command, Stdio},
        sync::mpsc::{self, Receiver},
        time::Duration,
    };

    #[derive(Clone, Debug, PartialEq)]
    struct Disk {
        name: String,
        sectors: u64,
        device: String,
    }
    fn eligible(name: &str, sectors: u64) -> bool {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            && !["loop", "ram", "zram", "sr", "dm-", "md"]
                .iter()
                .any(|p| name.starts_with(p))
            && (2_097_152..=4_294_967_295).contains(&sectors)
    }
    fn disks() -> Result<Vec<Disk>, String> {
        scan_disks(Path::new("/sys/block"), Path::new("/proc"), &|name| {
            fs::metadata(format!("/dev/{name}")).is_ok_and(|m| m.file_type().is_block_device())
        })
    }
    fn scan_disks(
        sys_block: &Path,
        proc: &Path,
        is_block: &impl Fn(&str) -> bool,
    ) -> Result<Vec<Disk>, String> {
        let mounts = fs::read_to_string(proc.join("self/mountinfo")).map_err(|e| e.to_string())?;
        // Kernels built without CONFIG_SWAP do not expose /proc/swaps.
        let swaps = match fs::read_to_string(proc.join("swaps")) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("Cannot inspect active swap: {e}")),
        };
        let mut result = Vec::new();
        for entry in fs::read_dir(sys_block).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let p = entry.path();
            let sectors = fs::read_to_string(p.join("size"))
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0);
            if !eligible(&name, sectors)
                || fs::read_to_string(p.join("ro")).unwrap_or_default().trim() != "0"
            {
                continue;
            }
            let mut devices = vec![p.clone()];
            for child in fs::read_dir(&p).map_err(|e| e.to_string())? {
                let child = child.map_err(|e| e.to_string())?.path();
                if child.join("partition").exists() {
                    devices.push(child);
                }
            }
            let mut busy = false;
            for device in devices {
                let dev = fs::read_to_string(device.join("dev")).map_err(|e| e.to_string())?;
                let devname = format!("/dev/{}", device.file_name().unwrap().to_string_lossy());
                busy |= mounts
                    .lines()
                    .any(|l| l.split_whitespace().nth(2) == Some(dev.trim()));
                busy |= swaps
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some(devname.as_str()));
                busy |= fs::read_dir(device.join("holders"))
                    .map_err(|e| e.to_string())?
                    .next()
                    .is_some();
            }
            if !busy && is_block(&name) {
                result.push(Disk {
                    name,
                    sectors,
                    device: fs::read_to_string(p.join("dev")).map_err(|e| e.to_string())?,
                });
            }
        }
        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }
    fn run(program: &str, args: &[&str], input: &[u8]) -> Result<String, String> {
        let mut child = Command::new(program)
            .args(args)
            .env("PATH", "/bin:/sbin:/usr/bin:/usr/sbin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{program}: {e}"))?;
        let written = child.stdin.take().unwrap().write_all(input);
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "{program}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        written.map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
    fn utility(args: &[&str]) -> Result<String, String> {
        run(&format!("/bin/{}", args[0]), &args[1..], &[])
    }
    fn write(root: &Path, name: &str, content: &str, mode: u32) -> Result<(), String> {
        let p = root.join(name);
        fs::write(&p, content)
            .and_then(|_| fs::set_permissions(p, fs::Permissions::from_mode(mode)))
            .map_err(|e| e.to_string())
    }
    fn hostname_valid(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 63
            && !s.starts_with('-')
            && !s.ends_with('-')
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }
    fn username_valid(s: &str) -> bool {
        !s.is_empty()
            && s.len() <= 32
            && !s.starts_with('-')
            && !s.as_bytes()[0].is_ascii_digit()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    }
    fn root_partition(device: &str) -> String {
        format!(
            "{device}{}1",
            if device.ends_with(|c: char| c.is_ascii_digit()) {
                "p"
            } else {
                ""
            }
        )
    }

    /// Modes the largest-resolution entries offer GRUB, biggest first: GRUB
    /// takes the first one the firmware has, having no "maximum" keyword.
    const LARGEST_MODES: &str = "3840x2160x32,2560x1600x32,2560x1440x32,1920x1200x32,1920x1080x32,1680x1050x32,1600x1200x32,1600x900x32,1440x900x32,1366x768x32,1280x1024x32,1280x800x32,1280x720x32,1152x864x32,1024x768x32,800x600x32,auto";

    fn installed_grub_config(partition: &str) -> String {
        // There is no initramfs in the installed system. GRUB can resolve a
        // filesystem label, but the kernel needs a device path for root=.
        let head = "\n insmod ext2\n search --no-floppy --label HOSROOT --set=root\n insmod all_video";
        let kernel = format!(
            "linux /boot/vmlinuz root={partition} rootfstype=ext4 rootwait ro console=ttyS0,115200n8 console=tty0"
        );
        // The desktop is drawn at 800x600 and scaled to the screen, so the
        // largest-resolution entries only change the mode GRUB sets and passes
        // on with gfxpayload=keep. hos.fbdev=1 keeps the session off KMS, which
        // would otherwise program 800x600 on the hardware itself.
        format!(
            "set timeout=3\nset default=0\n\
             menuentry \"hOS\" {{{head}\n set gfxmode=800x600x32,1024x768x32,800x600x24,800x600x16,auto\n set gfxpayload=$gfxmode\n {kernel} nomodeset\n}}\n\
             menuentry \"hOS (largest resolution)\" {{{head}\n set gfxmode={LARGEST_MODES}\n set gfxpayload=keep\n {kernel} nomodeset\n}}\n\
             menuentry \"hOS (largest resolution, native GPU)\" {{{head}\n set gfxmode={LARGEST_MODES}\n set gfxpayload=keep\n {kernel} hos.fbdev=1\n}}\n"
        )
    }

    fn install(
        disk: Disk,
        hostname: String,
        password: String,
        username: String,
        user_password: String,
        report: &impl Fn(&str),
    ) -> Result<(), String> {
        report("Checking media and password...");
        let hash = run("/bin/hos-password", &[], format!("{password}\n").as_bytes())?;
        drop(password);
        if !hash.trim().starts_with("$6$") || hash.trim().contains(':') {
            return Err("Password hashing failed.".into());
        }
        let user_hash = run(
            "/bin/hos-password",
            &[],
            format!("{user_password}\n").as_bytes(),
        )?;
        drop(user_password);
        if !user_hash.trim().starts_with("$6$") || user_hash.trim().contains(':') {
            return Err("User password hashing failed.".into());
        }
        // Private mount points prevent reuse of an unrelated existing mount.
        let base = format!("/mnt/hos-install-{}", std::process::id());
        fs::create_dir_all("/mnt").map_err(|e| e.to_string())?;
        fs::create_dir(&base).map_err(|e| e.to_string())?;
        let root = format!("{base}/root");
        let iso = format!("{base}/iso");
        fs::create_dir(&root)
            .and_then(|_| fs::create_dir(&iso))
            .map_err(|e| e.to_string())?;
        let mut mounted_root = false;
        let mut mounted_iso = false;
        let result = (|| {
            // The live initramfs includes its boot kernel, so installation also
            // works when GRUB loaded the ISO from USB rather than an optical drive.
            // Keep the optical-drive search as a fallback for older live media.
            let mut kernel = Path::new("/boot/vmlinuz")
                .is_file()
                .then(|| "/boot/vmlinuz".to_string());
            if kernel.is_none() {
                let mut drives: Vec<_> = fs::read_dir("/sys/block")
                    .map_err(|e| e.to_string())?
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("sr") && n[2..].bytes().all(|b| b.is_ascii_digit()))
                    .collect();
                drives.sort();
                for drive in drives {
                    if utility(&[
                        "mount",
                        "-t",
                        "iso9660",
                        "-o",
                        "ro",
                        &format!("/dev/{drive}"),
                        &iso,
                    ])
                    .is_err()
                    {
                        continue;
                    }
                    mounted_iso = true;
                    let candidate = format!("{iso}/boot/vmlinuz");
                    if Path::new(&candidate).is_file() {
                        kernel = Some(candidate);
                        break;
                    }
                    utility(&["umount", &iso])?;
                    mounted_iso = false;
                }
            }
            let kernel = kernel.ok_or("No installation kernel found. Rebuild the installer ISO with the RAM installer files, then boot its RAM installer entry. No disk has been erased.")?;
            for p in [
                "/sbin/mkfs.ext4",
                "/usr/sbin/grub-install",
                "/usr/lib/grub/i386-pc/grub-bios-setup",
            ] {
                fs::metadata(p).map_err(|e| format!("Missing installer dependency {p}: {e}"))?;
            }
            if !disks()?.contains(&disk) {
                return Err("Disk changed or is in use. Reopen the installer.".into());
            }
            let dev = format!("/dev/{}", disk.name);
            report("Partitioning disk (all previous data is being erased)...");
            run("/bin/fdisk", &[&dev], b"o\nn\np\n1\n\n\na\n1\nw\n")?;
            let part = root_partition(&dev);
            for _ in 0..50 {
                if Path::new(&part).exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            if !fs::metadata(&part).is_ok_and(|m| m.file_type().is_block_device()) {
                return Err("Partition did not appear. Reboot the live ISO.".into());
            }
            report("Formatting ext4 filesystem...");
            run("/sbin/mkfs.ext4", &["-F", "-L", "HOSROOT", &part], &[])?;
            utility(&["mount", &part, &root])?;
            mounted_root = true;
            report("Copying hOS and GUI applications...");
            let r = Path::new(&root);
            for dir in [
                "dev",
                "proc",
                "sys",
                "tmp",
                "root",
                "run",
                "etc/init.d",
                // Configuration and saved state for the init services.
                "etc/hos",
                "var/lib/hos",
                "boot/grub",
            ] {
                fs::create_dir_all(r.join(dir)).map_err(|e| e.to_string())?;
            }
            fs::set_permissions(r.join("tmp"), fs::Permissions::from_mode(0o1777))
                .map_err(|e| e.to_string())?;
            fs::set_permissions(r.join("root"), fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
            for dir in ["bin", "sbin", "usr", "lib", "lib64"] {
                if Path::new(&format!("/{dir}")).exists() {
                    utility(&["cp", "-a", &format!("/{dir}"), &root])?;
                }
            }
            write(
                r,
                "etc/bash.bashrc",
                include_str!("../../../tooling/bash.bashrc"),
                0o644,
            )?;
            write(
                r,
                "etc/inputrc",
                include_str!("../../../tooling/inputrc"),
                0o644,
            )?;
            fs::copy(kernel, r.join("boot/vmlinuz")).map_err(|e| e.to_string())?;
            write(r, "etc/hostname", &format!("{hostname}\n"), 0o644)?;
            let uid = 1000;
            write(
                r,
                "etc/passwd",
                &format!(
                    "root:x:0:0:root:/root:/bin/bash\n{username}:x:{uid}:{uid}:{username}:/home/{username}:/bin/bash\n"
                ),
                0o644,
            )?;
            write(
                r,
                "etc/group",
                &format!(
                    "root:x:0:\nusers:x:100:{username}\n{username}:x:{uid}:\nsudo:x:27:{username}\n"
                ),
                0o644,
            )?;
            write(
                r,
                "etc/shadow",
                &format!(
                    "root:{}:19000:0:99999:7:::\n{username}:{}:19000:0:99999:7:::\n",
                    hash.trim(),
                    user_hash.trim()
                ),
                0o600,
            )?;
            fs::create_dir_all(r.join(format!("home/{username}"))).map_err(|e| e.to_string())?;
            fs::set_permissions(
                r.join(format!("home/{username}")),
                fs::Permissions::from_mode(0o750),
            )
            .map_err(|e| e.to_string())?;
            run(
                "/bin/chown",
                &["-R", "1000:1000", &format!("{root}/home/{username}")],
                &[],
            )?;
            write(
                r,
                "etc/init.d/rcS",
                include_str!("../installed_rcS.sh"),
                0o755,
            )?;
            write(
                r,
                "etc/profile",
                "export PATH=/bin:/sbin:/usr/bin:/usr/sbin TERM=linux\n[ -n \"$BASH_VERSION\" ] && . /etc/bash.bashrc\n",
                0o644,
            )?;
            let account_app = r.join("bin/hos-account");
            if !account_app.is_file() {
                return Err("Missing /bin/hos-account in installer media.".into());
            }
            fs::set_permissions(&account_app, fs::Permissions::from_mode(0o4755))
                .map_err(|e| format!("Enable privileged account GUI: {e}"))?;
            for app in ["useradd", "usermod", "userdel", "sudo"] {
                let link = r.join("bin").join(app);
                match fs::read_link(&link) {
                    Ok(target) if target == Path::new("hos-account") => (),
                    Ok(_) => return Err(format!("Unexpected /bin/{app} link target.")),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        std::os::unix::fs::symlink("hos-account", link)
                            .map_err(|e| format!("Create /bin/{app} link: {e}"))?;
                    }
                    Err(e) => return Err(format!("Inspect /bin/{app} link: {e}")),
                }
            }
            write(
                r,
                "boot/grub/grub.cfg",
                &installed_grub_config(&part),
                0o644,
            )?;
            report("Installing BIOS GRUB bootloader...");
            run(
                "/usr/sbin/grub-install",
                &[
                    "--target=i386-pc",
                    "--directory=/usr/lib/grub/i386-pc",
                    &format!("--boot-directory={root}/boot"),
                    "--recheck",
                    &dev,
                ],
                &[],
            )?;
            utility(&["sync"])?;
            Ok(())
        })();
        let mut cleanup = Ok(());
        for (mounted, path) in [(mounted_root, &root), (mounted_iso, &iso)] {
            if mounted {
                if let Err(e) = utility(&["umount", path]) {
                    cleanup = Err(e);
                }
            }
        }
        let _ = fs::remove_dir(&root);
        let _ = fs::remove_dir(&iso);
        let _ = fs::remove_dir(&base);
        result.and(cleanup)
    }

    pub struct Installer {
        disks: Vec<Disk>,
        selected: usize,
        page: u8,
        field: usize,
        values: [String; 7],
        status: String,
        worker: Option<Receiver<Result<String, String>>>,
    }
    impl Default for Installer {
        fn default() -> Self {
            Self::new()
        }
    }
    impl Installer {
        pub fn new() -> Self {
            let mut s = Self {
                disks: vec![],
                selected: 0,
                page: 0,
                field: 0,
                values: [
                    "hOS".into(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ],
                status: String::new(),
                worker: None,
            };
            s.refresh();
            s
        }
        pub fn busy(&self) -> bool {
            self.page == 3
        }
        fn refresh(&mut self) {
            self.selected = 0;
            self.disks.clear();
            match disks() {
                Ok(d) => {
                    self.disks = d;
                    self.status = if self.disks.is_empty() {
                        "No unused writable disks (1 GiB to 2 TiB). Attach a disk and refresh."
                            .into()
                    } else {
                        String::new()
                    };
                }
                Err(e) => self.status = e,
            }
        }
        fn next(&mut self) {
            self.status.clear();
            match self.page {
                0 if !self.disks.is_empty() => self.page = 1,
                1 => {
                    if !hostname_valid(&self.values[0]) {
                        self.status = "Hostname: 1-63 letters, digits or interior hyphens.".into();
                    } else if !username_valid(&self.values[3]) {
                        self.status = "Use a lowercase username (letters, digits, _ or -), not starting with a digit or dash.".into();
                    } else if self.values[1].is_empty()
                        || self.values[1] != self.values[2]
                        || self.values[4].is_empty()
                        || self.values[4] != self.values[5]
                    {
                        self.status = "Enter matching, nonempty root and user passwords.".into();
                    } else {
                        self.page = 2;
                        self.field = 3;
                    }
                }
                2 => {
                    let disk = self.disks[self.selected].clone();
                    if self.values[6].trim() != format!("ERASE /dev/{}", disk.name) {
                        self.status = "Type the exact erase confirmation shown above.".into();
                        return;
                    }
                    let host = self.values[0].clone();
                    let username = self.values[3].clone();
                    let password = std::mem::take(&mut self.values[1]);
                    let user_password = std::mem::take(&mut self.values[4]);
                    self.values[2].clear();
                    self.values[5].clear();
                    let (tx, rx) = mpsc::channel();
                    match std::thread::Builder::new().name("installer".into()).spawn(move || {
                    let result = install(disk, host, password, username, user_password, &|s| { let _ = tx.send(Ok(s.into())); });
                    let _ = tx.send(result.map(|_| "Installation complete. Shut down, remove the ISO, then boot from disk.".into()));
                }) {
                    Ok(_) => { self.worker = Some(rx); self.page = 3; self.status = "Starting installation... Do not power off.".into(); }
                    Err(e) => { self.page = 1; self.field = 1; self.status = format!("Cannot start installer: {e}"); }
                }
                }
                _ => (),
            }
        }
        pub fn tick(&mut self) -> bool {
            let Some(rx) = &self.worker else {
                return false;
            };
            let mut dirty = false;
            loop {
                match rx.try_recv() {
                    Ok(Ok(s)) => {
                        self.status = s;
                        dirty = true;
                    }
                    Ok(Err(e)) => {
                        self.status = format!(
                            "Installation failed: {e}\nThe disk may be partially written. Close and retry from the live ISO."
                        );
                        self.page = 5;
                        dirty = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        if self.page == 3 {
                            self.page = if self.status.starts_with("Installation complete.") {
                                4
                            } else {
                                self.status =
                                "Installer stopped unexpectedly. Disk may be partially written."
                                    .into();
                                5
                            };
                        }
                        self.worker = None;
                        return true;
                    }
                }
            }
            dirty
        }
        pub fn click(&mut self, x: i32, y: i32) {
            if self.page == 0 && (70..294).contains(&y) && (20..620).contains(&x) {
                let i = ((y - 70) / 28) as usize;
                // Show a rolling list, navigable with arrow keys for additional disks.
                let start = self.selected.saturating_sub(7);
                if start + i < self.disks.len() {
                    self.selected = start + i;
                }
            }
            if self.page == 1 {
                for i in 0..6 {
                    if field_rect(i).contains(x, y) {
                        self.field = i;
                    }
                }
            }
            if self.page == 2 && field_rect(6).contains(x, y) {
                self.field = 6;
            }
            if (Rect {
                x: 20,
                y: 350,
                w: 120,
                h: 30,
            })
            .contains(x, y)
            {
                if self.page == 0 {
                    self.refresh();
                } else if self.page < 3 {
                    self.page -= 1;
                    self.field = 0;
                    self.values[6].clear();
                    self.status.clear();
                }
            }
            if (Rect {
                x: 460,
                y: 350,
                w: 160,
                h: 30,
            })
            .contains(x, y)
            {
                self.next();
            }
        }
        pub fn key(&mut self, bytes: &[u8]) {
            if self.page >= 3 {
                return;
            }
            if self.page == 0 {
                if bytes == b"\x1b[B" && self.selected + 1 < self.disks.len() {
                    self.selected += 1;
                }
                if bytes == b"\x1b[A" {
                    self.selected = self.selected.saturating_sub(1);
                }
                if bytes == b"r" {
                    self.refresh();
                }
            }
            if bytes == b"\r" {
                self.next();
                return;
            }
            if bytes == b"\t" && self.page == 1 {
                self.field = (self.field + 1) % 6;
                return;
            }
            if self.page == 1 || self.page == 2 {
                let value = &mut self.values[self.field];
                if bytes == [127] || bytes == [8] {
                    value.pop();
                } else if value.len() + bytes.len() <= 128
                    && bytes.iter().all(|b| (32..127).contains(b))
                {
                    value.push_str(&String::from_utf8_lossy(bytes));
                }
            }
        }
        pub fn draw(&self, s: &mut Surface, f: &Font<'_>) {
            s.pixels_mut().fill(BLACK);
            f.draw(
                s,
                20,
                18,
                &[
                    "1. Choose a disk",
                    "2. Configure hOS",
                    "3. Review and erase",
                    "4. Installing hOS",
                    "Installation complete",
                    "Installation failed",
                ][self.page as usize],
                ACCENT,
            );
            let label = |s: &mut Surface, y, text: &str| {
                f.draw(s, 20, y, text, 0xffdedede);
            };
            if self.page == 0 {
                label(
                    s,
                    44,
                    "BIOS / MBR installation. The entire selected disk will be erased.",
                );
                for (i, d) in self
                    .disks
                    .iter()
                    .enumerate()
                    .skip(self.selected.saturating_sub(7))
                    .take(8)
                {
                    let y = 70 + (i - self.selected.saturating_sub(7)) as i32 * 28;
                    if i == self.selected {
                        s.fill_rect(20, y, 600, 26, 0xff263e34);
                    }
                    f.draw(
                        s,
                        28,
                        y + 8,
                        &format!("/dev/{}  {} MiB", d.name, d.sectors / 2048),
                        0xffffffff,
                    );
                }
                label(s, 302, "Click a disk or use Up/Down. R refreshes the list.");
            }
            if self.page == 1 {
                for (i, title) in [
                    "Hostname",
                    "Root password",
                    "Confirm root password",
                    "Username",
                    "User password",
                    "Confirm user password",
                ]
                .iter()
                .enumerate()
                {
                    label(s, 34 + i as i32 * 43, title);
                    self.draw_field(s, f, i);
                }
                label(
                    s,
                    274,
                    "Tab switches fields. Initial user receives sudo access.",
                );
            }
            if self.page == 2 {
                let d = &self.disks[self.selected];
                label(
                    s,
                    60,
                    &format!("Disk: /dev/{} ({} MiB)", d.name, d.sectors / 2048),
                );
                label(
                    s,
                    88,
                    &format!(
                        "Hostname: {}   User: {} (sudo access)",
                        self.values[0], self.values[3]
                    ),
                );
                label(s, 116, "Layout: one ext4 partition, BIOS GRUB bootloader.");
                label(
                    s,
                    158,
                    "WARNING: All data on this disk will be permanently erased.",
                );
                label(
                    s,
                    190,
                    &format!("Type ERASE /dev/{} then click Erase & install:", d.name),
                );
                self.draw_field(s, f, 6);
            }
            let y = if self.page >= 3 { 80 } else { 316 };
            for (i, line) in self
                .status
                .as_bytes()
                .chunks(74)
                .take(if self.page >= 3 { 18 } else { 2 })
                .enumerate()
            {
                f.draw(
                    s,
                    20,
                    y + i as i32 * 14,
                    &String::from_utf8_lossy(line),
                    0xffe4c878,
                );
            }
            if self.page < 3 {
                for (x, w, text) in [
                    (20, 120, if self.page == 0 { "Refresh" } else { "Back" }),
                    (
                        460,
                        160,
                        if self.page == 2 {
                            "Erase & install"
                        } else {
                            "Next"
                        },
                    ),
                ] {
                    s.fill_rect(x, 350, w, 30, 0xff263e34);
                    f.draw(s, x + 12, 360, text, 0xffffffff);
                }
            }
        }
        fn draw_field(&self, s: &mut Surface, f: &Font<'_>, i: usize) {
            let r = field_rect(i);
            s.fill_rect(
                r.x,
                r.y,
                r.w,
                r.h,
                if self.field == i { ACCENT } else { 0xff555555 },
            );
            s.fill_rect(r.x + 1, r.y + 1, r.w - 2, r.h - 2, BLACK);
            let text = if i == 1 || i == 2 || i == 4 || i == 5 {
                "*".repeat(self.values[i].len())
            } else {
                self.values[i].clone()
            };
            let tail: String = text.chars().skip(text.len().saturating_sub(70)).collect();
            f.draw(s, r.x + 8, r.y + 10, &tail, 0xffffffff);
        }
    }
    fn field_rect(i: usize) -> Rect {
        Rect {
            x: 20,
            y: if i == 6 { 218 } else { 46 + i as i32 * 43 },
            w: 600,
            h: 30,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn installed_boot_uses_kernel_resolvable_root_without_initramfs() {
            for (device, partition) in [
                ("/dev/sda", "/dev/sda1"),
                ("/dev/sdb", "/dev/sdb1"),
                ("/dev/vda", "/dev/vda1"),
                ("/dev/nvme0n1", "/dev/nvme0n1p1"),
                ("/dev/mmcblk0", "/dev/mmcblk0p1"),
            ] {
                assert_eq!(root_partition(device), partition);
                let config = installed_grub_config(&root_partition(device));
                let entries: Vec<&str> = config
                    .lines()
                    .filter(|line| line.trim_start().starts_with("linux "))
                    .collect();
                // The default entry, then the two largest-resolution ones.
                assert_eq!(entries.len(), 3, "{config}");
                for linux in entries {
                    assert!(
                        linux
                            .split_whitespace()
                            .any(|arg| arg == format!("root={partition}"))
                    );
                    assert!(linux.contains("rootfstype=ext4 rootwait ro"));
                    assert!(!linux.contains("LABEL="));
                }
                assert_eq!(config.matches("set gfxpayload").count(), 3, "{config}");
                assert!(config.contains("hos.fbdev=1"), "{config}");
            }
        }

        #[test]
        fn sata_disk_discovery_without_live_marker_or_swap_support() {
            let base = std::env::temp_dir().join(format!("hos-disk-test-{}", std::process::id()));
            fs::create_dir(&base).unwrap();
            let sys = base.join("sys");
            let proc = base.join("proc");
            fs::create_dir_all(proc.join("self")).unwrap();
            fs::write(proc.join("self/mountinfo"), "").unwrap();
            let disk = sys.join("sda");
            fs::create_dir_all(disk.join("holders")).unwrap();
            fs::write(disk.join("size"), "16777216\n").unwrap();
            fs::write(disk.join("ro"), "0\n").unwrap();
            fs::write(disk.join("dev"), "8:0\n").unwrap();
            let scan = || scan_disks(&sys, &proc, &|name| name == "sda").unwrap();
            assert_eq!(scan()[0].name, "sda");
            assert!(scan_disks(&sys, &proc, &|_| false).unwrap().is_empty());
            fs::write(disk.join("ro"), "1").unwrap();
            assert!(scan().is_empty());
            fs::write(disk.join("ro"), "0").unwrap();
            let part = disk.join("sda1");
            fs::create_dir_all(part.join("holders")).unwrap();
            fs::write(part.join("partition"), "1").unwrap();
            fs::write(part.join("dev"), "8:1\n").unwrap();
            fs::write(
                proc.join("self/mountinfo"),
                "23 1 8:1 / / rw - ext4 /dev/sda1 rw\n",
            )
            .unwrap();
            assert!(scan().is_empty());
            fs::write(proc.join("self/mountinfo"), "").unwrap();
            fs::write(proc.join("swaps"), "/dev/sda1 partition 100 0 -2\n").unwrap();
            assert!(scan().is_empty());
            fs::write(proc.join("swaps"), "").unwrap();
            fs::write(part.join("holders/dm-0"), "").unwrap();
            assert!(scan().is_empty());
            fs::remove_file(part.join("holders/dm-0")).unwrap();
            assert_eq!(scan().len(), 1);
            fs::remove_file(proc.join("self/mountinfo")).unwrap();
            assert!(scan_disks(&sys, &proc, &|_| true).is_err());
            fs::remove_dir_all(base).unwrap();
        }

        #[test]
        fn validate_targets_and_configuration() {
            assert!(eligible("nvme0n1", 8_388_608));
            assert!(!eligible("vda", 4_294_967_296));
            for n in ["", "../vda", "loop0", "sr0", "dm-0", "md0"] {
                assert!(!eligible(n, 8_388_608));
            }
            assert!(hostname_valid("my-hos"));
            for h in ["", "-hos", "hos-", "a b", "a\nb"] {
                assert!(!hostname_valid(h));
            }
        }
        #[test]
        fn wizard_requires_matching_password_and_exact_confirmation() {
            let mut s = Installer::new();
            s.disks = vec![Disk {
                name: "vda".into(),
                sectors: 8_388_608,
                device: "252:0".into(),
            }];
            s.next();
            assert_eq!(s.page, 1);
            s.next();
            assert_eq!(s.page, 1);
            s.values[1] = "secret".into();
            s.values[2] = "different".into();
            s.next();
            assert_eq!(s.page, 1);
            s.values[2] = "secret".into();
            // The account page also needs a valid username and a user password
            // that matches its confirmation.
            s.next();
            assert_eq!(s.page, 1);
            s.values[3] = "Operator".into();
            s.next();
            assert_eq!(s.page, 1);
            s.values[3] = "operator".into();
            s.values[4] = "user-secret".into();
            s.values[5] = "user-different".into();
            s.next();
            assert_eq!(s.page, 1);
            s.values[5] = "user-secret".into();
            s.next();
            assert_eq!(s.page, 2);
            s.values[6] = "ERASE /dev/vdb".into();
            s.next();
            assert_eq!(s.page, 2);
            assert!(s.worker.is_none());
            // The confirmation for the right disk starts the installation, so
            // this test stops here rather than erasing the machine it runs on.
            // Going back drops it and returns to the account page.
            s.click(30, 360);
            assert_eq!(s.page, 1);
            assert!(s.values[6].is_empty());
            assert!(s.worker.is_none());
        }
    }
}
use installer::Installer;
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let c = Client::connect()?;
    let w = c.create("Install hOS", 640, 400, 0xff72dbac)?;
    c.flags(w, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)?;
    let mut app = Installer::new();
    let f = Font::builtin();
    let mut s = Surface::new(640, 400);
    let mut dirty = true;
    loop {
        let (width, height, min) = c.size(w)?;
        if min {
            thread::sleep(Duration::from_millis(40));
            continue;
        }
        if width as usize != s.width() || height as usize != s.height() {
            s.reset(width as usize, height as usize, 0xff000000);
            dirty = true
        }
        for _ in 0..32 {
            let Some(Event {
                kind,
                control: _,
                text,
            }) = c.poll(w)?
            else {
                break;
            };
            match kind {
                6 => app.key(text.as_bytes()),
                8 => {
                    let mut p = text.split_whitespace();
                    let x = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let y = p.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                    let action = p.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
                    if action == 1 {
                        app.click(x, y)
                    }
                }
                9 if !app.busy() => {
                    c.flags(w, WINDOW_RAW_INPUT)?;
                    let _ = c.close(w);
                    return Ok(());
                }
                9 => (),
                _ => (),
            }
            dirty = true;
        }
        dirty |= app.tick();
        c.flags(
            w,
            WINDOW_RAW_INPUT
                | WINDOW_DEFER_CLOSE
                | if app.busy() { WINDOW_PROTECT_EXIT } else { 0 },
        )?;
        if dirty {
            s.pixels_mut().fill(0xff000000);
            app.draw(&mut s, &f);
            c.present(w, width, height, s.pixels())?;
            dirty = false
        }
        thread::sleep(Duration::from_millis(if app.busy() { 100 } else { 25 }));
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-installer: {e}");
    }
}
