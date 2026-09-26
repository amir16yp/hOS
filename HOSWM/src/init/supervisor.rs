//! PID 1: start the services and the desktop session, keep them running,
//! answer the control socket and bring the system down.
//!
//! The supervisor is deliberately small. It owns no policy of its own beyond
//! restarting what it started: the network, power, time and sound policies
//! live in the services, which it treats as ordinary supervised programs.
use crate::init::{
    Settings, ipc,
    ipc::{Fields, Peer, Request, Response, Service},
    log,
    sys::{self, Signals},
    unit::{State, Supervised, Unit},
};
use std::{
    fs::OpenOptions,
    os::fd::AsRawFd,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

/// Written to `/etc/hos` the first time the supervisor runs.
const SERVICES_DEFAULT: &str = include_str!("config/services.conf");

/// How the machine should come down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shutdown {
    Reboot,
    PowerOff,
    Halt,
}
impl Shutdown {
    /// The `reboot(2)` command that performs it.
    fn command(self) -> i32 {
        match self {
            Shutdown::Reboot => 0x0123_4567,
            Shutdown::PowerOff => 0x4321_fedc,
            Shutdown::Halt => 0xcdef_0123u32 as i32,
        }
    }
    /// The signal that requests it from PID 1, matching `reboot`, `poweroff`
    /// and `halt`, which are links to this program.
    pub fn signal(self) -> i32 {
        match self {
            Shutdown::Reboot => sys::SIGTERM,
            Shutdown::PowerOff => sys::SIGUSR2,
            Shutdown::Halt => sys::SIGUSR1,
        }
    }
    pub fn from_signal(number: i32) -> Option<Self> {
        match number {
            // Ctrl+Alt+Delete reaches PID 1 as SIGINT.
            sys::SIGTERM | sys::SIGINT => Some(Shutdown::Reboot),
            sys::SIGUSR2 => Some(Shutdown::PowerOff),
            sys::SIGUSR1 => Some(Shutdown::Halt),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Shutdown::Reboot => "reboot",
            Shutdown::PowerOff => "poweroff",
            Shutdown::Halt => "halt",
        }
    }
    pub fn parse(verb: &str) -> Option<Self> {
        match verb {
            "REBOOT" => Some(Shutdown::Reboot),
            "POWEROFF" | "SHUTDOWN" => Some(Shutdown::PowerOff),
            "HALT" => Some(Shutdown::Halt),
            _ => None,
        }
    }
}

/// The desktop session: the greeter, or the live installer startup script.
struct SessionRunner {
    live: bool,
    pid: i32,
    next: Instant,
    starts: u32,
}
impl SessionRunner {
    fn spawn(&mut self) {
        let mut command = Command::new("/bin/setsid");
        command.arg("--wait");
        if self.live {
            command.args(["/bin/bash", "/etc/hos-live-start"]);
        } else {
            command.arg("--ctty");
            if Path::new("/etc/hos-session").exists() {
                command.args(["/bin/bash", "/etc/hos-session"]);
            } else {
                command.args(["/bin/hoswm", "--greeter"]);
            }
            if let Ok(tty) = OpenOptions::new().read(true).write(true).open("/dev/tty1") {
                if let (Ok(input), Ok(output)) = (tty.try_clone(), tty.try_clone()) {
                    command
                        .stdin(Stdio::from(input))
                        .stdout(Stdio::from(output))
                        .stderr(Stdio::from(tty));
                }
            }
        }
        match command.spawn() {
            Ok(child) => {
                self.pid = child.id() as i32;
                self.starts += 1;
            }
            Err(e) => {
                log("hos-init", &format!("session: {e}"));
                self.next = Instant::now() + Duration::from_secs(2);
            }
        }
    }
}

pub struct Supervisor {
    units: Vec<Supervised>,
    session: SessionRunner,
    events: Vec<String>,
    /// Set once a shutdown is requested, by signal or over the socket.
    pub requested: Option<Shutdown>,
    booted: Instant,
}

impl Supervisor {
    pub fn new(live: bool) -> Self {
        let settings = Settings::install("services.conf", SERVICES_DEFAULT);
        for warning in &settings.warnings {
            log("hos-init", &format!("services.conf: {warning}"));
        }
        Supervisor {
            units: Unit::table(&settings)
                .into_iter()
                .map(Supervised::new)
                .collect(),
            session: SessionRunner {
                live,
                pid: 0,
                next: Instant::now(),
                starts: 0,
            },
            events: Vec::new(),
            requested: None,
            booted: Instant::now(),
        }
    }
    fn unit(&mut self, name: &str) -> Result<&mut Supervised, String> {
        let name = name.to_string();
        self.units
            .iter_mut()
            .find(|u| u.unit.name == name || u.unit.name == format!("hos-{name}"))
            .ok_or(format!("{name}: no such unit"))
    }
    /// Whether every unit this one waits for is already running.
    fn ready(&self, index: usize) -> bool {
        self.units[index].unit.after.iter().all(|name| {
            self.units
                .iter()
                .find(|u| &u.unit.name == name)
                .is_none_or(|u| u.running() || !u.unit.enabled)
        })
    }
    fn spawn(&mut self, index: usize) {
        let now = Instant::now();
        let unit = self.units[index].unit.clone();
        let result = Command::new(&unit.program)
            .args(&unit.args)
            .stdin(Stdio::null())
            .env("PATH", "/bin:/sbin:/usr/bin:/usr/sbin")
            .spawn();
        match result {
            Ok(child) => {
                let pid = child.id() as i32;
                // The child is reaped by the supervisor's own waitpid loop.
                std::mem::forget(child);
                self.units[index].started(pid, now);
                log("hos-init", &format!("started {} (pid {pid})", unit.name));
                self.events
                    .push(Fields::new().text("unit", &unit.name).text("state", "running").line());
            }
            Err(e) => {
                log("hos-init", &format!("{}: {e}", unit.name));
                self.units[index].spawn_failed(now);
            }
        }
    }
    /// Reap exited children and decide what to restart.
    fn reap(&mut self) {
        while let Some((pid, status)) = sys::reap() {
            let now = Instant::now();
            if pid == self.session.pid {
                self.session.pid = 0;
                self.session.next = now + Duration::from_secs(1);
                continue;
            }
            let Some(index) = self.units.iter().position(|u| u.pid == pid) else {
                continue; // An orphan the supervisor inherited.
            };
            let name = self.units[index].unit.name.clone();
            let action = self.units[index].exited(status, now);
            let state = self.units[index].state;
            log(
                "hos-init",
                &format!("{name} exited with status {status} ({})", state.name()),
            );
            if state == State::Failed {
                log(
                    "hos-init",
                    &format!("{name} failed too often; use hosctl init start {name}"),
                );
            }
            self.events.push(
                Fields::new()
                    .text("unit", &name)
                    .text("state", state.name())
                    .number("status", status)
                    .line(),
            );
            let _ = action;
        }
    }
    /// One pass of the supervision loop: reap, start what is due, and report
    /// how long the caller may sleep.
    pub fn step(&mut self) -> Duration {
        self.reap();
        let now = Instant::now();
        for index in 0..self.units.len() {
            if self.units[index].due(now) && self.ready(index) {
                self.spawn(index);
            }
        }
        if self.session.pid == 0 && now >= self.session.next {
            self.session.spawn();
        }
        let mut wait = Duration::from_secs(5);
        for unit in &self.units {
            if let Some(next) = unit.wait(now) {
                wait = wait.min(next);
            }
        }
        if self.session.pid == 0 {
            wait = wait.min(self.session.next.saturating_duration_since(now));
        }
        wait
    }
    /// Stop a unit and wait for it, so services save state before the machine
    /// goes down.
    fn stop_unit(&mut self, index: usize, signal: i32) {
        let pid = self.units[index].pid;
        if pid > 0 {
            // SAFETY: signalling a child this supervisor started.
            unsafe { sys::kill(pid, signal) };
        }
    }
    /// Bring the system down: stop services, flush disks and reboot.
    pub fn shutdown(&mut self, how: Shutdown) -> ! {
        log("hos-init", &format!("{} requested", how.name()));
        for index in (0..self.units.len()).rev() {
            self.units[index].disable();
            self.stop_unit(index, sys::SIGTERM);
        }
        if self.session.pid > 0 {
            // SAFETY: signalling the session process group this init started.
            unsafe { sys::kill(-self.session.pid, sys::SIGTERM) };
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            self.reap();
            if self.units.iter().all(|u| u.pid == 0) && self.session.pid == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: the usual last rites of PID 1; everything else is gone.
        unsafe {
            sys::kill(-1, sys::SIGTERM);
        }
        std::thread::sleep(Duration::from_secs(1));
        unsafe {
            sys::kill(-1, sys::SIGKILL);
            sys::sync();
        }
        let _ = Command::new("/bin/umount").args(["-a", "-r"]).status();
        unsafe {
            sys::sync();
            sys::reboot(how.command());
        }
        // Never exit PID 1, even if the reboot call failed.
        log("hos-init", "the kernel refused the shutdown; halting here");
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    }
}

impl Service for Supervisor {
    fn handle(&mut self, request: &Request, _peer: &Peer) -> Result<Response, String> {
        let now = Instant::now();
        match request.verb.as_str() {
            "LIST" => Ok(Response::ok().records(self.units.iter().map(|u| u.fields(now)))),
            "STATUS" => match request.arg(0) {
                Some(name) => {
                    let fields = self.unit(name)?.fields(now);
                    Ok(Response::ok().record(fields))
                }
                None => Ok(Response::ok()
                    .record(
                        Fields::new()
                            .text("system", if self.session.live { "live" } else { "installed" })
                            .number("uptime", self.booted.elapsed().as_secs())
                            .number("units", self.units.len())
                            .number("running", self.units.iter().filter(|u| u.running()).count())
                            .number("session_pid", self.session.pid)
                            .number("session_starts", self.session.starts),
                    )
                    .records(self.units.iter().map(|u| u.fields(now)))),
            },
            "START" => {
                let name = request.need(0, "a unit name")?;
                let unit = self.unit(name)?;
                unit.enable();
                let name = unit.unit.name.clone();
                Ok(Response::message(format!("{name} will start")))
            }
            "STOP" => {
                let name = request.need(0, "a unit name")?.to_string();
                let index = self.index(&name)?;
                self.units[index].disable();
                self.stop_unit(index, sys::SIGTERM);
                Ok(Response::message(format!(
                    "{} stopping",
                    self.units[index].unit.name
                )))
            }
            "RESTART" => {
                let name = request.need(0, "a unit name")?.to_string();
                let index = self.index(&name)?;
                self.units[index].request_restart();
                self.stop_unit(index, sys::SIGTERM);
                Ok(Response::message(format!(
                    "{} restarting",
                    self.units[index].unit.name
                )))
            }
            "SESSION" => match request.keyword(0).as_str() {
                "RESTART" => {
                    if self.session.pid > 0 {
                        // SAFETY: signalling the session's process group.
                        unsafe { sys::kill(-self.session.pid, sys::SIGTERM) };
                    }
                    Ok(Response::message("session restarting"))
                }
                "" | "STATUS" => Ok(Response::ok().record(
                    Fields::new()
                        .number("pid", self.session.pid)
                        .number("starts", self.session.starts)
                        .flag("live", self.session.live),
                )),
                other => Err(format!("SESSION {other} is not a session command")),
            },
            "VERSION" => Ok(Response::ok().record(
                Fields::new()
                    .text("service", "hos-init")
                    .text("version", env!("CARGO_PKG_VERSION"))
                    .number("pid", std::process::id()),
            )),
            verb => match Shutdown::parse(verb) {
                Some(how) => {
                    self.requested = Some(how);
                    Ok(Response::message(format!("{} requested", how.name())))
                }
                None => Err(format!("{verb} is not an init command")),
            },
        }
    }
    fn events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }
    fn public(&self) -> &'static [&'static str] {
        &["LIST", "STATUS", "SESSION", "VERSION"]
    }
    fn help(&self) -> &'static [&'static str] {
        &[
            "LIST - every supervised unit and its state",
            "STATUS [unit] - the system summary, or one unit",
            "START unit - start a unit and keep it running",
            "STOP unit - stop a unit and leave it down",
            "RESTART unit - stop a unit and start it again",
            "SESSION [STATUS|RESTART] - the desktop session",
            "REBOOT | POWEROFF | HALT - bring the system down",
            "VERSION - the running init version",
        ]
    }
    fn reload(&mut self) {
        let settings = Settings::install("services.conf", SERVICES_DEFAULT);
        for unit in Unit::table(&settings) {
            if let Some(existing) = self.units.iter_mut().find(|u| u.unit.name == unit.name) {
                existing.unit = unit;
            } else {
                self.units.push(Supervised::new(unit));
            }
        }
        log("hos-init", "reloaded services.conf");
    }
}

impl Supervisor {
    fn index(&mut self, name: &str) -> Result<usize, String> {
        let wanted = self.unit(name)?.unit.name.clone();
        Ok(self
            .units
            .iter()
            .position(|u| u.unit.name == wanted)
            .expect("the unit was just found"))
    }
}

/// Run PID 1 until a shutdown is requested. Never returns.
pub fn run(live: bool) -> ! {
    let mut supervisor = Supervisor::new(live);
    let mut signals = match Signals::catch(&[
        sys::SIGTERM,
        sys::SIGINT,
        sys::SIGUSR1,
        sys::SIGUSR2,
        sys::SIGCHLD,
        sys::SIGHUP,
    ]) {
        Ok(signals) => signals,
        Err(e) => {
            log("hos-init", &format!("signal setup failed: {e}"));
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    };
    // Ctrl+Alt+Delete should reach PID 1 as a signal, not reset the machine.
    // SAFETY: reboot(0) only changes the kernel's Ctrl+Alt+Delete handling.
    unsafe { sys::reboot(0) };
    let mut server = match ipc::Server::bind("init", 0o666) {
        Ok(server) => Some(server),
        Err(e) => {
            log("hos-init", &format!("control socket unavailable: {e}"));
            None
        }
    };
    let mut reactor = crate::reactor::Reactor::default();
    loop {
        for signal in signals.take() {
            if let Some(how) = Shutdown::from_signal(signal) {
                supervisor.requested = Some(how);
            } else if signal == sys::SIGHUP {
                supervisor.reload();
            }
        }
        if let Some(how) = supervisor.requested {
            drop(server);
            supervisor.shutdown(how);
        }
        let wait = supervisor.step();
        if let Some(server) = server.as_mut() {
            server.poll(&mut supervisor);
            let events = supervisor.events();
            server.broadcast(&events);
        }
        if supervisor.requested.is_some() {
            continue;
        }
        reactor.clear();
        reactor.watch(signals.as_raw_fd(), true, false);
        if let Some(server) = server.as_ref() {
            server.watch(&mut reactor);
        }
        if reactor.wait(wait).is_err() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shutdown_requests_map_between_signals_verbs_and_commands() {
        for how in [Shutdown::Reboot, Shutdown::PowerOff, Shutdown::Halt] {
            assert_eq!(Shutdown::from_signal(how.signal()), Some(how));
            assert_eq!(
                Shutdown::parse(&how.name().to_ascii_uppercase()),
                Some(how)
            );
            assert_ne!(how.command(), 0);
        }
        assert_eq!(Shutdown::from_signal(sys::SIGINT), Some(Shutdown::Reboot));
        assert_eq!(Shutdown::parse("SHUTDOWN"), Some(Shutdown::PowerOff));
        assert_eq!(Shutdown::parse("HOSWM"), None);
    }
    #[test]
    fn units_wait_for_the_services_they_depend_on() {
        let mut supervisor = Supervisor::new(true);
        let netd = supervisor.index("hos-netd").unwrap();
        let ntpd = supervisor.index("hos-ntpd").unwrap();
        assert!(supervisor.ready(netd), "hos-netd has no dependencies");
        assert!(!supervisor.ready(ntpd), "hos-ntpd waits for the network");
        supervisor.units[netd].started(1234, Instant::now());
        assert!(supervisor.ready(ntpd));
    }
    #[test]
    fn the_control_socket_lists_and_stops_units() {
        let mut supervisor = Supervisor::new(true);
        let peer = Peer { pid: 1, uid: 0 };
        let list = supervisor
            .handle(&Request::parse("LIST"), &peer)
            .unwrap()
            .records;
        assert_eq!(list.len(), 4);
        assert!(list[0].contains("unit=hos-netd"));
        supervisor
            .handle(&Request::parse("STOP netd"), &peer)
            .unwrap();
        let index = supervisor.index("hos-netd").unwrap();
        assert!(supervisor.units[index].stopped_by_hand());
        assert!(!supervisor.units[index].due(Instant::now()));
        supervisor
            .handle(&Request::parse("START netd"), &peer)
            .unwrap();
        assert!(supervisor.units[index].due(Instant::now()));
        assert!(
            supervisor
                .handle(&Request::parse("STATUS nothing"), &peer)
                .is_err()
        );
    }
}
