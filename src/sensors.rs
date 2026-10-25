use crate::config::Sensor;
use std::ffi::{c_char, c_void};

/// Temperature sources:
/// - Linux: every hwmon temp input (CPU, GPU, NVMe, board...).
/// - NVIDIA GPUs via NVML (both OSes, ships with the driver).
/// - Windows CPU: the PawnIO driver (Windows has no driver-free CPU temperature API),
///   plus HWiNFO shared memory if HWiNFO runs with "Shared Memory Support" on.
pub struct Sensors {
    nvml: Option<Nvml>,
    #[cfg(windows)]
    cpu: Result<pawnio::Cpu, CpuIssue>,
}

/// Why there's no CPU temperature on Windows; shown in the window with a fix button.
#[derive(Clone, Copy, PartialEq, Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum CpuIssue {
    NoDriver,
    NoAdmin,
    Unsupported,
}

impl Sensors {
    pub fn new() -> Self {
        Self {
            nvml: unsafe { Nvml::load() },
            #[cfg(windows)]
            cpu: pawnio::Cpu::open(),
        }
    }

    /// A temperature source failed to open (driver not loaded yet, or not installed).
    pub fn incomplete(&self) -> bool {
        self.nvml.is_none() || self.cpu_issue().is_some()
    }

    pub fn cpu_issue(&self) -> Option<CpuIssue> {
        #[cfg(windows)]
        return self.cpu.as_ref().err().copied();
        #[cfg(not(windows))]
        None
    }

    pub fn read(&mut self) -> Vec<Sensor> {
        let mut out = Vec::new();
        #[cfg(target_os = "linux")]
        hwmon(&mut out);
        #[cfg(windows)]
        if let Ok(cpu) = &self.cpu
            && let Some(t) = cpu.temp()
        {
            out.push(Sensor { id: "CPU Package".into(), temp: t });
        }
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

/// CPU package temperature through the PawnIO driver (https://pawnio.eu), the way
/// LibreHardwareMonitor does it: load a signed module, then call its ioctls.
#[cfg(windows)]
mod pawnio {
    use super::CpuIssue;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};

    const DEVICE_TYPE: u32 = 41394 << 16;
    const IOCTL_LOAD: u32 = DEVICE_TYPE | (0x821 << 2);
    const IOCTL_EXEC: u32 = DEVICE_TYPE | (0x841 << 2);
    const INTEL_MSR: &[u8] = include_bytes!("../third_party/pawnio/IntelMSR.bin");
    const AMD_17H: &[u8] = include_bytes!("../third_party/pawnio/AMDFamily17.bin");

    pub struct Cpu {
        h: HANDLE,
        amd: bool,
        /// Intel TjMax (°C); the MSR reports distance below it.
        tjmax: f32,
        /// AMD SMN access must be serialized with other tools via this global mutex.
        pci_mutex: HANDLE,
    }

    impl Cpu {
        pub fn open() -> Result<Self, CpuIssue> {
            let r = std::arch::x86_64::__cpuid(0);
            let vendor: Vec<u8> = [r.ebx, r.edx, r.ecx].iter().flat_map(|v| v.to_le_bytes()).collect();
            let vendor = String::from_utf8_lossy(&vendor);
            let amd = vendor == "AuthenticAMD";
            if !amd && vendor != "GenuineIntel" {
                return Err(CpuIssue::Unsupported);
            }
            let path: Vec<u16> = r"\\?\GLOBALROOT\Device\PawnIO".encode_utf16().chain([0]).collect();
            let h = unsafe {
                CreateFileW(path.as_ptr(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE, null(), OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, null_mut())
            };
            if h == INVALID_HANDLE_VALUE {
                return Err(if unsafe { GetLastError() } == ERROR_ACCESS_DENIED { CpuIssue::NoAdmin } else { CpuIssue::NoDriver });
            }
            let mut cpu = Cpu { h, amd, tjmax: 100.0, pci_mutex: null_mut() };
            let blob = if amd { AMD_17H } else { INTEL_MSR };
            let mut ret = 0;
            // The module refuses to load on unsupported families (e.g. pre-Zen AMD).
            if unsafe { DeviceIoControl(h, IOCTL_LOAD, blob.as_ptr() as _, blob.len() as u32, null_mut(), 0, &mut ret, null_mut()) } == 0 {
                return Err(CpuIssue::Unsupported);
            }
            if amd {
                let name: Vec<u16> = r"Global\Access_PCI".encode_utf16().chain([0]).collect();
                cpu.pci_mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
            } else if let Some(v) = cpu.exec("ioctl_read_msr", &[0x1A2]) {
                // IA32_TEMPERATURE_TARGET bits 23:16
                let t = (v >> 16) & 0xFF;
                if t > 0 {
                    cpu.tjmax = t as f32;
                }
            }
            Ok(cpu)
        }

        fn exec(&self, name: &str, input: &[u64]) -> Option<u64> {
            let mut buf = vec![0u8; 32];
            buf[..name.len().min(31)].copy_from_slice(&name.as_bytes()[..name.len().min(31)]);
            buf.extend(input.iter().flat_map(|v| v.to_le_bytes()));
            let mut out = [0u64; 1];
            let mut ret = 0;
            let ok = unsafe { DeviceIoControl(self.h, IOCTL_EXEC, buf.as_ptr() as _, buf.len() as u32, out.as_mut_ptr() as _, 8, &mut ret, null_mut()) };
            (ok != 0 && ret == 8).then_some(out[0])
        }

        pub fn temp(&self) -> Option<f32> {
            if !self.amd {
                // IA32_PACKAGE_THERM_STATUS bits 22:16 = degrees below TjMax
                let v = self.exec("ioctl_read_msr", &[0x1B1])?;
                return Some(self.tjmax - ((v >> 16) & 0x7F) as f32);
            }
            // THM_TCON_CUR_TMP: bits 31:21 in 1/8 °C; bit 19 selects the -49 °C range.
            unsafe { WaitForSingleObject(self.pci_mutex, 100) };
            let v = self.exec("ioctl_read_smn", &[0x0005_9800]);
            unsafe { ReleaseMutex(self.pci_mutex) };
            let v = v?;
            let t = ((v >> 21) & 0x7FF) as f32 / 8.0;
            Some(if v & (1 << 19) != 0 { t - 49.0 } else { t })
        }
    }

    impl Drop for Cpu {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.h);
                if !self.pci_mutex.is_null() {
                    CloseHandle(self.pci_mutex);
                }
            }
        }
    }
}
