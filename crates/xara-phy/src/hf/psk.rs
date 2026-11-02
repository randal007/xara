use crate::constellation::{constellation, take_bits};
use crate::dat::marker_map;
use crate::hf::level::{Level, Pulse, Waveform};
use num_complex::Complex64;
use std::f32::consts::TAU;
use std::f64::consts::PI;

pub const SPS: usize = 32;
pub const SCALE: f64 = 178.0;

fn f32r(x: f64) -> f64 {
    x as f32 as f64
}

pub fn pulse_taps(p: Pulse) -> Vec<f64> {
    match p {
        Pulse::Rc => {
            let t = (SPS / 2) as f64;
            (-16..=16)
                .map(|n: i32| {
                    let n = f64::from(n);
                    f32r(if n == 0.0 {
                        1.0
                    } else if n.abs() == t / 2.0 {
                        0.5 * (PI / 2.0).sin()
                    } else {
                        let x = n / t;
                        (PI * x).sin() * (PI * x).cos() / (x * PI * (1.0 - 4.0 * x * x))
                    })
                })
                .collect()
        }
        Pulse::Rrc { span } => {
            let t = SPS as f64;
            let half = (t * f64::from(span)).round() as i32;
            (-half..=half)
                .map(|n| {
                    let n = f64::from(n);
                    f32r(if n == 0.0 {
                        4.0 / PI
                    } else if n.abs() == t / 4.0 {
                        1.0 / 2f64.sqrt() * ((1.0 + 2.0 / PI) * (PI / 4.0).sin() + (1.0 - 2.0 / PI) * (PI / 4.0).cos())
                    } else {
                        let x = n / t;
                        4.0 * x * (PI * x * 2.0).cos() / (PI * x * (1.0 - (4.0 * x).powi(2)))
                    })
                })
                .collect()
        }
    }
}

pub fn tx_pulse_taps(p: Pulse) -> Vec<f64> {
    let mut h = pulse_taps(p);
    *h.last_mut().unwrap() = 0.0;
    h
}

pub fn interp16_taps() -> [f64; 32] {
    let f2 = 125.0 / 48000.0;
    std::array::from_fn(|i| {
        let m = i as f64 - 16.0;
        let h = if m == 0.0 { 2.0 * f2 } else { (2.0 * PI * f2 * m).sin() / (PI * m) };
        let w = f32r(0.54 - 0.46 * (2.0 * PI * i as f64 / 32.0).cos());
        f32r(h * w)
    })
}

pub const fn stagger(c: usize, nc: usize) -> usize {
    if nc == 3 {
        [0, 11, 21][c]
    } else if c % 2 == 1 {
        16
    } else {
        0
    }
}

pub fn nco_phase(fc: u32, n: usize) -> Vec<f32> {
    let step = (2.0 * PI * f64::from(fc) / 48000.0) as f32;
    let mut phi = 0.0f32;
    (0..n)
        .map(|_| {
            phi += step;
            if phi > TAU {
                phi -= TAU;
            }
            phi
        })
        .collect()
}

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

// One carrier before the x16 interpolation (3 kHz), its length at 48 kHz, and
// the NCO's phase: carrier_audio, split so that the interpolation, the mixing
// and the sum over carriers run together in time slices (no 48 kHz arrays).
fn carrier_parts(symbols: &[Complex64], fc: u32, delay: usize, hp: &[f64], hi: &[f64]) -> (Vec<Complex64>, Vec<f32>) {
    let len3 = symbols.len() * SPS + delay;
    let mut y3 = scatter(symbols, SPS, delay, hp);
    y3.resize(len3 + hp.len() - 1, Complex64::default());
    let ph = nco_phase(fc, y3.len() * 16 + hi.len() - 1);
    (y3, ph)
}

// Sample n of scatter(y3, 16, 0, hi), adding the same terms in the same order
// (rising input index, zero inputs skipped).
fn interp_at(y3: &[Complex64], hi: &[f64], n: usize) -> Complex64 {
    let mut acc = Complex64::default();
    let lo = (n + 1).saturating_sub(hi.len()).div_ceil(16);
    for i in lo..=(n / 16).min(y3.len() - 1) {
        let v = y3[i];
        if v != Complex64::default() {
            acc += v * hi[n - 16 * i];
        }
    }
    acc
}

fn mix(v: Complex64, p: f32) -> f64 {
    v.re * f64::from(p).cos() - v.im * f64::from(p).sin()
}

pub fn symbol_map(l: &Level, other: &[u16]) -> Vec<Vec<bool>> {
    marker_map(other, l.nc() as usize, l.markers() as usize, l.spf() as usize - 1)
}

pub fn modulate(
    l: &Level,
    other: &[u16],
    phasors: &[Vec<Complex64>],
    txs: &[Vec<u8>],
    rand: &mut dyn FnMut(u32) -> u32,
) -> Vec<f64> {
    let Waveform::Psk { carriers_hz, pulse, .. } = l.waveform else { panic!("not a PSK level") };
    let cst = constellation(l.bps);
    let m = cst.len() as u32;
    let nsym = l.data_symbols() as usize;
    let map = symbol_map(l, other);
    let (hp, hi) = (tx_pulse_taps(pulse), interp16_taps());
    let nc = carriers_hz.len();
    let refs: Vec<Vec<Complex64>> = txs.iter().map(|_| (0..nc).map(|_| cst[rand(m) as usize]).collect()).collect();
    let lead: Vec<Vec<Complex64>> =
        (0..l.lead_in()).map(|_| (0..nc).map(|_| cst[rand(m) as usize]).collect()).collect();
    let mut ptrs = vec![0usize; txs.len()];
    // The symbols, carrier by carrier (they take the bits in this order).
    let xs: Vec<Vec<Complex64>> = (0..nc)
        .map(|c| {
            let mut x: Vec<Complex64> =
                lead.iter().enumerate().map(|(k, v)| v[c] * phasors[c][k].conj() * SCALE).collect();
            for ((tx, ptr), d0) in txs.iter().zip(ptrs.iter_mut()).zip(&refs) {
                let mut d = d0[c];
                x.push(d * phasors[c][0].conj() * SCALE);
                for s in 1..nsym {
                    if !(s >= 2 && map[c][s - 2]) {
                        d *= cst[take_bits(tx, ptr, l.bps)];
                    }
                    x.push(d * phasors[c][s].conj() * SCALE);
                }
            }
            x.push(Complex64::default());
            x
        })
        .collect();
    assert!(ptrs.iter().all(|&p| p == l.n() as usize));
    // X6100: each carrier's audio on its own thread (up to 4 cores); a slow
    // CPU took ~1 s for L10 and the frame went out after VARA stopped
    // listening (2026-10-31). Then the x16 interpolation, the NCO mixing and
    // the sum over carriers together in time slices on the same threads: on
    // the radio the cos/sin were ~2/3 of the work, the sum was serial and the
    // 48 kHz arrays (3 MB a carrier) cost memory time (L10 0.85 s, 2026-11-01).
    // Every sample is still interpolated, mixed and summed in the same order,
    // so the same samples.
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4).min(nc).max(1);
    let mut bb: Vec<(Vec<Complex64>, Vec<f32>)> = vec![(Vec::new(), Vec::new()); nc];
    std::thread::scope(|sc| {
        for (t, chunk) in bb.chunks_mut(nc.div_ceil(threads)).enumerate() {
            let first = t * nc.div_ceil(threads);
            let (xs, hp, hi) = (&xs, &hp, &hi);
            sc.spawn(move || {
                for (i, out) in chunk.iter_mut().enumerate() {
                    let c = first + i;
                    *out = carrier_parts(&xs[c], carriers_hz[c], stagger(c, nc), hp, hi);
                }
            });
        }
    });
    let mut total: Vec<f64> = vec![0.0; bb.iter().map(|(_, ph)| ph.len()).max().unwrap_or(0)];
    let slice = total.len().div_ceil(threads).max(1);
    std::thread::scope(|sc| {
        for (t, out) in total.chunks_mut(slice).enumerate() {
            let (a, bb, hi) = (t * slice, &bb, &hi);
            sc.spawn(move || {
                for (y3, ph) in bb {
                    let b = ph.len().min(a + out.len());
                    for (n, o) in (a..b).zip(out.iter_mut()) {
                        *o += mix(interp_at(y3, hi, n), ph[n]);
                    }
                }
            });
        }
    });
    total
}
