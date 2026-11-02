// Every data frame in a recording (raw f64le 48 kHz mono, argv[1]), expecting argv[2..] levels, linked:
// end time, level, S/N and the frame's control byte (queue hint, last, full) to compare with the sender's queue.
use xara_phy::hf::ctrl::{Codebook, Role};
use xara_phy::hf::frame::Control;
use xara_phy::hf::level::{Bandwidth, Record};
use xara_phy::hf::rx::ctrl::rx_entries;
use xara_phy::hf::rx::receiver::Event;
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
    let show = |e: Event| {
        if let Event::Frame(f) = e {
            let d = &f.frame;
            let b = &d.bytes;
            if !d.ok || b.len() < 4 {
                return println!("{:8.2} L{:<2} ok={} ", d.t_end as f64 / 3000.0, d.record.get(), d.ok);
            }
            let (c, last, full) = Control::from_byte(b[b.len() - 3]);
            println!(
                "{:8.2} L{:<2} sn {:5.1} len {:4} hint {} last {} full {} next {:?}",
                d.t_end as f64 / 3000.0,
                d.record.get(),
                d.snr_db,
                b.len(),
                c.hint,
                last as u8,
                full as u8,
                last.then(|| b[b.len() - 4])
            );
        }
    };
    for chunk in audio.chunks(960) {
        rx.feed(chunk).into_iter().for_each(show);
    }
    rx.flush().into_iter().for_each(show);
}
