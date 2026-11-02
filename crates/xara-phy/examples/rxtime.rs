// How fast the HF receiver runs while it waits (noise in), per expected level, linked:
// seconds of CPU per second of audio. Above 1 on the radio it falls behind.
use std::time::Instant;
use xara_phy::hf::ctrl::{Codebook, Role};
use xara_phy::hf::level::{Bandwidth, Record};
use xara_phy::hf::rx::ctrl::rx_entries;
use xara_phy::hf::rx::receiver::Receiver;

fn main() {
    let bw = Bandwidth::Hz2750;
    let cb = Codebook::new(12345, [1, 2, 3, 4, 5], Role::Caller, bw);
    let entries = rx_entries(&cb, 0);
    let secs = 20.0;
    let mut seed = 99u64;
    let mut noise = move || {
        seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
        (seed as f64 / u64::MAX as f64 - 0.5) * 0.02
    };
    let audio: Vec<f64> = (0..(secs * 48000.0) as usize).map(|_| noise()).collect();
    let mut sets: Vec<(String, Vec<Record>)> = vec![("ctrl only".into(), vec![])];
    for n in [1, 4, 5, 7, 9, 10, 11, 12, 14, 17] {
        sets.push((format!("L{n}"), vec![Record::new(n).unwrap()]));
    }
    // Or the sets in argv, e.g. "10,13" "10,11,12,13".
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        sets = args
            .iter()
            .map(|a| (format!("L{a}"), a.split(',').map(|n| Record::new(n.parse().unwrap()).unwrap()).collect()))
            .collect();
    }
    for (name, recs) in sets {
        let mut rx = Receiver::new(bw, &recs, 1, Some(&entries));
        rx.linked = true;
        let t = Instant::now();
        for chunk in audio.chunks(960) {
            rx.feed(chunk);
        }
        let s = t.elapsed().as_secs_f64();
        println!("{name:10}: {:6.1} ms CPU per s of audio", s * 1000.0 / secs);
    }
}
