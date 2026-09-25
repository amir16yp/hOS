#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
compile_error!("This first slice supports Linux x86_64 only");
use hoswm::{
    abi,
    config::Config,
    desktop::{Desktop, Shot},
    drm, framebuffer,
    input::{self, Devices},
    keyboard::Keyboard,
    qoi,
    shortcuts::Action,
    surface::Surface,
    toast,
};
use std::{
    process::{Child, Command},
    time::{Duration, Instant},
};

const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);

enum Output {
    Framebuffer(framebuffer::Display),
    Drm(drm::Display),
}
/// `hos.fbdev=1` on the kernel command line: use the framebuffer device even
/// where KMS is available. The largest-resolution boot entry passes it, because
/// KMS programs the display at 800x600 while the framebuffer path scales the
/// desktop up to whatever mode the display is already in.
fn cmdline_prefers_fbdev() -> bool {
    std::fs::read_to_string("/proc/cmdline")
        .is_ok_and(|line| line.split_whitespace().any(|word| word == "hos.fbdev=1"))
}
impl Output {
    fn open() -> Result<Self, String> {
        // Prefer KMS and the hardware cursor; explicit fbdev selection wins.
        if std::env::var_os("HOS_FB_DEVICE").is_none() && !cmdline_prefers_fbdev() {
            let paths = if let Ok(path) = std::env::var("HOS_DRM_DEVICE") {
                vec![path]
            } else {
                let mut paths: Vec<String> = std::fs::read_dir("/dev/dri")
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .strip_prefix("card")
                            .is_some_and(|suffix| {
                                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                            })
                    })
                    .map(|entry| entry.path().to_string_lossy().into_owned())
                    .collect();
                paths.sort();
                paths
            };
            for path in paths {
                match drm::Display::open(&path) {
                    Ok(display) => return Ok(Self::Drm(display)),
                    Err(error) => eprintln!("HOSWM DRM unavailable on {path}: {error}"),
                }
            }
        }
        let path = std::env::var("HOS_FB_DEVICE").unwrap_or_else(|_| "/dev/fb0".into());
        framebuffer::Display::open(&path).map(Self::Framebuffer)
    }
    fn present(&mut self, pixels: &[u32]) -> Result<(), String> {
        match self {
            Self::Framebuffer(d) => d.present(pixels),
            Self::Drm(d) => d.present(pixels),
        }
    }
    fn ready(&mut self) -> Result<bool, String> {
        match self {
            Self::Framebuffer(_) => Ok(true),
            Self::Drm(d) => d.ready(),
        }
    }
    fn watch(&self, reactor: &mut hoswm::reactor::Reactor) {
        if let Self::Drm(d) = self {
            d.watch(reactor);
        }
    }
    fn cursor(&mut self, x: i32, y: i32) -> bool {
        match self {
            Self::Framebuffer(_) => false,
            Self::Drm(d) => d.cursor(x, y),
        }
    }
}
// Keep phase at 60 Hz without replaying missed frames after a slow presentation.
fn advance_frame(deadline: Instant, now: Instant) -> Instant {
    let elapsed = now.saturating_duration_since(deadline);
    deadline + FRAME * (elapsed.as_nanos() / FRAME.as_nanos() + 1).min(u32::MAX as u128) as u32
}
fn arm(reactor: &mut hoswm::reactor::Reactor, devices: &Devices, display: &Output) {
    reactor.clear();
    devices.watch(reactor);
    display.watch(reactor);
}

fn greet(devices: &mut Devices, display: &mut Output) -> Result<(), String> {
    let mut greeter = hoswm::greeter::Greeter::default();
    let mut keyboard = Keyboard::default();
    let mut surface = Surface::new(800, 600);
    let mut dirty = true;
    let mut pending = Vec::with_capacity(512);
    let mut reactor = hoswm::reactor::Reactor::default();
    let mut deadline = Instant::now();
    loop {
        for change in devices.poll() {
            eprintln!("HOSWM input: {}", change.message());
        }
        for change in devices.read(&mut pending) {
            eprintln!("HOSWM input: {}", change.message());
            if matches!(change, input::Change::Overflow(_)) {
                keyboard.reset();
            }
        }
        for &(kind, code, value) in &pending {
            match (kind, code) {
                (2, 0) => greeter.x = greeter.x.saturating_add(value).clamp(0, 799),
                (2, 1) => greeter.y = greeter.y.saturating_add(value).clamp(0, 599),
                (input::ABSOLUTE, 0) => greeter.x = value.clamp(0, 799),
                (input::ABSOLUTE, 1) => greeter.y = value.clamp(0, 599),
                (1, 272) if value == 1 => greeter.click(),
                (1, code) if code < 256 => {
                    if let Some(bytes) = keyboard.event(code, value) {
                        greeter.key(&bytes);
                    }
                }
                _ => (),
            }
            dirty |= matches!(kind, 1 | 2 | input::ABSOLUTE);
            if greeter.submitted {
                if let Some(user) = greeter.login() {
                    hoswm::greeter::enter_session(&user)
                        .map_err(|e| format!("start user session: {e}"))?;
                    return Ok(());
                }
            }
        }
        let ready = display.ready()?;
        if dirty && ready && Instant::now() >= deadline {
            greeter.draw(&mut surface);
            display.present(surface.pixels())?;
            dirty = false;
            deadline = advance_frame(deadline, Instant::now());
        }
        arm(&mut reactor, devices, display);
        reactor
            .wait(if dirty && ready {
                deadline.saturating_duration_since(Instant::now())
            } else {
                Duration::from_millis(100)
            })
            .map_err(|e| format!("input wait: {e}"))?;
    }
}
/// Write a capture of the last presented frame, returning its path.
fn screenshot(state: &Desktop, frame: &Surface, shot: Shot) -> Result<String, String> {
    let stamp = toast::format_time(toast::now_ms());
    let path = state.config.screenshots.join(format!(
        "shot-{}-{}-{:03}.qoi",
        stamp[..10].replace('-', ""),
        stamp[11..].replace(':', ""),
        toast::now_ms() % 1000
    ));
    match shot {
        Shot::Screen => qoi::save_surface(&path, frame),
        Shot::Window(id) => {
            let r = state.window_rect(id).ok_or("the window has closed")?;
            qoi::save_surface(&path, &frame.crop(r.x, r.y, r.w, r.h))
        }
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path.display().to_string())
}
fn run() -> Result<(), String> {
    let greeter = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--greeter") => true,
        _ => return Err("Usage: hoswm [--greeter]".into()),
    };
    // Input is discovered continuously: a session starts with no keyboard or
    // mouse and picks them up when they are plugged in.
    let mut devices = Devices::default();
    let mut startup = devices.scan();
    for change in &startup {
        eprintln!("HOSWM input: {}", change.message());
    }
    if !devices.has_keyboard() || !devices.has_pointer() {
        eprintln!(
            "HOSWM input: waiting for devices (keyboard={}, pointer={})",
            devices.has_keyboard(),
            devices.has_pointer()
        );
    }
    let mut display = Output::open()?;
    if greeter {
        greet(&mut devices, &mut display)?;
        startup.clear();
    }
    // No desktop, shell or application socket exists before authentication,
    // and the configuration belongs to the account that just logged in.
    let mut state = Desktop::with_config(Config::load());
    state.login_session = greeter;
    for warning in std::mem::take(&mut state.config.warnings) {
        eprintln!("HOSWM config: {warning}");
        state.toast(format!("Configuration: {warning}"), 0xffe4c878, 6000);
    }
    for change in startup {
        if matches!(change, input::Change::Unavailable(..)) {
            state.toast(change.message(), change.color(), 6000);
        }
    }
    for (missing, kind) in [(!devices.has_keyboard(), "keyboard"), (!devices.has_pointer(), "mouse")] {
        if missing {
            state.toast(format!("No {kind} connected yet"), 0xffe4c878, 8000);
        }
    }
    let mut keyboard = Keyboard::default();
    let mut server = abi::Server::bind().map_err(|e| format!("window ABI: {e}"))?;
    let mut applications: Vec<Child> = Vec::new();
    let mut framebuffer = Surface::new(800, 600);
    let mut dirty = true;
    let mut scene_dirty = true;
    let mut cursor = hoswm::cursor::SoftwareCursor::default();
    let mut pending = Vec::with_capacity(512);
    let mut reactor = hoswm::reactor::Reactor::default();
    let mut deadline = Instant::now();
    loop {
        let mut changes = devices.poll();
        changes.extend(devices.read(&mut pending));
        for change in changes {
            eprintln!("HOSWM input: {}", change.message());
            if matches!(change, input::Change::Overflow(_)) {
                keyboard.reset();
            }
            state.toast(change.message(), change.color(), 0);
            scene_dirty = true;
        }
        for &(kind, code, value) in &pending {
            match (kind, code) {
                (2, 0) => {
                    scene_dirty |= state.motion(state.x.saturating_add(value), state.y);
                    state.raw_pointer_motion(value, 0);
                }
                (2, 1) => {
                    scene_dirty |= state.motion(state.x, state.y.saturating_add(value));
                    state.raw_pointer_motion(0, value);
                }
                // Tablets report where the pointer is, not how far it moved.
                (input::ABSOLUTE, 0) => {
                    scene_dirty |= state.motion(value, state.y);
                    state.raw_pointer_motion(0, 0);
                }
                (input::ABSOLUTE, 1) => {
                    scene_dirty |= state.motion(state.x, value);
                    state.raw_pointer_motion(0, 0);
                }
                (2, 8) => state.scroll_wheel(value, 0),
                (2, 6) => state.scroll_wheel(value, 1),
                (1, 272) if value != 2 => state.mouse(value != 0),
                (1, 273) if value == 1 => {
                    state.raw_pointer_right();
                    state.right_click();
                }
                (1, code) if code < 256 => {
                    let bytes = keyboard.event(code, value);
                    let modifiers = keyboard.modifiers();
                    if !modifiers.alt {
                        state.end_window_cycle();
                    }
                    let action = state.config.bindings.resolve(
                        code,
                        value,
                        modifiers,
                        state.raw_input_focused(),
                    );
                    match action {
                        Some(Action::Exit) if !state.installing() => return Ok(()),
                        Some(Action::Exit) => {
                            state.toast("Installation in progress; exit is disabled", 0xffe4c878, 0);
                            scene_dirty = true;
                        }
                        Some(action) if state.shortcut(action) => scene_dirty = true,
                        _ => {
                            if let Some(bytes) = bytes {
                                state.key_mod(
                                    &bytes,
                                    modifiers.ctrl as u32
                                        | ((modifiers.shift as u32) << 1)
                                        | ((modifiers.alt as u32) << 2),
                                );
                            }
                        }
                    }
                }
                _ => (),
            }
            if state.quit {
                return Ok(());
            }
            scene_dirty |= kind == 1;
            dirty |= kind == 1 || (kind == 2 && code <= 1) || kind == input::ABSOLUTE;
        }
        if let Some(program) = state.launch.take() {
            match Command::new(&program).spawn() {
                Ok(child) => applications.push(child),
                Err(e) => state.toast(format!("Could not start {program}: {e}"), 0xffef6976, 6000),
            }
            scene_dirty = true;
        }
        applications.retain_mut(|child| match child.try_wait() {
            Ok(Some(_)) => {
                state.close_owner(child.id());
                scene_dirty = true;
                false
            }
            Ok(None) => true,
            Err(_) => {
                state.close_owner(child.id());
                scene_dirty = true;
                false
            }
        });
        scene_dirty |= server.tick(&mut state);
        scene_dirty |= state.tick();
        if let Some(error) = state.toasts.error.take() {
            eprintln!("HOSWM notification log: {error}");
        }
        dirty |= scene_dirty;
        let ready = display.ready()?;
        let mut presented = false;
        if dirty && ready && Instant::now() >= deadline {
            cursor.hide(&mut framebuffer);
            if scene_dirty {
                state.draw_scene(&mut framebuffer);
            }
            let hardware_cursor = display.cursor(state.x, state.y);
            if !hardware_cursor {
                cursor.show(&mut framebuffer, state.x, state.y);
            }
            if scene_dirty || !hardware_cursor {
                display.present(framebuffer.pixels())?;
            }
            dirty = false;
            scene_dirty = false;
            presented = true;
            deadline = advance_frame(deadline, Instant::now());
        }
        // Capture what was actually presented, so a screenshot never contains
        // the notification announcing itself.
        if presented {
            if let Some(shot) = state.screenshot.take() {
                let (message, color) = match screenshot(&state, &framebuffer, shot) {
                    Ok(path) => (format!("Saved {path}"), state.config.accent),
                    Err(e) => (format!("Screenshot failed: {e}"), 0xffef6976),
                };
                state.toast(message, color, 0);
                scene_dirty = true;
            }
        }
        arm(&mut reactor, &devices, &display);
        server.watch(&mut reactor);
        reactor
            .wait(if dirty && ready {
                deadline.saturating_duration_since(Instant::now())
            } else {
                Duration::from_millis(100)
            })
            .map_err(|e| format!("session wait: {e}"))?;
    }
}
fn main() {
    if let Err(e) = run() {
        eprintln!("HOSWM ERROR: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_deadlines_preserve_phase_and_skip_missed_frames() {
        let start = Instant::now();
        assert_eq!(
            advance_frame(start, start + Duration::from_millis(4)),
            start + FRAME
        );
        assert_eq!(advance_frame(start, start + FRAME), start + FRAME * 2);
        assert_eq!(
            advance_frame(start, start + FRAME * 8 + Duration::from_millis(2)),
            start + FRAME * 9
        );
    }
    #[test]
    fn screenshots_are_named_by_capture_time_and_cropped_to_a_window() {
        let dir = std::env::temp_dir().join(format!("hoswm-shot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut config = Config::defaults_in(&dir);
        config.screenshots = dir.join("screenshots");
        let mut state = Desktop::with_config(config);
        let id = state.create("Shot".into(), 200, 100, 0xff72dbac).unwrap();
        let mut frame = Surface::new(800, 600);
        state.draw_scene(&mut frame);
        let path = screenshot(&state, &frame, Shot::Screen).unwrap();
        assert!(path.ends_with(".qoi"), "{path}");
        let image = qoi::load(&path).unwrap();
        assert_eq!((image.width, image.height), (800, 600));
        assert_eq!(image.pixels, frame.pixels());
        let rect = state.window_rect(id).unwrap();
        let window = qoi::load(screenshot(&state, &frame, Shot::Window(id)).unwrap()).unwrap();
        assert_eq!((window.width, window.height), (rect.w as usize, rect.h as usize));
        state.close(id);
        assert!(screenshot(&state, &frame, Shot::Window(id)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
