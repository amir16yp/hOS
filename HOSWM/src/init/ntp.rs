//! `hos-ntpd`: an SNTP client that stays asleep while the machine is offline.
//!
//! The service subscribes to `hos-netd`. While there is no network it parks:
//! no packets, no timers beyond its own socket, nothing to wake the machine
//! for. When the network comes up it queries the configured servers, and
//! either steps the clock or slews it, depending on how far off it is.
//!
//! ```text
//! [ntp]
//! servers = pool.ntp.org time.cloudflare.com
//! step = 0.5          # seconds of error above which the clock is stepped
//! min-poll = 64       # seconds between the first queries
//! max-poll = 1024     # longest interval once the clock is settled
//! ```
use crate::init::{
    Settings, ipc,
    ipc::{Fields, Peer, Request, Response, Service},
    log, state_path, sys,
};
use std::{
    io,
    net::{ToSocketAddrs, UdpSocket},
    os::fd::RawFd,
    time::{Duration, Instant},
};

/// Seconds between the NTP epoch (1900) and the Unix epoch (1970).
const NTP_EPOCH: f64 = 2_208_988_800.0;
/// Written to `/etc/hos` the first time the service runs.
const NTPD_DEFAULT: &str = include_str!("config/ntpd.conf");

/// An SNTP packet is 48 bytes; anything shorter is not one.
const PACKET: usize = 48;
/// How long to wait for one server to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How long to wait before trying again after every server failed.
const RETRY: Duration = Duration::from_secs(32);
/// Where the last known good time is remembered.
const STAMP: &str = "ntp.stamp";
/// What `SET` may change in `ntpd.conf`.
const SETTABLE: &[(&str, &str)] = &[
    ("ntp", "servers"),
    ("ntp", "step"),
    ("ntp", "min-poll"),
    ("ntp", "max-poll"),
];

/// Turn a Unix timestamp into the 64-bit NTP format.
fn to_ntp(seconds: f64) -> u64 {
    let ntp = seconds + NTP_EPOCH;
    let whole = ntp.trunc().max(0.0) as u64;
    let fraction = ((ntp.fract()) * 4_294_967_296.0) as u64;
    (whole << 32) | (fraction & 0xffff_ffff)
}
/// Turn an NTP timestamp back into seconds since the Unix epoch.
fn from_ntp(value: u64) -> f64 {
    let whole = (value >> 32) as f64;
    let fraction = (value & 0xffff_ffff) as f64 / 4_294_967_296.0;
    whole + fraction - NTP_EPOCH
}
/// The current time as a floating point Unix timestamp.
fn now() -> f64 {
    let (seconds, nanos) = sys::realtime();
    seconds as f64 + nanos as f64 / 1e9
}

/// Build a client request. The transmit timestamp is echoed back by the
/// server, which is what makes a stale or spoofed reply easy to reject.
pub fn request(transmit: f64) -> [u8; PACKET] {
    let mut packet = [0u8; PACKET];
    // Leap 0, version 4, mode 3 (client).
    packet[0] = (4 << 3) | 3;
    packet[40..48].copy_from_slice(&to_ntp(transmit).to_be_bytes());
    packet
}

/// One measurement against one server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// How far the local clock is behind the server, in seconds.
    pub offset: f64,
    /// Round trip time, in seconds.
    pub delay: f64,
    pub stratum: u8,
}

/// Check a reply and turn it into a sample.
///
/// `sent` and `received` are the local times around the exchange.
pub fn sample(packet: &[u8], sent: f64, received: f64) -> Result<Sample, String> {
    if packet.len() < PACKET {
        return Err("the reply was too short".into());
    }
    let leap = packet[0] >> 6;
    let mode = packet[0] & 7;
    let stratum = packet[1];
    if leap == 3 {
        return Err("the server is not synchronized".into());
    }
    if mode != 4 && mode != 5 {
        return Err(format!("the reply was mode {mode}, not a server reply"));
    }
    if stratum == 0 || stratum > 15 {
        return Err(format!("the server reported stratum {stratum}"));
    }
    let number = |at: usize| u64::from_be_bytes(packet[at..at + 8].try_into().unwrap());
    let originate = from_ntp(number(24));
    if (originate - sent).abs() > 0.001 {
        return Err("the reply did not echo this request".into());
    }
    let receive = from_ntp(number(32));
    let transmit = from_ntp(number(40));
    if receive <= 0.0 || transmit <= 0.0 {
        return Err("the server sent an empty timestamp".into());
    }
    Ok(Sample {
        // The usual NTP estimate: half the difference of the two legs.
        offset: ((receive - sent) + (transmit - received)) / 2.0,
        delay: (received - sent) - (transmit - receive),
        stratum,
    })
}

/// Query one server. `server` is a host name or address; port 123 is added.
pub fn query(server: &str) -> Result<Sample, String> {
    let host = if server.contains(':') {
        server.to_string()
    } else {
        format!("{server}:123")
    };
    let address = host
        .to_socket_addrs()
        .map_err(|e| format!("{server}: {e}"))?
        .next()
        .ok_or_else(|| format!("{server}: no address"))?;
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("{server}: {e}"))?;
    socket
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let sent = now();
    socket
        .send_to(&request(sent), address)
        .map_err(|e| format!("{server}: {e}"))?;
    let mut buffer = [0u8; 128];
    let (length, from) = socket
        .recv_from(&mut buffer)
        .map_err(|e| format!("{server}: {e}"))?;
    if from.ip() != address.ip() {
        return Err(format!("{server}: a reply from somewhere else"));
    }
    sample(&buffer[..length], sent, now()).map_err(|e| format!("{server}: {e}"))
}

/// What the service is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Offline: no queries until the network comes back.
    Parked,
    /// Online and waiting for the next poll.
    Waiting,
    /// Online, and no server has answered yet.
    Unsynchronized,
}
impl State {
    fn name(self) -> &'static str {
        match self {
            State::Parked => "parked",
            State::Waiting => "waiting",
            State::Unsynchronized => "unsynchronized",
        }
    }
}

pub struct Ntpd {
    servers: Vec<String>,
    /// Error above which the clock is stepped instead of slewed.
    step: f64,
    min_poll: u64,
    max_poll: u64,
    poll: u64,
    state: State,
    online: bool,
    next: Instant,
    events: Vec<String>,
    /// The last successful measurement, for `STATUS`.
    last: Option<(String, Sample, f64)>,
    syncs: u32,
    failures: u32,
    /// The connection that tells this service when the network changes.
    netd: Option<ipc::Client>,
    netd_retry: Instant,
}
impl Ntpd {
    pub fn new() -> Ntpd {
        let settings = Settings::install("ntpd.conf", NTPD_DEFAULT);
        for warning in &settings.warnings {
            log("hos-ntpd", &format!("ntpd.conf: {warning}"));
        }
        let servers: Vec<String> = settings
            .get("ntp", "servers")
            .unwrap_or("pool.ntp.org time.cloudflare.com")
            .split([' ', ','])
            .map(str::trim)
            .filter(|server| !server.is_empty())
            .map(str::to_string)
            .collect();
        let min_poll = settings.number::<u64>("ntp", "min-poll").unwrap_or(64).clamp(8, 3600);
        let mut ntpd = Ntpd {
            servers,
            step: settings.number::<f64>("ntp", "step").unwrap_or(0.5).max(0.01),
            min_poll,
            max_poll: settings
                .number::<u64>("ntp", "max-poll")
                .unwrap_or(1024)
                .clamp(min_poll, 86400),
            poll: min_poll,
            state: State::Parked,
            online: false,
            next: Instant::now(),
            events: Vec::new(),
            last: None,
            syncs: 0,
            failures: 0,
            netd: None,
            netd_retry: Instant::now(),
        };
        ntpd.restore_stamp();
        ntpd.watch_network();
        log(
            "hos-ntpd",
            &format!("{} server(s), {}", ntpd.servers.len(), ntpd.state.name()),
        );
        ntpd
    }
    /// A machine without a battery-backed clock starts in 1970. Move it
    /// forward to the last time this service knew about, which is wrong but
    /// far less wrong, and keeps file timestamps ordered.
    fn restore_stamp(&mut self) {
        let Some(stamp) = sys::read_number::<i64>(state_path(STAMP)) else {
            return;
        };
        let (seconds, _) = sys::realtime();
        if seconds < stamp {
            match sys::set_realtime(stamp, 0) {
                Ok(()) => log(
                    "hos-ntpd",
                    &format!("clock moved forward to the last known time ({stamp})"),
                ),
                Err(e) => log("hos-ntpd", &format!("could not set the clock: {e}")),
            }
        }
    }
    fn save_stamp(&self) {
        let (seconds, _) = sys::realtime();
        if let Err(e) = sys::write_atomic(&state_path(STAMP), &format!("{seconds}\n")) {
            log("hos-ntpd", &format!("could not save the time stamp: {e}"));
        }
    }
    /// Subscribe to `hos-netd`, or fall back to reading the routing table.
    fn watch_network(&mut self) {
        self.netd_retry = Instant::now() + Duration::from_secs(30);
        let mut client = match ipc::Client::connect("netd") {
            Ok(client) => client,
            Err(_) => {
                // Without the network service, the routing table still says
                // whether a query has any chance of leaving the machine.
                self.set_online(crate::init::net::current_gateway().is_some());
                return;
            }
        };
        if client.subscribe().is_err() {
            return;
        }
        let online = client
            .call("STATUS")
            .ok()
            .map(|reply| reply.first().flag("online"))
            .unwrap_or(false);
        self.netd = Some(client);
        self.set_online(online);
    }
    fn set_online(&mut self, online: bool) {
        if online == self.online {
            return;
        }
        self.online = online;
        self.state = if online {
            State::Unsynchronized
        } else {
            State::Parked
        };
        if online {
            // Query soon after the link comes up, but not instantly: DHCP and
            // DNS need a moment to finish first.
            self.next = Instant::now() + Duration::from_secs(2);
            self.poll = self.min_poll;
        }
        log(
            "hos-ntpd",
            if online {
                "network is up; synchronizing"
            } else {
                "network is down; parked"
            },
        );
        self.events.push(
            Fields::new()
                .text("event", "state")
                .text("state", self.state.name())
                .flag("online", online)
                .line(),
        );
    }
    /// Apply one measurement to the clock.
    fn apply(&mut self, server: &str, sample: Sample) {
        let before = now();
        let stepped = sample.offset.abs() >= self.step;
        let result = if stepped {
            let target = before + sample.offset;
            sys::set_realtime(target.trunc() as i64, (target.fract() * 1e9) as i64)
        } else {
            sys::slew_realtime(sample.offset)
        };
        match result {
            Ok(()) => {
                self.syncs += 1;
                self.failures = 0;
                self.state = State::Waiting;
                self.last = Some((server.to_string(), sample, now()));
                log(
                    "hos-ntpd",
                    &format!(
                        "{server}: offset {:+.3}s {} (stratum {}, delay {:.3}s)",
                        sample.offset,
                        if stepped { "stepped" } else { "slewed" },
                        sample.stratum,
                        sample.delay
                    ),
                );
                self.events.push(
                    Fields::new()
                        .text("event", "sync")
                        .text("server", server)
                        .number("offset", format!("{:.6}", sample.offset))
                        .number("stratum", sample.stratum)
                        .flag("stepped", stepped)
                        .line(),
                );
                self.save_stamp();
                // A clock that agrees with the server does not need asking
                // again soon; one that was stepped is checked again sooner.
                self.poll = if stepped {
                    self.min_poll
                } else {
                    (self.poll * 2).min(self.max_poll)
                };
                self.next = Instant::now() + Duration::from_secs(self.poll);
            }
            Err(e) => {
                log("hos-ntpd", &format!("could not set the clock: {e}"));
                self.next = Instant::now() + RETRY;
            }
        }
    }
    /// Try each server in turn until one answers.
    fn synchronize(&mut self) {
        let servers = self.servers.clone();
        for server in servers {
            match query(&server) {
                Ok(sample) => {
                    self.apply(&server, sample);
                    return;
                }
                Err(e) => log("hos-ntpd", &e),
            }
        }
        self.failures += 1;
        if self.state != State::Waiting {
            self.state = State::Unsynchronized;
        }
        // Back off gently: a network that is up but has no route to a server
        // should not be retried every few seconds forever.
        self.next = Instant::now() + RETRY * self.failures.min(8);
    }
}

impl Default for Ntpd {
    fn default() -> Self {
        Ntpd::new()
    }
}

impl Service for Ntpd {
    fn handle(&mut self, request: &Request, _peer: &Peer) -> Result<Response, String> {
        match request.verb.as_str() {
            "STATUS" => {
                let mut fields = Fields::new()
                    .text("state", self.state.name())
                    .flag("online", self.online)
                    .number("poll", self.poll)
                    .number("syncs", self.syncs)
                    .number("failures", self.failures)
                    .number(
                        "next",
                        self.next.saturating_duration_since(Instant::now()).as_secs(),
                    );
                if let Some((server, sample, at)) = &self.last {
                    fields = fields
                        .text("server", server)
                        .number("offset", format!("{:.6}", sample.offset))
                        .number("delay", format!("{:.6}", sample.delay))
                        .number("stratum", sample.stratum)
                        .number("synced", (now() - at) as i64);
                }
                Ok(Response::ok().record(fields))
            }
            "SERVERS" => Ok(Response::ok().records(
                self.servers
                    .iter()
                    .map(|server| Fields::new().text("server", server)),
            )),
            "SYNC" => {
                if !self.online && request.keyword(0) != "FORCE" {
                    return Err("the system is offline; SYNC FORCE tries anyway".into());
                }
                self.synchronize();
                match &self.last {
                    Some((server, sample, _)) if self.state == State::Waiting => Ok(
                        Response::message(format!("{server}: offset {:+.3}s", sample.offset)),
                    ),
                    _ => Err("no server answered".into()),
                }
            }
            "SET" => {
                let response = ipc::setting("ntpd.conf", SETTABLE, request)?;
                self.reload();
                Ok(response)
            }
            verb => Err(format!("{verb} is not a time command")),
        }
    }
    fn tick(&mut self) -> Duration {
        // Drain whatever hos-netd has to say about the link.
        let mut updates: Vec<bool> = Vec::new();
        if let Some(netd) = self.netd.as_mut() {
            loop {
                match netd.event(Duration::from_millis(0)) {
                    Ok(Some(event)) => {
                        let record = ipc::Record::parse(&event);
                        if record.get("event") == Some("online") {
                            updates.push(record.flag("online"));
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        // The network service restarted; reconnect shortly.
                        self.netd = None;
                        self.netd_retry = Instant::now() + Duration::from_secs(5);
                        break;
                    }
                }
            }
        } else if Instant::now() >= self.netd_retry {
            self.watch_network();
        }
        for online in updates {
            self.set_online(online);
        }
        if !self.online {
            // Parked: nothing to do until the network comes back.
            return Duration::from_secs(60);
        }
        if Instant::now() >= self.next {
            self.synchronize();
        }
        self.next
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(250))
    }
    fn sources(&mut self) -> Vec<RawFd> {
        self.netd.as_ref().map(|netd| netd.as_raw_fd()).into_iter().collect()
    }
    fn events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }
    fn public(&self) -> &'static [&'static str] {
        &["STATUS", "SERVERS"]
    }
    fn help(&self) -> &'static [&'static str] {
        &[
            "STATUS - state, poll interval and the last measurement",
            "SERVERS - the configured time servers",
            "SYNC [FORCE] - query the servers now",
            "SET ntp servers host... - change the servers in ntpd.conf",
        ]
    }
    fn reload(&mut self) {
        let settings = Settings::install("ntpd.conf", NTPD_DEFAULT);
        if let Some(servers) = settings.get("ntp", "servers") {
            self.servers = servers
                .split([' ', ','])
                .map(str::trim)
                .filter(|server| !server.is_empty())
                .map(str::to_string)
                .collect();
        }
        log("hos-ntpd", "reloaded ntpd.conf");
    }
    fn stop(&mut self) {
        self.save_stamp();
    }
}

/// Run the service.
pub fn main() -> io::Result<()> {
    log("hos-ntpd", "starting");
    let mut ntpd = Ntpd::new();
    ipc::serve("ntpd", 0o666, &mut ntpd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the reply a server would send for a request sent at `sent`.
    fn reply(sent: f64, server_time: f64, stratum: u8, mode: u8) -> [u8; PACKET] {
        let mut packet = [0u8; PACKET];
        packet[0] = (4 << 3) | mode;
        packet[1] = stratum;
        packet[24..32].copy_from_slice(&to_ntp(sent).to_be_bytes());
        packet[32..40].copy_from_slice(&to_ntp(server_time).to_be_bytes());
        packet[40..48].copy_from_slice(&to_ntp(server_time + 0.001).to_be_bytes());
        packet
    }

    #[test]
    fn timestamps_survive_the_conversion_to_ntp_format() {
        for seconds in [0.0, 1.0, 1_700_000_000.25, 2_000_000_000.5] {
            assert!((from_ntp(to_ntp(seconds)) - seconds).abs() < 1e-6, "{seconds}");
        }
        let packet = request(1_700_000_000.0);
        assert_eq!(packet[0], 0x23, "leap 0, version 4, mode 3");
        assert_eq!(packet.len(), PACKET);
    }
    #[test]
    fn a_reply_measures_the_offset_and_the_round_trip() {
        // The client is 10 seconds behind; the round trip takes 20 ms.
        let sent = 1_700_000_000.0;
        let received = sent + 0.020;
        let packet = reply(sent, sent + 10.010, 2, 4);
        let sample = sample(&packet, sent, received).unwrap();
        assert!((sample.offset - 10.0).abs() < 0.005, "{}", sample.offset);
        assert!(sample.delay > 0.0 && sample.delay < 0.05);
        assert_eq!(sample.stratum, 2);
    }
    #[test]
    fn replies_that_do_not_belong_to_this_request_are_refused() {
        let sent = 1_700_000_000.0;
        let good = reply(sent, sent, 2, 4);
        assert!(sample(&good, sent + 1.0, sent + 1.1).is_err(), "not echoed");
        assert!(sample(&good[..40], sent, sent).is_err(), "truncated");
        assert!(sample(&reply(sent, sent, 0, 4), sent, sent).is_err(), "kiss of death");
        assert!(sample(&reply(sent, sent, 2, 3), sent, sent).is_err(), "not a server");
        let mut unsynchronized = reply(sent, sent, 2, 4);
        unsynchronized[0] |= 0xc0;
        assert!(sample(&unsynchronized, sent, sent).is_err(), "leap 3");
        let mut empty = reply(sent, sent, 2, 4);
        empty[32..48].fill(0);
        assert!(sample(&empty, sent, sent).is_err(), "no server timestamp");
    }
}
