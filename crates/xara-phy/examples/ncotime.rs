// One PSK carrier's audio as psk::carrier_audio builds it, step by step on this CPU (L10-like:
// 360 symbols, RRC 2 pulse), plus a table + short-series NCO of the same f32 phase.
use num_complex::Complex64;
use std::time::Instant;
use xara_phy::hf::level::Pulse;
use xara_phy::hf::psk::{SPS, interp16_taps, nco_phase, tx_pulse_taps};

const TBITS: u32 = 10;

fn scatter(x: &[Complex64], stride: usize, offset: usize, h: &[f64]) -> Vec<Complex64> {
    let mut y = vec![Complex64::default(); (x.len() - 1) * stride + offset + 1 + h.len() - 1];
    for (i, &v) in x.iter().enumerate() {
        if v == Complex64::default() {
            continue;
        }
        let base = offset + i * stride;
        for (j, &t) in h.iter().enumerate() {
            y[base + j] += v * t;
        }
    }
    y
}

fn main() {
    let n_tab = 1usize << TBITS;
    let d = 2.0 * std::f64::consts::PI / n_tab as f64;
    let inv = 1.0 / d;
    let tab: Vec<(f64, f64)> = (0..=n_tab).map(|k| ((k as f64 * d).cos(), (k as f64 * d).sin())).collect();
    let fast = |p: f64| {
        let k = (p * inv + 0.5) as usize;
        let r = p - k as f64 * d;
        let (c0, s0) = tab[k];
        let r2 = r * r;
        let cr = 1.0 - r2 * (0.5 - r2 * (1.0 / 24.0 - r2 * (1.0 / 720.0)));
        let sr = r * (1.0 - r2 * (1.0 / 6.0 - r2 * (1.0 / 120.0 - r2 * (1.0 / 5040.0))));
        (c0 * cr - s0 * sr, s0 * cr + c0 * sr)
    };
    let (hp, hi) = (tx_pulse_taps(Pulse::Rrc { span: 2.0 }), interp16_taps());
    let mut seed = 7u32;
    let sym: Vec<Complex64> = (0..360)
        .map(|_| {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            Complex64::from_polar(178.0, f64::from(seed >> 8))
        })
        .collect();
    let reps = 5;
    let mut t = [0.0f64; 6];
    let mut worst = 0.0f64;
    for _ in 0..reps {
        let c = Instant::now();
        let mut y3 = scatter(&sym, SPS, 0, &hp);
        y3.resize(sym.len() * SPS + hp.len() - 1, Complex64::default());
        t[0] += c.elapsed().as_secs_f64();
        let c = Instant::now();
        let mut y = scatter(&y3, 16, 0, &hi);
        y.resize(y3.len() * 16 + hi.len() - 1, Complex64::default());
        t[1] += c.elapsed().as_secs_f64();
        let c = Instant::now();
        let ph = nco_phase(1500, y.len());
        t[2] += c.elapsed().as_secs_f64();
        let c = Instant::now();
        let a: Vec<f64> =
            y.iter().zip(&ph).map(|(v, &p)| v.re * f64::from(p).cos() - v.im * f64::from(p).sin()).collect();
        t[3] += c.elapsed().as_secs_f64();
        let c = Instant::now();
        let b: Vec<f64> = y
            .iter()
            .zip(&ph)
            .map(|(v, &p)| {
                let (co, si) = fast(f64::from(p));
                v.re * co - v.im * si
            })
            .collect();
        t[4] += c.elapsed().as_secs_f64();
        let c = Instant::now();
        let mut total = vec![0.0; a.len()];
        total.iter_mut().zip(&a).for_each(|(t, s)| *t += s);
        t[5] += c.elapsed().as_secs_f64();
        worst = a.iter().zip(&b).fold(worst, |w, (x, y)| w.max((x - y).abs()));
        std::hint::black_box((&a, &b, &total));
    }
    let ms = |x: f64| x * 1000.0 / reps as f64;
    println!(
        "one carrier, {} samples: pulse {:.1} ms, x16 interp {:.1}, nco phase {:.1}, mix libm {:.1} | table {:.1}, add to sum {:.1}; largest difference {:.1e} (of ~{:.0})",
        360 * SPS * 16,
        ms(t[0]),
        ms(t[1]),
        ms(t[2]),
        ms(t[3]),
        ms(t[4]),
        ms(t[5]),
        worst,
        178.0 * 4.0
    );
}
