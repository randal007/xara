// The receiver on real audio (raw f64le 48 kHz mono, path in argv[1]), expecting argv[2..] levels, linked:
// CPU per second of audio, and the slowest single feed() call. XARA_CHUNK: samples per feed (default 960).
use std::time::Instant;
use xara_phy::hf::ctrl::{Codebook, Role};
use xara_phy::hf::level::{Bandwidth, Record};
use xara_phy::hf::rx::ctrl::rx_entries;
use xara_phy::hf::rx::receiver::Receiver;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let raw = std::fs::read(&a[1]).unwrap();
    let audio: Vec<f64> = raw.chunks_exact(8).map(|b| f64::from_le_bytes(b.try_into().unwrap())).collect();
    let recs: Vec<Record> = a[2..].iter().map(|s| Record::new(s.parse().unwrap()).unwrap()).collect();
    let bw = Bandwidth::Hz2750;
    let cb = Codebook::new(12345, [1, 2, 3, 4, 5], Role::Caller, bw);
    let entries = rx_entries(&cb, 0);
    let mut rx = Receiver::new(bw, &recs, 1, Some(&entries));
    rx.linked = true;
    let (t, mut worst, mut n) = (Instant::now(), 0.0f64, 0usize);
    let chunk_n = std::env::var("XARA_CHUNK").ok().and_then(|v| v.parse().ok()).unwrap_or(960);
    for chunk in audio.chunks(chunk_n) {
        let c = Instant::now();
        n += rx.feed(chunk).len();
        worst = worst.max(c.elapsed().as_secs_f64());
    }
    let secs = audio.len() as f64 / 48000.0;
    println!(
        "expect {:?}: {:6.1} ms CPU per s of audio, worst feed {:6.1} ms, {} events, {secs:.0} s audio",
        a[2..].to_vec(),
        t.elapsed().as_secs_f64() * 1000.0 / secs,
        worst * 1000.0,
        n
    );
}
