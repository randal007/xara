use xara_arq::compression::Compression;
use xara_arq::fm;
use xara_arq::host::HostEvent;
use xara_arq::textcomp::compress;
use xara_arq::time::{SECOND, Time};
use xara_phy::fm::ctrl::code::{ACK_STAY, CONNECT_REPLY_NARROW, GO_AHEAD, NACK};
use xara_phy::fm::frame::{Trailer, build_data};
use xara_phy::fm::level::{FmBandwidth, Record};

fn drain(s: &mut fm::Session) -> Vec<fm::Output> {
    std::iter::from_fn(|| s.pop()).collect()
}

fn ctrl_codes(out: &[fm::Output]) -> Vec<u32> {
    out.iter()
        .filter_map(|o| if let fm::Output::Tx(fm::Tx::Ctrl { code, .. }) = o { Some(*code) } else { None })
        .collect()
}

fn delivered(out: &[fm::Output]) -> Vec<u8> {
    out.iter()
        .filter_map(|o| if let fm::Output::Host(HostEvent::Data(d)) = o { Some(d.clone()) } else { None })
        .flatten()
        .collect()
}

fn text_receiver() -> (fm::Session, Time) {
    let mut cfg = fm::Config::new(FmBandwidth::Narrow, "N0CALL");
    (cfg.host.registered, cfg.host.compression) = (true, Compression::Text);
    let mut s = fm::Session::new(cfg);
    assert!(s.connect(0, None, "N1CALL", &[]));
    drain(&mut s);
    s.on_tx_done(SECOND);
    let t = SECOND + SECOND / 2;
    s.on_ctrl(t, fm::CtrlIn { code: CONNECT_REPLY_NARROW, lead_row: 1, t_end: t });
    assert_eq!(ctrl_codes(&drain(&mut s)), [GO_AHEAD]);
    assert_eq!(s.state(), fm::State::Receiving);
    s.on_tx_done(2 * SECOND);
    (s, 3 * SECOND)
}

fn start_frame(data: &[u8]) -> fm::DataIn {
    let len = FmBandwidth::Narrow.level(Record::Start).frame_bytes();
    let trailer = Trailer { lead_report: 1, level: 0, chain_snr: None };
    let bytes = build_data(Record::Start, len, data, true, 2, false, trailer);
    fm::DataIn { record: Record::Start, frames: vec![fm::FrameIn { ok: true, bytes, snr_db: 30.0 }], t_end: 3 * SECOND }
}

#[test]
fn compressed_frame_is_expanded_and_acked() {
    let (mut s, now) = text_receiver();
    let e = compress(b"the message ", 10, 1);
    s.on_data(now, start_frame(&e.bytes));
    let out = drain(&mut s);
    assert_eq!(delivered(&out), &b"the message "[..e.consumed]);
    assert_eq!(ctrl_codes(&out), [ACK_STAY]);
}

#[test]
fn compressed_frame_that_does_not_expand_is_nacked() {
    let (mut s, now) = text_receiver();
    s.on_data(now, start_frame(&[0]));
    let out = drain(&mut s);
    assert!(delivered(&out).is_empty(), "{out:?}");
    assert_eq!(ctrl_codes(&out), [NACK]);
}

#[test]
fn compressed_frame_with_a_lost_frame_2_is_still_expanded() {
    let (mut s, now) = text_receiver();
    let e = compress(b"[WL2K-5.0-B2FWIHJM$]\r", 30, 1);
    let mut d = start_frame(&e.bytes);
    d.frames.push(fm::FrameIn { ok: false, bytes: Vec::new(), snr_db: 0.0 });
    s.on_data(now, d);
    let out = drain(&mut s);
    assert_eq!(delivered(&out), &b"[WL2K-5.0-B2FWIHJM$]\r"[..e.consumed], "{out:?}");
}
