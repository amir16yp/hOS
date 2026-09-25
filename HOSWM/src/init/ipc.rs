//! The hOS service API: one line protocol shared by init and every service.
//!
//! A client connects to `/run/hos/NAME.sock` and sends one request per line:
//!
//! ```text
//! VERB arg arg
//! ```
//!
//! The service answers with zero or more data records and exactly one result:
//!
//! ```text
//! =iface=eth0 state=up address=10.0.2.15/24
//! +ok
//! ```
//!
//! | Prefix | Meaning |
//! | --- | --- |
//! | `=` | one record of `key=value` fields |
//! | `+` | the request succeeded; the rest of the line is a message |
//! | `-` | the request failed; the rest of the line is the reason |
//! | `*` | an event, sent only after `SUBSCRIBE`, never inside a response |
//!
//! Values escape the characters that would break the framing: a space is
//! `\s`, a backslash `\\`, a newline `\n` and a tab `\t`. A Wi-Fi network
//! named `Cafe Wifi` is therefore `ssid=Cafe\sWifi`.
//!
//! Requests arriving from a process that is not root are refused unless the
//! service lists the verb as public, which covers the read-only verbs the
//! desktop needs plus whatever a service's policy opens up.
use crate::reactor::Reactor;
use std::{
    collections::VecDeque,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Longest request line accepted from a client.
const MAX_REQUEST: usize = 8192;
/// Most responses and events buffered for one slow client before it is dropped.
const MAX_OUTPUT: usize = 256 * 1024;
const MAX_CLIENTS: usize = 32;

/// Escape one value so it survives the space-separated line format.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ' ' => out.push_str("\\s"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push('?'),
            c => out.push(c),
        }
    }
    if out.is_empty() { "\\e".into() } else { out }
}
/// Reverse [`escape`]. Unknown escapes keep the character that followed them.
pub fn unescape(value: &str) -> String {
    if value == "\\e" {
        return String::new();
    }
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// A record under construction: `key=value` fields on one line.
#[derive(Clone, Debug, Default)]
pub struct Fields(String);
impl Fields {
    pub fn new() -> Self {
        Self::default()
    }
    fn push(&mut self, key: &str, value: String) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(key);
        self.0.push('=');
        self.0.push_str(&value);
    }
    pub fn text(mut self, key: &str, value: &str) -> Self {
        self.push(key, escape(value));
        self
    }
    pub fn number(mut self, key: &str, value: impl std::fmt::Display) -> Self {
        self.push(key, escape(&value.to_string()));
        self
    }
    pub fn flag(mut self, key: &str, value: bool) -> Self {
        self.push(key, if value { "yes".into() } else { "no".into() });
        self
    }
    /// The finished record line, without its `=` prefix.
    pub fn line(self) -> String {
        self.0
    }
}

/// A request line split into its verb and arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// Upper-case verb, so `status` and `STATUS` are the same request.
    pub verb: String,
    pub args: Vec<String>,
}
impl Request {
    pub fn parse(line: &str) -> Self {
        let mut words = line.split_whitespace().map(unescape);
        Request {
            verb: words.next().unwrap_or_default().to_ascii_uppercase(),
            args: words.collect(),
        }
    }
    pub fn arg(&self, index: usize) -> Option<&str> {
        self.args.get(index).map(String::as_str)
    }
    /// An argument, or an error naming what the verb expects.
    pub fn need(&self, index: usize, what: &str) -> Result<&str, String> {
        self.arg(index)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{} needs {what}", self.verb))
    }
    /// The argument at `index` upper-cased, for sub-verbs such as `WIFI SCAN`.
    pub fn keyword(&self, index: usize) -> String {
        self.arg(index).unwrap_or_default().to_ascii_uppercase()
    }
}

/// What a service answers: any number of records and a closing message.
#[derive(Clone, Debug, Default)]
pub struct Response {
    pub records: Vec<String>,
    pub message: String,
}
impl Response {
    pub fn ok() -> Self {
        Response {
            records: Vec::new(),
            message: "ok".into(),
        }
    }
    pub fn message(text: impl Into<String>) -> Self {
        Response {
            records: Vec::new(),
            message: text.into(),
        }
    }
    pub fn record(mut self, fields: Fields) -> Self {
        self.records.push(fields.line());
        self
    }
    pub fn records(mut self, fields: impl IntoIterator<Item = Fields>) -> Self {
        self.records.extend(fields.into_iter().map(Fields::line));
        self
    }
}

/// The process on the other end of a connection.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    pub pid: i32,
    pub uid: u32,
}
impl Peer {
    pub fn root(&self) -> bool {
        self.uid == 0
    }
}

#[repr(C)]
struct Ucred {
    pid: i32,
    uid: u32,
    gid: u32,
}
fn peer_of(stream: &UnixStream) -> Peer {
    let mut cred = Ucred {
        pid: 0,
        uid: u32::MAX,
        gid: 0,
    };
    let mut length = std::mem::size_of::<Ucred>() as u32;
    // SAFETY: SO_PEERCRED writes a ucred into the live structure above.
    let rc = unsafe {
        crate::init::sys::getsockopt(
            stream.as_raw_fd(),
            1,
            17,
            &mut cred as *mut Ucred as *mut u8,
            &mut length,
        )
    };
    if rc < 0 || length as usize != std::mem::size_of::<Ucred>() {
        // An unidentified peer is treated as an unprivileged one.
        return Peer {
            pid: 0,
            uid: u32::MAX,
        };
    }
    Peer {
        pid: cred.pid,
        uid: cred.uid,
    }
}

/// A long-running service behind one socket.
///
/// Implementations answer requests, do periodic work in [`Service::tick`], and
/// publish state changes as events. The loop in [`serve`] owns the socket,
/// the poll and the signal handling.
pub trait Service {
    /// Handle one request. Built-in verbs never reach this.
    fn handle(&mut self, request: &Request, peer: &Peer) -> Result<Response, String>;
    /// Periodic work. The returned duration bounds the next sleep.
    fn tick(&mut self) -> Duration {
        Duration::from_secs(1)
    }
    /// Extra descriptors that should wake the loop when they become readable.
    fn sources(&mut self) -> Vec<RawFd> {
        Vec::new()
    }
    /// Events to deliver to subscribers, taken since the last call.
    fn events(&mut self) -> Vec<String> {
        Vec::new()
    }
    /// Verbs that a process which is not root may use.
    fn public(&self) -> &'static [&'static str] {
        &[]
    }
    /// One line per verb, answered by `HELP`.
    fn help(&self) -> &'static [&'static str] {
        &[]
    }
    /// Re-read configuration; called on `SIGHUP` and `RELOAD`.
    fn reload(&mut self) {}
    /// Release devices and children before the process exits.
    fn stop(&mut self) {}
}

struct Connection {
    stream: UnixStream,
    peer: Peer,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    subscribed: bool,
    closing: bool,
}
impl Connection {
    fn send(&mut self, line: &str) {
        if self.output.len() + line.len() + 1 > MAX_OUTPUT {
            // A client that never reads is disconnected instead of growing
            // the service's memory without a bound.
            self.closing = true;
            return;
        }
        self.output.extend_from_slice(line.as_bytes());
        self.output.push(b'\n');
    }
    fn flush(&mut self) -> bool {
        while self.written < self.output.len() {
            match self.stream.write(&self.output[self.written..]) {
                Ok(0) => return false,
                Ok(n) => self.written += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        if self.written == self.output.len() {
            self.output.clear();
            self.written = 0;
            if self.closing {
                return false;
            }
        }
        true
    }
}

/// The listening socket and its connected clients.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    clients: Vec<Connection>,
}
impl Server {
    /// Bind `/run/hos/NAME.sock`, replacing a socket no one is listening on.
    ///
    /// `mode` is the socket's permission bits: `0o666` lets the desktop user
    /// reach the service, which then applies its own per-verb policy.
    pub fn bind(service: &str, mode: u32) -> io::Result<Self> {
        let path = crate::init::socket_path(service);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o755))?;
        }
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("{service} is already running"),
                    ));
                }
                // Nothing is listening: the socket outlived its service.
                Err(_) => fs::remove_file(&path)?,
            }
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        Ok(Server {
            listener,
            path,
            clients: Vec::new(),
        })
    }
    /// Register the listener and every client with the poll reactor.
    pub fn watch(&self, reactor: &mut Reactor) {
        reactor.watch(self.listener.as_raw_fd(), true, false);
        for client in &self.clients {
            reactor.watch(
                client.stream.as_raw_fd(),
                !client.closing,
                client.written < client.output.len(),
            );
        }
    }
    /// Accept new clients, answer pending requests and flush replies.
    pub fn poll(&mut self, service: &mut impl Service) {
        while self.clients.len() < MAX_CLIENTS {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if stream.set_nonblocking(true).is_ok() {
                        let peer = peer_of(&stream);
                        self.clients.push(Connection {
                            stream,
                            peer,
                            input: Vec::new(),
                            output: Vec::new(),
                            written: 0,
                            subscribed: false,
                            closing: false,
                        });
                    }
                }
                Err(_) => break,
            }
        }
        self.clients.retain_mut(|client| {
            let mut buffer = [0u8; 4096];
            let mut closed = false;
            loop {
                match client.stream.read(&mut buffer) {
                    Ok(0) => {
                        closed = true;
                        break;
                    }
                    Ok(n) => client.input.extend_from_slice(&buffer[..n]),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return false,
                }
            }
            while let Some(end) = client.input.iter().position(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(&client.input[..end]).into_owned();
                client.input.drain(..=end);
                let request = Request::parse(line.trim());
                if request.verb.is_empty() {
                    continue;
                }
                let peer = client.peer;
                match answer(service, &request, &peer, &mut client.subscribed) {
                    Ok(response) => {
                        for record in &response.records {
                            client.send(&format!("={record}"));
                        }
                        client.send(&format!("+{}", response.message));
                    }
                    Err(message) => client.send(&format!("-{message}")),
                }
                if request.verb == "QUIT" {
                    client.closing = true;
                    break;
                }
            }
            if client.input.len() > MAX_REQUEST {
                return false;
            }
            if !client.flush() {
                return false;
            }
            !(closed && client.written == client.output.len())
        });
    }
    /// Deliver events to every subscribed client.
    pub fn broadcast(&mut self, events: &[String]) {
        if events.is_empty() {
            return;
        }
        self.clients.retain_mut(|client| {
            if client.subscribed {
                for event in events {
                    client.send(&format!("*{event}"));
                }
            }
            client.flush()
        });
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Handle the verbs every service shares, then defer to the service itself.
fn answer(
    service: &mut impl Service,
    request: &Request,
    peer: &Peer,
    subscribed: &mut bool,
) -> Result<Response, String> {
    match request.verb.as_str() {
        "PING" => return Ok(Response::message("pong")),
        "QUIT" => return Ok(Response::message("bye")),
        "SUBSCRIBE" => {
            *subscribed = true;
            return Ok(Response::message("subscribed"));
        }
        "UNSUBSCRIBE" => {
            *subscribed = false;
            return Ok(Response::message("unsubscribed"));
        }
        "HELP" => {
            let mut response = Response::ok();
            for line in service.help() {
                response.records.push(format!("verb={}", escape(line)));
            }
            return Ok(response);
        }
        _ => (),
    }
    if !peer.root() && !service.public().contains(&request.verb.as_str()) {
        return Err(format!("{} requires root", request.verb));
    }
    if request.verb == "RELOAD" {
        service.reload();
        return Ok(Response::message("reloaded"));
    }
    service.handle(request, peer)
}

/// Run a service until it is asked to stop.
///
/// The loop polls the socket, the service's own descriptors and the signal
/// pipe, so an idle service uses no processor time between events.
pub fn serve(name: &str, mode: u32, service: &mut impl Service) -> io::Result<()> {
    let mut server = Server::bind(name, mode)?;
    let mut signals = crate::init::sys::Signals::catch(&[
        crate::init::sys::SIGTERM,
        crate::init::sys::SIGINT,
        crate::init::sys::SIGHUP,
    ])?;
    let mut reactor = Reactor::default();
    loop {
        let timeout = service.tick();
        let events = service.events();
        server.broadcast(&events);
        reactor.clear();
        server.watch(&mut reactor);
        reactor.watch(signals.as_raw_fd(), true, false);
        for fd in service.sources() {
            reactor.watch(fd, true, false);
        }
        reactor.wait(timeout.min(Duration::from_secs(60)))?;
        for signal in signals.take() {
            match signal {
                crate::init::sys::SIGHUP => service.reload(),
                _ => {
                    service.stop();
                    crate::init::log(name, "stopping");
                    return Ok(());
                }
            }
        }
        server.poll(service);
        let events = service.events();
        server.broadcast(&events);
    }
}

/// Handle a `SET section key value` request against a list of settings the
/// service owns, writing the change to its configuration file.
///
/// A pattern ending in `*`, such as `interface *`, matches any section with
/// that prefix, which is how per-interface settings are allowed. The caller
/// reloads afterwards, so the change takes effect immediately and survives a
/// restart; this is what the settings application uses.
pub fn setting(
    file: &str,
    allowed: &[(&str, &str)],
    request: &Request,
) -> Result<Response, String> {
    let section = request.need(0, "a section, such as power")?.to_string();
    let key = request.need(1, "a key")?.to_ascii_lowercase();
    let value = request
        .args
        .get(2..)
        .map(|rest| rest.join(" "))
        .unwrap_or_default();
    let permitted = allowed.iter().any(|(pattern, name)| {
        *name == key
            && (*pattern == section
                || pattern.strip_suffix('*').is_some_and(|prefix| {
                    section.starts_with(prefix) && section.len() > prefix.len()
                }))
    });
    if !permitted {
        return Err(format!(
            "{section} {key} is not a setting this service owns"
        ));
    }
    crate::init::store_setting(file, &section, &key, &value).map_err(|e| {
        format!(
            "{}: {e}",
            crate::init::config_path(file).display()
        )
    })?;
    Ok(Response::message(format!("{section} {key} = {value}")))
}

/// One record from a reply: the `key=value` fields of a `=` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record(pub Vec<(String, String)>);
impl Record {
    pub fn parse(line: &str) -> Self {
        Record(
            line.split_whitespace()
                .map(|field| match field.split_once('=') {
                    Some((key, value)) => (key.to_string(), unescape(value)),
                    None => (unescape(field), String::new()),
                })
                .collect(),
        )
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
    pub fn number<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.get(key)?.parse().ok()
    }
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some("yes" | "true" | "1"))
    }
}

/// A completed request: its records and the closing message.
#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub records: Vec<Record>,
    pub message: String,
}
impl Reply {
    pub fn first(&self) -> Record {
        self.records.first().cloned().unwrap_or_default()
    }
}

/// A connection to one service, used by `hosctl`, the desktop and by
/// services that depend on each other, such as `hos-ntpd` watching the network.
pub struct Client {
    stream: BufReader<UnixStream>,
    events: VecDeque<String>,
}
impl Client {
    pub fn connect(service: &str) -> io::Result<Self> {
        let path = crate::init::socket_path(service);
        Self::connect_path(&path)
    }
    pub fn connect_path(path: &Path) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Client {
            stream: BufReader::new(stream),
            events: VecDeque::new(),
        })
    }
    /// Send one request and read its reply. Events that arrive first are kept.
    pub fn call(&mut self, request: &str) -> io::Result<Reply> {
        let line = format!("{}\n", request.trim());
        self.stream.get_mut().write_all(line.as_bytes())?;
        let mut reply = Reply::default();
        loop {
            let line = self.line()?;
            match line.chars().next() {
                Some('=') => reply.records.push(Record::parse(&line[1..])),
                Some('+') => {
                    reply.message = line[1..].to_string();
                    return Ok(reply);
                }
                Some('-') => {
                    return Err(io::Error::other(line[1..].to_string()));
                }
                Some('*') => {
                    if self.events.len() < 256 {
                        self.events.push_back(line[1..].to_string());
                    }
                }
                _ => return Err(io::Error::other(format!("bad service reply: {line}"))),
            }
        }
    }
    fn line(&mut self) -> io::Result<String> {
        let mut line = String::new();
        if self.stream.read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "service closed the connection",
            ));
        }
        Ok(line.trim_end().to_string())
    }
    /// Ask for events; they arrive through [`Client::event`].
    pub fn subscribe(&mut self) -> io::Result<()> {
        self.call("SUBSCRIBE").map(|_| ())
    }
    /// The next event, or `None` if none arrived before the timeout.
    pub fn event(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        self.stream
            .get_ref()
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let deadline = Instant::now() + timeout;
        loop {
            match self.line() {
                Ok(line) if line.starts_with('*') => return Ok(Some(line[1..].to_string())),
                // Replies to a request whose caller stopped reading are dropped.
                Ok(_) => (),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
        }
    }
    /// The raw descriptor, so a service can wait on another service's events.
    pub fn as_raw_fd(&self) -> RawFd {
        self.stream.get_ref().as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaping_round_trips_spaces_and_backslashes() {
        for value in ["Cafe Wifi", "back\\slash", "line\nbreak", "", "plain"] {
            let escaped = escape(value);
            assert!(!escaped.contains(' ') && !escaped.contains('\n'));
            assert_eq!(unescape(&escaped), value);
        }
        assert_eq!(escape("bell\u{7}"), "bell?");
    }
    #[test]
    fn requests_split_into_a_verb_and_unescaped_arguments() {
        let request = Request::parse("wifi connect Cafe\\sWifi secret");
        assert_eq!(request.verb, "WIFI");
        assert_eq!(request.keyword(0), "CONNECT");
        assert_eq!(request.arg(1), Some("Cafe Wifi"));
        assert_eq!(request.need(3, "a password").unwrap_err(), "WIFI needs a password");
    }
    #[test]
    fn records_carry_typed_fields_through_the_line_format() {
        let line = Fields::new()
            .text("ssid", "Cafe Wifi")
            .number("signal", -52)
            .flag("saved", true)
            .line();
        assert_eq!(line, "ssid=Cafe\\sWifi signal=-52 saved=yes");
        let record = Record::parse(&line);
        assert_eq!(record.get("ssid"), Some("Cafe Wifi"));
        assert_eq!(record.number::<i32>("signal"), Some(-52));
        assert!(record.flag("saved"));
        assert!(!record.flag("missing"));
    }

    /// A service that echoes its request, to exercise the loop end to end.
    struct Echo {
        events: Vec<String>,
    }
    impl Service for Echo {
        fn handle(&mut self, request: &Request, peer: &Peer) -> Result<Response, String> {
            match request.verb.as_str() {
                "ECHO" => Ok(Response::ok().record(
                    Fields::new()
                        .text("text", &request.args.join(" "))
                        .number("uid", peer.uid),
                )),
                "EVENT" => {
                    self.events.push("test happened".into());
                    Ok(Response::message("queued"))
                }
                "FAIL" => Err("nothing to do".into()),
                other => Err(format!("unknown verb {other}")),
            }
        }
        fn events(&mut self) -> Vec<String> {
            std::mem::take(&mut self.events)
        }
        fn public(&self) -> &'static [&'static str] {
            &["ECHO", "EVENT", "FAIL"]
        }
    }
    #[test]
    fn server_answers_records_errors_and_events() {
        let dir = std::env::temp_dir().join(format!("hos-ipc-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // SAFETY: the test is single threaded and only redirects its own paths.
        unsafe { std::env::set_var("HOS_RUN_DIR", &dir) };
        let mut service = Echo { events: Vec::new() };
        let mut server = Server::bind("echo", 0o600).unwrap();
        let mut client = Client::connect("echo").unwrap();
        client.stream.get_mut().set_nonblocking(false).unwrap();
        let pump = |server: &mut Server, service: &mut Echo| {
            for _ in 0..50 {
                server.poll(service);
                let events = service.events();
                server.broadcast(&events);
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let request = std::thread::spawn(move || {
            let reply = client.call("ECHO hello there").unwrap();
            assert_eq!(reply.first().get("text"), Some("hello there"));
            assert_eq!(reply.message, "ok");
            assert_eq!(client.call("PING").unwrap().message, "pong");
            assert_eq!(
                client.call("FAIL").unwrap_err().to_string(),
                "nothing to do"
            );
            // A verb the service does not publish needs root.
            let refused = client.call("SECRET").unwrap_err().to_string();
            // SAFETY: geteuid only reads this process's effective user.
            if unsafe { crate::init::sys::geteuid() } == 0 {
                assert_eq!(refused, "unknown verb SECRET");
            } else {
                assert_eq!(refused, "SECRET requires root");
            }
            client.subscribe().unwrap();
            client.call("EVENT").unwrap();
            assert_eq!(
                client.event(Duration::from_secs(3)).unwrap().as_deref(),
                Some("test happened")
            );
        });
        while !request.is_finished() {
            pump(&mut server, &mut service);
        }
        request.join().unwrap();
        drop(server);
        fs::remove_dir_all(&dir).unwrap();
    }
}
