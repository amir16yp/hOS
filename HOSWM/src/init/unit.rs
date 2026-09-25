//! What the supervisor keeps running, and when it gives up.
//!
//! A unit is one supervised program. The decisions around restarting live
//! here, separated from the process handling in [`crate::init::supervisor`],
//! so the backoff and give-up behavior can be tested without spawning
//! anything.
use crate::init::{Settings, ipc::Fields};
use std::time::{Duration, Instant};

/// Shortest wait before restarting a service that exited.
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
/// Longest wait between restarts of a service that keeps failing.
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// A service that stays up this long is considered healthy again.
pub const HEALTHY: Duration = Duration::from_secs(30);
/// Consecutive failures before the supervisor stops trying by itself.
pub const GIVE_UP: u32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    /// Restart whenever it exits, however it exited.
    Always,
    /// Restart only after a nonzero exit or a signal.
    OnFailure,
    /// Run once.
    Never,
}

/// One supervised program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub restart: Restart,
    /// Units start in table order; this one waits for the named units first.
    pub after: Vec<String>,
    pub enabled: bool,
    pub description: String,
}
impl Unit {
    fn service(name: &str, description: &str, after: &[&str]) -> Self {
        Unit {
            name: name.to_string(),
            program: format!("/bin/{name}"),
            args: Vec::new(),
            restart: Restart::Always,
            after: after.iter().map(|n| n.to_string()).collect(),
            enabled: true,
            description: description.to_string(),
        }
    }
    /// The services `hos-init` starts, in start order.
    ///
    /// `/etc/hos/services.conf` may turn one off or point it somewhere else:
    ///
    /// ```text
    /// [service hos-ntpd]
    /// enabled = no
    /// ```
    pub fn table(settings: &Settings) -> Vec<Unit> {
        let mut units = vec![
            Unit::service("hos-netd", "network links, DHCP, DNS and Wi-Fi", &[]),
            Unit::service("hos-power", "battery, power button and suspend", &[]),
            Unit::service("hos-soundd", "default sound device, volume and mute", &[]),
            Unit::service("hos-ntpd", "time synchronization, parked offline", &["hos-netd"]),
        ];
        for unit in &mut units {
            let section = format!("service {}", unit.name);
            if let Some(enabled) = settings.boolean(&section, "enabled") {
                unit.enabled = enabled;
            }
            if let Some(program) = settings.get(&section, "program") {
                unit.program = program.to_string();
            }
            if let Some(arguments) = settings.get(&section, "arguments") {
                unit.args = arguments.split_whitespace().map(String::from).collect();
            }
        }
        units
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Not started yet, or stopped on request.
    Inactive,
    Running,
    /// Exited and waiting out its restart delay.
    Backoff,
    /// Failed too often; the supervisor waits for a `START` or `RESTART`.
    Failed,
    /// Exited as expected and is not restarted.
    Done,
}
impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Inactive => "inactive",
            State::Running => "running",
            State::Backoff => "backoff",
            State::Failed => "failed",
            State::Done => "done",
        }
    }
}

/// What the supervisor should do after a unit exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start it again once the returned instant passes.
    RestartAt(Instant),
    /// Leave it alone.
    Leave,
}

/// A unit plus everything the supervisor knows about its current run.
#[derive(Clone, Debug)]
pub struct Supervised {
    pub unit: Unit,
    pub state: State,
    pub pid: i32,
    /// Consecutive failures; reset once a run stays up for [`HEALTHY`].
    pub failures: u32,
    pub starts: u32,
    pub last_status: Option<i32>,
    started: Option<Instant>,
    due_at: Option<Instant>,
    /// Set by `STOP`, cleared by `START`: a unit stopped by hand stays down.
    stopped_by_hand: bool,
    /// Set by `RESTART`, so the exit that follows is not treated as a failure.
    restart_requested: bool,
}
impl Supervised {
    pub fn new(unit: Unit) -> Self {
        Supervised {
            unit,
            state: State::Inactive,
            pid: 0,
            failures: 0,
            starts: 0,
            last_status: None,
            started: None,
            due_at: None,
            stopped_by_hand: false,
            restart_requested: false,
        }
    }
    pub fn running(&self) -> bool {
        self.state == State::Running
    }
    /// How long the current run has been up.
    pub fn uptime(&self, now: Instant) -> Duration {
        self.started.map_or(Duration::ZERO, |at| now - at)
    }
    /// Record a successful spawn.
    pub fn started(&mut self, pid: i32, now: Instant) {
        self.pid = pid;
        self.state = State::Running;
        self.started = Some(now);
        self.due_at = None;
        self.starts += 1;
    }
    /// Record a failed spawn, which counts as a failure like any other exit.
    pub fn spawn_failed(&mut self, now: Instant) -> Action {
        self.pid = 0;
        self.last_status = None;
        self.fail(now)
    }
    /// Record an exit and decide what happens next.
    pub fn exited(&mut self, status: i32, now: Instant) -> Action {
        let healthy = self.uptime(now) >= HEALTHY;
        self.pid = 0;
        self.started = None;
        self.last_status = Some(status);
        if healthy {
            self.failures = 0;
        }
        if self.restart_requested {
            self.restart_requested = false;
            self.state = State::Inactive;
            self.due_at = None;
            return Action::RestartAt(now);
        }
        if self.stopped_by_hand {
            self.state = State::Inactive;
            return Action::Leave;
        }
        let restart = match self.unit.restart {
            Restart::Always => true,
            Restart::OnFailure => status != 0,
            Restart::Never => false,
        };
        if !restart {
            self.state = if status == 0 { State::Done } else { State::Failed };
            return Action::Leave;
        }
        self.fail(now)
    }
    fn fail(&mut self, now: Instant) -> Action {
        self.failures += 1;
        if self.failures >= GIVE_UP {
            self.state = State::Failed;
            self.due_at = None;
            return Action::Leave;
        }
        let delay = BACKOFF_MIN
            .saturating_mul(1 << (self.failures - 1).min(5))
            .min(BACKOFF_MAX);
        let at = now + delay;
        self.state = State::Backoff;
        self.due_at = Some(at);
        Action::RestartAt(at)
    }
    /// Whether the supervisor should spawn this unit now.
    pub fn due(&self, now: Instant) -> bool {
        if !self.unit.enabled || self.stopped_by_hand {
            return false;
        }
        match self.state {
            State::Inactive => true,
            State::Backoff => self.due_at.is_none_or(|at| now >= at),
            _ => false,
        }
    }
    /// How long the loop may sleep before this unit needs attention.
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        match self.state {
            State::Backoff => Some(
                self.due_at
                    .map_or(Duration::ZERO, |at| at.saturating_duration_since(now)),
            ),
            State::Inactive if self.due(now) => Some(Duration::ZERO),
            _ => None,
        }
    }
    /// `START`: clear a manual stop and any give-up state.
    pub fn enable(&mut self) {
        self.stopped_by_hand = false;
        self.unit.enabled = true;
        self.failures = 0;
        self.due_at = None;
        if matches!(self.state, State::Failed | State::Done) {
            self.state = State::Inactive;
        }
    }
    /// `STOP`: keep it down until someone starts it again.
    pub fn disable(&mut self) {
        self.stopped_by_hand = true;
        self.due_at = None;
        if self.state == State::Backoff {
            self.state = State::Inactive;
        }
    }
    pub fn stopped_by_hand(&self) -> bool {
        self.stopped_by_hand
    }
    /// `RESTART`: the next exit starts the unit again without a backoff delay,
    /// because it was asked for rather than a symptom of something failing.
    pub fn request_restart(&mut self) {
        self.enable();
        self.restart_requested = true;
        if self.pid == 0 {
            self.state = State::Inactive;
        }
    }
    /// The record `hos-init` reports for `LIST` and `STATUS`.
    pub fn fields(&self, now: Instant) -> Fields {
        let mut fields = Fields::new()
            .text("unit", &self.unit.name)
            .text("state", self.state.name())
            .number("pid", self.pid)
            .number("starts", self.starts)
            .number("failures", self.failures)
            .number("uptime", self.uptime(now).as_secs())
            .flag("enabled", self.unit.enabled && !self.stopped_by_hand);
        if let Some(status) = self.last_status {
            fields = fields.number("status", status);
        }
        fields.text("description", &self.unit.description)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unit() -> Unit {
        Unit::service("hos-test", "test unit", &[])
    }
    #[test]
    fn restarts_back_off_and_then_give_up() {
        let mut supervised = Supervised::new(unit());
        let mut now = Instant::now();
        assert!(supervised.due(now));
        let mut delays = Vec::new();
        for _ in 0..(GIVE_UP - 1) {
            supervised.started(42, now);
            assert!(!supervised.due(now));
            match supervised.exited(1, now) {
                Action::RestartAt(at) => delays.push(at - now),
                other => panic!("expected a restart, got {other:?}"),
            }
            now = supervised.due_at.unwrap();
            assert!(supervised.due(now));
        }
        assert_eq!(delays[0], BACKOFF_MIN);
        assert_eq!(delays[1], BACKOFF_MIN * 2);
        assert_eq!(*delays.last().unwrap(), BACKOFF_MAX);
        supervised.started(42, now);
        assert_eq!(supervised.exited(1, now), Action::Leave);
        assert_eq!(supervised.state, State::Failed);
        assert!(!supervised.due(now));
        // A manual start clears the failure count and tries again.
        supervised.enable();
        assert!(supervised.due(now));
    }
    #[test]
    fn a_healthy_run_clears_earlier_failures() {
        let mut supervised = Supervised::new(unit());
        let start = Instant::now();
        supervised.started(1, start);
        supervised.exited(1, start);
        assert_eq!(supervised.failures, 1);
        supervised.started(2, start);
        let later = start + HEALTHY + Duration::from_secs(1);
        supervised.exited(1, later);
        assert_eq!(supervised.failures, 1, "the long run reset the count first");
        assert_eq!(supervised.state, State::Backoff);
    }
    #[test]
    fn a_unit_stopped_by_hand_stays_down() {
        let mut supervised = Supervised::new(unit());
        let now = Instant::now();
        supervised.started(7, now);
        supervised.disable();
        assert_eq!(supervised.exited(0, now), Action::Leave);
        assert_eq!(supervised.state, State::Inactive);
        assert!(!supervised.due(now));
        supervised.enable();
        assert!(supervised.due(now));
    }
    #[test]
    fn a_requested_restart_skips_the_backoff_delay() {
        let mut supervised = Supervised::new(unit());
        let now = Instant::now();
        supervised.started(9, now);
        supervised.request_restart();
        assert_eq!(supervised.exited(143, now), Action::RestartAt(now));
        assert!(supervised.due(now), "it starts again immediately");
        assert_eq!(supervised.failures, 0, "an asked-for restart is not a failure");
    }
    #[test]
    fn restart_policies_decide_what_happens_after_a_clean_exit() {
        let now = Instant::now();
        for (policy, status, expected) in [
            (Restart::Always, 0, State::Backoff),
            (Restart::OnFailure, 0, State::Done),
            (Restart::OnFailure, 1, State::Backoff),
            (Restart::Never, 0, State::Done),
            (Restart::Never, 3, State::Failed),
        ] {
            let mut supervised = Supervised::new(Unit {
                restart: policy,
                ..unit()
            });
            supervised.started(1, now);
            supervised.exited(status, now);
            assert_eq!(supervised.state, expected, "{policy:?} exit {status}");
        }
    }
    #[test]
    fn the_table_can_be_turned_off_in_configuration() {
        let settings = Settings::parse("[service hos-ntpd]\nenabled = no\nprogram = /bin/other\n");
        let units = Unit::table(&settings);
        let ntpd = units.iter().find(|u| u.name == "hos-ntpd").unwrap();
        assert!(!ntpd.enabled);
        assert_eq!(ntpd.program, "/bin/other");
        assert_eq!(ntpd.after, ["hos-netd"]);
        assert!(units.iter().filter(|u| u.enabled).count() == 3);
    }
}
