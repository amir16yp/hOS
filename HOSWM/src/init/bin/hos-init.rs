//! `hos-init`: PID 1. Mount the installed system, supervise the services and
//! the desktop session, answer the control socket and shut the machine down.
//!
//! The same executable is `reboot`, `poweroff` and `halt`: when it runs under
//! one of those names it sends the matching request to PID 1 and exits.
use hoswm::init::{ipc, supervisor::Shutdown, sys};
use std::{path::Path, process::Command};

/// Ask the running init to bring the system down, over its socket if possible.
fn request(how: Shutdown) -> i32 {
    if let Ok(mut client) = ipc::Client::connect("init") {
        match client.call(&how.name().to_ascii_uppercase()) {
            Ok(_) => return 0,
            Err(e) => eprintln!("{}: {e}", how.name()),
        }
    }
    // SAFETY: signalling PID 1, which treats these as shutdown requests.
    if unsafe { sys::kill(1, how.signal()) } != 0 {
        eprintln!("{}: {}", how.name(), std::io::Error::last_os_error());
        return 1;
    }
    0
}

fn main() {
    let name = std::env::args().next().unwrap_or_default();
    let how = match name.rsplit('/').next().unwrap_or("") {
        "reboot" => Some(Shutdown::Reboot),
        "poweroff" => Some(Shutdown::PowerOff),
        "halt" => Some(Shutdown::Halt),
        _ => None,
    };
    if let Some(how) = how {
        std::process::exit(request(how));
    }
    if std::process::id() != 1 {
        eprintln!("hos-init must run as PID 1");
        std::process::exit(1);
    }
    // The live image mounts everything in its own startup script.
    let live = Path::new("/etc/hos-live").exists();
    if !live {
        let _ = Command::new("/bin/bash").arg("/etc/init.d/rcS").status();
    }
    hoswm::init::supervisor::run(live);
}
