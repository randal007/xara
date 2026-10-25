use std::collections::VecDeque;
use xara::station::{Modem, Report};
use xara_arq::host::HostEvent;

pub const FS: f64 = 48_000.0;
pub const BLOCK: usize = 480;

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn gauss(&mut self) -> f64 {
        let (u, v) = (self.next().max(1e-300), self.next());
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
}

pub fn payload(n: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(seed);
    (0..n).map(|_| (r.next() * 256.0) as u8).collect()
}

pub enum Failure {
    NoLink(String),
    Stalled(String),
    NoDisconnect(String),
}

impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Failure::NoLink(t) => write!(f, "no link:\n{t}"),
            Failure::Stalled(t) => write!(f, "transfer stalled at {t}"),
            Failure::NoDisconnect(t) => write!(f, "no disconnect:\n{t}"),
        }
    }
}

pub struct Air<M: Modem, const N: usize> {
    pub st: [M; N],
    pub wire: [VecDeque<f64>; N],
    pub ears: [&'static [usize]; N],
    pub noise: f64,
    pub rng: Rng,
    pub skip: fn(&Report) -> bool,
    pub got: [Vec<u8>; N],
    pub events: [Vec<(f64, Report)>; N],
}

impl<M: Modem> Air<M, 2> {
    pub fn pair(st: [M; 2], delay: [usize; 2], noise: f64, skip: fn(&Report) -> bool) -> Self {
        Self::new(st, delay, [&[1], &[0]], noise, skip)
    }
}

impl<M: Modem, const N: usize> Air<M, N> {
    pub fn new(
        st: [M; N],
        delay: [usize; N],
        ears: [&'static [usize]; N],
        noise: f64,
        skip: fn(&Report) -> bool,
    ) -> Self {
        Self {
            st,
            wire: delay.map(|d| VecDeque::from(vec![0.0; d])),
            ears,
            noise,
            rng: Rng(0x9E37_79B9_7F4A_7C15),
            skip,
            got: std::array::from_fn(|_| Vec::new()),
            events: std::array::from_fn(|_| Vec::new()),
        }
    }

    pub fn secs(&self) -> f64 {
        self.st[0].now() as f64 / FS
    }

    pub fn step(&mut self) {
        let heard: [Vec<f64>; N] =
            std::array::from_fn(|i| (0..BLOCK).map(|_| self.wire[i].pop_front().unwrap_or(0.0)).collect());
        let mut outs: [Vec<f64>; N] = std::array::from_fn(|_| vec![0.0; BLOCK]);
        for (i, out) in outs.iter_mut().enumerate() {
            let input: Vec<f64> = (0..BLOCK)
                .map(|k| self.ears[i].iter().map(|&j| heard[j][k]).sum::<f64>() + self.noise * self.rng.gauss())
                .collect();
            self.st[i].process(&input, out);
            let t = self.secs();
            while let Some(r) = self.st[i].pop() {
                match r {
                    Report::Host(HostEvent::Data(d)) => self.got[i].extend(d),
                    r if (self.skip)(&r) => {}
                    r => self.events[i].push((t, r)),
                }
            }
        }
        for (w, out) in self.wire.iter_mut().zip(outs) {
            w.extend(out);
        }
    }

    pub fn run_until(&mut self, secs: f64, mut done: impl FnMut(&Self) -> bool) -> bool {
        let (t0, w0) = (self.secs(), std::time::Instant::now());
        let limit = t0 + secs;
        let mut ok = false;
        while self.secs() < limit {
            if done(self) {
                ok = true;
                break;
            }
            self.step();
        }
        let ok = ok || done(self);
        if std::env::var_os("XARA_BENCH").is_some() {
            let (air, wall) = (self.secs() - t0, w0.elapsed().as_secs_f64());
            eprintln!("bench: {air:.1} s on air in {wall:.2} s ({:.1}x real time, both stations)", air / wall);
        }
        ok
    }

    pub fn has(&self, i: usize, r: &Report) -> bool {
        self.events[i].iter().any(|(_, x)| x == r)
    }

    pub fn time_of(&self, i: usize, want: &Report) -> Option<f64> {
        self.events[i].iter().find(|(_, r)| r == want).map(|(t, _)| *t)
    }

    pub fn top_tx(&self, i: usize) -> usize {
        self.events[i]
            .iter()
            .filter_map(|(_, r)| if let Report::Bitrate { level, tx: true, .. } = r { Some(*level) } else { None })
            .max()
            .unwrap_or(0)
    }

    pub fn trace(&self) -> String {
        let mut v: Vec<String> = (0..N)
            .flat_map(|i| self.events[i].iter().map(move |(t, r)| format!("{t:8.2} {i} {r:?}")))
            .filter(|s| !s.contains("Ptt"))
            .collect();
        v.sort();
        v.join("\n")
    }
}
