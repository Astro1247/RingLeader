use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub const PORTS: usize = 5;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    pub ports: Vec<Port>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Port {
    pub name: String,
    pub leds: u8,
    pub fan: Fan,
    pub light: Light,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum FanMode {
    Fixed,
    Curve,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Fan {
    pub mode: FanMode,
    /// Fixed duty in percent.
    pub speed: u8,
    /// Sensor id as reported in status (e.g. "coretemp Package id 0").
    pub sensor: String,
    /// (°C, %) points, sorted by temperature.
    pub curve: Vec<[f32; 2]>,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Off,
    Static,
    Breathe,
    Spectrum,
    Rainbow,
    Comet,
    Temperature,
}

impl Effect {
    pub const ALL: [Effect; 7] = [
        Effect::Off,
        Effect::Static,
        Effect::Breathe,
        Effect::Spectrum,
        Effect::Rainbow,
        Effect::Comet,
        Effect::Temperature,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Effect::Off => "Off",
            Effect::Static => "Static",
            Effect::Breathe => "Breathe",
            Effect::Spectrum => "Spectrum",
            Effect::Rainbow => "Rainbow",
            Effect::Comet => "Comet",
            Effect::Temperature => "Temperature",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Light {
    pub effect: Effect,
    pub color: [u8; 3],
    pub color2: [u8; 3],
    /// Animation speed multiplier.
    pub speed: f32,
    /// 0..=100
    pub brightness: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self { ports: (1..=PORTS).map(|i| Port { name: format!("Fan {i}"), ..Port::default() }).collect() }
    }
}

impl Default for Port {
    fn default() -> Self {
        // ponytail: 12 LEDs = Riing Plus / FanType 0 in the original app; pick yours in the UI.
        Self { name: "Fan".into(), leds: 12, fan: Fan::default(), light: Light::default() }
    }
}

impl Default for Fan {
    fn default() -> Self {
        Self {
            mode: FanMode::Fixed,
            speed: 50,
            sensor: String::new(),
            curve: vec![[30.0, 30.0], [50.0, 40.0], [65.0, 60.0], [75.0, 80.0], [85.0, 100.0]],
        }
    }
}

impl Default for Light {
    fn default() -> Self {
        Self { effect: Effect::Rainbow, color: [0, 200, 255], color2: [0, 0, 0], speed: 1.0, brightness: 100 }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Status {
    /// Unix seconds of last daemon write; used as liveness heartbeat.
    pub updated: u64,
    pub connected: bool,
    pub device: String,
    pub firmware: String,
    pub error: String,
    pub ports: Vec<PortStatus>,
    pub sensors: Vec<Sensor>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct PortStatus {
    pub speed: u8,
    pub rpm: u16,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct Sensor {
    pub id: String,
    pub temp: f32,
}

pub fn dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.unwrap_or_else(|| PathBuf::from(".")).join("ringleader")
}

pub fn config_path() -> PathBuf {
    dir().join("config.toml")
}

pub fn status_path() -> PathBuf {
    dir().join("status.toml")
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub fn load<T: for<'de> Deserialize<'de> + Default>(p: &std::path::Path) -> T {
    std::fs::read_to_string(p).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

/// Write via temp file + rename so readers never see a half-written file.
pub fn save<T: Serialize>(p: &std::path::Path, v: &T) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, toml::to_string(v).map_err(std::io::Error::other)?)?;
    std::fs::rename(tmp, p)
}

impl Config {
    pub fn load() -> Self {
        let mut c: Config = load(&config_path());
        c.ports.resize_with(PORTS, Port::default);
        c
    }
}
