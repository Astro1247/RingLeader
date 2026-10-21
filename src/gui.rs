use crate::config::{self, Config, Effect, FanMode, Light, Status, PORTS};
use crate::device::MAX_LEDS;
use crate::effects::{self, Rgb};
use eframe::egui::{
    self, Align, Color32, ComboBox, CornerRadius, FontId, Frame, Id, Layout, Margin, Pos2, Rect, RichText, Sense, Slider,
    Stroke, Ui, Vec2, pos2, vec2,
};
use std::f32::consts::TAU;
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
    spawn_daemon();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RingLeader")
            .with_inner_size([1000.0, 660.0])
            .with_min_inner_size([860.0, 600.0]),
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

/// The daemon owns the controller; the GUI only edits config.toml and reads status.toml.
fn spawn_daemon() {
    // Taking the lock succeeds only when no daemon holds it (released again right away).
    if crate::daemon::lock().is_none() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x8 | 0x200); // DETACHED | NEW_PROCESS_GROUP
    let _ = cmd.spawn();
}

struct App {
    cfg: Config,
    saved: Config,
    edited: Option<Instant>,
    status: Status,
    polled: Instant,
    sel: usize,
    t0: Instant,
    autostart: bool,
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
            t0: Instant::now(),
            autostart: autostart::get(),
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

    fn header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Ring").size(22.0).strong());
            ui.label(RichText::new("Leader").size(22.0).strong().color(ACCENT));
            ui.add_space(16.0);
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
            if !self.alive() && ui.button("Start service").clicked() {
                spawn_daemon();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.checkbox(&mut self.autostart, "Start with system").changed() {
                    autostart::set(self.autostart);
                    self.autostart = autostart::get();
                }
            });
        });
    }

    fn port_list(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("FANS").small().color(MUTED));
        ui.add_space(4.0);
        for i in 0..PORTS {
            let st = self.status.ports.get(i).cloned().unwrap_or_default();
            let selected = self.sel == i;
            let frame = Frame::new()
                .fill(if selected { CARD_HI } else { CARD })
                .stroke(Stroke::new(1.0, if selected { ACCENT } else { Color32::TRANSPARENT }))
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
                    });
                })
                .response
                .interact(Sense::click());
            if resp.clicked() {
                self.sel = i;
            }
            ui.add_space(2.0);
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.label(RichText::new(config::config_path().display().to_string()).small().color(MUTED));
        });
    }

    fn detail(&mut self, ui: &mut Ui) {
        let i = self.sel;
        let st = self.status.ports.get(i).cloned().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.cfg.ports[i].name)
                    .font(FontId::proportional(22.0))
                    .frame(Frame::NONE)
                    .desired_width(320.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new("RPM").color(MUTED));
                ui.label(RichText::new(st.rpm.to_string()).size(26.0).strong().color(ACCENT));
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
        ui.add_space(8.0);
        if ui.button("Apply cooling to all fans").clicked() {
            let fan = self.cfg.ports[i].fan.clone();
            self.cfg.ports.iter_mut().for_each(|p| p.fan = fan.clone());
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
            ui.label(RichText::new("Blue at 30°C → red at 80°C, from:").color(MUTED));
            self.sensor_combo(ui, "sensor_light");
        }
        ui.add_space(8.0);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 170.0), Sense::hover());
        ring(ui, rect.center(), 72.0, &self.preview(i));
        ui.add_space(8.0);
        if ui.button("Apply lighting to all fans").clicked() {
            let (light, leds) = (self.cfg.ports[i].light.clone(), self.cfg.ports[i].leds);
            self.cfg.ports.iter_mut().for_each(|p| {
                p.light = light.clone();
                p.leds = leds;
            });
        }
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
        if self.polled.elapsed() > Duration::from_millis(500) {
            self.polled = Instant::now();
            self.status = config::load(&config::status_path());
        }
        let pad = Frame::new().fill(BG).inner_margin(Margin::symmetric(18, 12));
        egui::Panel::top("top").frame(pad).show(ui, |ui| self.header(ui));
        egui::Panel::left("ports")
            .exact_size(270.0)
            .resizable(false)
            .frame(pad)
            .show(ui, |ui| self.port_list(ui));
        egui::CentralPanel::default().frame(pad).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.detail(ui));
        });
        self.persist();
        // ponytail: 30 fps repaint for the LED previews; egui throttles it when minimized.
        ui.ctx().request_repaint_after(Duration::from_millis(33));
    }
}

const NO_SENSORS: &str = if cfg!(windows) {
    "No sensors. GPU temps need an NVIDIA driver; CPU temps need HWiNFO running with Shared Memory Support enabled."
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
            ui.label(RichText::new(format!("● {text}")).color(color));
        });
}

/// LED ring: one dot per LED with a soft glow behind lit ones.
fn ring(ui: &Ui, c: Pos2, r: f32, leds: &[Rgb]) {
    let p = ui.painter();
    p.circle_filled(c, r + r * 0.12, Color32::from_rgb(8, 9, 12));
    p.circle_stroke(c, r * 0.55, Stroke::new(r * 0.08, Color32::from_rgb(28, 31, 40)));
    let n = leds.len().max(1);
    let dot = (r * TAU / n as f32 * 0.32).clamp(1.5, r * 0.12);
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
            format!("{t:.0}°C → {d}%"),
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
        s.spacing.slider_width = 200.0;
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

/// "Start with system" launches only the headless daemon; the window is opened on demand.
mod autostart {
    #[cfg(windows)]
    mod imp {
        use std::os::windows::process::CommandExt;
        const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        fn reg(args: &[&str]) -> bool {
            std::process::Command::new("reg")
                .args(args)
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        }
        pub fn get() -> bool {
            reg(&["query", KEY, "/v", "RingLeader"])
        }
        pub fn set(on: bool) {
            let Ok(exe) = std::env::current_exe() else { return };
            if on {
                reg(&["add", KEY, "/v", "RingLeader", "/t", "REG_SZ", "/d", &format!("\"{}\" daemon", exe.display()), "/f"]);
            } else {
                reg(&["delete", KEY, "/v", "RingLeader", "/f"]);
            }
        }
    }

    #[cfg(not(windows))]
    mod imp {
        fn path() -> std::path::PathBuf {
            let cfg = crate::config::dir();
            cfg.parent().unwrap_or(&cfg).join("autostart/ringleader-daemon.desktop")
        }
        pub fn get() -> bool {
            path().exists()
        }
        pub fn set(on: bool) {
            let p = path();
            if !on {
                let _ = std::fs::remove_file(p);
                return;
            }
            let Ok(exe) = std::env::current_exe() else { return };
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(
                p,
                format!(
                    "[Desktop Entry]\nType=Application\nName=RingLeader daemon\nExec=\"{}\" daemon\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
                    exe.display()
                ),
            );
        }
    }

    pub use imp::{get, set};
}
