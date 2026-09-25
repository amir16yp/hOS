//! Wi-Fi through `wpa_supplicant`'s control socket.
//!
//! Association and the WPA handshake are not reimplemented here: `hos-netd`
//! starts one `wpa_supplicant` per wireless interface and drives it over its
//! documented control protocol, a Unix datagram socket that takes text
//! commands. Once a network is selected, the DHCP client in
//! [`crate::init::dhcp`] configures the interface exactly as it does a wired one.
//!
//! When `wpa_supplicant` is not installed, every Wi-Fi verb answers with that
//! fact instead of failing in a way that looks like broken hardware.
use crate::init::{config_path, log, run_dir};
use std::{
    fs,
    io,
    os::unix::net::UnixDatagram,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

/// Where `wpa_supplicant` places its per-interface control sockets.
const CONTROL_DIR: &str = "/run/wpa_supplicant";
/// The configuration file hOS generates for it.
const CONFIG: &str = "wpa_supplicant.conf";

/// One network seen in a scan, or saved in the configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    pub ssid: String,
    pub bssid: String,
    /// Signal level in dBm, as the driver reports it.
    pub signal: i32,
    pub frequency: u32,
    /// `open`, `wep`, `wpa`, `wpa2`, `wpa3` or `wpa2-enterprise`.
    pub security: String,
    pub saved: bool,
}

/// The association state of one interface.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// `wpa_state`: `SCANNING`, `ASSOCIATING`, `COMPLETED` and so on.
    pub state: String,
    pub ssid: String,
    pub bssid: String,
    pub frequency: u32,
    pub security: String,
}
impl Status {
    pub fn connected(&self) -> bool {
        self.state == "COMPLETED"
    }
}

/// Turn the flag field of a scan result into one security name.
pub fn security(flags: &str) -> String {
    let flags = flags.to_ascii_uppercase();
    if flags.contains("WPA2-EAP") || flags.contains("WPA-EAP") {
        "wpa2-enterprise".into()
    } else if flags.contains("SAE") {
        "wpa3".into()
    } else if flags.contains("WPA2") || flags.contains("RSN") {
        "wpa2".into()
    } else if flags.contains("WPA") {
        "wpa".into()
    } else if flags.contains("WEP") {
        "wep".into()
    } else {
        "open".into()
    }
}

/// Parse `SCAN_RESULTS` output: a header line, then one tab-separated row per
/// network. Rows without an SSID are hidden networks and are left out.
pub fn parse_scan(text: &str) -> Vec<Network> {
    let mut networks: Vec<Network> = Vec::new();
    for line in text.lines().skip(1) {
        let mut columns = line.split('\t');
        let (Some(bssid), Some(frequency), Some(level), Some(flags)) = (
            columns.next(),
            columns.next(),
            columns.next(),
            columns.next(),
        ) else {
            continue;
        };
        let ssid = columns.next().unwrap_or("").trim().to_string();
        if ssid.is_empty() {
            continue;
        }
        let network = Network {
            ssid,
            bssid: bssid.trim().to_string(),
            signal: level.trim().parse().unwrap_or(-100),
            frequency: frequency.trim().parse().unwrap_or(0),
            security: security(flags),
            saved: false,
        };
        // A network on several access points is listed once, at its best signal.
        match networks.iter_mut().find(|n| n.ssid == network.ssid) {
            Some(existing) if existing.signal < network.signal => *existing = network,
            Some(_) => (),
            None => networks.push(network),
        }
    }
    networks.sort_by_key(|network| -network.signal);
    networks
}

/// Parse `LIST_NETWORKS` output into the configured network IDs and names.
pub fn parse_networks(text: &str) -> Vec<(i32, String)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let id = columns.next()?.trim().parse().ok()?;
            Some((id, columns.next()?.trim().to_string()))
        })
        .collect()
}

/// Parse `STATUS` output, which is one `key=value` per line.
pub fn parse_status(text: &str) -> Status {
    let mut status = Status::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "wpa_state" => status.state = value.to_string(),
            "ssid" => status.ssid = value.to_string(),
            "bssid" => status.bssid = value.to_string(),
            "freq" => status.frequency = value.parse().unwrap_or(0),
            "key_mgmt" => status.security = security(value),
            _ => (),
        }
    }
    status
}

/// The `wpa_supplicant` program, if the system has one.
pub fn program() -> Option<PathBuf> {
    ["/sbin/wpa_supplicant", "/usr/sbin/wpa_supplicant", "/bin/wpa_supplicant"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

/// A control connection to `wpa_supplicant` for one wireless interface.
pub struct Supplicant {
    pub interface: String,
    socket: UnixDatagram,
    local: PathBuf,
    /// The supplicant this service started, if it was not already running.
    child: Option<Child>,
}
impl Supplicant {
    /// Connect to the supplicant for `interface`, starting one if needed.
    pub fn open(interface: &str) -> io::Result<Self> {
        let program = program().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "wpa_supplicant is not installed; Wi-Fi is unavailable",
            )
        })?;
        let control = Path::new(CONTROL_DIR).join(interface);
        let mut child = None;
        if !control.exists() {
            child = Some(spawn(&program, interface)?);
            // The supplicant creates its socket a moment after it starts.
            for _ in 0..50 {
                if control.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if !control.exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("wpa_supplicant did not open {}", control.display()),
            ));
        }
        let local = run_dir().join(format!("wpa-{interface}-{}.sock", std::process::id()));
        let _ = fs::remove_file(&local);
        let socket = UnixDatagram::bind(&local)?;
        socket.connect(&control)?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        socket.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut supplicant = Supplicant {
            interface: interface.to_string(),
            socket,
            local,
            child,
        };
        supplicant.command("PING")?;
        Ok(supplicant)
    }
    /// Send one command and read its reply.
    pub fn command(&mut self, text: &str) -> io::Result<String> {
        self.socket.send(text.as_bytes())?;
        let mut buffer = [0u8; 8192];
        // Event lines may arrive between a command and its reply.
        for _ in 0..16 {
            let length = self.socket.recv(&mut buffer)?;
            let reply = String::from_utf8_lossy(&buffer[..length]).into_owned();
            if reply.starts_with('<') {
                continue;
            }
            if reply.starts_with("FAIL") {
                return Err(io::Error::other(format!("{text}: wpa_supplicant refused it")));
            }
            return Ok(reply);
        }
        Err(io::Error::other(format!("{text}: no reply")))
    }
    /// Ask the driver to scan. Results appear a few seconds later.
    pub fn scan(&mut self) -> io::Result<()> {
        match self.command("SCAN") {
            Ok(_) => Ok(()),
            // A scan already in progress is not a failure.
            Err(_) => Ok(()),
        }
    }
    /// The networks from the most recent scan, with saved ones marked.
    pub fn networks(&mut self) -> io::Result<Vec<Network>> {
        let scan = self.command("SCAN_RESULTS")?;
        let saved = parse_networks(&self.command("LIST_NETWORKS")?);
        let mut networks = parse_scan(&scan);
        for network in &mut networks {
            network.saved = saved.iter().any(|(_, ssid)| *ssid == network.ssid);
        }
        Ok(networks)
    }
    pub fn status(&mut self) -> io::Result<Status> {
        Ok(parse_status(&self.command("STATUS")?))
    }
    /// Select a network, adding it to the configuration if it is new.
    ///
    /// `psk` is the passphrase; an open network takes `None`.
    pub fn connect(&mut self, ssid: &str, psk: Option<&str>) -> io::Result<()> {
        check(ssid)?;
        if let Some(psk) = psk {
            check(psk)?;
            if !(8..=63).contains(&psk.chars().count()) {
                return Err(io::Error::other(
                    "a WPA passphrase is between 8 and 63 characters",
                ));
            }
        }
        let existing = parse_networks(&self.command("LIST_NETWORKS")?)
            .into_iter()
            .find(|(_, name)| name == ssid)
            .map(|(id, _)| id);
        let id = match existing {
            Some(id) => id,
            None => self.command("ADD_NETWORK")?.trim().parse::<i32>().map_err(|_| {
                io::Error::other("wpa_supplicant did not return a network ID")
            })?,
        };
        let result = (|| -> io::Result<()> {
            self.command(&format!("SET_NETWORK {id} ssid \"{ssid}\""))?;
            match psk {
                Some(psk) => {
                    self.command(&format!("SET_NETWORK {id} psk \"{psk}\""))?;
                }
                None => {
                    self.command(&format!("SET_NETWORK {id} key_mgmt NONE"))?;
                }
            }
            self.command(&format!("ENABLE_NETWORK {id}"))?;
            self.command(&format!("SELECT_NETWORK {id}"))?;
            Ok(())
        })();
        if result.is_err() && existing.is_none() {
            // Do not leave a half-configured network behind.
            let _ = self.command(&format!("REMOVE_NETWORK {id}"));
            return result;
        }
        result?;
        // Keep the network for the next boot; an unwritable file is not fatal.
        let _ = self.command("SAVE_CONFIG");
        Ok(())
    }
    /// Remove a saved network.
    pub fn forget(&mut self, ssid: &str) -> io::Result<()> {
        check(ssid)?;
        let id = parse_networks(&self.command("LIST_NETWORKS")?)
            .into_iter()
            .find(|(_, name)| name == ssid)
            .map(|(id, _)| id)
            .ok_or_else(|| io::Error::other(format!("{ssid} is not a saved network")))?;
        self.command(&format!("REMOVE_NETWORK {id}"))?;
        let _ = self.command("SAVE_CONFIG");
        Ok(())
    }
    pub fn disconnect(&mut self) -> io::Result<()> {
        self.command("DISCONNECT").map(|_| ())
    }
    pub fn reconnect(&mut self) -> io::Result<()> {
        self.command("RECONNECT").map(|_| ())
    }
    /// Whether the supplicant this service started is still alive.
    pub fn alive(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            // A supplicant someone else started is checked with a ping.
            None => self.command("PING").is_ok(),
        }
    }
}
impl Drop for Supplicant {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_file(&self.local);
    }
}

/// Reject values that would break out of a quoted control command.
fn check(value: &str) -> io::Result<()> {
    if value.is_empty() || value.contains(['"', '\n', '\r', '\0']) {
        return Err(io::Error::other(
            "a network name or passphrase cannot contain quotes or line breaks",
        ));
    }
    Ok(())
}

fn spawn(program: &Path, interface: &str) -> io::Result<Child> {
    let config = config_path(CONFIG);
    if !config.exists() {
        if let Some(parent) = config.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &config,
            format!("ctrl_interface={CONTROL_DIR}\nupdate_config=1\n"),
        )?;
    }
    fs::create_dir_all(CONTROL_DIR)?;
    log(
        "hos-netd",
        &format!("starting wpa_supplicant on {interface}"),
    );
    Command::new(program)
        .args([
            "-i",
            interface,
            "-c",
            &config.to_string_lossy(),
            "-O",
            CONTROL_DIR,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCAN: &str = "bssid / frequency / signal level / flags / ssid\n\
        02:00:00:00:01:00\t2412\t-42\t[WPA2-PSK-CCMP][ESS]\tCafe Wifi\n\
        02:00:00:00:02:00\t5180\t-70\t[WPA2-PSK-CCMP][ESS]\tCafe Wifi\n\
        02:00:00:00:03:00\t2437\t-55\t[ESS]\tAirport Free\n\
        02:00:00:00:04:00\t2462\t-80\t[WPA2-EAP-CCMP][ESS]\tCampus\n\
        02:00:00:00:05:00\t2462\t-30\t[WPA2-PSK-CCMP][ESS]\t\n";

    #[test]
    fn scan_results_are_merged_per_network_and_sorted_by_signal() {
        let networks = parse_scan(SCAN);
        assert_eq!(networks.len(), 3, "hidden networks are left out");
        assert_eq!(networks[0].ssid, "Cafe Wifi");
        assert_eq!(networks[0].signal, -42, "the stronger access point wins");
        assert_eq!(networks[0].frequency, 2412);
        assert_eq!(networks[0].security, "wpa2");
        assert_eq!(networks[1].ssid, "Airport Free");
        assert_eq!(networks[1].security, "open");
        assert_eq!(networks[2].security, "wpa2-enterprise");
    }
    #[test]
    fn security_names_cover_the_flag_spellings() {
        assert_eq!(security("[WPA2-PSK-CCMP][ESS]"), "wpa2");
        assert_eq!(security("[RSN-SAE-CCMP][ESS]"), "wpa3");
        assert_eq!(security("[WPA-PSK-TKIP]"), "wpa");
        assert_eq!(security("[WEP][ESS]"), "wep");
        assert_eq!(security("[ESS]"), "open");
    }
    #[test]
    fn saved_networks_and_status_parse_into_their_fields() {
        let saved = parse_networks(
            "network id / ssid / bssid / flags\n0\tCafe Wifi\tany\t[CURRENT]\n1\tHome\tany\t\n",
        );
        assert_eq!(saved, [(0, "Cafe Wifi".to_string()), (1, "Home".to_string())]);
        let status = parse_status(
            "bssid=02:00:00:00:01:00\nfreq=2412\nssid=Cafe Wifi\nwpa_state=COMPLETED\nkey_mgmt=WPA2-PSK\n",
        );
        assert!(status.connected());
        assert_eq!(status.ssid, "Cafe Wifi");
        assert_eq!(status.frequency, 2412);
        assert_eq!(status.security, "wpa2");
        assert!(!parse_status("wpa_state=SCANNING\n").connected());
    }
    #[test]
    fn names_that_would_break_the_control_protocol_are_refused() {
        assert!(check("Cafe Wifi").is_ok());
        assert!(check("say \"hello\"").is_err());
        assert!(check("line\nbreak").is_err());
        assert!(check("").is_err());
    }
}
