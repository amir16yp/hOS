//! A DHCPv4 client: enough of RFC 2131 to configure one interface.
//!
//! `hos-netd` runs one of these per interface set to `dhcp`. It sends from
//! UDP port 68 bound to the interface, and asks servers to broadcast their
//! replies, because the interface has no address yet when the exchange starts.
//!
//! Only the options the desktop needs are requested: subnet mask, router, DNS
//! servers, domain name and the lease timers.
use crate::init::sys;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    os::fd::{AsRawFd, RawFd},
    time::{Duration, Instant},
};

pub const CLIENT_PORT: u16 = 68;
pub const SERVER_PORT: u16 = 67;

pub const DISCOVER: u8 = 1;
pub const OFFER: u8 = 2;
pub const REQUEST: u8 = 3;
pub const DECLINE: u8 = 4;
pub const ACK: u8 = 5;
pub const NAK: u8 = 6;
pub const RELEASE: u8 = 7;

const OPTION_NETMASK: u8 = 1;
const OPTION_ROUTER: u8 = 3;
const OPTION_DNS: u8 = 6;
const OPTION_HOSTNAME: u8 = 12;
const OPTION_DOMAIN: u8 = 15;
const OPTION_REQUESTED_IP: u8 = 50;
const OPTION_LEASE_TIME: u8 = 51;
const OPTION_TYPE: u8 = 53;
const OPTION_SERVER_ID: u8 = 54;
const OPTION_PARAMETERS: u8 = 55;
const OPTION_MAX_SIZE: u8 = 57;
const OPTION_T1: u8 = 58;
const OPTION_T2: u8 = 59;
const OPTION_CLIENT_ID: u8 = 61;
const OPTION_END: u8 = 255;

const COOKIE: [u8; 4] = [99, 130, 83, 99];
/// Fixed part of a BOOTP message, before the magic cookie.
const HEADER: usize = 236;

/// Build one client message. `ciaddr` is set while renewing an address.
pub fn build(
    kind: u8,
    xid: u32,
    mac: [u8; 6],
    ciaddr: Ipv4Addr,
    requested: Option<Ipv4Addr>,
    server: Option<Ipv4Addr>,
    hostname: Option<&str>,
    broadcast: bool,
) -> Vec<u8> {
    let mut packet = vec![0u8; HEADER];
    packet[0] = 1; // BOOTREQUEST
    packet[1] = 1; // Ethernet
    packet[2] = 6; // MAC length
    packet[4..8].copy_from_slice(&xid.to_be_bytes());
    if broadcast {
        packet[10] = 0x80; // Ask for a broadcast reply: we have no address yet.
    }
    packet[12..16].copy_from_slice(&ciaddr.octets());
    packet[28..34].copy_from_slice(&mac);
    packet.extend_from_slice(&COOKIE);
    packet.extend_from_slice(&[OPTION_TYPE, 1, kind]);
    let mut client_id = vec![1];
    client_id.extend_from_slice(&mac);
    packet.extend_from_slice(&[OPTION_CLIENT_ID, client_id.len() as u8]);
    packet.extend_from_slice(&client_id);
    packet.extend_from_slice(&[OPTION_MAX_SIZE, 2]);
    packet.extend_from_slice(&576u16.to_be_bytes());
    if let Some(address) = requested {
        packet.extend_from_slice(&[OPTION_REQUESTED_IP, 4]);
        packet.extend_from_slice(&address.octets());
    }
    if let Some(address) = server {
        packet.extend_from_slice(&[OPTION_SERVER_ID, 4]);
        packet.extend_from_slice(&address.octets());
    }
    if let Some(name) = hostname.map(str::trim).filter(|n| !n.is_empty()) {
        let name = name.as_bytes();
        let length = name.len().min(63);
        packet.extend_from_slice(&[OPTION_HOSTNAME, length as u8]);
        packet.extend_from_slice(&name[..length]);
    }
    if matches!(kind, DISCOVER | REQUEST) {
        packet.extend_from_slice(&[
            OPTION_PARAMETERS,
            5,
            OPTION_NETMASK,
            OPTION_ROUTER,
            OPTION_DNS,
            OPTION_DOMAIN,
            OPTION_LEASE_TIME,
        ]);
    }
    packet.push(OPTION_END);
    packet
}

/// A server reply, split into its message type and options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub kind: u8,
    pub yiaddr: Ipv4Addr,
    pub options: Vec<(u8, Vec<u8>)>,
}
impl Message {
    fn option(&self, code: u8) -> Option<&[u8]> {
        self.options
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, value)| value.as_slice())
    }
    fn address(&self, code: u8) -> Option<Ipv4Addr> {
        let value = self.option(code)?;
        Some(Ipv4Addr::from(<[u8; 4]>::try_from(value.get(..4)?).ok()?))
    }
    fn addresses(&self, code: u8) -> Vec<Ipv4Addr> {
        self.option(code)
            .unwrap_or_default()
            .chunks_exact(4)
            .map(|c| Ipv4Addr::new(c[0], c[1], c[2], c[3]))
            .collect()
    }
    fn seconds(&self, code: u8) -> Option<u32> {
        let value = self.option(code)?;
        Some(u32::from_be_bytes(
            <[u8; 4]>::try_from(value.get(..4)?).ok()?,
        ))
    }
    pub fn server(&self) -> Option<Ipv4Addr> {
        self.address(OPTION_SERVER_ID)
    }
}

/// Parse a reply addressed to this client. Anything else returns `None`.
pub fn parse(data: &[u8], xid: u32, mac: [u8; 6]) -> Option<Message> {
    if data.len() < HEADER + 4 || data[0] != 2 || data[HEADER..HEADER + 4] != COOKIE {
        return None;
    }
    if u32::from_be_bytes(data[4..8].try_into().ok()?) != xid || data[28..34] != mac {
        return None;
    }
    let mut options = Vec::new();
    let mut rest = &data[HEADER + 4..];
    while let Some((&code, tail)) = rest.split_first() {
        match code {
            OPTION_END => break,
            0 => rest = tail, // Padding between options.
            _ => {
                let (&length, tail) = tail.split_first()?;
                let length = length as usize;
                if tail.len() < length {
                    return None;
                }
                options.push((code, tail[..length].to_vec()));
                rest = &tail[length..];
            }
        }
    }
    let kind = options
        .iter()
        .find(|(code, _)| *code == OPTION_TYPE)
        .and_then(|(_, value)| value.first().copied())?;
    Some(Message {
        kind,
        yiaddr: Ipv4Addr::new(data[16], data[17], data[18], data[19]),
        options,
    })
}

/// An accepted lease and everything it configures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4Addr,
    pub netmask: Ipv4Addr,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub domain: Option<String>,
    pub server: Ipv4Addr,
    /// Lease length in seconds, as the server granted it.
    pub seconds: u32,
    /// Renewal and rebinding times, in seconds from when it was granted.
    pub t1: u32,
    pub t2: u32,
}
impl Lease {
    /// Build a lease from an ACK. Returns `None` if the server left out what
    /// an address is useless without.
    pub fn from_ack(message: &Message) -> Option<Lease> {
        if message.kind != ACK || message.yiaddr.is_unspecified() {
            return None;
        }
        let seconds = message.seconds(OPTION_LEASE_TIME).unwrap_or(3600).max(60);
        let netmask = message
            .address(OPTION_NETMASK)
            .unwrap_or_else(|| default_netmask(message.yiaddr));
        Some(Lease {
            address: message.yiaddr,
            netmask,
            router: message.address(OPTION_ROUTER),
            dns: message.addresses(OPTION_DNS),
            domain: message
                .option(OPTION_DOMAIN)
                .map(|value| String::from_utf8_lossy(value).trim().to_string())
                .filter(|domain| !domain.is_empty()),
            server: message.server().unwrap_or(Ipv4Addr::UNSPECIFIED),
            seconds,
            t1: message
                .seconds(OPTION_T1)
                .filter(|t1| *t1 < seconds)
                .unwrap_or(seconds / 2),
            t2: message
                .seconds(OPTION_T2)
                .filter(|t2| *t2 < seconds)
                .unwrap_or(seconds / 8 * 7),
        })
    }
    /// The prefix length, for `10.0.2.15/24`.
    pub fn prefix(&self) -> u32 {
        u32::from(self.netmask).count_ones()
    }
}

/// Classful fallback for a server that sent no subnet mask.
fn default_netmask(address: Ipv4Addr) -> Ipv4Addr {
    match address.octets()[0] {
        0..=127 => Ipv4Addr::new(255, 0, 0, 0),
        128..=191 => Ipv4Addr::new(255, 255, 0, 0),
        _ => Ipv4Addr::new(255, 255, 255, 0),
    }
}

/// Where the exchange has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Looking for a server.
    Discovering,
    /// An offer was accepted and the request is out.
    Requesting,
    Bound,
    /// Renewing with the server that granted the lease.
    Renewing,
    /// The lease is past T2: asking anyone.
    Rebinding,
}
impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Discovering => "discovering",
            Phase::Requesting => "requesting",
            Phase::Bound => "bound",
            Phase::Renewing => "renewing",
            Phase::Rebinding => "rebinding",
        }
    }
}

/// What one [`Client::poll`] produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Nothing changed.
    Waiting,
    /// A new or renewed lease; the caller applies it to the interface.
    Bound(Box<Lease>),
    /// The lease expired or the server refused it.
    Lost(String),
}

/// Shortest and longest wait between retransmissions.
const RETRY_MIN: Duration = Duration::from_secs(4);
const RETRY_MAX: Duration = Duration::from_secs(64);

/// The DHCP client for one interface.
pub struct Client {
    pub interface: String,
    mac: [u8; 6],
    hostname: Option<String>,
    socket: UdpSocket,
    xid: u32,
    phase: Phase,
    /// The lease in use, or the offer being requested.
    lease: Option<Lease>,
    offer: Option<Lease>,
    granted: Instant,
    next_send: Instant,
    retry: Duration,
}
impl Client {
    pub fn start(interface: &str, mac: [u8; 6], hostname: Option<String>) -> io::Result<Self> {
        let socket = bind_to_interface(interface)?;
        let mut client = Client {
            interface: interface.to_string(),
            mac,
            hostname,
            socket,
            xid: 0,
            phase: Phase::Discovering,
            lease: None,
            offer: None,
            granted: Instant::now(),
            next_send: Instant::now(),
            retry: RETRY_MIN,
        };
        client.restart();
        Ok(client)
    }
    /// Start a fresh exchange, after a link came back or a lease was lost.
    pub fn restart(&mut self) {
        self.xid = new_xid(self.mac);
        self.phase = Phase::Discovering;
        self.offer = None;
        self.retry = RETRY_MIN;
        self.next_send = Instant::now();
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn lease(&self) -> Option<&Lease> {
        self.lease.as_ref()
    }
    /// Seconds left on the current lease.
    pub fn remaining(&self) -> u32 {
        match &self.lease {
            Some(lease) => lease
                .seconds
                .saturating_sub(self.granted.elapsed().as_secs() as u32),
            None => 0,
        }
    }
    pub fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
    /// How long the caller may sleep before calling [`Client::poll`] again.
    pub fn timeout(&self) -> Duration {
        self.next_send.saturating_duration_since(Instant::now())
    }
    /// Give the address back, so the server can reuse it right away.
    pub fn release(&mut self) {
        if let Some(lease) = self.lease.take() {
            let packet = build(
                RELEASE,
                self.xid,
                self.mac,
                lease.address,
                None,
                Some(lease.server),
                None,
                false,
            );
            let target = SocketAddrV4::new(lease.server, SERVER_PORT);
            let _ = self.socket.send_to(&packet, SocketAddr::V4(target));
        }
        self.phase = Phase::Discovering;
    }
    /// Read replies and send whatever the current phase is due to send.
    pub fn poll(&mut self) -> Progress {
        let mut progress = Progress::Waiting;
        let mut buffer = [0u8; 1500];
        loop {
            match self.socket.recv_from(&mut buffer) {
                Ok((length, _)) => {
                    if let Some(message) = parse(&buffer[..length], self.xid, self.mac) {
                        if let Some(next) = self.receive(&message) {
                            progress = next;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        if progress != Progress::Waiting {
            return progress;
        }
        let now = Instant::now();
        if let Some(lease) = &self.lease {
            let age = self.granted.elapsed().as_secs() as u32;
            if age >= lease.seconds {
                self.lease = None;
                self.restart();
                return Progress::Lost("the lease expired".into());
            }
            if age >= lease.t2 && self.phase != Phase::Rebinding {
                self.phase = Phase::Rebinding;
                self.next_send = now;
                self.retry = RETRY_MIN;
            } else if age >= lease.t1 && self.phase == Phase::Bound {
                self.phase = Phase::Renewing;
                self.next_send = now;
                self.retry = RETRY_MIN;
            }
        }
        if now >= self.next_send {
            self.send();
        }
        progress
    }
    fn receive(&mut self, message: &Message) -> Option<Progress> {
        match (self.phase, message.kind) {
            (Phase::Discovering, OFFER) => {
                let mut offer = Lease::from_ack(&Message {
                    kind: ACK,
                    ..message.clone()
                })?;
                offer.server = message.server().unwrap_or(Ipv4Addr::UNSPECIFIED);
                self.offer = Some(offer);
                self.phase = Phase::Requesting;
                self.retry = RETRY_MIN;
                self.send();
                None
            }
            (_, ACK) => {
                let lease = Lease::from_ack(message)?;
                self.granted = Instant::now();
                self.phase = Phase::Bound;
                self.retry = RETRY_MIN;
                // Wake again at T1 to renew.
                self.next_send = self.granted + Duration::from_secs(lease.t1.max(1) as u64);
                self.lease = Some(lease.clone());
                self.offer = None;
                Some(Progress::Bound(Box::new(lease)))
            }
            (_, NAK) => {
                self.lease = None;
                self.restart();
                Some(Progress::Lost("the server refused the lease".into()))
            }
            _ => None,
        }
    }
    fn send(&mut self) {
        let hostname = self.hostname.clone();
        let packet = match self.phase {
            Phase::Discovering => build(
                DISCOVER,
                self.xid,
                self.mac,
                Ipv4Addr::UNSPECIFIED,
                self.lease.as_ref().map(|l| l.address),
                None,
                hostname.as_deref(),
                true,
            ),
            Phase::Requesting => {
                let offer = self.offer.clone();
                build(
                    REQUEST,
                    self.xid,
                    self.mac,
                    Ipv4Addr::UNSPECIFIED,
                    offer.as_ref().map(|l| l.address),
                    offer.as_ref().map(|l| l.server),
                    hostname.as_deref(),
                    true,
                )
            }
            Phase::Bound | Phase::Renewing | Phase::Rebinding => {
                let Some(lease) = self.lease.clone() else {
                    return;
                };
                build(
                    REQUEST,
                    self.xid,
                    self.mac,
                    lease.address,
                    None,
                    None,
                    hostname.as_deref(),
                    self.phase == Phase::Rebinding,
                )
            }
        };
        let target = match (self.phase, &self.lease) {
            // Renewal goes straight to the server that granted the lease.
            (Phase::Renewing, Some(lease)) if !lease.server.is_unspecified() => lease.server,
            _ => Ipv4Addr::BROADCAST,
        };
        let _ = self
            .socket
            .send_to(&packet, SocketAddr::V4(SocketAddrV4::new(target, SERVER_PORT)));
        self.next_send = Instant::now() + self.retry;
        self.retry = (self.retry * 2).min(RETRY_MAX);
    }
}

/// A transaction ID that differs per interface and per attempt.
fn new_xid(mac: [u8; 6]) -> u32 {
    let (seconds, nanos) = sys::realtime();
    let mut xid = (seconds as u32) ^ nanos ^ std::process::id();
    xid ^= u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]);
    xid
}

/// A UDP socket on port 68 that only sees one interface's traffic.
fn bind_to_interface(interface: &str) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, CLIENT_PORT))?;
    socket.set_broadcast(true)?;
    socket.set_nonblocking(true)?;
    let name = interface.as_bytes();
    // SO_BINDTODEVICE keeps one client per interface from answering for another.
    // SAFETY: the option value is this slice, with its own length.
    let rc = unsafe {
        sys::setsockopt(
            socket.as_raw_fd(),
            1,
            25,
            name.as_ptr(),
            name.len().min(15) as u32,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    const MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

    /// Build the ACK a server would send, to parse it back.
    fn ack(xid: u32, extra: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut packet = vec![0u8; HEADER];
        packet[0] = 2;
        packet[4..8].copy_from_slice(&xid.to_be_bytes());
        packet[16..20].copy_from_slice(&[10, 0, 2, 15]);
        packet[28..34].copy_from_slice(&MAC);
        packet.extend_from_slice(&COOKIE);
        packet.extend_from_slice(&[OPTION_TYPE, 1, ACK, 0, 0]); // padding is skipped
        for (code, value) in extra {
            packet.push(*code);
            packet.push(value.len() as u8);
            packet.extend_from_slice(value);
        }
        packet.push(OPTION_END);
        packet
    }

    #[test]
    fn a_discover_carries_the_client_id_and_parameter_list() {
        let packet = build(
            DISCOVER,
            0x1234_5678,
            MAC,
            Ipv4Addr::UNSPECIFIED,
            None,
            None,
            Some("hos"),
            true,
        );
        assert_eq!(&packet[..4], &[1, 1, 6, 0]);
        assert_eq!(&packet[4..8], &0x1234_5678u32.to_be_bytes());
        assert_eq!(packet[10], 0x80, "the reply must be broadcast");
        assert_eq!(&packet[28..34], &MAC);
        assert_eq!(&packet[HEADER..HEADER + 4], &COOKIE);
        let options = &packet[HEADER + 4..];
        assert_eq!(&options[..3], &[OPTION_TYPE, 1, DISCOVER]);
        assert!(options.windows(2).any(|w| w == [OPTION_HOSTNAME, 3]));
        assert!(options.windows(2).any(|w| w == [OPTION_PARAMETERS, 5]));
        assert_eq!(*packet.last().unwrap(), OPTION_END);
    }
    #[test]
    fn an_ack_becomes_a_lease_with_routes_and_servers() {
        let xid = 42;
        let packet = ack(
            xid,
            &[
                (OPTION_NETMASK, vec![255, 255, 255, 0]),
                (OPTION_ROUTER, vec![10, 0, 2, 2]),
                (OPTION_DNS, vec![10, 0, 2, 3, 1, 1, 1, 1]),
                (OPTION_DOMAIN, b"lan".to_vec()),
                (OPTION_LEASE_TIME, 86400u32.to_be_bytes().to_vec()),
                (OPTION_SERVER_ID, vec![10, 0, 2, 2]),
            ],
        );
        let message = parse(&packet, xid, MAC).expect("a reply for this client");
        let lease = Lease::from_ack(&message).expect("an ACK carries a lease");
        assert_eq!(lease.address, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(lease.prefix(), 24);
        assert_eq!(lease.router, Some(Ipv4Addr::new(10, 0, 2, 2)));
        assert_eq!(lease.dns, [Ipv4Addr::new(10, 0, 2, 3), Ipv4Addr::new(1, 1, 1, 1)]);
        assert_eq!(lease.domain.as_deref(), Some("lan"));
        assert_eq!(lease.seconds, 86400);
        assert_eq!((lease.t1, lease.t2), (43200, 75600));
    }
    #[test]
    fn replies_for_other_clients_and_damaged_packets_are_ignored() {
        let packet = ack(7, &[]);
        assert!(parse(&packet, 8, MAC).is_none(), "another transaction");
        assert!(parse(&packet, 7, [0; 6]).is_none(), "another client");
        assert!(parse(&packet[..HEADER], 7, MAC).is_none(), "truncated");
        let mut broken = packet.clone();
        broken[HEADER] = 0;
        assert!(parse(&broken, 7, MAC).is_none(), "no magic cookie");
        // An option that claims more bytes than the packet holds.
        let mut overrun = ack(7, &[]);
        overrun.pop();
        overrun.extend_from_slice(&[OPTION_ROUTER, 8, 1, 2]);
        assert!(parse(&overrun, 7, MAC).is_none());
    }
    #[test]
    fn a_missing_netmask_falls_back_to_the_classful_default() {
        let message = parse(&ack(1, &[]), 1, MAC).unwrap();
        let lease = Lease::from_ack(&message).unwrap();
        assert_eq!(lease.netmask, Ipv4Addr::new(255, 0, 0, 0));
        assert_eq!(lease.seconds, 3600, "a server that sent no lease time");
        assert_eq!(lease.t1, 1800);
    }
}
