use crate::config::Sensor;
use std::ffi::{c_char, c_void};

/// Temperature sources:
/// - Linux: every hwmon temp input (CPU, GPU, NVMe, board...).
/// - NVIDIA GPUs via NVML (both OSes, ships with the driver).
/// - Windows: HWiNFO shared memory, if HWiNFO runs with "Shared Memory Support" on.
///   Windows has no driver-free CPU temperature API.
pub struct Sensors {
    nvml: Option<Nvml>,
}

impl Sensors {
    pub fn new() -> Self {
        Self { nvml: unsafe { Nvml::load() } }
    }

    pub fn read(&mut self) -> Vec<Sensor> {
        let mut out = Vec::new();
        #[cfg(target_os = "linux")]
        hwmon(&mut out);
        #[cfg(windows)]
        unsafe { hwinfo(&mut out) };
        if let Some(n) = &self.nvml {
            n.read(&mut out);
        }
        // Make ids unique so a fan curve always binds to the same reading.
        let mut seen = std::collections::HashMap::new();
        for s in &mut out {
            let n: &mut u32 = seen.entry(s.id.clone()).or_default();
            *n += 1;
            if *n > 1 {
                s.id = format!("{} #{n}", s.id);
            }
        }
        out
    }
}

#[cfg(target_os = "linux")]
fn hwmon(out: &mut Vec<Sensor>) {
    use std::fs::read_to_string;
    let Ok(dirs) = std::fs::read_dir("/sys/class/hwmon") else { return };
    let mut dirs: Vec<_> = dirs.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        let name = read_to_string(d.join("name")).unwrap_or_default().trim().to_string();
        let Ok(files) = std::fs::read_dir(&d) else { continue };
        let mut inputs: Vec<String> = files
            .flatten()
            .filter_map(|f| f.file_name().into_string().ok())
            .filter(|f| f.starts_with("temp") && f.ends_with("_input"))
            .collect();
        inputs.sort();
        for f in inputs {
            let base = f.trim_end_matches("_input");
            let Some(v) = read_to_string(d.join(&f)).ok().and_then(|s| s.trim().parse::<f32>().ok()) else { continue };
            let label = read_to_string(d.join(format!("{base}_label"))).map(|s| s.trim().to_string()).unwrap_or(base.into());
            out.push(Sensor { id: format!("{name} {label}"), temp: v / 1000.0 });
        }
    }
}

type NvmlFn0 = unsafe extern "C" fn() -> i32;
type NvmlCount = unsafe extern "C" fn(*mut u32) -> i32;
type NvmlHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type NvmlName = unsafe extern "C" fn(*mut c_void, *mut c_char, u32) -> i32;
type NvmlTemp = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;

struct Nvml {
    _lib: libloading::Library,
    temp: NvmlTemp,
    gpus: Vec<(String, *mut c_void)>,
}

impl Nvml {
    unsafe fn load() -> Option<Self> {
        unsafe {
            #[cfg(windows)]
            let lib = libloading::Library::new("nvml.dll").ok()?;
            #[cfg(not(windows))]
            let lib = libloading::Library::new("libnvidia-ml.so.1").ok()?;
            let init: NvmlFn0 = *lib.get(b"nvmlInit_v2\0").ok()?;
            let count: NvmlCount = *lib.get(b"nvmlDeviceGetCount_v2\0").ok()?;
            let handle: NvmlHandle = *lib.get(b"nvmlDeviceGetHandleByIndex_v2\0").ok()?;
            let name: NvmlName = *lib.get(b"nvmlDeviceGetName\0").ok()?;
            let temp: NvmlTemp = *lib.get(b"nvmlDeviceGetTemperature\0").ok()?;
            if init() != 0 {
                return None;
            }
            let mut n = 0;
            count(&mut n);
            let mut gpus = Vec::new();
            for i in 0..n {
                let mut h = std::ptr::null_mut();
                if handle(i, &mut h) != 0 {
                    continue;
                }
                let mut buf = [0 as c_char; 96];
                name(h, buf.as_mut_ptr(), buf.len() as u32);
                let s = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
                gpus.push((s, h));
            }
            Some(Nvml { _lib: lib, temp, gpus })
        }
    }

    fn read(&self, out: &mut Vec<Sensor>) {
        for (name, h) in &self.gpus {
            let mut t = 0u32;
            // 0 = NVML_TEMPERATURE_GPU
            if unsafe { (self.temp)(*h, 0, &mut t) } == 0 {
                out.push(Sensor { id: name.clone(), temp: t as f32 });
            }
        }
    }
}

/// HWiNFO "Global\HWiNFO_SENS_SM2" layout (packed):
/// header: sig u32, ver u32, rev u32, poll_time i64, sensor_off u32 @20, sensor_size @24, sensor_num @28,
///         reading_off @32, reading_size @36, reading_num @40
/// sensor: id u32, inst u32, name_orig [128], name_user [128] @136
/// reading: type u32 (1 = temp), sensor_idx u32 @4, id u32, label_orig [128], label_user [128] @140,
///          unit [16], value f64 @284
#[cfg(windows)]
unsafe fn hwinfo(out: &mut Vec<Sensor>) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Memory::*;
    unsafe {
        let name: Vec<u16> = "Global\\HWiNFO_SENS_SM2\0".encode_utf16().collect();
        let h = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
        if h.is_null() {
            return;
        }
        let view = MapViewOfFile(h, FILE_MAP_READ, 0, 0, 0);
        if !view.Value.is_null() {
            let p = view.Value as *const u8;
            let u = |off: usize| std::ptr::read_unaligned(p.add(off) as *const u32) as usize;
            let s = |at: *const u8| std::ffi::CStr::from_ptr(at as *const c_char).to_string_lossy().trim().to_string();
            let (s_off, s_size, s_num) = (u(20), u(24), u(28));
            let (r_off, r_size, r_num) = (u(32), u(36), u(40));
            for i in 0..r_num {
                let r = p.add(r_off + i * r_size);
                if std::ptr::read_unaligned(r as *const u32) != 1 {
                    continue;
                }
                let si = std::ptr::read_unaligned(r.add(4) as *const u32) as usize;
                let sensor = if si < s_num { s(p.add(s_off + si * s_size + 136)) } else { String::new() };
                let value = std::ptr::read_unaligned(r.add(284) as *const f64);
                out.push(Sensor { id: format!("{sensor}: {}", s(r.add(140))), temp: value as f32 });
            }
            UnmapViewOfFile(view);
        }
        CloseHandle(h);
    }
}
