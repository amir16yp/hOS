//! Small Linux display/input building blocks used by the hOS session.
pub mod abi;
pub mod client;
pub mod desktop;
pub mod drm;
pub mod font;
pub mod framebuffer;
pub mod keyboard;
pub mod surface;

pub mod context_menu;
pub mod shortcuts;
pub mod text;

pub mod greeter;

mod damage;

pub mod audio;
pub mod config;
pub mod init;
pub mod input;
pub mod menu;
pub mod preview;
pub mod qoi;
pub mod toast;
pub mod cursor;
pub mod reactor;
