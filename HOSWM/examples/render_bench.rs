//! CPU rendering microbenchmark; excludes display-driver and scanout latency.
use hoswm::{
    cursor::SoftwareCursor,
    desktop::{ACCENT, Desktop},
    surface::Surface,
};
use std::{hint::black_box, time::Instant};
fn main() {
    let mut desktop = Desktop::new();
    for i in 0..4 {
        desktop
            .create(format!("Window {i}"), 640, 400, ACCENT)
            .unwrap();
    }
    let mut surface = Surface::new(800, 600);
    let frames = 600;
    let start = Instant::now();
    for i in 0..frames {
        desktop.motion(100 + i % 500, 300);
        desktop.draw(&mut surface);
        black_box(surface.pixels());
    }
    let full = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    desktop.draw_scene(&mut surface);
    let mut cursor = SoftwareCursor::default();
    let start = Instant::now();
    for i in 0..frames {
        let dirty = desktop.motion(100 + i % 500, 300);
        cursor.hide(&mut surface);
        if dirty {
            desktop.draw_scene(&mut surface);
        }
        cursor.show(&mut surface, desktop.x, desktop.y);
        black_box(surface.pixels());
    }
    let pointer = start.elapsed().as_secs_f64() * 1000.0 / frames as f64;
    println!("Four 640x400 windows, 800x600 canvas, {frames} frames");
    println!("Full composition: {full:.3} ms/frame; cached pointer: {pointer:.4} ms/frame");
    println!("60 Hz CPU frame budget: 16.667 ms (upload/scanout excluded)");
}
