//! `hos-toast`: post a notification to the running HOSWM session.
//!
//! The session shows it in the configured corner and appends it to
//! `~/.hoswm/toastdb` once it leaves the screen, where `hos-notifications`
//! can read it back.
use hoswm::client::Client;

const USAGE: &str = "\
Usage: hos-toast [options] TEXT...

Options:
  -c, --color COLOR   0xAARRGGBB, #RRGGBB, or a name (default: accent)
  -m, --ms MS         how long to show it, 500-60000 (default: session setting)
  -l, --list          list the color names
  -h, --help          show this help

Colors: accent, green, red, yellow, blue, white, grey";

/// Theme colors, so scripts do not have to carry hexadecimal constants.
const NAMES: [(&str, u32); 7] = [
    ("accent", 0xff72dbac),
    ("green", 0xff72dbac),
    ("red", 0xffef6976),
    ("yellow", 0xffe4c878),
    ("blue", 0xff80afff),
    ("white", 0xffeeeeee),
    ("grey", 0xff9aa69f),
];

fn color(value: &str) -> Option<u32> {
    NAMES
        .iter()
        .find(|(name, _)| *name == value.trim().to_ascii_lowercase())
        .map(|(_, color)| *color)
        .or_else(|| hoswm::config::color(value))
}

fn run() -> Result<(), String> {
    let mut color_value = NAMES[0].1;
    let mut milliseconds = 0u32;
    let mut words: Vec<String> = Vec::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| {
            arguments
                .next()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-l" | "--list" => {
                for (name, color) in NAMES {
                    println!("{name:8} 0x{color:08x}");
                }
                return Ok(());
            }
            "-c" | "--color" => {
                let text = value("--color")?;
                color_value = color(&text).ok_or(format!("{text}: unknown color"))?;
            }
            "-m" | "--ms" => {
                let text = value("--ms")?;
                milliseconds = text.parse().map_err(|e| format!("{text}: {e}"))?;
            }
            // Everything after the first bare word is notification text, so a
            // message may start with a dash after "--".
            "--" => {
                words.extend(arguments.by_ref());
                break;
            }
            other if other.starts_with('-') && words.is_empty() => {
                return Err(format!("{other}: unknown option\n\n{USAGE}"));
            }
            other => words.push(other.to_string()),
        }
    }
    let text = words.join(" ");
    if text.trim().is_empty() {
        return Err(format!("no notification text\n\n{USAGE}"));
    }
    Client::connect()
        .and_then(|client| client.toast(&text, color_value, milliseconds))
        .map_err(|e| format!("{e}; is a HOSWM session running?"))
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-toast: {e}");
        std::process::exit(1);
    }
}
