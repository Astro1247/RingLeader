//! OS integration: launching the window/service, autostart, elevation and controller USB reset.
use crate::config;
use std::process::{Command, Stdio};

/// Command with no console window and no inherited stdio.
fn quiet(program: &str) -> Command {
    let mut c = Command::new(program);
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut c, 0x0800_0000); // CREATE_NO_WINDOW
    c
}

fn spawn_self(arg: Option<&str>) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut c = quiet(&exe.to_string_lossy());
    c.args(arg);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    let _ = c.spawn();
}

pub fn open_gui() {
    spawn_self(None);
}

/// Start the service unless one is running. On Windows prefer the tray task: with no service
/// running it becomes the service, elevated, and running it needs no UAC prompt. (The SYSTEM boot
/// task can't be started, or even seen, without admin rights.)
pub fn start_daemon() {
    if crate::daemon::lock().is_none() {
        return;
    }
    #[cfg(windows)]
    if autostart::get() && quiet("schtasks").args(["/run", "/tn", autostart::TRAY_TASK]).status().is_ok_and(|s| s.success()) {
        return;
    }
    spawn_self(Some("daemon"));
}

/// Ask a running service to do something ("quit", "reset"); it polls this file every second.
pub fn send_command(cmd: &str) {
    let _ = std::fs::write(config::dir().join("command"), cmd);
}

pub fn take_command() -> Option<String> {
    let p = config::dir().join("command");
    let c = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(p);
    Some(c.trim().to_string())
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut e = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut len = 0;
        let ok = GetTokenInformation(token, TokenElevation, &mut e as *mut _ as _, size_of::<TOKEN_ELEVATION>() as u32, &mut len);
        CloseHandle(token);
        ok != 0 && e.TokenIsElevated != 0
    }
}

/// Run a PowerShell script elevated (one UAC prompt) and wait for it. False if declined or failed.
#[cfg(windows)]
pub fn run_elevated(script: &str) -> bool {
    let path = config::dir().join("elevated.ps1");
    if std::fs::create_dir_all(config::dir()).is_err() || std::fs::write(&path, script).is_err() {
        return false;
    }
    let path = path.display().to_string().replace('\'', "''");
    let ok = quiet("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "$p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden \
                 -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','\"{path}\"'; exit $p.ExitCode"
            ),
        ])
        .status()
        .is_ok_and(|s| s.success());
    let _ = std::fs::remove_file(config::dir().join("elevated.ps1"));
    ok
}

/// Power-cycle the controller's USB connection (what "replug it" means, without opening the case).
/// Windows needs admin: done silently when elevated, otherwise only with `prompt` (UAC).
pub fn reset_controller(prompt: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        let out = quiet("pnputil")
            .args(["/enum-devices", "/connected"])
            .stdout(Stdio::piped())
            .output()
            .map_err(|e| e.to_string())?;
        // Match the id itself, not the (localized) "Instance ID:" label.
        let text = String::from_utf8_lossy(&out.stdout);
        let id = text
            .lines()
            .filter_map(|l| l.find("USB\\VID_264A&PID_226").map(|i| l[i..].trim().to_string()))
            .next()
            .ok_or("controller not found")?;
        if is_elevated() {
            let ok = quiet("pnputil").args(["/restart-device", &id]).status().is_ok_and(|s| s.success());
            return if ok { Ok(()) } else { Err("pnputil failed".into()) };
        }
        if prompt && run_elevated(&format!("pnputil /restart-device '{id}'; exit $LASTEXITCODE")) {
            return Ok(());
        }
        Err("restarting the controller needs admin rights".into())
    }
    #[cfg(target_os = "linux")]
    {
        let _ = prompt;
        linux_usb_reset()
    }
}

/// USBDEVFS_RESET on /dev/bus/usb/BBB/DDD; the udev rule grants the logged-in user access.
#[cfg(target_os = "linux")]
fn linux_usb_reset() -> Result<(), String> {
    use std::fs::read_to_string;
    use std::os::fd::AsRawFd;
    let read = |d: &std::path::Path, f: &str| read_to_string(d.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
    let dev = std::fs::read_dir("/sys/bus/usb/devices")
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .find(|d| read(d, "idVendor") == "264a" && read(d, "idProduct").starts_with("226"))
        .ok_or("controller not found")?;
    let node = format!("/dev/bus/usb/{:0>3}/{:0>3}", read(&dev, "busnum"), read(&dev, "devnum"));
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(&node)
        .map_err(|e| format!("{node}: {e} (install linux/60-ringleader.rules)"))?;
    const USBDEVFS_RESET: libc::c_ulong = 0x5514; // _IO('U', 20)
    if unsafe { libc::ioctl(f.as_raw_fd(), USBDEVFS_RESET as _, 0) } < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

/// "Start with system" starts only the service (tray icon); the window opens on demand.
pub mod autostart {
    #[cfg(windows)]
    pub const TASK: &str = "RingLeader";
    #[cfg(windows)]
    pub const TRAY_TASK: &str = "RingLeader Tray";

    /// Windows: two scheduled tasks, created with one UAC prompt.
    /// - TASK runs the service as SYSTEM at boot, so fans and LEDs are controlled before anyone
    ///   signs in, and CPU temperature / controller restarts work without prompts.
    /// - TRAY_TASK runs `tray` at logon: the tray icon for that service, or the service itself
    ///   (elevated) if the boot one isn't running.
    ///
    /// Checks TRAY_TASK: it belongs to the user, while TASK is invisible without admin rights.
    #[cfg(windows)]
    pub fn get() -> bool {
        super::quiet("schtasks").args(["/query", "/tn", TRAY_TASK]).status().is_ok_and(|s| s.success())
    }

    #[cfg(windows)]
    pub fn set(on: bool) -> bool {
        script(on).is_some_and(|s| super::run_elevated(&s))
    }

    /// PowerShell that (un)registers the task; run it elevated.
    #[cfg(windows)]
    pub fn script(on: bool) -> Option<String> {
        let exe = std::env::current_exe().ok()?;
        let user = format!(
            "{}\\{}",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        let q = |s: &str| s.replace('\'', "''");
        let script = if on {
            format!(
                "$ErrorActionPreference = 'Stop'\n\
                 $s = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit 0 -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)\n\
                 $a = New-ScheduledTaskAction -Execute '{exe}' -Argument 'daemon \"{dir}\"'\n\
                 $t = New-ScheduledTaskTrigger -AtStartup\n\
                 $p = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest\n\
                 Register-ScheduledTask -TaskName '{TASK}' -Action $a -Trigger $t -Settings $s -Principal $p -Force | Out-Null\n\
                 $a = New-ScheduledTaskAction -Execute '{exe}' -Argument 'tray'\n\
                 $t = New-ScheduledTaskTrigger -AtLogOn -User '{user}'\n\
                 $p = New-ScheduledTaskPrincipal -UserId '{user}' -RunLevel Highest -LogonType Interactive\n\
                 Register-ScheduledTask -TaskName '{TRAY_TASK}' -Action $a -Trigger $t -Settings $s -Principal $p -Force | Out-Null\n\
                 # Swap the running service/tray for the tasks now instead of at the next boot. A tray\n\
                 # would take over a service that merely quit, so stop both together.\n\
                 # ponytail: hard stop between USB writes is fine in practice; the window survives.\n\
                 Get-CimInstance Win32_Process -Filter \"Name='ringleader.exe'\" | ? {{ $_.CommandLine -match '\\s(daemon|tray)' }} | % {{ Stop-Process -Id $_.ProcessId -Force }}\n\
                 Start-Sleep -Milliseconds 500\n\
                 Start-ScheduledTask -TaskName '{TASK}'\n\
                 Start-Sleep 2\n\
                 Start-ScheduledTask -TaskName '{TRAY_TASK}'\n",
                exe = q(&exe.display().to_string()),
                dir = q(&crate::config::dir().display().to_string()),
                user = q(&user),
            )
        } else {
            // Stopping the SYSTEM service lets the running tray take over the controller.
            format!(
                "Stop-ScheduledTask -TaskName '{TASK}' -ErrorAction SilentlyContinue\n\
                 Unregister-ScheduledTask -TaskName '{TASK}' -Confirm:$false -ErrorAction SilentlyContinue\n\
                 Unregister-ScheduledTask -TaskName '{TRAY_TASK}' -Confirm:$false -ErrorAction SilentlyContinue\n"
            )
        };
        Some(script)
    }

    #[cfg(not(windows))]
    fn path() -> std::path::PathBuf {
        let cfg = crate::config::dir();
        cfg.parent().unwrap_or(&cfg).join("autostart/ringleader-daemon.desktop")
    }

    #[cfg(not(windows))]
    pub fn get() -> bool {
        path().exists()
    }

    #[cfg(not(windows))]
    pub fn set(on: bool) -> bool {
        let p = path();
        if !on {
            return std::fs::remove_file(p).is_ok();
        }
        let Ok(exe) = std::env::current_exe() else { return false };
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        std::fs::write(
            p,
            format!(
                "[Desktop Entry]\nType=Application\nName=RingLeader\nComment=Fan and RGB control service\nExec=\"{}\" daemon\nIcon=ringleader\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
                exe.display()
            ),
        )
        .is_ok()
    }
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn autostart_script_escapes() {
        let s = super::autostart::script(true).unwrap();
        assert!(s.contains("-Argument 'daemon \""));
        assert!(s.contains(r"? { $_.CommandLine -match '\s(daemon|tray)' } | % { Stop-Process"));
        assert!(s.contains("-Filter \"Name='ringleader.exe'\""));
        assert!(s.contains("Start-ScheduledTask -TaskName 'RingLeader Tray'"));
    }
}
