use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::pulse_ffi::{
    BufferAttr, Conn, Peek, S16LE, STREAM_ADJUST_LATENCY, STREAM_AUTO_TIMING_UPDATE, STREAM_DONT_MOVE,
    STREAM_INTERPOLATE_TIMING, SampleSpec, Stream,
};
use super::{Audio, BLOCK, DeviceList, RATE, STALL, capture_only};

const SETUP: Duration = Duration::from_secs(5);

/// Dropouts (X6100 XARA): PulseAudio's underflow notices on the playback
/// stream, overflow notices on the record stream, and holes (audio lost
/// before it was read, filled with silence below). Logged as they happen,
/// so a gap on the air can be told from a glitch elsewhere.
static PLAY_UNDERFLOWS: AtomicU64 = AtomicU64::new(0);
static REC_OVERFLOWS: AtomicU64 = AtomicU64::new(0);
static REC_HOLES: AtomicU64 = AtomicU64::new(0);
const SPEC: SampleSpec = SampleSpec { format: S16LE, rate: RATE, channels: 1 };

fn connect() -> io::Result<Conn> {
    Conn::open(Instant::now() + SETUP)
}

struct Sink {
    name: String,
    null: bool,
}

fn sinks(c: &Conn) -> io::Result<Vec<Sink>> {
    let what = "listing playback sinks";
    let mut v = Vec::new();
    let op = c
        .sinks(|s| {
            v.push(Sink {
                name: s.name().unwrap_or_default(),
                null: s.prop(c"factory.name").as_deref() == Some("support.null-audio-sink")
                    && s.has_prop(c"device.api") == Some(false),
            })
        })
        .ok_or_else(|| c.error(what))?;
    let until = Instant::now() + SETUP;
    while op.running() {
        c.iterate(until, what)?;
    }
    if op.failed() {
        return Err(c.error(what));
    }
    drop(op);
    Ok(v)
}

pub fn list() -> io::Result<(DeviceList, DeviceList)> {
    let what = "listing devices";
    let c = connect()?;
    let (mut src, mut snk): (DeviceList, DeviceList) = (Vec::new(), Vec::new());
    let found = |v: &mut DeviceList, name: Option<String>, desc: Option<String>| {
        if let Some(name) = name {
            v.push((name, desc.unwrap_or_default()));
        }
    };
    let sources = c.sources(|i| found(&mut src, i.name(), i.description())).ok_or_else(|| c.error(what))?;
    let sinks = c.sinks(|i| found(&mut snk, i.name(), i.description())).ok_or_else(|| c.error(what))?;
    let until = Instant::now() + SETUP;
    while sources.running() || sinks.running() {
        c.iterate(until, what)?;
    }
    if sources.failed() || sinks.failed() {
        return Err(c.error(what));
    }
    drop((sources, sinks));
    Ok((src, snk))
}

pub fn check(playback: &str, radio: bool) -> io::Result<()> {
    refuse_default(playback, radio)?;
    let c = connect()?;
    check_playback(&sinks(&c)?, playback, radio)
}

fn refuse_default(name: &str, radio: bool) -> io::Result<()> {
    if radio && (name.is_empty() || name.starts_with('@') || name == "default") {
        return Err(io::Error::other("--radio needs the playback sink named explicitly, not a default"));
    }
    Ok(())
}

fn check_playback(sinks: &[Sink], name: &str, radio: bool) -> io::Result<()> {
    refuse_default(name, radio)?;
    if radio {
        if !sinks.iter().any(|s| s.name == name) {
            return Err(io::Error::other(format!("no playback sink named {name:?} (pactl list short sinks)")));
        }
    } else if !sinks.iter().any(|s| s.name == name && s.null) {
        return Err(io::Error::other(format!(
            "refusing playback to {name:?}: not a PipeWire null sink (a real radio interface needs --radio)"
        )));
    }
    Ok(())
}

fn bytes(ms: u32) -> u32 {
    RATE / 1000 * ms * 2
}

pub struct PulseIo {
    rec: Stream,
    play: Option<Stream>,
    conn: Conn,
    rx: Vec<u8>,
    tx: Vec<u8>,
    seen: [u64; 3],
    base: [u64; 3],
    reads: u32,
    /// Silence blocks not written because playback was full (see write).
    skipped: u64,
    last_latency_log: Instant,
}

impl PulseIo {
    pub fn open(capture: &str, playback: &str, radio: bool) -> io::Result<Self> {
        Self::connect(capture, Some((playback, radio)))
    }

    pub fn open_capture(capture: &str) -> io::Result<Self> {
        Self::connect(capture, None)
    }

    fn connect(capture: &str, playback: Option<(&str, bool)>) -> io::Result<Self> {
        if let Some((name, radio)) = playback {
            refuse_default(name, radio)?;
        }
        let conn = connect()?;
        if let Some((name, radio)) = playback {
            check_playback(&sinks(&conn)?, name, radio)?;
        }
        let flags = STREAM_DONT_MOVE | STREAM_ADJUST_LATENCY | STREAM_INTERPOLATE_TIMING | STREAM_AUTO_TIMING_UPDATE;
        let attr = |fragsize, tlength| BufferAttr {
            maxlength: u32::MAX,
            tlength,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize,
        };
        let mut rec = Stream::new(&conn, c"rx", &SPEC).ok_or_else(|| io::Error::other("pulse stream"))?;
        rec.count_overflows(&REC_OVERFLOWS);
        rec.connect_record(capture, &attr(bytes(10), u32::MAX), flags)?;
        let play = match playback {
            Some((name, _)) => {
                let mut p = Stream::new(&conn, c"tx", &SPEC).ok_or_else(|| io::Error::other("pulse stream"))?;
                p.count_underflows(&PLAY_UNDERFLOWS);
                p.connect_playback(name, &attr(u32::MAX, bytes(20)), flags)?;
                Some((p, name))
            }
            None => None,
        };
        let until = Instant::now() + SETUP;
        while !(rec.live(capture)? & play.as_ref().map_or(Ok(true), |(p, name)| p.live(name))?) {
            conn.iterate(until, "opening audio streams")?;
        }
        for (s, want) in [(&rec, capture)].into_iter().chain(play.as_ref().map(|(p, name)| (p, *name))) {
            let got = s.device_name().unwrap_or_default();
            if got != want {
                return Err(io::Error::other(format!("{want}: the sound server attached {got:?} instead")));
            }
        }
        Ok(Self {
            rec,
            play: play.map(|(p, _)| p),
            conn,
            rx: Vec::new(),
            tx: Vec::new(),
            seen: [0; 3],
            base: [0; 3],
            reads: 0,
            skipped: 0,
            last_latency_log: Instant::now(),
        })
    }

    /// One line per new dropout (they should be rare; a burst of them is
    /// summarised), e.g. "audio dropout: playback underflow (3 so far)".
    fn report(&mut self) {
        /* The first half second doesn't count: playback starts with an
         * underflow before the first audio arrives. */
        const SETTLE_READS: u32 = RATE / BLOCK as u32 / 2;
        if self.reads < SETTLE_READS {
            self.reads += 1;
            if self.reads == SETTLE_READS {
                for (i, c) in [&PLAY_UNDERFLOWS, &REC_OVERFLOWS, &REC_HOLES].into_iter().enumerate() {
                    self.seen[i] = c.load(Ordering::Relaxed);
                    self.base[i] = self.seen[i];
                }
                say!("audio dropouts: counting from here");
            }
            return;
        }
        // Every 10 s: the real delays, so a backlog shows (X6100 XARA).
        if self.last_latency_log.elapsed() >= Duration::from_secs(10) {
            self.last_latency_log = Instant::now();
            let f = |v: Option<f64>| v.map_or("?".to_string(), |ms| format!("{ms:.0} ms"));
            let pending = self.rx.len() / 2 * 1000 / RATE as usize;
            say!(
                "audio latency: capture {} (+{pending} ms unread), playback {}, silence skipped {}",
                f(self.rec.latency_ms()),
                f(self.play.as_ref().and_then(Stream::latency_ms)),
                self.skipped
            );
        }
        for (i, (c, what)) in [
            (&PLAY_UNDERFLOWS, "playback underflow"),
            (&REC_OVERFLOWS, "capture overflow"),
            (&REC_HOLES, "capture hole"),
        ]
        .into_iter()
        .enumerate()
        {
            let n = c.load(Ordering::Relaxed);
            if n != self.seen[i] {
                say!("audio dropout: {what} ({} since start)", n - self.base[i]);
                self.seen[i] = n;
            }
        }
    }
}

impl Audio for PulseIo {
    fn read(&mut self, x: &mut [i16]) -> io::Result<()> {
        let need = 2 * x.len();
        let until = Instant::now() + STALL;
        while self.rx.len() < need {
            self.rec.live("capture")?;
            match self.rec.peek("capture")? {
                Peek::Empty => {
                    self.conn.iterate(until, "capture (no audio)")?;
                    continue;
                }
                Peek::Hole(n) => {
                    REC_HOLES.fetch_add(1, Ordering::Relaxed);
                    self.rx.resize(self.rx.len() + n, 0)
                }
                Peek::Data(d) => self.rx.extend_from_slice(d),
            }
            self.rec.discard("capture")?;
        }
        for (v, c) in x.iter_mut().zip(self.rx.chunks_exact(2)) {
            *v = i16::from_le_bytes([c[0], c[1]]);
        }
        self.rx.drain(..need);
        self.report();
        Ok(())
    }

    fn write(&mut self, x: &[i16]) -> io::Result<()> {
        let Some(play) = &mut self.play else { return Err(capture_only()) };
        self.tx.clear();
        self.tx.extend(x.iter().flat_map(|v| v.to_le_bytes()));
        let mut off = 0;
        let until = Instant::now() + STALL;
        while off < self.tx.len() {
            play.live("playback")?;
            let n = play.writable_size().min(self.tx.len() - off) & !1;
            if n == 0 {
                // X6100 XARA: a block of pure silence while playback is full
                // is dropped rather than waited for. Waiting here held the
                // loop to real time, so received audio that piled up during
                // any stall was never caught up: the receive delay grew to
                // ~1 s and replies came "too late" for the ARQ timing.
                if off == 0 && x.iter().all(|&v| v == 0) {
                    self.skipped += 1;
                    return Ok(());
                }
                self.conn.iterate(until, "playback (sink not draining)")?;
                continue;
            }
            play.write(&self.tx[off..off + n], "playback")?;
            off += n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_sink_checks() {
        let sinks = [Sink { name: "xara-null".into(), null: true }, Sink { name: "usb-radio".into(), null: false }];
        let err = |name: &str, radio: bool| check_playback(&sinks, name, radio).err().map(|e| e.to_string());
        for name in ["@DEFAULT_SINK@", "default", ""] {
            assert!(err(name, true).is_some_and(|e| e.contains("named explicitly")), "{name}");
        }
        assert!(err("xara-no-such-sink", true).is_some_and(|e| e.contains("no playback sink")));
        assert!(err("xara-no-such-sink", false).is_some_and(|e| e.contains("not a PipeWire null sink")));
        assert!(err("usb-radio", false).is_some_and(|e| e.contains("not a PipeWire null sink")));
        assert_eq!((err("usb-radio", true), err("xara-null", false), err("xara-null", true)), (None, None, None));
    }
}
