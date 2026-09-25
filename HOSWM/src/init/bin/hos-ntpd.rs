//! `hos-ntpd`: the hOS time service. See `hoswm::init::ntp`.
fn main() {
    if let Err(e) = hoswm::init::ntp::main() {
        eprintln!("hos-ntpd: {e}");
        std::process::exit(1);
    }
}
