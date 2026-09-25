//! `hos-netd`: the hOS network service. See `hoswm::init::net`.
fn main() {
    if let Err(e) = hoswm::init::net::main() {
        eprintln!("hos-netd: {e}");
        std::process::exit(1);
    }
}
