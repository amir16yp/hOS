//! `hosctl`: talk to `hos-init` and the system services from a shell.
//!
//! ```text
//! hosctl                        # what every service is doing
//! hosctl net status             # one service's own verbs
//! hosctl net wifi connect "Cafe Wifi" passphrase
//! hosctl sound volume +5
//! hosctl power suspend
//! hosctl init restart hos-netd
//! hosctl watch net              # follow events until interrupted
//! ```
use hoswm::init::ipc;
use std::time::Duration;

const USAGE: &str = "\
Usage: hosctl [service] [verb] [argument...]
       hosctl watch SERVICE
       hosctl SERVICE help

Services: init, net (hos-netd), power, time (hos-ntpd), sound (hos-soundd)

With no arguments, every service is asked for its status.";

/// Map the short names people type to the socket names.
fn service(name: &str) -> Option<&'static str> {
    match name.trim_start_matches("hos-") {
        "init" => Some("init"),
        "net" | "netd" | "network" => Some("netd"),
        "power" | "powerd" | "battery" => Some("power"),
        "time" | "ntp" | "ntpd" => Some("ntpd"),
        "sound" | "soundd" | "audio" => Some("soundd"),
        _ => None,
    }
}

/// Print one reply: its records, then its message when it says something new.
fn show(reply: &ipc::Reply) {
    for record in &reply.records {
        let line: Vec<String> = record
            .0
            .iter()
            .map(|(key, value)| {
                if value.is_empty() {
                    key.clone()
                } else {
                    format!("{key}={value}")
                }
            })
            .collect();
        println!("{}", line.join("  "));
    }
    if !reply.message.is_empty() && reply.message != "ok" {
        println!("{}", reply.message);
    }
}

fn call(name: &str, request: &str) -> Result<(), String> {
    let socket = service(name).ok_or_else(|| format!("{name}: unknown service"))?;
    let mut client = ipc::Client::connect(socket)
        .map_err(|e| format!("{socket}: {e}; is hos-{socket} running?"))?;
    let reply = client
        .call(request)
        .map_err(|e| format!("{socket}: {e}"))?;
    show(&reply);
    Ok(())
}

/// Follow one service's events until the process is interrupted.
fn watch(name: &str) -> Result<(), String> {
    let socket = service(name).ok_or_else(|| format!("{name}: unknown service"))?;
    let mut client =
        ipc::Client::connect(socket).map_err(|e| format!("{socket}: {e}"))?;
    client
        .subscribe()
        .map_err(|e| format!("{socket}: {e}"))?;
    eprintln!("Watching {socket}. Press Ctrl+C to stop.");
    loop {
        match client.event(Duration::from_secs(3600)) {
            Ok(Some(event)) => {
                let record = ipc::Record::parse(&event);
                let line: Vec<String> = record
                    .0
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                println!("{}", line.join("  "));
            }
            Ok(None) => (),
            Err(e) => return Err(format!("{socket}: {e}")),
        }
    }
}

/// The default view: one status block per service.
fn overview() {
    for (name, socket) in [
        ("init", "init"),
        ("network", "netd"),
        ("power", "power"),
        ("time", "ntpd"),
        ("sound", "soundd"),
    ] {
        println!("[{name}]");
        match ipc::Client::connect(socket).and_then(|mut client| client.call("STATUS")) {
            Ok(reply) => show(&reply),
            Err(e) => println!("unavailable: {e}"),
        }
        println!();
    }
}

fn run() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        None => {
            overview();
            Ok(())
        }
        Some("-h" | "--help" | "help") if arguments.len() == 1 => {
            println!("{USAGE}");
            Ok(())
        }
        Some("watch") => watch(arguments.get(1).ok_or("watch needs a service name")?),
        Some(name) => {
            let request = arguments[1..]
                .iter()
                .map(|argument| ipc::escape(argument))
                .collect::<Vec<_>>()
                .join(" ");
            let request = if request.is_empty() {
                "STATUS".to_string()
            } else {
                request
            };
            call(name, &request)
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("hosctl: {e}");
        std::process::exit(1);
    }
}
