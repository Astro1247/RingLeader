# RingLeader

Fast, lightweight fan and RGB control for the Thermaltake **Riing Quad** fan controller
(USB `264A:2260`–`226F`), on Windows and Linux. A small replacement for TT RGB Plus: a single
~6 MB binary with no runtime to install.

- Fixed speed or a draggable temperature curve per fan, with a 100% fail-safe
- Software LED effects: static, breathe, spectrum, rainbow, comet, temperature
- Control several fans at once: Ctrl/Shift-click fans (or **All**) and edit them as a group
- CPU and GPU temperatures: Linux hwmon, NVIDIA via NVML, Windows CPU via PawnIO
- A tray icon and background service keep fans controlled with the window closed
- **Restart controller** power-cycles its USB connection, so you never have to open the case

Made by Astro · [ZonAstro.com](https://zonastro.com)

## Usage

    ringleader           # open the window (starts the service if needed)
    ringleader daemon    # the background service: owns the controller, shows the tray icon
    ringleader tray      # tray icon for a running service, or become the service if none runs

**Close TT RGB Plus first** (and remove it from startup), or the two apps fight over the LEDs.

Closing the window leaves the service and its tray icon running. Click the tray icon to reopen
the window; use the tray's **Quit** to stop the service. Turn on **Start with system** in the
window to run the service automatically. On Windows that creates two scheduled tasks (one UAC
prompt): **RingLeader** runs the service as SYSTEM at boot, so fans and LEDs are under control
before anyone signs in, and **RingLeader Tray** adds the tray icon at login. On a fresh Windows
install, turning that switch on (plus **Install PawnIO** for CPU temperature) is the whole setup.
On Linux it starts the service at login.

Settings live in `%APPDATA%\ringleader\config.toml` (Windows) or `~/.config/ringleader/config.toml`
(Linux). The file is plain TOML and safe to edit by hand; the service reloads it on change.

## Download

Prebuilt binaries for Windows (x64) and Linux (x64, glibc 2.35+: Ubuntu 22.04+, Debian 12+) are
attached to each [release](../../releases), with `SHA256SUMS`.

## Install

### Windows

Download the Windows zip (or build, below), put `ringleader.exe` anywhere, and run it.

For **CPU temperature**, Windows needs a kernel driver; there is no driver-free API. RingLeader
uses [PawnIO](https://pawnio.eu), the free, signed driver LibreHardwareMonitor also uses. The
window offers a one-click setup: it installs PawnIO with winget and sets up the startup tasks
above (one UAC prompt). Running as administrator also
lets the service restart a stuck controller automatically.

### Linux (Ubuntu/Debian first, any distro should work)

    # from the release tarball (or target/release/ after building)
    sudo install -m755 ringleader /usr/local/bin/
    sudo cp linux/60-ringleader.rules /etc/udev/rules.d/
    sudo udevadm control --reload && sudo udevadm trigger

The udev rule lets the logged-in user drive the controller and restart it without root. The
tray icon uses StatusNotifierItem: KDE, XFCE and others support it natively, and Ubuntu's GNOME
does through its built-in AppIndicator extension.

## Build

    cargo build --release      # target/release/ringleader(.exe)

Rust 1.92+. No system libraries are needed on Linux or Windows.

Releases are built by GitHub Actions: push a tag matching the `Cargo.toml` version
(`git tag -a v1.2.0 && git push origin v1.2.0`) and `.github/workflows/release.yml` builds both
platforms and publishes the release, using the tag message as release notes.

## Protocol

Recovered from TT RGB Plus and verified on firmware 3.0.3.
Output report: `00` + 192 bytes. Input report: 64 bytes. Ports are 1–5.

| Command | Reply | Meaning |
|---|---|---|
| `FE 33` | `FE 33 FC ..` | init |
| `33 50` | `33 50 maj min patch` | firmware |
| `33 51 port` | `33 51 port ?? duty rpm_lo rpm_hi` | read fan |
| `32 51 port 01 duty` | `32 51 FC ..` | set duty 0–100 % |
| `32 52 port 24 G R B ...` | `32 52 FC ..` | per-LED colors, max 62 LEDs |

The controller has no built-in effects in this mode, so the service renders effects at 30 fps.
Commands go one at a time: write, then wait for the acknowledgement, as the original does.

LEDs per fan, by the original app's fan type: Pure Plus 9, Riing Plus 12, Riing Trio 30
(rings 12/12/6), Riing Quad 54 (rings 18/18/9/9).

Known quirk: if a program is killed in the middle of a write, the controller can stop
*answering* (RPM reads 0) while still obeying commands. A USB reset fixes it.
**Restart controller** does that, and the service does it by itself when it has the rights.
The service always stops between writes when quit from the tray or at logoff.

## License

MIT, see [LICENSE](LICENSE). Not affiliated with Thermaltake; the protocol was
reverse-engineered for interoperability.

`third_party/pawnio/` contains unmodified PawnIO modules under the LGPL-2.1; see its README.
