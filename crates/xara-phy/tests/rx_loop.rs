mod common;

use common::{Rng, random_frame};
use xara_phy::hf::level::{Bandwidth, Record};
use xara_phy::hf::rx::receiver::{Event, Receiver};
use xara_phy::hf::tx::Modulator;

fn normalise(x: &[f64]) -> Vec<f64> {
    let rms = (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt();
    x.iter().map(|v| v / (rms + 1e-12) * 0.12).collect()
}

fn burst(rng: &mut Rng, m: &Modulator, rec: Record, frames: &[&[u8]], noise: f64) -> Vec<f64> {
    let tx = normalise(&m.transmission(rec, frames, &mut |n| rng.below(n)));
    let mut x = vec![0.0; 24000];
    x.extend(tx);
    x.extend(vec![0.0; 24000]);
    x.iter().map(|v| v + noise * rng.gauss()).collect()
}

fn run(rx: &mut Receiver, x: &[f64]) -> Vec<Event> {
    let mut ev: Vec<Event> = x.chunks(480).flat_map(|b| rx.feed(b)).collect();
    ev.extend(rx.flush());
    ev
}

fn decoded(ev: &[Event]) -> Vec<Vec<u8>> {
    ev.iter()
        .filter_map(|e| match e {
            Event::Frame(f) if f.frame.ok => Some(f.frame.bytes.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn single_frames_every_level() {
    let mut rng = Rng::new(7);
    let mut fails = Vec::new();
    for bw in Bandwidth::ALL {
        let m = Modulator::new(bw);
        for rec in bw.levels() {
            let frame = random_frame(&mut rng, bw.level(rec).frame_bytes() as usize);
            let x = burst(&mut rng, &m, rec, &[&frame], 0.002);
            let mut rx = Receiver::new(bw, &[rec], 1, None);
            let ev = run(&mut rx, &x);
            let txs = ev.iter().filter(|e| matches!(e, Event::Tx(_))).count();
            if decoded(&ev) != [frame.clone()] || txs != 1 {
                fails.push((bw, rec, decoded(&ev).len(), txs));
            }
        }
    }
    assert!(fails.is_empty(), "{fails:?}");
}

fn shift(x: &[f64], hz: f64) -> Vec<f64> {
    let n = x.len().next_power_of_two() * 2;
    let mut a: Vec<rustfft::num_complex::Complex64> = x.iter().map(|&v| v.into()).collect();
    a.resize(n, Default::default());
    let mut p = rustfft::FftPlanner::new();
    p.plan_fft_forward(n).process(&mut a);
    for (k, v) in a.iter_mut().enumerate() {
        *v *= if k == 0 {
            1.0
        } else if k < n / 2 {
            2.0
        } else {
            0.0
        };
    }
    p.plan_fft_inverse(n).process(&mut a);
    a.iter()
        .take(x.len())
        .enumerate()
        .map(|(i, v)| {
            (v / n as f64
                * rustfft::num_complex::Complex64::from_polar(
                    1.0,
                    2.0 * std::f64::consts::PI * hz * i as f64 / 48000.0,
                ))
            .re
        })
        .collect()
}

#[test]
fn double_frames() {
    let mut rng = Rng::new(8);
    let mut fails = Vec::new();
    for (bw, recs) in [
        (Bandwidth::Hz2300, vec![1, 4, 5, 9, 12, 15]),
        (Bandwidth::Hz500, vec![2, 4, 9]),
        (Bandwidth::Hz2750, vec![7, 16]),
    ] {
        let m = Modulator::new(bw);
        for rec in recs.into_iter().map(|r| Record::new(r).unwrap()) {
            let n = bw.level(rec).frame_bytes() as usize;
            let (f1, f2) = (random_frame(&mut rng, n), random_frame(&mut rng, n));
            let x = burst(&mut rng, &m, rec, &[&f1, &f2], 0.002);
            let mut rx = Receiver::new(bw, &[rec], 2, None);
            let ev = run(&mut rx, &x);
            let tx: Vec<_> = ev.iter().filter_map(|e| if let Event::Tx(t) = e { Some(t) } else { None }).collect();
            if decoded(&ev) != [f1.clone(), f2.clone()] || tx.len() != 1 || !tx[0].complete {
                fails.push((bw, rec, decoded(&ev).len(), tx.len()));
            }
        }
    }
    assert!(fails.is_empty(), "{fails:?}");
}

#[test]
fn unexpected_second_frame_is_picked_up() {
    let mut rng = Rng::new(9);
    let bw = Bandwidth::Hz2300;
    let m = Modulator::new(bw);
    for rec in [3, 6, 10].map(|r| Record::new(r).unwrap()) {
        let n = bw.level(rec).frame_bytes() as usize;
        let (f1, f2) = (random_frame(&mut rng, n), random_frame(&mut rng, n));
        let x = burst(&mut rng, &m, rec, &[&f1, &f2], 0.002);
        let mut rx = Receiver::new(bw, &[rec], 1, None);
        let ev = run(&mut rx, &x);
        assert_eq!(decoded(&ev), [f1, f2], "record {rec}");
    }
}

#[test]
fn offsets_and_noise() {
    let mut rng = Rng::new(10);
    let bw = Bandwidth::Hz2300;
    let m = Modulator::new(bw);
    let mut fails = Vec::new();
    for rec in bw.levels() {
        for hz in [-30.0, 25.0] {
            let frame = random_frame(&mut rng, bw.level(rec).frame_bytes() as usize);
            let x = shift(&burst(&mut rng, &m, rec, &[&frame], 0.0), hz);
            let noise = if bw.level(rec).bps >= 4 && !bw.level(rec).is_fsk() { 0.004 } else { 0.03 };
            let x: Vec<f64> = x.iter().map(|v| v + noise * rng.gauss()).collect();
            let near: Vec<Record> =
                [rec.get().saturating_sub(1), rec.get(), rec.get() + 1].into_iter().filter_map(Record::new).collect();
            let mut rx = Receiver::new(bw, &near, 1, None);
            let ev = run(&mut rx, &x);
            if !decoded(&ev).contains(&frame) {
                fails.push((rec, hz));
            }
        }
    }
    assert!(fails.is_empty(), "{fails:?}");
}

#[test]
fn control_frames() {
    use xara_phy::call::call_hash;
    use xara_phy::hf::ctrl::{Codebook, Fmt, Role, leadin_grids, leadin_symbols, synth};
    use xara_phy::hf::rx::ctrl::rx_entries;
    let mut rng = Rng::new(11);
    let grids = leadin_grids();
    for bw in Bandwidth::ALL {
        let mut cb = Codebook::new(call_hash(b"N1CALL"), [call_hash(b"N2CALL"), 0, 0, 0, 0], Role::Caller, bw);
        cb.set_connect_codes(call_hash(b"N2CALL"), Role::Caller);
        let entries = rx_entries(&cb, 0);
        for (fmt, code, hz) in
            [(Fmt::F0, 1, 0.0), (Fmt::F1, 7, 30.0), (Fmt::F2, 49, -60.0), (Fmt::F0, 12, 90.0), (Fmt::F2, 5, 0.0)]
        {
            let lead = &grids[fmt.index()][..leadin_symbols(fmt, code, 0) as usize];
            let mut x = vec![0.0; 24000];
            x.extend(normalise(&synth(lead, cb.rx(fmt, code))));
            x.extend(vec![0.0; 24000]);
            let x: Vec<f64> = shift(&x, hz).iter().map(|v| v + 0.02 * rng.gauss()).collect();
            let mut rx = Receiver::new(bw, &[], 1, Some(&entries));
            let ev = run(&mut rx, &x);
            let got: Vec<_> = ev.iter().filter_map(|e| if let Event::Ctrl(c) = e { Some(c) } else { None }).collect();
            assert_eq!(got.len(), 1, "{bw:?} fmt {fmt} code {code}: {got:?}");
            assert_eq!((got[0].det.code, got[0].det.fmt), (code, fmt), "{bw:?}");
            assert!((got[0].afc_hz - hz).abs() < 6.0, "{bw:?} fmt {fmt}: afc {} for {hz}", got[0].afc_hz);
            assert!(got[0].det.hits >= fmt.params().min_hits as usize);
        }
    }
}

#[test]
fn noise_after_a_single_frame_is_not_frame_2() {
    let mut rng = Rng::new(11);
    let mut fails = Vec::new();
    for bw in Bandwidth::ALL {
        let m = Modulator::new(bw);
        for rec in bw.levels() {
            for (burst_ms, clicks, late_ok) in [(30, 0, false), (100, 0, false), (0, 20, false), (1000, 0, true)] {
                let frame = random_frame(&mut rng, bw.level(rec).frame_bytes() as usize);
                let tx = normalise(&m.transmission(rec, &[&frame], &mut |n| rng.below(n)));
                let mut x = vec![0.0; 24000];
                x.extend(&tx);
                let end = x.len();
                x.extend(vec![0.0; 384000]);
                let tones: Vec<(f64, f64)> = (6..60)
                    .map(|k| (k as f64 * 50.0, rng.below(1000) as f64 / 1000.0 * std::f64::consts::TAU))
                    .collect();
                let g = 0.12 / (tones.len() as f64 / 2.0).sqrt();
                let noise = |i: usize| {
                    let t = i as f64 / 48000.0;
                    g * tones.iter().map(|&(f, ph)| (std::f64::consts::TAU * f * t + ph).sin()).sum::<f64>()
                };
                for i in 0..burst_ms * 48 {
                    x[end + i] += noise(i);
                }
                for k in 0..clicks {
                    let at = end + k * 48000 / clicks;
                    for i in 0..96 {
                        x[at + i] += noise(i);
                    }
                }
                let x: Vec<f64> = x.iter().map(|v| v + 0.002 * rng.gauss()).collect();
                let mut rx = Receiver::new(bw, &[rec], 1, None);
                let mut txs = Vec::new();
                for (i, b) in x.chunks(480).enumerate() {
                    for e in rx.feed(b) {
                        if let Event::Tx(t) = e {
                            txs.push((t.ok(), (i + 1) * 480 - end));
                        }
                    }
                }
                let ok = txs.len() == 1 && txs[0].0 == [true] && (late_ok || txs[0].1 < 24000);
                if !ok {
                    fails.push((bw, rec, burst_ms, clicks, txs));
                }
            }
        }
    }
    assert!(fails.is_empty(), "{fails:?}");
}
