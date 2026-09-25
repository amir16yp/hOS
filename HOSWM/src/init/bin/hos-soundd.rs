//! `hos-soundd`: the hOS sound service. See `hoswm::init::sound`.
fn main() {
    if let Err(e) = hoswm::init::sound::main() {
        eprintln!("hos-soundd: {e}");
        std::process::exit(1);
    }
}
