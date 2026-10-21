#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod config;
mod daemon;
mod device;
mod effects;
mod gui;
mod sensors;
mod system;
mod tray;

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => gui::run(),
        Some("daemon") => {
            if let Err(e) = daemon::run() {
                eprintln!("ringleader: {e}");
                std::process::exit(1);
            }
        }
        Some(_) => {
            eprintln!("usage: ringleader [daemon]");
            std::process::exit(2);
        }
    }
}
