// How long a data frame takes to build, per level (2750), single frame.
use std::time::Instant;
use xara_phy::hf::level::{Bandwidth, Record};
use xara_phy::hf::tx::Modulator;

fn main() {
    let m = Modulator::new(Bandwidth::Hz2750);
    let mut seed = 1u32;
    let mut rand = move |n: u32| {
        seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
        (seed >> 8) % n
    };
    for n in 1..=17 {
        let Some(r) = Record::new(n) else { continue };
        let l = m.bw.level(r);
        let frame = vec![0x5au8; (l.k as usize + 7) / 8];
        let t = Instant::now();
        let reps = 3;
        let mut len = 0;
        for _ in 0..reps {
            len = m.transmission(r, &[&frame], &mut rand).len();
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / reps as f64;
        println!("L{n:2} nc {:2}  audio {:5.2} s  build {:7.1} ms", l.nc(), len as f64 / 48000.0, ms);
    }
}
