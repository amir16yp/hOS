use hoswm::{
    client::{Client, Event, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT},
    font::Font,
    surface::Surface,
};
use std::{thread, time::Duration};
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let c = Client::connect()?;
    let w = c.create("About hOS", 460, 300, 0xff80afff)?;
    c.flags(w, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)?;
    let font = Font::builtin();
    let mut s = Surface::new(460, 300);
    loop {
        let (width, height, min) = c.size(w)?;
        if min {
            thread::sleep(Duration::from_millis(40));
            continue;
        }
        if (width as usize, height as usize) != (s.width(), s.height()) {
            s.reset(width as usize, height as usize, 0xff080b0a)
        } else {
            s.pixels_mut().fill(0xff080b0a)
        }
        font.draw(&mut s, 24, 24, "hOS Window Manager", 0xff80afff);
        font.draw(&mut s, 24, 70, "A small Linux desktop for hOS.", 0xffe4e8e5);
        font.draw(
            &mut s,
            24,
            102,
            "Independent Rust applications use the",
            0xffc3cdc7,
        );
        font.draw(
            &mut s,
            24,
            124,
            "HOSWM local window ABI for graphics",
            0xffc3cdc7,
        );
        font.draw(&mut s, 24, 146, "and input.", 0xffc3cdc7);
        font.draw(&mut s, 24, 200, "ABI version 1", 0xff72dbac);
        font.draw(
            &mut s,
            24,
            252,
            "Press Escape or close this window.",
            0xff9aa69f,
        );
        c.present(w, width, height, s.pixels())?;
        if let Some(Event { kind, text, .. }) = c.poll(w)? {
            if kind == 7 || kind == 9 || (kind == 6 && text == "\u{1b}") {
                let _ = c.close(w);
                break;
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("hos-about: {e}");
    }
}
