use crate::config::{self, Config, FanMode, PortStatus, Sensor, Status, PORTS};
use crate::device::Quad;
use crate::effects::{self, Rgb};
use crate::sensors::{CpuIssue, Sensors};
use crate::system;
use std::collections::HashMap;
use std::fs::File;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(33);
const TICK: Duration = Duration::from_secs(1);
/// Resend unchanged LED frames this often, so a controller that lost power/state self-heals.
const REFRESH: Duration = Duration::from_secs(3);
/// Fan duty when the curve's sensor disappears.
const FAILSAFE: u8 = 100;
/// Minimum gap between automatic controller restarts.
const AUTO_RESET_GAP: Duration = Duration::from_secs(120);

/// Requests from the tray (or the window, via `system::send_command`).
pub static QUIT: AtomicBool = AtomicBool::new(false);
pub static RESET: AtomicBool = AtomicBool::new(false);
/// Tray tooltip text.
pub static TIP: Mutex<String> = Mutex::new(String::new());

/// Lock held while a daemon runs. Returns None if another daemon holds it.
pub fn lock() -> Option<File> {
    std::fs::create_dir_all(config::dir()).ok()?;
    let f = File::create(config::dir().join("daemon.lock")).ok()?;
    f.try_lock().ok()?;
    Some(f)
}

pub fn run() -> Result<(), String> {
    let _lock = lock().ok_or("daemon already running")?;
    let tray = crate::tray::start();
    let cfg_path = config::config_path();
    let mut cfg = Config::load();
    let mut cfg_mtime = config::mtime(&cfg_path);
    let mut api = hidapi::HidApi::new().map_err(|e| e.to_string())?;
    let mut dev: Option<Quad> = None;
    let mut sensors = Sensors::new();
    let mut temps: HashMap<String, f32> = HashMap::new();
    let mut sent: Vec<Vec<Rgb>> = vec![Vec::new(); PORTS];
    let mut status = Status { ports: vec![PortStatus::default(); PORTS], ..Status::default() };
    #[cfg(windows)]
    {
        status.elevated = system::is_elevated();
    }
    status.cpu_issue = match sensors.cpu_issue() {
        None => "",
        Some(CpuIssue::NoDriver) => "driver",
        Some(CpuIssue::NoAdmin) => "admin",
        Some(CpuIssue::Unsupported) => "unsupported",
    }
    .into();
    let start = Instant::now();
    let (mut next_tick, mut next_open, mut next_refresh) = (start, start, start);
    let mut last_reset: Option<Instant> = None;

    while !QUIT.load(Relaxed) {
        let now = Instant::now();
        let m = config::mtime(&cfg_path);
        if m != cfg_mtime {
            cfg_mtime = m;
            cfg = Config::load();
        }

        let ticked = now >= next_tick;
        if ticked {
            match system::take_command().as_deref() {
                Some("quit") => QUIT.store(true, Relaxed),
                Some("reset") => RESET.store(true, Relaxed),
                _ => {}
            }
            // A controller that stopped answering only recovers with a USB reset; do it ourselves
            // when allowed (Linux with the udev rule, Windows when running as admin).
            let stuck = dev.as_ref().is_some_and(|d| !d.answering());
            if stuck && last_reset.is_none_or(|t| t.elapsed() > AUTO_RESET_GAP) && can_reset(&status) {
                RESET.store(true, Relaxed);
            }
        }
        if RESET.swap(false, Relaxed) {
            last_reset = Some(now);
            dev = None; // the handle dies with the reset; reopen once it re-enumerates
            next_open = now + Duration::from_secs(3);
            status.error = match system::reset_controller(false) {
                Ok(()) => "Restarting controller…".into(),
                Err(e) => format!("Controller restart failed: {e}"),
            };
        }

        if dev.is_none() && now >= next_open {
            next_open = now + Duration::from_secs(2);
            match Quad::open(&mut api) {
                Ok(q) => {
                    status.firmware = q.firmware.clone();
                    status.error.clear();
                    sent.iter_mut().for_each(Vec::clear);
                    dev = Some(q);
                }
                Err(e) => status.error = e,
            }
        }

        let result = (|| -> Result<(), String> {
            if ticked {
                next_tick = now + TICK;
                let readings = sensors.read();
                // EMA so a one-second spike doesn't make fans surge.
                for s in &readings {
                    let t = temps.entry(s.id.clone()).or_insert(s.temp);
                    *t += (s.temp - *t) * 0.3;
                }
                temps.retain(|id, _| readings.iter().any(|s| &s.id == id));
                status.sensors = readings;
                if let Some(d) = &dev {
                    for (i, p) in cfg.ports.iter().enumerate() {
                        let duty = match p.fan.mode {
                            FanMode::Fixed => p.fan.speed,
                            FanMode::Curve => temps.get(&p.fan.sensor).map_or(FAILSAFE, |&t| effects::curve(&p.fan.curve, t)),
                        };
                        d.set_speed(i, duty)?;
                        status.ports[i] = match d.read_fan(i) {
                            Ok((speed, rpm)) => PortStatus { speed, rpm },
                            Err(_) => PortStatus::default(),
                        };
                    }
                    status.error = if d.answering() {
                        String::new()
                    } else if can_reset(&status) {
                        "Controller stopped answering; restarting it automatically.".into()
                    } else {
                        "Controller stopped answering (fans still obey). Use Restart controller.".into()
                    };
                }
            }
            if let Some(d) = &dev {
                let refresh = now >= next_refresh;
                if refresh {
                    next_refresh = now + REFRESH;
                }
                let t = (now - start).as_secs_f32();
                for (i, p) in cfg.ports.iter().enumerate() {
                    let frame = effects::render(&p.light, p.leds as usize, t, temps.get(&p.fan.sensor).copied());
                    if refresh || frame != sent[i] {
                        d.set_leds(i, &frame)?;
                        sent[i] = frame;
                    }
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            // USB unplug, resume from sleep, etc: drop the handle and reopen next round.
            status.error = e;
            dev = None;
        }

        status.connected = dev.is_some();
        status.device = if dev.is_some() { "Riing Quad".into() } else { String::new() };
        if ticked {
            status.updated = config::now();
            let _ = config::save(&config::status_path(), &status);
            *TIP.lock().unwrap() = tooltip(&status);
            if let Some(t) = &tray {
                t.refresh();
            }
        }
        std::thread::sleep(FRAME.saturating_sub(now.elapsed()));
    }

    // Graceful exit: we're between USB writes here, so the controller is left in a clean state.
    if let Some(t) = &tray {
        t.close();
    }
    Ok(())
}

fn can_reset(status: &Status) -> bool {
    cfg!(target_os = "linux") || status.elevated
}

fn tooltip(s: &Status) -> String {
    if !s.connected {
        return "Controller not connected".into();
    }
    let find = |keys: &[&str]| -> Option<&Sensor> { s.sensors.iter().find(|x| keys.iter().any(|k| x.id.contains(k))) };
    let mut parts = Vec::new();
    if let Some(c) = find(&["CPU", "Package id", "Tctl", "k10temp"]) {
        parts.push(format!("CPU {:.0}°C", c.temp));
    }
    if let Some(g) = find(&["NVIDIA", "GeForce", "Radeon", "amdgpu"]) {
        parts.push(format!("GPU {:.0}°C", g.temp));
    }
    let spinning: Vec<u16> = s.ports.iter().map(|p| p.rpm).filter(|&r| r > 0).collect();
    if !spinning.is_empty() {
        parts.push(format!("{} fans ~{} RPM", spinning.len(), spinning.iter().map(|&r| r as u32).sum::<u32>() / spinning.len() as u32));
    }
    parts.join(" · ")
}
