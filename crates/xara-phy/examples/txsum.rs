// A checksum of each level's frame audio (2750, fixed input), to check a change keeps the samples.
use xara_phy::hf::level::Bandwidth;
use xara_phy::hf::tx::Modulator;

fn main() {
    for bw in [Bandwidth::Hz500, Bandwidth::Hz2300, Bandwidth::Hz2750] {
        let m = Modulator::new(bw);
        for r in bw.levels() {
            let mut seed = 7u32;
            let mut rand = move |n: u32| {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                (seed >> 8) % n
            };
            let l = m.bw.level(r);
            let a: Vec<u8> = (0..(l.k as usize + 7) / 8).map(|i| (i * 37 + 11) as u8).collect();
            let b: Vec<u8> = a.iter().map(|v| v ^ 0xa5).collect();
            let x = m.transmission(r, &[&a, &b], &mut rand);
            let mut h: u64 = 0xcbf29ce484222325;
            for v in &x {
                for byte in v.to_bits().to_le_bytes() {
                    h = (h ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
            println!("{bw:?} {r:?} {} {h:016x}", x.len());
        }
    }
}
