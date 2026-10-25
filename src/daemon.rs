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
/// Curve-mode duty change per second, %: fans glide instead of stepping. Fixed speed and the
/// fail-safe apply at once.
const DUTY_UP: i16 = 5;
const DUTY_DOWN: i16 = 2;
/// Temperature-effect LEDs glide to a new reading over roughly this long, like TT RGB Plus.
const LED_FADE: f32 = 1.5;
/// How long after start to keep retrying sensor drivers that weren't ready.
const SENSOR_RETRY: Duration = Duration::from_secs(120);
/// Minimum gap between automatic controller restarts.
const AUTO_RESET_GAP: Duration = Duration::from_secs(120);

/// Requests from the tray (or the window, via `system::send_command`).
pub static QUIT: AtomicBool = AtomicBool::new(false);
pub static RESET: AtomicBool = AtomicBool::new(false);
/// The user session is ending (logoff/shutdown), as opposed to the user choosing Quit.
pub static ENDING: AtomicBool = AtomicBool::new(false);
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
    let lock = lock().ok_or("daemon already running")?;
    serve(lock, crate::tray::start())
}

/// Tray icon for a service running elsewhere (Windows: the boot-time SYSTEM task, which has no
/// desktop). Becomes the service itself whenever none is running, so there's never a second one.
pub fn tray() -> Result<(), String> {
    let tray = crate::tray::start();
    loop {
        if let Some(l) = lock() {
            return serve(l, tray);
        }
        if ENDING.load(Relaxed) {
            return Ok(()); // logoff: the service keeps running
        }
        if QUIT.load(Relaxed) {
            system::send_command("quit");
            if let Some(t) = &tray {
                t.close();
            }
            return Ok(());
        }
        if RESET.swap(false, Relaxed) {
            system::send_command("reset");
        }
        *TIP.lock().unwrap() = tooltip(&config::load(&config::status_path()));
        if let Some(t) = &tray {
            t.refresh();
        }
        std::thread::sleep(TICK);
    }
}

fn serve(_lock: File, tray: Option<crate::tray::Tray>) -> Result<(), String> {
    let cfg_path = config::config_path();
    let mut cfg = Config::load();
    let mut cfg_mtime = config::mtime(&cfg_path);
    let mut api = hidapi::HidApi::new().map_err(|e| e.to_string())?;
    let mut dev: Option<Quad> = None;
    let mut sensors = Sensors::new();
    let mut temps: HashMap<String, f32> = HashMap::new();
    let mut sent: Vec<Vec<Rgb>> = vec![Vec::new(); PORTS];
    let mut duty: Vec<Option<u8>> = vec![None; PORTS];
    let mut led_temp: Vec<Option<f32>> = vec![None; PORTS];
    let mut status = Status { ports: vec![PortStatus::default(); PORTS], ..Status::default() };
    #[cfg(windows)]
    {
        status.elevated = system::is_elevated();
    }
    status.cpu_issue = cpu_issue(&sensors);
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
                // At boot the PawnIO / NVIDIA drivers may still be loading: keep retrying briefly.
                if now - start < SENSOR_RETRY && sensors.incomplete() {
                    sensors = Sensors::new();
                    status.cpu_issue = cpu_issue(&sensors);
                }
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
                        let target = match p.fan.mode {
                            FanMode::Fixed => None,
                            FanMode::Curve => temps.get(&p.fan.sensor).map(|&t| effects::curve(&p.fan.curve, t)),
                        };
                        let next = match (target, duty[i]) {
                            (Some(t), Some(cur)) => slew(cur, t),
                            (Some(t), None) => t,
                            (None, _) if p.fan.mode == FanMode::Fixed => p.fan.speed,
                            (None, _) => FAILSAFE,
                        };
                        duty[i] = Some(next);
                        d.set_speed(i, next)?;
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
                let k = FRAME.as_secs_f32() / LED_FADE;
                for (i, p) in cfg.ports.iter().enumerate() {
                    let target = temps.get(&p.fan.sensor).copied();
                    led_temp[i] = match (led_temp[i], target) {
                        (Some(cur), Some(x)) => Some(cur + (x - cur) * k),
                        _ => target,
                    };
                    let frame = effects::render(&p.light, p.leds as usize, t, led_temp[i]);
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

/// One second's step from `cur` toward `target`, limited to DUTY_UP / DUTY_DOWN.
fn slew(cur: u8, target: u8) -> u8 {
    (cur as i16 + (target as i16 - cur as i16).clamp(-DUTY_DOWN, DUTY_UP)) as u8
}

fn cpu_issue(s: &Sensors) -> String {
    match s.cpu_issue() {
        None => "",
        Some(CpuIssue::NoDriver) => "driver",
        Some(CpuIssue::NoAdmin) => "admin",
        Some(CpuIssue::Unsupported) => "unsupported",
    }
    .into()
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

#[cfg(test)]
mod tests {
    #[test]
    fn slew_limits_steps() {
        assert_eq!(super::slew(40, 100), 45);
        assert_eq!(super::slew(40, 0), 38);
        assert_eq!(super::slew(40, 41), 41);
    }
}
