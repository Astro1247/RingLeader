# RingLeader

Small, fast replacement for Thermaltake TT RGB Plus for the **Riing Quad** fan controller
(USB `264A:2260`–`226F`). Windows and Linux, one ~8 MB binary.

- `ringleader` opens the window (and starts the service if it isn't running).
- `ringleader daemon` is the headless service. It owns the controller and keeps fans
  controlled with the window closed. "Start with system" in the window registers it.

The window and the service talk through two files in `%APPDATA%\ringleader` / `~/.config/ringleader`:
`config.toml` (yours, hand-editable) and `status.toml` (written by the service every second).

**Close TT RGB Plus first** (and remove it from startup), or the two apps fight over the LEDs.

## Build

    cargo build --release      # target/release/ringleader(.exe)

Linux needs no system libraries to build. To run without root:

    sudo cp linux/60-ringleader.rules /etc/udev/rules.d/ && sudo udevadm control --reload && sudo udevadm trigger

## Temperatures (for fan curves)

- Linux: everything in `/sys/class/hwmon` (CPU, AMD GPU, NVMe, ...).
- NVIDIA GPUs via NVML, on both OSes.
- Windows CPU: Windows has no driver-free API for this, so it is read from **HWiNFO**
  (enable *Settings → Shared Memory Support*).

If a curve's sensor disappears, that fan goes to 100%.

## Protocol (recovered from TT RGB Plus, verified on firmware 3.0.3)

Output report: `00` + 192 bytes. Input report: 64 bytes. Ports are 1-5.

| Command | Reply | Meaning |
|---|---|---|
| `FE 33` | `FE 33 FC ..` | init |
| `33 50` | `33 50 maj min patch` | firmware |
| `33 51 port` | `33 51 port ?? duty rpm_lo rpm_hi` | read fan |
| `32 51 port 01 duty` | `32 51 FC ..` | set duty 0–100 % |
| `32 52 port 24 G R B ...` | `32 52 FC ..` | per-LED colors, max 62 LEDs |

The controller has no built-in effects in this mode, so effects are rendered by the service at 30 fps.

LEDs per fan, by the original app's fan type: Pure Plus 9, Riing Plus 12, Riing Trio 30
(rings 12/12/6), Riing Quad 54 (rings 18/18/9/9).

Known quirk: if an app is killed while writing, the controller can stop *answering*
(RPM reads 0) while still obeying commands. Replugging its USB fixes the readings.
