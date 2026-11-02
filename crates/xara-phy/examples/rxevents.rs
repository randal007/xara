// Every receiver event on real audio (raw f64le 48 kHz mono, path in argv[1]), expecting argv[2..] levels,
// linked, one line each: to check that a change to the receiver gives the same results.
// XARA_CHUNK: samples per feed (default 960).
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
    let chunk_n = std::env::var("XARA_CHUNK").ok().and_then(|v| v.parse().ok()).unwrap_or(960);
    for chunk in audio.chunks(chunk_n) {
        for e in rx.feed(chunk) {
            println!("{e:?}");
        }
    }
    for e in rx.flush() {
        println!("{e:?}");
    }
}
