//! Thermaltake Riing Quad controller (USB 264A:2260..226F, one PID per box number).
//! Protocol recovered from TT RGB Plus 2.x (Quad module), verified on firmware 3.0.3:
//!   out report: 0x00 + 192 bytes, in report: 64 bytes
//!   FE 33                     init
//!   33 50                  -> 33 50 maj min patch           firmware
//!   33 51 port             -> 33 51 port ?? duty rpm_lo rpm_hi
//!   32 51 port 01 duty        set fan duty (0..100 %)
//!   32 52 port 24 G R B ...   per-LED colors (mode 0x24 = direct)
//! Ports are 1-based on the wire, 0-based here.
use crate::effects::Rgb;
use hidapi::{HidApi, HidDevice};

pub const VID: u16 = 0x264A;
pub const PIDS: std::ops::RangeInclusive<u16> = 0x2260..=0x226F;
/// (192 - 4 header bytes) / 3
pub const MAX_LEDS: usize = 62;

pub struct Quad {
    dev: HidDevice,
    pub firmware: String,
    answering: std::cell::Cell<bool>,
}

type Res<T> = Result<T, String>;

impl Quad {
    // ponytail: first box only; the original supports up to 16 chained boxes (PID 0x2260 + box).
    pub fn open(api: &mut HidApi) -> Res<Self> {
        api.refresh_devices().map_err(|e| e.to_string())?;
        let info = api
            .device_list()
            .find(|d| d.vendor_id() == VID && PIDS.contains(&d.product_id()))
            .ok_or("Riing Quad controller not found")?;
        let dev = info.open_device(api).map_err(|e| format!("open failed: {e}"))?;
        let mut q = Quad { dev, firmware: String::new(), answering: true.into() };
        q.send(&[0xFE, 0x33])?;
        // Some controllers stop answering (seen after another app closed mid-transfer) while
        // still obeying writes, so a missing reply isn't fatal: fans/LEDs only need writes.
        q.firmware = match q.query(&[0x33, 0x50]) {
            Ok(r) => format!("{}.{}.{}", r[2], r[3], r[4]),
            Err(_) => "?".into(),
        };
        Ok(q)
    }

    fn write(&self, data: &[u8]) -> Res<()> {
        let mut buf = [0u8; 193];
        buf[1..1 + data.len()].copy_from_slice(data);
        self.dev.write(&buf).map_err(|e| format!("write failed: {e}"))?;
        Ok(())
    }

    /// Write and wait for the reply echoing the command header (up to `ms`).
    /// Commands are strictly one-at-a-time like the original app: write, then read the ack.
    fn cmd(&self, data: &[u8], ms: i32, match_port: bool) -> Res<Option<[u8; 64]>> {
        self.write(data)?;
        let mut r = [0u8; 64];
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms.max(0) as u64);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now()).as_millis() as i32;
            if self.dev.read_timeout(&mut r, left).map_err(|e| e.to_string())? == 0 {
                return Ok(None);
            }
            if r[..2] == data[..2] && (!match_port || r[2] == data[2]) {
                return Ok(Some(r));
            }
        }
    }

    /// Set-type command. Waits for the ack while the controller answers; once it stops
    /// (it still obeys writes), stops waiting so LED animation doesn't stall.
    fn send(&self, data: &[u8]) -> Res<()> {
        let ms = if self.answering.get() { 30 } else { 0 };
        if self.cmd(data, ms, false)?.is_none() && ms > 0 {
            self.answering.set(false);
        }
        Ok(())
    }

    fn query(&self, data: &[u8]) -> Res<[u8; 64]> {
        let r = self.cmd(data, 50, data.len() > 2)?.ok_or_else(|| format!("no reply to {:02X} {:02X}", data[0], data[1]))?;
        self.answering.set(true);
        Ok(r)
    }

    pub fn answering(&self) -> bool {
        self.answering.get()
    }

    pub fn set_speed(&self, port: usize, duty: u8) -> Res<()> {
        self.send(&[0x32, 0x51, port as u8 + 1, 0x01, duty.min(100)])
    }

    /// Returns (duty %, rpm).
    pub fn read_fan(&self, port: usize) -> Res<(u8, u16)> {
        let r = self.query(&[0x33, 0x51, port as u8 + 1])?;
        Ok((r[4], u16::from_le_bytes([r[5], r[6]])))
    }

    pub fn set_leds(&self, port: usize, leds: &[Rgb]) -> Res<()> {
        let mut data = vec![0x32, 0x52, port as u8 + 1, 0x24];
        for [r, g, b] in leds.iter().take(MAX_LEDS) {
            data.extend([*g, *r, *b]);
        }
        self.send(&data)
    }
}
