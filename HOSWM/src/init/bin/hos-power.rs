//! `hos-power`: the hOS power service. See `hoswm::init::power`.
fn main() {
    if let Err(e) = hoswm::init::power::main() {
        eprintln!("hos-power: {e}");
        std::process::exit(1);
    }
}
