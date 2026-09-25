//! `hos-netd`: link state, addresses, DHCP, DNS and Wi-Fi.
//!
//! The service keeps one [`Link`] per kernel interface. Wired links are
//! configured as soon as they carry a signal; wireless links are associated
//! through `wpa_supplicant` first (see [`crate::init::wifi`]) and then
//! configured the same way. Addresses and routes are set with the classic
//! `SIOC*` ioctls, and a netlink socket wakes the service when the kernel
//! reports a change, so an idle network costs nothing.
//!
//! `/etc/hos/netd.conf` decides what each interface does:
//!
//! ```text
//! [interface eth0]
//! method = dhcp            # dhcp, static or off
//!
//! [interface eth1]
//! method = static
//! address = 192.168.1.5/24
//! gateway = 192.168.1.1
//! dns = 192.168.1.1
//! ```
use crate::init::{
    Settings, dhcp, ipc,
    ipc::{Fields, Peer, Request, Response, Service},
    log,
    sys::{self, Fd},
    wifi,
};
use std::{
    fs, io,
    net::Ipv4Addr,
    os::fd::{AsRawFd, RawFd},
    path::Path,
    time::{Duration, Instant},
};

/// Where the kernel lists interfaces.
const SYS_NET: &str = "/sys/class/net";
/// Written to `/etc/hos` the first time the service runs.
const NETD_DEFAULT: &str = include_str!("config/netd.conf");

/// The resolver configuration this service owns.
pub const RESOLV_CONF: &str = "/etc/resolv.conf";
/// How often links are re-read when netlink is unavailable.
const SWEEP: Duration = Duration::from_secs(2);
/// How long a Wi-Fi scan result is considered current.
const SCAN_AGE: Duration = Duration::from_secs(20);
/// What `SET` may change in `netd.conf`.
const SETTABLE: &[(&str, &str)] = &[
    ("interface *", "method"),
    ("interface *", "address"),
    ("interface *", "gateway"),
    ("interface *", "dns"),
    ("network", "allow-users"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Loopback,
    Wired,
    Wireless,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Loopback => "loopback",
            Kind::Wired => "wired",
            Kind::Wireless => "wireless",
        }
    }
}

/// What the kernel says about one interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    pub kind: Kind,
    pub mac: [u8; 6],
    /// `up`, `down`, `dormant` or `unknown`, from `operstate`.
    pub operstate: String,
    /// Whether the cable or the association is live.
    pub carrier: bool,
    /// Whether the interface is administratively up.
    pub up: bool,
}
impl Interface {
    pub fn mac_text(&self) -> String {
        self.mac
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    }
}

/// Parse a MAC address such as `52:54:00:12:34:56`.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let mut mac = [0u8; 6];
    let mut parts = text.trim().split(':');
    for byte in &mut mac {
        *byte = u8::from_str_radix(parts.next()?, 16).ok()?;
    }
    if parts.next().is_some() { None } else { Some(mac) }
}

/// Read every interface the kernel knows about, in name order.
pub fn interfaces() -> Vec<Interface> {
    read_interfaces(Path::new(SYS_NET))
}
fn read_interfaces(root: &Path) -> Vec<Interface> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut interfaces: Vec<Interface> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let flags: u32 = sys::read_text(path.join("flags"))
                .and_then(|text| u32::from_str_radix(text.trim_start_matches("0x"), 16).ok())
                .unwrap_or(0);
            let kind = if flags & 0x8 != 0 || name == "lo" {
                Kind::Loopback
            } else if path.join("wireless").is_dir() || path.join("phy80211").exists() {
                Kind::Wireless
            } else {
                Kind::Wired
            };
            Some(Interface {
                name,
                kind,
                mac: sys::read_text(path.join("address"))
                    .and_then(|text| parse_mac(&text))
                    .unwrap_or_default(),
                operstate: sys::read_text(path.join("operstate"))
                    .unwrap_or_else(|| "unknown".into()),
                // A down interface reports no carrier at all.
                carrier: sys::read_number::<u32>(path.join("carrier")) == Some(1),
                up: flags & 1 != 0,
            })
        })
        .collect();
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    interfaces
}

// Interface and route configuration through the classic socket ioctls.

const SIOCSIFFLAGS: u64 = 0x8914;
const SIOCGIFFLAGS: u64 = 0x8913;
const SIOCSIFADDR: u64 = 0x8916;
const SIOCGIFADDR: u64 = 0x8915;
const SIOCSIFNETMASK: u64 = 0x891c;
const SIOCADDRT: u64 = 0x890b;
const SIOCDELRT: u64 = 0x890c;
const IFF_UP: i16 = 1;

#[repr(C)]
struct IfReq {
    name: [u8; 16],
    data: [u8; 24],
}
impl IfReq {
    fn new(interface: &str) -> IfReq {
        let mut name = [0u8; 16];
        let bytes = interface.as_bytes();
        let length = bytes.len().min(15);
        name[..length].copy_from_slice(&bytes[..length]);
        IfReq {
            name,
            data: [0; 24],
        }
    }
    fn with_address(interface: &str, address: Ipv4Addr) -> IfReq {
        let mut request = IfReq::new(interface);
        request.data[..2].copy_from_slice(&2u16.to_ne_bytes()); // AF_INET
        request.data[4..8].copy_from_slice(&address.octets());
        request
    }
    fn address(&self) -> Ipv4Addr {
        Ipv4Addr::new(self.data[4], self.data[5], self.data[6], self.data[7])
    }
}

/// A socket used only as a handle for interface ioctls.
fn control_socket() -> io::Result<Fd> {
    // SAFETY: AF_INET SOCK_DGRAM, the conventional ioctl handle.
    Fd::new(unsafe { sys::socket(2, 2, 0) })
}
fn call(request: u64, data: &mut IfReq) -> io::Result<()> {
    let socket = control_socket()?;
    // SAFETY: every caller passes the ifreq these SIOC requests expect.
    if unsafe { sys::ioctl(socket.as_raw_fd(), request, data as *mut IfReq) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Give an interface an address and netmask.
pub fn set_address(interface: &str, address: Ipv4Addr, netmask: Ipv4Addr) -> io::Result<()> {
    call(SIOCSIFADDR, &mut IfReq::with_address(interface, address))?;
    call(
        SIOCSIFNETMASK,
        &mut IfReq::with_address(interface, netmask),
    )?;
    set_up(interface, true)
}
/// The address an interface currently has, if any.
pub fn address_of(interface: &str) -> Option<Ipv4Addr> {
    let mut request = IfReq::new(interface);
    call(SIOCGIFADDR, &mut request).ok()?;
    Some(request.address()).filter(|address| !address.is_unspecified())
}
/// Remove an interface's address.
pub fn clear_address(interface: &str) -> io::Result<()> {
    call(
        SIOCSIFADDR,
        &mut IfReq::with_address(interface, Ipv4Addr::UNSPECIFIED),
    )
}
/// Bring an interface up or down.
pub fn set_up(interface: &str, up: bool) -> io::Result<()> {
    let mut request = IfReq::new(interface);
    call(SIOCGIFFLAGS, &mut request)?;
    let mut flags = i16::from_ne_bytes([request.data[0], request.data[1]]);
    flags = if up { flags | IFF_UP } else { flags & !IFF_UP };
    request.data[..2].copy_from_slice(&flags.to_ne_bytes());
    call(SIOCSIFFLAGS, &mut request)
}

#[repr(C)]
struct RtEntry {
    pad1: u64,
    destination: [u8; 16],
    gateway: [u8; 16],
    genmask: [u8; 16],
    flags: u16,
    pad2: i16,
    pad3: [u8; 4],
    pad4: u64,
    pad5: u64,
    metric: i16,
    pad6: [u8; 6],
    device: *mut u8,
    mtu: u64,
    window: u64,
    irtt: u16,
    pad7: [u8; 6],
}
fn sockaddr_in(address: Ipv4Addr) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..2].copy_from_slice(&2u16.to_ne_bytes());
    bytes[4..8].copy_from_slice(&address.octets());
    bytes
}
fn default_route(request: u64, gateway: Ipv4Addr, interface: &str) -> io::Result<()> {
    let mut name: Vec<u8> = interface.as_bytes().to_vec();
    name.push(0);
    let mut route = RtEntry {
        pad1: 0,
        destination: sockaddr_in(Ipv4Addr::UNSPECIFIED),
        gateway: sockaddr_in(gateway),
        genmask: sockaddr_in(Ipv4Addr::UNSPECIFIED),
        // RTF_UP | RTF_GATEWAY
        flags: 0x0001 | 0x0002,
        pad2: 0,
        pad3: [0; 4],
        pad4: 0,
        pad5: 0,
        metric: 0,
        pad6: [0; 6],
        device: name.as_mut_ptr(),
        mtu: 0,
        window: 0,
        irtt: 0,
        pad7: [0; 6],
    };
    let socket = control_socket()?;
    // SAFETY: SIOCADDRT/SIOCDELRT take this rtentry; `name` outlives the call.
    let rc = unsafe { sys::ioctl(socket.as_raw_fd(), request, &mut route as *mut RtEntry) };
    drop(name);
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Route everything else through `gateway` on `interface`.
pub fn add_default_route(gateway: Ipv4Addr, interface: &str) -> io::Result<()> {
    default_route(SIOCADDRT, gateway, interface)
}
pub fn remove_default_route(gateway: Ipv4Addr, interface: &str) -> io::Result<()> {
    default_route(SIOCDELRT, gateway, interface)
}
/// The gateway of the current default route, read from `/proc/net/route`.
pub fn current_gateway() -> Option<(String, Ipv4Addr)> {
    parse_proc_route(&fs::read_to_string("/proc/net/route").ok()?)
}
fn parse_proc_route(text: &str) -> Option<(String, Ipv4Addr)> {
    for line in text.lines().skip(1) {
        let mut columns = line.split_whitespace();
        let interface = columns.next()?;
        let destination = columns.next()?;
        let gateway = columns.next()?;
        if destination != "00000000" {
            continue;
        }
        // The kernel prints little-endian hexadecimal.
        let value = u32::from_str_radix(gateway, 16).ok()?;
        return Some((interface.to_string(), Ipv4Addr::from(value.swap_bytes())));
    }
    None
}

/// The resolver file this service writes.
pub fn resolv_conf(servers: &[Ipv4Addr], domain: Option<&str>) -> String {
    let mut text = String::from("# Written by hos-netd. Changes are overwritten.\n");
    if let Some(domain) = domain.map(str::trim).filter(|d| !d.is_empty()) {
        text.push_str(&format!("search {domain}\n"));
    }
    for server in servers {
        text.push_str(&format!("nameserver {server}\n"));
    }
    text
}

/// A netlink socket that reports link, address and route changes.
pub struct Monitor(Fd);
impl Monitor {
    pub fn open() -> io::Result<Monitor> {
        // SAFETY: AF_NETLINK SOCK_RAW with the routing protocol.
        let socket = Fd::new(unsafe { sys::socket(16, 3, 0) })?.nonblocking()?;
        // struct sockaddr_nl { family, pad, pid, groups }
        let mut address = [0u8; 12];
        address[..2].copy_from_slice(&16u16.to_ne_bytes());
        // RTMGRP_LINK | RTMGRP_IPV4_IFADDR | RTMGRP_IPV4_ROUTE
        address[8..12].copy_from_slice(&(1u32 | 0x10 | 0x40).to_ne_bytes());
        // SAFETY: a sockaddr_nl of exactly this length.
        if unsafe { sys::bind(socket.as_raw_fd(), address.as_ptr(), address.len() as u32) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Monitor(socket))
    }
    /// Discard pending messages, reporting whether any arrived. The details
    /// are not parsed: sysfs is re-read instead, which is one code path.
    pub fn drain(&self) -> bool {
        let mut buffer = [0u8; 4096];
        let mut changed = false;
        loop {
            // SAFETY: recv into a local buffer with its own length.
            let n = unsafe {
                sys::recv(
                    self.0.as_raw_fd(),
                    buffer.as_mut_ptr(),
                    buffer.len(),
                    0x40, // MSG_DONTWAIT
                )
            };
            if n <= 0 {
                return changed;
            }
            changed = true;
        }
    }
}
impl AsRawFd for Monitor {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

/// How one interface is configured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Method {
    Dhcp,
    Static {
        address: Ipv4Addr,
        netmask: Ipv4Addr,
        gateway: Option<Ipv4Addr>,
        dns: Vec<Ipv4Addr>,
    },
    Off,
}
impl Method {
    pub fn name(&self) -> &'static str {
        match self {
            Method::Dhcp => "dhcp",
            Method::Static { .. } => "static",
            Method::Off => "off",
        }
    }
    /// Read one interface's method out of `netd.conf`.
    pub fn read(settings: &Settings, interface: &str, kind: Kind) -> Method {
        let section = format!("interface {interface}");
        match settings.get(&section, "method").unwrap_or(match kind {
            // Loopback is configured once and needs no client.
            Kind::Loopback => "off",
            _ => "dhcp",
        }) {
            "off" | "none" | "disabled" => Method::Off,
            "static" => {
                let (address, prefix) = settings
                    .get(&section, "address")
                    .and_then(parse_cidr)
                    .unwrap_or((Ipv4Addr::UNSPECIFIED, 24));
                Method::Static {
                    address,
                    netmask: netmask_of(prefix),
                    gateway: settings
                        .get(&section, "gateway")
                        .and_then(|value| value.parse().ok()),
                    dns: settings
                        .get(&section, "dns")
                        .map(parse_servers)
                        .unwrap_or_default(),
                }
            }
            _ => Method::Dhcp,
        }
    }
}
/// Parse `192.168.1.5/24`.
pub fn parse_cidr(value: &str) -> Option<(Ipv4Addr, u32)> {
    let (address, prefix) = match value.split_once('/') {
        Some((address, prefix)) => (address, prefix.trim().parse().ok()?),
        None => (value, 32),
    };
    if prefix > 32 {
        return None;
    }
    Some((address.trim().parse().ok()?, prefix))
}
/// The netmask for a prefix length: 24 becomes 255.255.255.0.
pub fn netmask_of(prefix: u32) -> Ipv4Addr {
    Ipv4Addr::from(if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix.min(32))
    })
}
fn parse_servers(value: &str) -> Vec<Ipv4Addr> {
    value
        .split([' ', ',', '\t'])
        .filter_map(|word| word.trim().parse().ok())
        .collect()
}

/// One interface and everything the service is doing with it.
struct Link {
    interface: Interface,
    method: Method,
    client: Option<dhcp::Client>,
    lease: Option<dhcp::Lease>,
    /// The address this service configured, if it did.
    address: Option<(Ipv4Addr, u32)>,
    gateway: Option<Ipv4Addr>,
    supplicant: Option<wifi::Supplicant>,
    wifi: wifi::Status,
    scanned: Option<Instant>,
    /// Reported once so a failing supplicant does not fill the log.
    wifi_error: Option<String>,
    /// Set by `DOWN`: the service leaves this link alone until `UP`.
    admin_down: bool,
    /// Reported once, for the same reason as `wifi_error`.
    up_error: Option<String>,
}
impl Link {
    fn new(interface: Interface, method: Method) -> Link {
        Link {
            interface,
            method,
            client: None,
            lease: None,
            address: None,
            gateway: None,
            supplicant: None,
            wifi: wifi::Status::default(),
            scanned: None,
            wifi_error: None,
            admin_down: false,
            up_error: None,
        }
    }
    fn configured(&self) -> bool {
        self.address.is_some()
    }
    /// Whether the service should bring this link up itself. Loopback is set
    /// up with its address, and a link the user turned off stays off.
    fn wants_up(&self) -> bool {
        !self.admin_down
            && !self.interface.up
            && self.method != Method::Off
            && self.interface.kind != Kind::Loopback
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new()
            .text("iface", &self.interface.name)
            .text("kind", self.interface.kind.name())
            .text("method", self.method.name())
            .text("operstate", &self.interface.operstate)
            .flag("carrier", self.interface.carrier)
            .flag("up", self.interface.up)
            .text("mac", &self.interface.mac_text());
        if let Some((address, prefix)) = self.address {
            fields = fields.text("address", &format!("{address}/{prefix}"));
        }
        if let Some(gateway) = self.gateway {
            fields = fields.text("gateway", &gateway.to_string());
        }
        if let Some(client) = &self.client {
            fields = fields
                .text("dhcp", client.phase().name())
                .number("lease_remaining", client.remaining());
        }
        if self.interface.kind == Kind::Wireless {
            fields = fields
                .text(
                    "wifi",
                    if self.supplicant.is_some() {
                        self.wifi.state.as_str()
                    } else {
                        "unavailable"
                    },
                )
                .text("ssid", &self.wifi.ssid);
            if !self.wifi.security.is_empty() {
                fields = fields.text("security", &self.wifi.security);
            }
        }
        fields
    }
}

/// The network service.
pub struct Netd {
    settings: Settings,
    links: Vec<Link>,
    monitor: Option<Monitor>,
    events: Vec<String>,
    /// The resolver servers currently written to `/etc/resolv.conf`.
    servers: Vec<Ipv4Addr>,
    domain: Option<String>,
    online: bool,
    hostname: Option<String>,
    /// Whether a desktop user may join networks and renew leases.
    allow_users: bool,
    swept: Instant,
}
impl Netd {
    pub fn new() -> Netd {
        let settings = Settings::install("netd.conf", NETD_DEFAULT);
        for warning in &settings.warnings {
            log("hos-netd", &format!("netd.conf: {warning}"));
        }
        let monitor = match Monitor::open() {
            Ok(monitor) => Some(monitor),
            Err(e) => {
                log(
                    "hos-netd",
                    &format!("netlink unavailable ({e}); polling sysfs instead"),
                );
                None
            }
        };
        let allow_users = settings.boolean("network", "allow-users").unwrap_or(true);
        let mut netd = Netd {
            settings,
            links: Vec::new(),
            monitor,
            events: Vec::new(),
            servers: Vec::new(),
            domain: None,
            online: false,
            hostname: sys::read_text("/etc/hostname").filter(|name| !name.is_empty()),
            allow_users,
            swept: Instant::now(),
        };
        netd.sweep();
        // Loopback is what local services expect to find working.
        if let Err(e) = set_address(
            "lo",
            Ipv4Addr::new(127, 0, 0, 1),
            Ipv4Addr::new(255, 0, 0, 0),
        ) {
            log("hos-netd", &format!("lo: {e}"));
        }
        netd
    }
    /// Re-read the interface list and start or stop clients as links change.
    fn sweep(&mut self) {
        self.swept = Instant::now();
        let interfaces = interfaces();
        self.links
            .retain(|link| interfaces.iter().any(|i| i.name == link.interface.name));
        for interface in interfaces {
            let method = Method::read(&self.settings, &interface.name, interface.kind);
            match self
                .links
                .iter_mut()
                .find(|link| link.interface.name == interface.name)
            {
                Some(link) => {
                    let was = std::mem::replace(&mut link.interface, interface);
                    if was.carrier != link.interface.carrier {
                        let name = link.interface.name.clone();
                        let carrier = link.interface.carrier;
                        self.events.push(
                            Fields::new()
                                .text("event", "link")
                                .text("iface", &name)
                                .flag("carrier", carrier)
                                .line(),
                        );
                        log(
                            "hos-netd",
                            &format!("{name} link {}", if carrier { "up" } else { "down" }),
                        );
                    }
                }
                None => {
                    let name = interface.name.clone();
                    self.links.push(Link::new(interface, method.clone()));
                    self.events.push(
                        Fields::new()
                            .text("event", "interface")
                            .text("iface", &name)
                            .text("method", method.name())
                            .line(),
                    );
                }
            }
        }
        self.raise_links();
    }
    /// Bring configured links up. The kernel reports no carrier at all for an
    /// interface that is administratively down, so a link left down after boot
    /// would never look connected and would never start its DHCP client.
    fn raise_links(&mut self) {
        for link in &mut self.links {
            if !link.wants_up() {
                continue;
            }
            match set_up(&link.interface.name, true) {
                Ok(()) => {
                    link.interface.up = true;
                    link.up_error = None;
                }
                Err(e) => {
                    let message = e.to_string();
                    if link.up_error.as_deref() != Some(&message) {
                        log("hos-netd", &format!("{}: {message}", link.interface.name));
                        link.up_error = Some(message);
                    }
                }
            }
        }
    }
    fn link(&mut self, name: &str) -> Result<&mut Link, String> {
        self.links
            .iter_mut()
            .find(|link| link.interface.name == name)
            .ok_or_else(|| format!("{name}: no such interface"))
    }
    /// The interface a Wi-Fi request applies to: the named one, or the only
    /// wireless interface present.
    fn wireless(&mut self, name: Option<&str>) -> Result<usize, String> {
        match name {
            Some(name) => self
                .links
                .iter()
                .position(|link| link.interface.name == name)
                .filter(|index| self.links[*index].interface.kind == Kind::Wireless)
                .ok_or_else(|| format!("{name} is not a wireless interface")),
            None => self
                .links
                .iter()
                .position(|link| link.interface.kind == Kind::Wireless)
                .ok_or_else(|| "no wireless interface".to_string()),
        }
    }
    /// Give one link the supplicant it needs, or report why it has none.
    fn supplicant(&mut self, index: usize) -> Result<&mut wifi::Supplicant, String> {
        if self.links[index]
            .supplicant
            .as_mut()
            .is_some_and(|s| !s.alive())
        {
            self.links[index].supplicant = None;
        }
        if self.links[index].supplicant.is_none() {
            let name = self.links[index].interface.name.clone();
            match wifi::Supplicant::open(&name) {
                Ok(supplicant) => {
                    self.links[index].supplicant = Some(supplicant);
                    self.links[index].wifi_error = None;
                }
                Err(e) => {
                    let message = e.to_string();
                    if self.links[index].wifi_error.as_deref() != Some(&message) {
                        log("hos-netd", &format!("{name}: {message}"));
                        self.links[index].wifi_error = Some(message.clone());
                    }
                    return Err(message);
                }
            }
        }
        Ok(self.links[index].supplicant.as_mut().expect("just opened"))
    }
    /// Apply an address, route and resolver settings to one link.
    fn configure(
        &mut self,
        index: usize,
        address: Ipv4Addr,
        netmask: Ipv4Addr,
        gateway: Option<Ipv4Addr>,
        dns: &[Ipv4Addr],
        domain: Option<String>,
    ) {
        let name = self.links[index].interface.name.clone();
        if let Err(e) = set_address(&name, address, netmask) {
            log("hos-netd", &format!("{name}: {e}"));
            return;
        }
        let prefix = u32::from(netmask).count_ones();
        self.links[index].address = Some((address, prefix));
        if let Some(old) = self.links[index].gateway.take() {
            let _ = remove_default_route(old, &name);
        }
        if let Some(gateway) = gateway {
            match add_default_route(gateway, &name) {
                Ok(()) => self.links[index].gateway = Some(gateway),
                Err(e) if e.raw_os_error() == Some(17) => {
                    // A default route already exists; the first link keeps it.
                    self.links[index].gateway = Some(gateway);
                }
                Err(e) => log("hos-netd", &format!("{name}: default route: {e}")),
            }
        }
        if !dns.is_empty() {
            self.servers = dns.to_vec();
            self.domain = domain;
            self.write_resolv();
        }
        log(
            "hos-netd",
            &format!("{name} configured as {address}/{prefix}"),
        );
        self.events.push(
            Fields::new()
                .text("event", "address")
                .text("iface", &name)
                .text("address", &format!("{address}/{prefix}"))
                .line(),
        );
        self.refresh_online();
    }
    /// Take an address off a link that lost its carrier or its lease.
    fn deconfigure(&mut self, index: usize) {
        let name = self.links[index].interface.name.clone();
        if let Some(gateway) = self.links[index].gateway.take() {
            let _ = remove_default_route(gateway, &name);
        }
        if self.links[index].address.take().is_some() {
            let _ = clear_address(&name);
            log("hos-netd", &format!("{name} lost its address"));
            self.events.push(
                Fields::new()
                    .text("event", "address")
                    .text("iface", &name)
                    .text("address", "")
                    .line(),
            );
        }
        self.links[index].lease = None;
        self.refresh_online();
    }
    fn write_resolv(&self) {
        let contents = resolv_conf(&self.servers, self.domain.as_deref());
        if let Err(e) = sys::write_atomic(Path::new(RESOLV_CONF), &contents) {
            log("hos-netd", &format!("{RESOLV_CONF}: {e}"));
        }
    }
    /// Recompute whether the system has a usable route, and say so once.
    fn refresh_online(&mut self) {
        let online = self.links.iter().any(|link| link.configured())
            && (current_gateway().is_some() || self.links.iter().any(|l| l.gateway.is_some()));
        if online != self.online {
            self.online = online;
            log(
                "hos-netd",
                if online { "network is up" } else { "network is down" },
            );
            self.events.push(
                Fields::new()
                    .text("event", "online")
                    .flag("online", online)
                    .line(),
            );
        }
    }
    /// Start or stop the DHCP client for one link, following its carrier.
    fn follow_carrier(&mut self, index: usize) {
        let carrier = self.links[index].interface.carrier;
        let wireless = self.links[index].interface.kind == Kind::Wireless;
        let associated = !wireless || self.links[index].wifi.connected();
        let wanted = carrier && associated && self.links[index].method == Method::Dhcp;
        if wanted && self.links[index].client.is_none() {
            let name = self.links[index].interface.name.clone();
            let mac = self.links[index].interface.mac;
            let _ = set_up(&name, true);
            match dhcp::Client::start(&name, mac, self.hostname.clone()) {
                Ok(client) => {
                    log("hos-netd", &format!("{name}: requesting a DHCP lease"));
                    self.links[index].client = Some(client);
                }
                Err(e) => log("hos-netd", &format!("{name}: DHCP: {e}")),
            }
        } else if !wanted && self.links[index].client.is_some() {
            self.links[index].client = None;
            self.deconfigure(index);
        }
    }
    /// Configure a link whose method is `static` and that is not set up yet.
    fn follow_static(&mut self, index: usize) {
        if self.links[index].configured() || !self.links[index].interface.carrier {
            return;
        }
        let Method::Static {
            address,
            netmask,
            gateway,
            dns,
        } = self.links[index].method.clone()
        else {
            return;
        };
        if address.is_unspecified() {
            return;
        }
        self.configure(index, address, netmask, gateway, &dns, None);
    }
    /// Check the association state of one wireless link.
    fn follow_wifi(&mut self, index: usize) {
        if self.links[index].interface.kind != Kind::Wireless
            || self.links[index].method == Method::Off
        {
            return;
        }
        let Ok(supplicant) = self.supplicant(index) else {
            return;
        };
        let Ok(status) = supplicant.status() else {
            return;
        };
        if status == self.links[index].wifi {
            return;
        }
        let name = self.links[index].interface.name.clone();
        let was_connected = self.links[index].wifi.connected();
        self.links[index].wifi = status.clone();
        self.events.push(
            Fields::new()
                .text("event", "wifi")
                .text("iface", &name)
                .text("state", &status.state)
                .text("ssid", &status.ssid)
                .line(),
        );
        if was_connected && !status.connected() {
            self.links[index].client = None;
            self.deconfigure(index);
        }
        if !was_connected && status.connected() {
            log("hos-netd", &format!("{name}: associated with {}", status.ssid));
        }
    }
}

impl Default for Netd {
    fn default() -> Self {
        Netd::new()
    }
}

impl Service for Netd {
    fn handle(&mut self, request: &Request, peer: &Peer) -> Result<Response, String> {
        match request.verb.as_str() {
            "LINKS" => Ok(Response::ok().records(self.links.iter().map(Link::fields))),
            "STATUS" => {
                if let Some(name) = request.arg(0) {
                    let fields = self.link(name)?.fields();
                    return Ok(Response::ok().record(fields));
                }
                let gateway = current_gateway();
                let summary = Fields::new()
                    .flag("online", self.online)
                    .number("links", self.links.len())
                    .text(
                        "gateway",
                        &gateway
                            .as_ref()
                            .map(|(_, address)| address.to_string())
                            .unwrap_or_default(),
                    )
                    .text(
                        "route_iface",
                        &gateway.map(|(name, _)| name).unwrap_or_default(),
                    )
                    .text(
                        "dns",
                        &self
                            .servers
                            .iter()
                            .map(Ipv4Addr::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                    );
                Ok(Response::ok()
                    .record(summary)
                    .records(self.links.iter().map(Link::fields)))
            }
            "UP" | "DOWN" => {
                let name = request.need(0, "an interface name")?.to_string();
                let up = request.verb == "UP";
                // Recorded before the sweep, which would otherwise raise the
                // interface again on its way past.
                let link = self.link(&name)?;
                link.admin_down = !up;
                link.up_error = None;
                set_up(&name, up).map_err(|e| format!("{name}: {e}"))?;
                self.sweep();
                Ok(Response::message(format!(
                    "{name} is now {}",
                    if up { "up" } else { "down" }
                )))
            }
            "RENEW" => {
                let names: Vec<String> = match request.arg(0) {
                    Some(name) => {
                        self.link(name)?;
                        vec![name.to_string()]
                    }
                    None => self
                        .links
                        .iter()
                        .filter(|link| link.method == Method::Dhcp)
                        .map(|link| link.interface.name.clone())
                        .collect(),
                };
                let mut renewed = 0;
                for name in &names {
                    if let Some(link) = self
                        .links
                        .iter_mut()
                        .find(|link| &link.interface.name == name)
                    {
                        if let Some(client) = link.client.as_mut() {
                            client.restart();
                            renewed += 1;
                        }
                    }
                }
                Ok(Response::message(format!("renewing {renewed} lease(s)")))
            }
            "DNS" => match request.keyword(0).as_str() {
                "" | "SHOW" => Ok(Response::ok().record(
                    Fields::new()
                        .text(
                            "servers",
                            &self
                                .servers
                                .iter()
                                .map(Ipv4Addr::to_string)
                                .collect::<Vec<_>>()
                                .join(","),
                        )
                        .text("domain", self.domain.as_deref().unwrap_or(""))
                        .text("file", RESOLV_CONF),
                )),
                "SET" => {
                    if !peer.root() {
                        return Err("DNS SET requires root".into());
                    }
                    let servers: Vec<Ipv4Addr> = request
                        .args
                        .iter()
                        .skip(1)
                        .filter_map(|value| value.parse().ok())
                        .collect();
                    if servers.is_empty() {
                        return Err("DNS SET needs at least one IPv4 address".into());
                    }
                    self.servers = servers;
                    self.write_resolv();
                    Ok(Response::message(format!(
                        "{} nameserver(s) written",
                        self.servers.len()
                    )))
                }
                other => Err(format!("DNS {other} is not a DNS command")),
            },
            "WIFI" => {
                let command = request.keyword(0);
                // Joining a network is something a desktop user does; the
                // policy in netd.conf decides whether that is allowed here.
                let allowed = peer.root()
                    || self.allow_users
                    || matches!(command.as_str(), "LIST" | "SCAN" | "STATUS" | "");
                if !allowed {
                    return Err(format!("WIFI {command} requires root"));
                }
                let index = match command.as_str() {
                    "CONNECT" | "FORGET" => self.wireless(request.arg(2))?,
                    _ => self.wireless(request.arg(1))?,
                };
                let name = self.links[index].interface.name.clone();
                match command.as_str() {
                    "SCAN" => {
                        self.supplicant(index)?
                            .scan()
                            .map_err(|e| format!("{name}: {e}"))?;
                        self.links[index].scanned = Some(Instant::now());
                        Ok(Response::message("scanning"))
                    }
                    "" | "LIST" => {
                        let stale = self.links[index]
                            .scanned
                            .is_none_or(|at| at.elapsed() > SCAN_AGE);
                        let supplicant = self.supplicant(index)?;
                        if stale {
                            let _ = supplicant.scan();
                        }
                        let networks = supplicant.networks().map_err(|e| format!("{name}: {e}"))?;
                        self.links[index].scanned = Some(Instant::now());
                        Ok(Response::ok().records(networks.iter().map(|network| {
                            Fields::new()
                                .text("ssid", &network.ssid)
                                .number("signal", network.signal)
                                .number("frequency", network.frequency)
                                .text("security", &network.security)
                                .flag("saved", network.saved)
                                .text("bssid", &network.bssid)
                        })))
                    }
                    "STATUS" => {
                        let status = self
                            .supplicant(index)?
                            .status()
                            .map_err(|e| format!("{name}: {e}"))?;
                        self.links[index].wifi = status.clone();
                        Ok(Response::ok().record(
                            Fields::new()
                                .text("iface", &name)
                                .text("state", &status.state)
                                .text("ssid", &status.ssid)
                                .text("bssid", &status.bssid)
                                .number("frequency", status.frequency)
                                .text("security", &status.security),
                        ))
                    }
                    "CONNECT" => {
                        let ssid = request.need(1, "a network name")?.to_string();
                        let psk = request.arg(2).map(str::to_string);
                        self.supplicant(index)?
                            .connect(&ssid, psk.as_deref())
                            .map_err(|e| format!("{name}: {e}"))?;
                        log("hos-netd", &format!("{name}: connecting to {ssid}"));
                        Ok(Response::message(format!("connecting to {ssid}")))
                    }
                    "FORGET" => {
                        let ssid = request.need(1, "a network name")?.to_string();
                        self.supplicant(index)?
                            .forget(&ssid)
                            .map_err(|e| format!("{name}: {e}"))?;
                        Ok(Response::message(format!("{ssid} removed")))
                    }
                    "DISCONNECT" => {
                        self.supplicant(index)?
                            .disconnect()
                            .map_err(|e| format!("{name}: {e}"))?;
                        Ok(Response::message("disconnected"))
                    }
                    "RECONNECT" => {
                        self.supplicant(index)?
                            .reconnect()
                            .map_err(|e| format!("{name}: {e}"))?;
                        Ok(Response::message("reconnecting"))
                    }
                    other => Err(format!("WIFI {other} is not a Wi-Fi command")),
                }
            }
            "SET" => {
                let response = ipc::setting("netd.conf", SETTABLE, request)?;
                self.reload();
                Ok(response)
            }
            verb => Err(format!("{verb} is not a network command")),
        }
    }
    fn tick(&mut self) -> Duration {
        let changed = self.monitor.as_ref().is_some_and(Monitor::drain);
        if changed || self.swept.elapsed() >= SWEEP {
            self.sweep();
        }
        let mut timeout = Duration::from_secs(5);
        for index in 0..self.links.len() {
            self.follow_wifi(index);
            match self.links[index].method {
                Method::Dhcp => self.follow_carrier(index),
                Method::Static { .. } => self.follow_static(index),
                Method::Off => (),
            }
            let Some(client) = self.links[index].client.as_mut() else {
                continue;
            };
            match client.poll() {
                dhcp::Progress::Bound(lease) => {
                    let lease = *lease;
                    self.links[index].lease = Some(lease.clone());
                    self.configure(
                        index,
                        lease.address,
                        lease.netmask,
                        lease.router,
                        &lease.dns,
                        lease.domain.clone(),
                    );
                }
                dhcp::Progress::Lost(reason) => {
                    let name = self.links[index].interface.name.clone();
                    log("hos-netd", &format!("{name}: {reason}"));
                    self.deconfigure(index);
                }
                dhcp::Progress::Waiting => (),
            }
            if let Some(client) = self.links[index].client.as_ref() {
                timeout = timeout.min(client.timeout());
            }
        }
        timeout.max(Duration::from_millis(100))
    }
    fn sources(&mut self) -> Vec<RawFd> {
        let mut sources: Vec<RawFd> = self
            .links
            .iter()
            .filter_map(|link| link.client.as_ref().map(dhcp::Client::as_raw_fd))
            .collect();
        if let Some(monitor) = &self.monitor {
            sources.push(monitor.as_raw_fd());
        }
        sources
    }
    fn events(&mut self) -> Vec<String> {
        std::mem::take(&mut self.events)
    }
    fn public(&self) -> &'static [&'static str] {
        &["LINKS", "STATUS", "DNS", "WIFI"]
    }
    fn help(&self) -> &'static [&'static str] {
        &[
            "LINKS - every interface, its method and its address",
            "STATUS [iface] - the network summary, or one interface",
            "UP iface | DOWN iface - bring an interface up or down",
            "RENEW [iface] - start the DHCP exchange again",
            "DNS [SHOW] | DNS SET server... - resolver configuration",
            "WIFI LIST [iface] - networks from the last scan",
            "WIFI SCAN [iface] - ask the driver to scan",
            "WIFI STATUS [iface] - association state",
            "WIFI CONNECT ssid [passphrase] [iface] - join a network",
            "WIFI FORGET ssid [iface] - remove a saved network",
            "WIFI DISCONNECT [iface] | WIFI RECONNECT [iface]",
            "SET \"interface eth0\" method dhcp - change netd.conf",
        ]
    }
    fn reload(&mut self) {
        self.settings = Settings::install("netd.conf", NETD_DEFAULT);
        self.allow_users = self
            .settings
            .boolean("network", "allow-users")
            .unwrap_or(true);
        for index in 0..self.links.len() {
            let name = self.links[index].interface.name.clone();
            let kind = self.links[index].interface.kind;
            let method = Method::read(&self.settings, &name, kind);
            if method != self.links[index].method {
                self.links[index].client = None;
                self.deconfigure(index);
                self.links[index].method = method;
            }
        }
        log("hos-netd", "reloaded netd.conf");
    }
    fn stop(&mut self) {
        for link in &mut self.links {
            if let Some(client) = link.client.as_mut() {
                client.release();
            }
        }
    }
}

/// Run the service.
pub fn main() -> io::Result<()> {
    log("hos-netd", "starting");
    let mut netd = Netd::new();
    ipc::serve("netd", 0o666, &mut netd)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn macs_and_prefixes_parse_both_ways() {
        assert_eq!(
            parse_mac("52:54:00:12:34:56"),
            Some([0x52, 0x54, 0, 0x12, 0x34, 0x56])
        );
        assert_eq!(parse_mac("52:54:00:12:34"), None);
        assert_eq!(parse_mac("52:54:00:12:34:56:78"), None);
        assert_eq!(netmask_of(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(netmask_of(0), Ipv4Addr::UNSPECIFIED);
        assert_eq!(netmask_of(32), Ipv4Addr::new(255, 255, 255, 255));
        assert_eq!(
            parse_cidr("192.168.1.5/24"),
            Some((Ipv4Addr::new(192, 168, 1, 5), 24))
        );
        assert_eq!(parse_cidr("192.168.1.5/33"), None);
        assert_eq!(parse_cidr("not an address"), None);
    }
    #[test]
    fn interfaces_are_read_out_of_sysfs() {
        let root = std::env::temp_dir().join(format!("hos-net-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (name, flags, wireless) in [("lo", "0x9", false), ("eth0", "0x1003", false), ("wlan0", "0x1003", true)] {
            let dir = root.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("flags"), format!("{flags}\n")).unwrap();
            fs::write(dir.join("address"), "52:54:00:12:34:56\n").unwrap();
            fs::write(dir.join("operstate"), "up\n").unwrap();
            fs::write(dir.join("carrier"), "1\n").unwrap();
            if wireless {
                fs::create_dir_all(dir.join("wireless")).unwrap();
            }
        }
        let interfaces = read_interfaces(&root);
        assert_eq!(interfaces.len(), 3);
        assert_eq!(interfaces[0].name, "eth0");
        assert_eq!(interfaces[0].kind, Kind::Wired);
        assert!(interfaces[0].carrier && interfaces[0].up);
        assert_eq!(interfaces[1].kind, Kind::Loopback, "the loopback flag wins");
        assert_eq!(interfaces[2].kind, Kind::Wireless);
        assert_eq!(interfaces[2].mac_text(), "52:54:00:12:34:56");
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn a_down_link_is_raised_unless_it_was_turned_off() {
        let interface = |name: &str, kind, up| Interface {
            name: name.into(),
            kind,
            mac: [0x52, 0x54, 0, 0x12, 0x34, 0x56],
            operstate: "down".into(),
            carrier: false,
            up,
        };
        // A wired link the kernel left down: until it is up it reports no
        // carrier, so nothing else would ever configure it.
        let mut link = Link::new(interface("eth0", Kind::Wired, false), Method::Dhcp);
        assert!(link.wants_up());
        link.interface.up = true;
        assert!(!link.wants_up(), "an interface that is up is left alone");
        // Turning it off in the settings must survive the next sweep.
        let mut link = Link::new(interface("eth0", Kind::Wired, false), Method::Dhcp);
        link.admin_down = true;
        assert!(!link.wants_up());
        assert!(!Link::new(interface("eth1", Kind::Wired, false), Method::Off).wants_up());
        // Loopback is brought up with the address the service gives it.
        assert!(!Link::new(interface("lo", Kind::Loopback, false), Method::Dhcp).wants_up());
        // A wireless link still needs to be up before it can scan.
        assert!(Link::new(interface("wlan0", Kind::Wireless, false), Method::Dhcp).wants_up());
    }
    #[test]
    fn configured_methods_come_from_the_settings_file() {
        let settings = Settings::parse(
            "[interface eth0]\nmethod = static\naddress = 192.168.1.5/24\ngateway = 192.168.1.1\ndns = 1.1.1.1, 9.9.9.9\n[interface eth1]\nmethod = off\n",
        );
        let Method::Static {
            address,
            netmask,
            gateway,
            dns,
        } = Method::read(&settings, "eth0", Kind::Wired)
        else {
            panic!("eth0 is configured statically");
        };
        assert_eq!(address, Ipv4Addr::new(192, 168, 1, 5));
        assert_eq!(netmask, Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(gateway, Some(Ipv4Addr::new(192, 168, 1, 1)));
        assert_eq!(dns.len(), 2);
        assert_eq!(Method::read(&settings, "eth1", Kind::Wired), Method::Off);
        assert_eq!(Method::read(&settings, "eth2", Kind::Wired), Method::Dhcp);
        assert_eq!(Method::read(&settings, "lo", Kind::Loopback), Method::Off);
    }
    #[test]
    fn the_default_route_is_read_from_proc() {
        // 00000000 destination, gateway 10.0.2.2 in little-endian hexadecimal.
        let text = "Iface\tDestination\tGateway\tFlags\n\
                    eth0\t0002000A\t00000000\t0001\n\
                    eth0\t00000000\t0202000A\t0003\n";
        assert_eq!(
            parse_proc_route(text),
            Some(("eth0".to_string(), Ipv4Addr::new(10, 0, 2, 2)))
        );
        assert_eq!(parse_proc_route("Iface\n"), None);
    }
    #[test]
    fn the_resolver_file_lists_servers_and_the_search_domain() {
        let text = resolv_conf(
            &[Ipv4Addr::new(10, 0, 2, 3), Ipv4Addr::new(1, 1, 1, 1)],
            Some("lan"),
        );
        assert!(text.starts_with("# Written by hos-netd"));
        assert!(text.contains("search lan\n"));
        assert!(text.contains("nameserver 10.0.2.3\nnameserver 1.1.1.1\n"));
        assert!(!resolv_conf(&[], None).contains("search"));
    }
}
