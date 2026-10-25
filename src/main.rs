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
        Some(mode @ ("daemon" | "tray")) => {
            if let Some(d) = std::env::args_os().nth(2) {
                config::set_dir(d.into());
            }
            let r = if mode == "daemon" { daemon::run() } else { daemon::tray() };
            if let Err(e) = r {
                eprintln!("ringleader: {e}");
                std::process::exit(1);
            }
        }
        Some(_) => {
            eprintln!("usage: ringleader [daemon [config-dir] | tray]");
            std::process::exit(2);
        }
    }
}
