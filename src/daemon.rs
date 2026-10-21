use crate::config::{self, Config, FanMode, PortStatus, Status, PORTS};
use crate::device::Quad;
use crate::effects::{self, Rgb};
use crate::sensors::Sensors;
use std::collections::HashMap;
use std::fs::File;
use std::time::{Duration, Instant};

const FRAME: Duration = Duration::from_millis(33);
const TICK: Duration = Duration::from_secs(1);
/// Resend unchanged LED frames this often, so a controller that lost power/state self-heals.
const REFRESH: Duration = Duration::from_secs(3);
/// Fan duty when the curve's sensor disappears.
const FAILSAFE: u8 = 100;

/// Lock held while a daemon runs. Returns None if another daemon holds it.
pub fn lock() -> Option<File> {
    std::fs::create_dir_all(config::dir()).ok()?;
    let f = File::create(config::dir().join("daemon.lock")).ok()?;
    f.try_lock().ok()?;
    Some(f)
}

pub fn run() -> Result<(), String> {
    let _lock = lock().ok_or("daemon already running")?;
    let cfg_path = config::config_path();
    let mut cfg = Config::load();
    let mut cfg_mtime = config::mtime(&cfg_path);
    let mut api = hidapi::HidApi::new().map_err(|e| e.to_string())?;
    let mut dev: Option<Quad> = None;
    let mut sensors = Sensors::new();
    let mut temps: HashMap<String, f32> = HashMap::new();
    let mut sent: Vec<Vec<Rgb>> = vec![Vec::new(); PORTS];
    let mut status = Status { ports: vec![PortStatus::default(); PORTS], ..Status::default() };
    let start = Instant::now();
    let (mut next_tick, mut next_open, mut next_refresh) = (start, start, start);

    loop {
        let now = Instant::now();
        let m = config::mtime(&cfg_path);
        if m != cfg_mtime {
            cfg_mtime = m;
            cfg = Config::load();
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

        let ticked = now >= next_tick;
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
                    } else {
                        "Controller stopped reporting RPM; fans still obey. Replug its USB to restore readings.".into()
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
        }
        std::thread::sleep(FRAME.saturating_sub(now.elapsed()));
    }
}
