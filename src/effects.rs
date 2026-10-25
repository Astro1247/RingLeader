use crate::config::{Effect, Light};
use std::f32::consts::TAU;

pub type Rgb = [u8; 3];

/// Render one frame of `l` for a fan with `n` LEDs at time `t` seconds.
/// `temp` (°C) drives the Temperature effect. Shared by daemon and GUI preview.
pub fn render(l: &Light, n: usize, t: f32, temp: Option<f32>) -> Vec<Rgb> {
    let t = t * l.speed;
    let c = l.color;
    let frame: Vec<Rgb> = match l.effect {
        Effect::Off => vec![[0; 3]; n],
        Effect::Static => vec![c; n],
        Effect::Breathe => vec![scale(c, 0.5 - 0.5 * (t * TAU / 3.0).cos()); n],
        Effect::Spectrum => vec![hsv(t * 40.0); n],
        Effect::Rainbow => (0..n).map(|i| hsv(i as f32 / n as f32 * 360.0 + t * 120.0)).collect(),
        Effect::Comet => {
            let head = (t / 1.5).rem_euclid(1.0) * n as f32;
            (0..n)
                .map(|i| {
                    let behind = (head - i as f32).rem_euclid(n as f32);
                    let k = (1.0 - behind / (n as f32 * 0.4)).max(0.0);
                    lerp(l.color2, c, k * k)
                })
                .collect()
        }
        // Blue at temp_range[0] .. red at temp_range[1]; the default 41..65 is TT RGB Plus's scale.
        // No reading = white so it's obvious.
        Effect::Temperature => match temp {
            Some(x) => {
                let [lo, hi] = l.temp_range;
                vec![hsv(240.0 * (1.0 - ((x - lo) / (hi - lo).max(1.0)).clamp(0.0, 1.0))); n]
            }
            None => vec![[255; 3]; n],
        },
    };
    let b = l.brightness.min(100) as f32 / 100.0;
    frame.into_iter().map(|p| scale(p, b)).collect()
}

/// Linear interpolation over (°C, %) points; clamps outside the range.
pub fn curve(points: &[[f32; 2]], x: f32) -> u8 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else { return 100 };
    if x <= first[0] {
        return first[1].clamp(0.0, 100.0) as u8;
    }
    for w in points.windows(2) {
        let ([x0, y0], [x1, y1]) = (w[0], w[1]);
        if x <= x1 {
            let k = if x1 > x0 { (x - x0) / (x1 - x0) } else { 1.0 };
            return (y0 + (y1 - y0) * k).round().clamp(0.0, 100.0) as u8;
        }
    }
    last[1].clamp(0.0, 100.0) as u8
}

pub fn hsv(h: f32) -> Rgb {
    let h = h.rem_euclid(360.0) / 60.0;
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

/// App icon: a ring of 12 rainbow LEDs, RGBA, `size`×`size`.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5 - s / 2.0, y as f32 + 0.5 - s / 2.0);
            let (mut best, mut col) = (0.0f32, [0u8; 3]);
            for k in 0..12 {
                let a = k as f32 / 12.0 * TAU - TAU / 4.0;
                let d = ((fx - a.cos() * s * 0.36).powi(2) + (fy - a.sin() * s * 0.36).powi(2)).sqrt();
                let cov = (s * 0.12 + 0.5 - d).clamp(0.0, 1.0);
                if cov > best {
                    (best, col) = (cov, hsv(k as f32 * 30.0));
                }
            }
            let i = ((y * size + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[col[0], col[1], col[2], (best * 255.0) as u8]);
        }
    }
    px
}

fn scale(c: Rgb, k: f32) -> Rgb {
    c.map(|v| (v as f32 * k).round() as u8)
}

fn lerp(a: Rgb, b: Rgb, k: f32) -> Rgb {
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * k).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_interpolates_and_clamps() {
        let c = [[30.0, 20.0], [70.0, 100.0]];
        assert_eq!(curve(&c, 10.0), 20);
        assert_eq!(curve(&c, 50.0), 60);
        assert_eq!(curve(&c, 90.0), 100);
        assert_eq!(curve(&[], 50.0), 100); // no curve = fail safe
    }

    #[test]
    fn effects_fill_and_dim() {
        let mut l = Light { effect: Effect::Static, color: [200, 100, 0], brightness: 50, ..Light::default() };
        assert_eq!(render(&l, 12, 0.0, None), vec![[100, 50, 0]; 12]);
        l.effect = Effect::Rainbow;
        l.brightness = 100;
        let f = render(&l, 54, 1.0, None);
        assert_eq!(f.len(), 54);
        assert_ne!(f[0], f[27]);
        assert_eq!(hsv(0.0), [255, 0, 0]);
        assert_eq!(hsv(120.0), [0, 255, 0]);
        l.effect = Effect::Temperature;
        assert_eq!(render(&l, 1, 0.0, Some(30.0)), vec![[0, 0, 255]]);
        assert_eq!(render(&l, 1, 0.0, Some(53.0)), vec![[0, 255, 0]]);
        assert_eq!(render(&l, 1, 0.0, Some(90.0)), vec![[255, 0, 0]]);
    }
}
