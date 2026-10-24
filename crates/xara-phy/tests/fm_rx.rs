mod common;
use common::*;
use xara_phy::call::call_hash;
use xara_phy::fm::ctrl::{Codebook, Side, frame_audio};
use xara_phy::fm::frame::{Record0, Trailer, build_data};
use xara_phy::fm::level::{FmBandwidth, Record};

fn rec(i: usize) -> Record {
    Record::from_index(i).unwrap()
}

fn recs(ids: &[usize]) -> Vec<Record> {
    ids.iter().map(|&i| rec(i)).collect()
}
use xara_phy::fm::rx::receiver::{Event, Receiver};
use xara_phy::fm::tx::{Modulator, TxOptions};

struct Burst {
    record: Option<usize>,
    code: Option<u32>,
    frames: Vec<Vec<u8>>,
}

struct Stream {
    x: Vec<f64>,
    bursts: Vec<Burst>,
    rng: Rng,
}

impl Stream {
    fn new() -> Self {
        Self { x: vec![0.0; 12000], bursts: Vec::new(), rng: Rng::new(99) }
    }

    fn gap(&mut self, n: usize) {
        self.x.extend(std::iter::repeat_n(0.0, n));
    }

    fn data(&mut self, m: &Modulator, record: usize, key: u32, frames: Vec<Vec<u8>>, lead: usize) {
        let refs: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
        let rng = &mut self.rng;
        let mut rand = |n: u32| rng.below(n);
        self.x.extend(m.transmission(
            rec(record),
            key,
            &refs,
            lead,
            TxOptions { drive_db: -5.0, pre_signal: false },
            &mut rand,
        ));
        self.bursts.push(Burst { record: Some(record), code: None, frames });
        self.gap(9000);
    }

    fn ctrl(&mut self, cb: &Codebook, code: u32, lead: usize) {
        self.x.extend(frame_audio(cb, code, lead, -5.0));
        self.bursts.push(Burst { record: None, code: Some(code), frames: vec![] });
        self.gap(9000);
    }

    fn noisy(&self, rng: &mut Rng, sigma: f64) -> Vec<f64> {
        self.x.iter().map(|v| v + sigma * rng.gauss()).collect()
    }
}

fn run(rx: &mut Receiver, x: &[f64]) -> Vec<Event> {
    let mut ev: Vec<Event> = x.chunks(480).flat_map(|b| rx.feed(b)).collect();
    ev.extend(rx.flush());
    ev
}

fn payload(rng: &mut Rng, record: usize, bw: FmBandwidth, n: usize, report: u8) -> Vec<u8> {
    let len = bw.level(rec(record)).frame_bytes();
    let data = rng.bytes(n);
    build_data(rec(record), len, &data, false, 3, false, Trailer { lead_report: report, level: 0, chain_snr: None })
}

fn check(ev: &[Event], s: &Stream) {
    let frames: Vec<Vec<u8>> =
        ev.iter().filter_map(|e| if let Event::Frame(f) = e { f.ok.then(|| f.bytes.clone()) } else { None }).collect();
    let txs: Vec<_> = ev.iter().filter_map(|e| if let Event::Tx(t) = e { Some(t) } else { None }).collect();
    let codes: Vec<u32> =
        ev.iter().filter_map(|e| if let Event::Ctrl(c) = e { Some(c.det.code) } else { None }).collect();
    let want_frames: Vec<Vec<u8>> = s.bursts.iter().flat_map(|b| b.frames.clone()).collect();
    let want_codes: Vec<u32> = s.bursts.iter().filter_map(|b| b.code).collect();
    let want_tx: Vec<(usize, usize)> = s.bursts.iter().filter_map(|b| b.record.map(|r| (r, b.frames.len()))).collect();
    let got_tx: Vec<(usize, usize)> = txs.iter().map(|t| (t.record.index(), t.frames.len())).collect();
    if std::env::var_os("FM_DEBUG").is_some() {
        for e in ev {
            match e {
                Event::Tx(t) => eprintln!(
                    "tx rec {} {:?}",
                    t.record,
                    t.frames.iter().map(|f| (f.t, f.ok, f.missing, f.coherence, f.esn0_db)).collect::<Vec<_>>()
                ),
                Event::Ctrl(c) => eprintln!("ctrl {} t {} lead {}", c.det.code, c.det.t_end, c.det.lead_row),
                Event::Frame(_) => {}
            }
        }
    }
    assert_eq!(got_tx, want_tx);
    assert!(txs.iter().all(|t| t.complete), "incomplete transmissions");
    assert_eq!(frames, want_frames);
    assert_eq!(codes, want_codes);
}

#[test]
fn loopback_link() {
    let bw = FmBandwidth::Narrow;
    let m = Modulator::new(bw);
    let mut rng = Rng::new(7);
    let key = call_hash(b"N1CALL");
    let cb = Codebook::for_link(b"N0CALL", b"N1CALL");
    let mut s = Stream::new();
    let r0 = Record0::connect("N0CALL", "N1CALL", "", "", false, false).to_bytes().unwrap();
    s.data(&m, 0, 0, vec![r0], 21);
    s.ctrl(&cb, 23, 28);
    s.ctrl(&cb, 14, 28);
    s.data(&m, 15, key, vec![payload(&mut rng, 15, bw, 11, 1)], 25);
    s.ctrl(&cb, 7, 1);
    s.data(&m, 2, key, vec![payload(&mut rng, 2, bw, 705, 27)], 1);
    s.ctrl(&cb, 1, 1);
    let fr = vec![payload(&mut rng, 2, bw, 705, 27), payload(&mut rng, 2, bw, 300, 27)];
    s.data(&m, 2, key, fr, 1);
    s.ctrl(&cb, 6, 1);
    s.data(&m, 1, key, vec![payload(&mut rng, 1, bw, 169, 27)], 1);
    s.data(&m, 14, key, vec![payload(&mut rng, 14, bw, 89, 27)], 1);
    let fr = vec![payload(&mut rng, 1, bw, 334, 27), payload(&mut rng, 1, bw, 334, 27)];
    s.data(&m, 1, key, fr, 2);
    s.ctrl(&cb, 4, 2);
    s.data(&m, 16, key, vec![payload(&mut rng, 16, bw, 12, 0)], 9);
    s.ctrl(&cb, 3, 2);
    let codes: Vec<u32> = (1..=24).collect();
    for (sigma, seed) in [(0.0, 1), (0.01, 2), (0.03, 3)] {
        let x = s.noisy(&mut Rng::new(seed), sigma);
        let mut rx = Receiver::new(bw, key, &recs(&[0, 1, 2, 14, 15, 16]), 1);
        rx.set_ctrl(Side::Source, Some(cb.clone()), &codes);
        let ev = run(&mut rx, &x);
        check(&ev, &s);
        assert_eq!(rx.stats.unexpected_frame2, 2, "sigma {sigma}");
    }
}

#[test]
fn wrong_key_is_silent() {
    let bw = FmBandwidth::Narrow;
    let m = Modulator::new(bw);
    let mut rng = Rng::new(9);
    let mut s = Stream::new();
    s.data(&m, 1, call_hash(b"N2CALL"), vec![payload(&mut rng, 1, bw, 20, 1)], 3);
    s.ctrl(&Codebook::for_link(b"N3CALL", b"N2CALL"), 1, 3);
    let mut rx = Receiver::new(bw, call_hash(b"N1CALL"), &recs(&[1, 14, 15]), 1);
    rx.set_ctrl(Side::Source, Some(Codebook::for_link(b"N0CALL", b"N1CALL")), &(1..=24).collect::<Vec<_>>());
    let ev = run(&mut rx, &s.noisy(&mut rng, 0.01));
    assert!(ev.is_empty(), "{ev:?}");
}

#[test]
fn all_levels_loopback() {
    for bw in [FmBandwidth::Narrow, FmBandwidth::Wide] {
        let m = Modulator::new(bw);
        let mut rng = Rng::new(11);
        let key = call_hash(b"N1CALL");
        let mut s = Stream::new();
        let levels: Vec<usize> = (1..=bw.num_levels()).collect();
        for &r in &levels {
            let n = bw.level(rec(r)).frame_bytes() - 10;
            s.data(&m, r, key, vec![payload(&mut rng, r, bw, n, 5)], 2);
        }
        let x = s.noisy(&mut Rng::new(5), 0.002);
        let mut rx = Receiver::new(bw, key, &recs(&levels), 1);
        let ev = run(&mut rx, &x);
        check(&ev, &s);
    }
}

#[test]
fn expected_double_with_lost_frame1() {
    let bw = FmBandwidth::Narrow;
    let m = Modulator::new(bw);
    let mut rng = Rng::new(13);
    let key = call_hash(b"N1CALL");
    let f = [payload(&mut rng, 2, bw, 705, 27), payload(&mut rng, 2, bw, 100, 27)];
    let refs: Vec<&[u8]> = f.iter().map(Vec::as_slice).collect();
    let mut rand = |n: u32| rng.below(n);
    let tx = m.transmission(rec(2), key, &refs, 2, TxOptions::default(), &mut rand);
    let mut x = vec![0.0; 24000];
    let a = x.len() + 2 * 1152;
    x.extend(&tx);
    let b = a + 199 * 1152;
    for v in &mut x[a..b] {
        *v = 0.0;
    }
    x.extend(vec![0.0; 24000]);
    let mut rx = Receiver::new(bw, key, &recs(&[1, 2]), 2);
    rx.mark_tx_end(20000);
    let ev = run(&mut rx, &x);
    let txs: Vec<_> = ev.iter().filter_map(|e| if let Event::Tx(t) = e { Some(t) } else { None }).collect();
    assert_eq!(txs.len(), 1, "{ev:?}");
    assert_eq!(txs[0].frames.iter().map(|f| (f.missing, f.ok)).collect::<Vec<_>>(), [(true, false), (false, true)]);
    assert_eq!(txs[0].frames[1].bytes, f[1]);
}

#[test]
fn hiss_after_a_single_frame_is_not_frame_2() {
    let bw = FmBandwidth::Narrow;
    let m = Modulator::new(bw);
    let key = call_hash(b"N1CALL");
    for (record, n) in [(15, 11), (14, 60), (1, 200)] {
        for tail_ms in [100, 300, 1000] {
            for level in [0.3, 1.0] {
                let mut rng = Rng::new(5);
                let mut s = Stream::new();
                let start = s.x.len();
                s.data(&m, record, key, vec![payload(&mut rng, record, bw, n, 1)], 25);
                let end = s.x.len() - 9000;
                s.gap(tail_ms * 48);
                let rms = (s.x[start..end].iter().map(|v| v * v).sum::<f64>() / (end - start) as f64).sqrt();
                let tones: Vec<(f64, f64)> = (6..60)
                    .map(|k| (k as f64 * 50.0, rng.below(1000) as f64 / 1000.0 * std::f64::consts::TAU))
                    .collect();
                let g = level * rms / (tones.len() as f64 / 2.0).sqrt();
                for (i, v) in s.x[end..end + tail_ms * 48].iter_mut().enumerate() {
                    let t = i as f64 / 48000.0;
                    *v = g * tones.iter().map(|&(f, ph)| (std::f64::consts::TAU * f * t + ph).sin()).sum::<f64>();
                }
                s.gap(48000);
                let mut rx = Receiver::new(bw, key, &recs(&[1, 14, 15]), 1);
                let mut txs = Vec::new();
                for (i, b) in s.x.chunks(480).enumerate() {
                    for e in rx.feed(b) {
                        if let Event::Tx(t) = e {
                            txs.push((t.frames.len(), t.complete, (i + 1) * 480 - end));
                        }
                    }
                }
                let what = format!("rec {record} tail {tail_ms} ms x{level}");
                assert_eq!(txs.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(), [(1, true)], "{what}");
                assert!(txs[0].2 < 12000, "{what}: closed {} samples after frame 1", txs[0].2);
            }
        }
    }
}
