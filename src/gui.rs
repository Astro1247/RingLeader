use crate::config::{self, Config, Effect, FanMode, Light, Status, PORTS};
use crate::device::MAX_LEDS;
use crate::effects::{self, Rgb};
use crate::system::{self, autostart};
use eframe::egui::{
    self, Align, Color32, ComboBox, CornerRadius, FontId, Frame, Id, Layout, Margin, Pos2, Rect, RichText, Sense, Slider,
    Stroke, Ui, Vec2, pos2, vec2,
};
use std::collections::BTreeSet;
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(51, 215, 255);
const BG: Color32 = Color32::from_rgb(13, 15, 20);
const CARD: Color32 = Color32::from_rgb(22, 25, 33);
const CARD_HI: Color32 = Color32::from_rgb(30, 34, 45);
const MUTED: Color32 = Color32::from_rgb(130, 138, 156);
const GOOD: Color32 = Color32::from_rgb(70, 220, 140);
const WARN: Color32 = Color32::from_rgb(255, 184, 77);
const BAD: Color32 = Color32::from_rgb(255, 92, 92);

const LED_PRESETS: [(u8, &str); 9] = [
    (6, "6 LEDs"),
    (9, "9 · Pure Plus"),
    (12, "12 · Riing Plus"),
    (18, "18 · 2 rings"),
    (24, "24"),
    (30, "30 · Riing Trio"),
    (36, "36 · 3 rings"),
    (48, "48"),
    (54, "54 · Riing Quad"),
];

pub fn run() {
    // One window at a time: a second launch (e.g. tray click) just focuses the first.
    let Some(_lock) = gui_lock() else {
        let _ = std::fs::write(config::dir().join("focus"), "");
        return;
    };
    system::start_daemon();
    let icon = egui::IconData { rgba: effects::icon_rgba(64), width: 64, height: 64 };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RingLeader")
            .with_app_id("ringleader")
            .with_icon(icon)
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([880.0, 620.0]),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "RingLeader",
        opts,
        Box::new(|cc| {
            style(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    );
}

fn gui_lock() -> Option<std::fs::File> {
    std::fs::create_dir_all(config::dir()).ok()?;
    let f = std::fs::File::create(config::dir().join("gui.lock")).ok()?;
    f.try_lock().ok()?;
    Some(f)
}

/// Slow OS operations (UAC prompts, installers) run off the UI thread.
#[derive(Clone, Default)]
struct Job {
    busy: Arc<AtomicBool>,
    message: Arc<Mutex<String>>,
}

impl Job {
    fn run(&self, label: &str, f: impl FnOnce() -> Result<String, String> + Send + 'static) {
        if self.busy.swap(true, Relaxed) {
            return;
        }
        *self.message.lock().unwrap() = label.into();
        let job = self.clone();
        std::thread::spawn(move || {
            let msg = f().unwrap_or_else(|e| e);
            *job.message.lock().unwrap() = msg;
            job.busy.store(false, Relaxed);
        });
    }
}

struct App {
    cfg: Config,
    saved: Config,
    edited: Option<Instant>,
    status: Status,
    polled: Instant,
    /// Fan whose settings are shown; `group` are all fans being edited together (includes `sel`).
    sel: usize,
    group: BTreeSet<usize>,
    t0: Instant,
    autostart: Arc<AtomicBool>,
    about: bool,
    job: Job,
}

impl App {
    fn new() -> Self {
        let cfg = Config::load();
        Self {
            saved: cfg.clone(),
            cfg,
            edited: None,
            status: config::load(&config::status_path()),
            polled: Instant::now(),
            sel: 0,
            group: BTreeSet::from([0]),
            t0: Instant::now(),
            autostart: Arc::new(AtomicBool::new(autostart::get())),
            about: false,
            job: Job::default(),
        }
    }

    fn alive(&self) -> bool {
        config::now().saturating_sub(self.status.updated) <= 3
    }

    fn temp(&self, id: &str) -> Option<f32> {
        self.status.sensors.iter().find(|s| s.id == id).map(|s| s.temp)
    }

    fn preview(&self, port: usize) -> Vec<Rgb> {
        let p = &self.cfg.ports[port];
        effects::render(&p.light, p.leds as usize, self.t0.elapsed().as_secs_f32(), self.temp(&p.fan.sensor))
    }

    fn restart_controller(&self) {
        if cfg!(target_os = "linux") || self.status.elevated {
            system::send_command("reset");
        } else {
            self.job.run("Waiting for administrator approval…", || {
                system::reset_controller(true).map(|_| "Controller restarted.".into())
            });
        }
    }

    /// Enable/disable start with system. On Windows the elevated script also swaps the running
    /// service for the scheduled tasks.
    fn set_autostart(&self, on: bool) {
        let flag = self.autostart.clone();
        let label = if cfg!(windows) { "Waiting for administrator approval…" } else { "Updating…" };
        self.job.run(label, move || {
            let ok = autostart::set(on);
            flag.store(autostart::get(), Relaxed);
            if !ok {
                return Err("Cancelled.".into());
            }
            Ok(if on { "RingLeader will start with your system." } else { "Autostart disabled." }.into())
        });
    }

    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
            let spin: Vec<Rgb> = (0..12).map(|k| effects::hsv(k as f32 * 30.0 + self.t0.elapsed().as_secs_f32() * 40.0)).collect();
            ring(ui, r.center(), 11.0, &spin);
            // One wordmark, two colors: separate labels would get item spacing between them.
            let mut mark = egui::text::LayoutJob::default();
            let font = egui::TextFormat { font_id: FontId::proportional(22.0), color: Color32::WHITE, ..Default::default() };
            mark.append("Ring", 0.0, font.clone());
            mark.append("Leader", 0.0, egui::TextFormat { color: ACCENT, ..font });
            ui.label(mark);
            ui.add_space(12.0);
            let (color, text) = if !self.alive() {
                (BAD, "Service not running".to_string())
            } else if self.status.connected && !self.status.error.is_empty() {
                (WARN, self.status.error.clone())
            } else if self.status.connected {
                (GOOD, format!("{} · firmware {}", self.status.device, self.status.firmware))
            } else {
                (WARN, self.status.error.clone())
            };
            pill(ui, color, &text);
            if !self.alive() {
                if ui.button("Start service").clicked() {
                    system::start_daemon();
                }
            } else if ui.button("⟳ Restart controller").on_hover_text("Power-cycles the controller's USB connection").clicked() {
                self.restart_controller();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("About").clicked() {
                    self.about = true;
                }
                let mut on = self.autostart.load(Relaxed);
                let hint = if cfg!(windows) {
                    "Runs the service at boot (as SYSTEM, before sign-in) and its tray icon at logon. One UAC prompt to set up."
                } else {
                    "Runs the background service (tray icon) at login."
                };
                if ui.checkbox(&mut on, "Start with system").on_hover_text(hint).changed() {
                    self.set_autostart(on);
                }
            });
        });
    }

    /// Banner explaining how to get CPU temperature on Windows, with a one-click fix.
    fn cpu_banner(&mut self, ui: &mut Ui) {
        let (text, button) = match self.status.cpu_issue.as_str() {
            "driver" => ("CPU temperature on Windows needs the free, signed PawnIO driver (also used by LibreHardwareMonitor).", "Install PawnIO"),
            "admin" => ("Reading CPU temperature needs the service to run as administrator.", "Run service as admin"),
            _ => return,
        };
        if !self.alive() {
            return;
        }
        Frame::new().fill(WARN.gamma_multiply(0.12)).corner_radius(10).inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(text).color(WARN));
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.job.busy.load(Relaxed), egui::Button::new(button)).clicked() {
                    let install = self.status.cpu_issue == "driver";
                    let flag = self.autostart.clone();
                    self.job.run("Waiting for administrator approval…", move || setup_cpu_temp(install, &flag));
                }
                if self.status.cpu_issue == "driver" {
                    ui.hyperlink_to("What is PawnIO?", "https://pawnio.eu");
                }
            });
        });
        ui.add_space(8.0);
    }

    fn port_list(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("FANS").small().color(MUTED));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("All").on_hover_text("Control all fans together").clicked() {
                    self.group = (0..PORTS).collect();
                }
            });
        });
        ui.add_space(2.0);
        let multi = ui.input(|i| i.modifiers.command || i.modifiers.shift);
        for i in 0..PORTS {
            let st = self.status.ports.get(i).cloned().unwrap_or_default();
            let (primary, member) = (self.sel == i, self.group.contains(&i));
            let frame = Frame::new()
                .fill(if member { CARD_HI } else { CARD })
                .stroke(Stroke::new(
                    1.0,
                    if primary {
                        ACCENT
                    } else if member {
                        ACCENT.gamma_multiply(0.4)
                    } else {
                        Color32::TRANSPARENT
                    },
                ))
                .corner_radius(12)
                .inner_margin(Margin::same(10));
            let resp = frame
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::hover());
                        ring(ui, rect.center(), 21.0, &self.preview(i));
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&self.cfg.ports[i].name).strong());
                            let rpm = if st.rpm > 0 { format!("{} RPM", st.rpm) } else { "— RPM".into() };
                            ui.label(RichText::new(format!("{rpm}  ·  {}%", st.speed)).color(MUTED));
                        });
                        if member && self.group.len() > 1 {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(RichText::new("✔").color(ACCENT)));
                        }
                    });
                })
                .response
                .interact(Sense::click());
            if resp.clicked() {
                if multi {
                    // Ctrl/Shift-click toggles membership; the group never becomes empty.
                    if !self.group.remove(&i) {
                        self.group.insert(i);
                        self.sel = i;
                    } else if self.group.is_empty() {
                        self.group.insert(i);
                    } else if primary {
                        self.sel = *self.group.first().unwrap();
                    }
                } else {
                    self.sel = i;
                    self.group = BTreeSet::from([i]);
                }
            }
            ui.add_space(2.0);
        }
        ui.label(RichText::new("Ctrl-click to control several fans together").small().color(MUTED));
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(RichText::new(config::config_path().display().to_string()).small().color(MUTED));
        });
    }

    fn detail(&mut self, ui: &mut Ui) {
        let i = self.sel;
        ui.horizontal(|ui| {
            if self.group.len() > 1 {
                let names: Vec<&str> = self.group.iter().map(|&g| self.cfg.ports[g].name.as_str()).collect();
                ui.vertical(|ui| {
                    ui.label(RichText::new(format!("{} fans", self.group.len())).size(22.0));
                    ui.label(RichText::new(format!("{} · showing {}, changes apply to all", names.join(", "), self.cfg.ports[i].name)).color(MUTED));
                });
            } else {
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.ports[i].name)
                        .font(FontId::proportional(22.0))
                        .frame(Frame::NONE)
                        .desired_width(320.0),
                );
            }
            let rpms: Vec<u32> = self.group.iter().map(|&g| self.status.ports.get(g).map_or(0, |p| p.rpm as u32)).collect();
            let avg = rpms.iter().sum::<u32>() / rpms.len().max(1) as u32;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(if rpms.len() > 1 { "RPM avg" } else { "RPM" }).color(MUTED));
                ui.label(RichText::new(avg.to_string()).size(26.0).strong().color(ACCENT));
            });
        });
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], |ui| self.cooling(ui));
            card(&mut cols[1], |ui| self.lighting(ui));
        });
    }

    fn sensor_combo(&mut self, ui: &mut Ui, salt: &str) {
        let sensor = &mut self.cfg.ports[self.sel].fan.sensor;
        let shown = match self.status.sensors.iter().find(|s| &s.id == sensor) {
            Some(s) => format!("{} — {:.0}°C", s.id, s.temp),
            None if sensor.is_empty() => "Pick a sensor".into(),
            None => format!("{sensor} (missing)"),
        };
        ComboBox::from_id_salt(salt).width(ui.available_width()).selected_text(shown).show_ui(ui, |ui| {
            for s in &self.status.sensors {
                ui.selectable_value(sensor, s.id.clone(), format!("{} — {:.0}°C", s.id, s.temp));
            }
            if self.status.sensors.is_empty() {
                ui.label(RichText::new(NO_SENSORS).color(MUTED));
            }
        });
    }

    fn cooling(&mut self, ui: &mut Ui) {
        let i = self.sel;
        section(ui, "Cooling");
        ui.horizontal(|ui| {
            let mode = &mut self.cfg.ports[i].fan.mode;
            ui.selectable_value(mode, FanMode::Fixed, "Fixed");
            ui.selectable_value(mode, FanMode::Curve, "Curve");
        });
        ui.add_space(6.0);
        match self.cfg.ports[i].fan.mode {
            FanMode::Fixed => {
                let fan = &mut self.cfg.ports[i].fan;
                ui.add(Slider::new(&mut fan.speed, 0..=100).suffix("%"));
                ui.horizontal(|ui| {
                    for (v, name) in [(30, "Silent"), (50, "Balanced"), (75, "Performance"), (100, "Max")] {
                        ui.selectable_value(&mut fan.speed, v, name);
                    }
                });
            }
            FanMode::Curve => {
                self.sensor_combo(ui, "sensor_fan");
                let t = self.temp(&self.cfg.ports[i].fan.sensor);
                curve_editor(ui, &mut self.cfg.ports[i].fan.curve, t);
                ui.label(RichText::new("Drag points · double-click to add · right-click to remove").small().color(MUTED));
                if t.is_none() {
                    ui.label(RichText::new("No reading: the fan runs at 100% for safety.").small().color(WARN));
                }
            }
        }
    }

    fn lighting(&mut self, ui: &mut Ui) {
        let i = self.sel;
        section(ui, "Lighting");
        ui.horizontal_wrapped(|ui| {
            for e in Effect::ALL {
                ui.selectable_value(&mut self.cfg.ports[i].light.effect, e, e.label());
            }
        });
        ui.add_space(6.0);
        let port = &mut self.cfg.ports[i];
        let (light, leds): (&mut Light, &mut u8) = (&mut port.light, &mut port.leds);
        let e = light.effect;
        egui::Grid::new("light_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            if matches!(e, Effect::Static | Effect::Breathe | Effect::Comet) {
                ui.label("Color");
                ui.color_edit_button_srgb(&mut light.color);
                ui.end_row();
            }
            if e == Effect::Comet {
                ui.label("Background");
                ui.color_edit_button_srgb(&mut light.color2);
                ui.end_row();
            }
            if matches!(e, Effect::Breathe | Effect::Spectrum | Effect::Rainbow | Effect::Comet) {
                ui.label("Speed");
                ui.add(Slider::new(&mut light.speed, 0.1..=5.0).fixed_decimals(1).suffix("×"));
                ui.end_row();
            }
            if e == Effect::Temperature {
                let [lo, hi] = &mut light.temp_range;
                ui.label("Blue at");
                ui.add(egui::DragValue::new(lo).range(0.0..=100.0).speed(0.2).fixed_decimals(0).suffix("°C"));
                ui.end_row();
                ui.label("Red at");
                ui.add(egui::DragValue::new(hi).range(*lo + 1.0..=110.0).speed(0.2).fixed_decimals(0).suffix("°C"));
                ui.end_row();
            }
            if e != Effect::Off {
                ui.label("Brightness");
                ui.add(Slider::new(&mut light.brightness, 0..=100).suffix("%"));
                ui.end_row();
            }
            ui.label("LEDs per fan");
            let name = LED_PRESETS.iter().find(|p| p.0 == *leds).map_or(format!("{leds} LEDs"), |p| p.1.to_string());
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("leds").selected_text(name).show_ui(ui, |ui| {
                    for (n, label) in LED_PRESETS {
                        ui.selectable_value(leds, n, label);
                    }
                });
                ui.add(egui::DragValue::new(leds).range(1..=MAX_LEDS as u8));
            });
            ui.end_row();
        });
        if e == Effect::Temperature {
            ui.add_space(4.0);
            ui.label(RichText::new("Green halfway between. 41–65°C is TT RGB Plus's scale. Sensor:").color(MUTED));
            self.sensor_combo(ui, "sensor_light");
        }
        ui.add_space(8.0);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 170.0), Sense::hover());
        ring(ui, rect.center(), 72.0, &self.preview(i));
    }

    fn about(&mut self, ctx: &egui::Context) {
        let mut open = self.about;
        egui::Window::new("About")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(Frame::window(&ctx.global_style()).fill(CARD).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.vertical_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(96.0), Sense::hover());
                    let t = self.t0.elapsed().as_secs_f32();
                    let leds: Vec<Rgb> = (0..24).map(|k| effects::hsv(k as f32 * 15.0 + t * 60.0)).collect();
                    ring(ui, r.center(), 40.0, &leds);
                    ui.label(RichText::new("RingLeader").size(24.0).strong());
                    ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(MUTED));
                    ui.add_space(8.0);
                    ui.label("Fast, lightweight fan and RGB control for Thermaltake Riing Quad controllers, on Windows and Linux.");
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 190.0).max(0.0) / 2.0);
                        ui.label("Made by Astro ·");
                        ui.hyperlink_to(RichText::new("ZonAstro.com").color(ACCENT).strong(), "https://zonastro.com");
                    });
                    ui.add_space(12.0);
                    ui.label(
                        RichText::new(
                            "MIT License. Not affiliated with Thermaltake; the controller protocol was \
                             reverse-engineered for interoperability. CPU temperature on Windows uses \
                             the PawnIO driver with its LGPL-2.1 modules.",
                        )
                        .small()
                        .color(MUTED),
                    );
                });
            });
        self.about = open;
    }

    /// Debounced save: the daemon picks the file up by mtime.
    fn persist(&mut self) {
        if self.cfg == self.saved {
            self.edited = None;
            return;
        }
        let since = *self.edited.get_or_insert_with(Instant::now);
        if since.elapsed() > Duration::from_millis(150) && config::save(&config::config_path(), &self.cfg).is_ok() {
            self.saved = self.cfg.clone();
            self.edited = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.polled.elapsed() > Duration::from_millis(500) {
            self.polled = Instant::now();
            self.status = config::load(&config::status_path());
            if std::fs::remove_file(config::dir().join("focus")).is_ok() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }
        let pad = Frame::new().fill(BG).inner_margin(Margin::symmetric(18, 12));
        egui::Panel::top("top").frame(pad).show(ui, |ui| self.header(ui));
        let message = self.job.message.lock().unwrap().clone();
        if !message.is_empty() {
            egui::Panel::bottom("job").frame(pad).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if self.job.busy.load(Relaxed) {
                        ui.spinner();
                    } else if ui.small_button("✕").clicked() {
                        self.job.message.lock().unwrap().clear();
                    }
                    ui.label(message);
                });
            });
        }
        egui::Panel::left("ports")
            .exact_size(270.0)
            .resizable(false)
            .frame(pad)
            .show(ui, |ui| self.port_list(ui));
        egui::CentralPanel::default().frame(pad).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.cpu_banner(ui);
                // Edit the shown fan, then copy what changed to the rest of the group.
                let before = self.cfg.ports[self.sel].clone();
                self.detail(ui);
                let after = self.cfg.ports[self.sel].clone();
                for &g in &self.group {
                    let p = &mut self.cfg.ports[g];
                    if after.fan != before.fan {
                        p.fan = after.fan.clone();
                    }
                    if after.light != before.light || after.leds != before.leds {
                        p.light = after.light.clone();
                        p.leds = after.leds;
                    }
                }
            });
        });
        if self.about {
            self.about(&ctx);
        }
        self.persist();
        // ponytail: 30 fps repaint for the LED previews; egui throttles it when minimized.
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}

/// Swap the running (unelevated) service for the elevated scheduled task.
#[cfg(windows)]
fn restart_service() {
    system::send_command("quit");
    for _ in 0..50 {
        if crate::daemon::lock().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    system::start_daemon();
}

/// Windows: install PawnIO if asked, then run the service elevated via the autostart task.
fn setup_cpu_temp(install: bool, autostart_flag: &AtomicBool) -> Result<String, String> {
    #[cfg(windows)]
    {
        // One elevated script, so one UAC prompt: install the driver, then register the task.
        let mut script = String::new();
        if install {
            script += "winget install --id namazso.PawnIO -e --silent --accept-package-agreements --accept-source-agreements\n\
                       if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) { exit 1 }\n"; // or already installed
        }
        // The autostart script restarts the service itself; otherwise restart it to load PawnIO.
        let restart = autostart::get();
        if !restart {
            script += &autostart::script(true).ok_or("Cannot locate ringleader.exe.")?;
        }
        if !script.is_empty() && !system::run_elevated(&script) {
            return Err("Setup failed or was cancelled. You can install PawnIO from pawnio.eu.".into());
        }
        autostart_flag.store(autostart::get(), Relaxed);
        if restart {
            restart_service();
        }
        Ok("Done. CPU temperature appears in a few seconds (a reboot may be needed after installing PawnIO).".into())
    }
    #[cfg(not(windows))]
    {
        let _ = (install, autostart_flag);
        Ok(String::new())
    }
}

const NO_SENSORS: &str = if cfg!(windows) {
    "No sensors. GPU temperature needs an NVIDIA driver; CPU temperature needs the PawnIO driver."
} else {
    "No sensors found in /sys/class/hwmon."
};

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new().fill(CARD).corner_radius(14).inner_margin(Margin::same(16)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    });
}

fn section(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).size(17.0).strong());
    ui.add_space(4.0);
}

fn pill(ui: &mut Ui, color: Color32, text: &str) {
    Frame::new()
        .fill(color.gamma_multiply(0.15))
        .corner_radius(20)
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 4.0, color);
                ui.label(RichText::new(text).color(color));
            });
        });
}

/// LED ring: one dot per LED with a soft glow behind lit ones.
fn ring(ui: &Ui, c: Pos2, r: f32, leds: &[Rgb]) {
    let p = ui.painter();
    p.circle_filled(c, r + r * 0.12, Color32::from_rgb(8, 9, 12));
    p.circle_stroke(c, r * 0.55, Stroke::new(r * 0.08, Color32::from_rgb(28, 31, 40)));
    let n = leds.len().max(1);
    // min/max rather than clamp: for tiny rings r * 0.12 drops below the 1.2 floor.
    let dot = (r * TAU / n as f32 * 0.32).min(r * 0.12).max(1.2);
    for (k, &[cr, cg, cb]) in leds.iter().enumerate() {
        let a = k as f32 / n as f32 * TAU - TAU / 4.0;
        let pos = c + vec2(a.cos(), a.sin()) * r;
        let col = Color32::from_rgb(cr, cg, cb);
        if cr as u16 + cg as u16 + cb as u16 > 30 {
            p.circle_filled(pos, dot * 2.4, col.gamma_multiply(0.18));
            p.circle_filled(pos, dot, col);
        } else {
            p.circle_filled(pos, dot, Color32::from_rgb(40, 44, 54));
        }
    }
}

/// Editable fan curve: x = 0..100 °C, y = 0..100 %.
fn curve_editor(ui: &mut Ui, curve: &mut Vec<[f32; 2]>, temp: Option<f32>) {
    let (outer, bg) = ui.allocate_exact_size(vec2(ui.available_width(), 210.0), Sense::click());
    let plot = Rect::from_min_max(outer.min + vec2(34.0, 8.0), outer.max - vec2(8.0, 22.0));
    let to = |[t, d]: [f32; 2]| pos2(plot.left() + plot.width() * t / 100.0, plot.bottom() - plot.height() * d / 100.0);
    let from = |p: Pos2| {
        [
            ((p.x - plot.left()) / plot.width() * 100.0).clamp(0.0, 100.0).round(),
            ((plot.bottom() - p.y) / plot.height() * 100.0).clamp(0.0, 100.0).round(),
        ]
    };
    let painter = ui.painter().clone();
    painter.rect_filled(plot, 8, Color32::from_rgb(15, 17, 23));
    let grid = Stroke::new(1.0, Color32::from_rgb(34, 38, 48));
    let font = FontId::proportional(11.0);
    for v in (0..=100).step_by(20) {
        let x = to([v as f32, 0.0]).x;
        painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], grid);
        painter.text(pos2(x, plot.bottom() + 4.0), egui::Align2::CENTER_TOP, format!("{v}°"), font.clone(), MUTED);
    }
    for v in (0..=100).step_by(25) {
        let y = to([0.0, v as f32]).y;
        painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], grid);
        painter.text(pos2(plot.left() - 6.0, y), egui::Align2::RIGHT_CENTER, format!("{v}%"), font.clone(), MUTED);
    }

    // Extend the line flat to both edges, like the daemon's clamping does.
    let mut line: Vec<Pos2> = curve.iter().map(|&p| to(p)).collect();
    if let (Some(&f), Some(&l)) = (curve.first(), curve.last()) {
        line.insert(0, to([0.0, f[1]]));
        line.push(to([100.0, l[1]]));
    }
    // Fill as vertical strips: the area under a curve isn't convex, so one polygon would draw wrong.
    for w in line.windows(2) {
        let quad = vec![w[0], w[1], pos2(w[1].x, plot.bottom()), pos2(w[0].x, plot.bottom())];
        painter.add(egui::Shape::convex_polygon(quad, ACCENT.gamma_multiply(0.10), Stroke::NONE));
    }
    painter.add(egui::Shape::line(line, Stroke::new(2.0, ACCENT)));

    if let Some(t) = temp {
        let x = to([t.clamp(0.0, 100.0), 0.0]).x;
        painter.line_segment([pos2(x, plot.top()), pos2(x, plot.bottom())], Stroke::new(1.0, WARN.gamma_multiply(0.7)));
        let d = effects::curve(curve, t);
        painter.circle_filled(to([t.clamp(0.0, 100.0), d as f32]), 4.0, WARN);
        painter.text(
            pos2(x + 6.0, plot.top() + 4.0),
            egui::Align2::LEFT_TOP,
            format!("{t:.0}°C: {d}%"),
            FontId::proportional(12.0),
            WARN,
        );
    }

    let mut remove = None;
    for k in 0..curve.len() {
        let pos = to(curve[k]);
        let resp = ui.interact(Rect::from_center_size(pos, Vec2::splat(18.0)), Id::new(("curve_pt", k)), Sense::click_and_drag());
        if resp.dragged()
            && let Some(m) = resp.interact_pointer_pos()
        {
            let [mut t, d] = from(m);
            // Keep points ordered so the curve stays a function of temperature.
            let lo = if k > 0 { curve[k - 1][0] + 1.0 } else { 0.0 };
            let hi = if k + 1 < curve.len() { curve[k + 1][0] - 1.0 } else { 100.0 };
            t = t.clamp(lo, hi.max(lo));
            curve[k] = [t, d];
        }
        if resp.secondary_clicked() && curve.len() > 2 {
            remove = Some(k);
        }
        let hot = resp.hovered() || resp.dragged();
        painter.circle(to(curve[k]), if hot { 7.0 } else { 5.5 }, Color32::WHITE, Stroke::new(2.0, ACCENT));
        if hot {
            let [t, d] = curve[k];
            painter.text(to(curve[k]) + vec2(0.0, -12.0), egui::Align2::CENTER_BOTTOM, format!("{t:.0}° · {d:.0}%"), FontId::proportional(12.0), Color32::WHITE);
        }
    }
    if let Some(k) = remove {
        curve.remove(k);
    }
    if bg.double_clicked()
        && let Some(m) = bg.interact_pointer_pos()
        && plot.contains(m)
    {
        let p = from(m);
        if !curve.iter().any(|q| q[0] == p[0]) {
            curve.push(p);
            curve.sort_by(|a, b| a[0].total_cmp(&b[0]));
        }
    }
}

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.extreme_bg_color = Color32::from_rgb(10, 12, 16);
    v.faint_bg_color = CARD_HI;
    v.selection.bg_fill = ACCENT.gamma_multiply(0.30);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.hyperlink_color = ACCENT;
    v.slider_trailing_fill = true;
    v.window_corner_radius = CornerRadius::same(12);
    for (w, fill) in [
        (&mut v.widgets.inactive, Color32::from_rgb(34, 38, 49)),
        (&mut v.widgets.hovered, Color32::from_rgb(45, 51, 65)),
        (&mut v.widgets.active, Color32::from_rgb(55, 62, 80)),
        (&mut v.widgets.open, Color32::from_rgb(45, 51, 65)),
    ] {
        w.corner_radius = CornerRadius::same(8);
        w.weak_bg_fill = fill;
        w.bg_fill = fill;
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.5));
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.slider_width = 150.0;
        s.spacing.interact_size.y = 26.0;
        for (style, size) in [
            (egui::TextStyle::Body, 14.0),
            (egui::TextStyle::Button, 14.0),
            (egui::TextStyle::Heading, 20.0),
            (egui::TextStyle::Small, 11.0),
        ] {
            if let Some(f) = s.text_styles.get_mut(&style) {
                f.size = size;
            }
        }
    });
}
