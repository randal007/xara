mod common;

use common::hexv;
use xara_arq::fm;
use xara_arq::host::HostEvent;
use xara_arq::time::{SECOND, Time};
use xara_phy::fm::ctrl::code::{CONNECT_REPLY_NARROW, GO_AHEAD};
use xara_phy::fm::level::{FmBandwidth, Record};
use xara_testkit::json::Value;

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

fn secs(t: f64) -> Time {
    (t * SECOND as f64) as Time
}

fn caller() -> fm::Session {
    let mut s = fm::Session::new(fm::Config::new(FmBandwidth::Narrow, "N0CALL"));
    assert!(s.connect(0, None, "N1CALL", &[]));
    drain(&mut s);
    s.on_tx_done(secs(8.0));
    s.on_ctrl(secs(9.0), fm::CtrlIn { code: CONNECT_REPLY_NARROW, lead_row: 1, t_end: secs(9.0) });
    assert_eq!(ctrl_codes(&drain(&mut s)), [GO_AHEAD]);
    s.on_tx_done(secs(9.9));
    s
}

fn cases(run: &str, dir: &str) -> Vec<Value> {
    let v = common::json("fm/lab_frames.json");
    v["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["run"] == run && c["dir"] == dir && c["record"].as_u64() != Some(0))
        .cloned()
        .collect()
}

fn frames(c: &Value) -> Vec<Vec<u8>> {
    match c["frame_hex"].as_array() {
        Some(a) => a.iter().map(hexv).collect(),
        None => vec![hexv(&c["frame_hex"])],
    }
}

fn replay(run: &str, polls: &[(u64, u32)], acks: &[u32]) -> (Vec<u8>, Vec<Vec<Vec<u8>>>) {
    let cs = cases(run, "ba");
    let mut s = caller();
    let (mut got, mut codes, mut pay) = (Vec::new(), Vec::new(), Vec::new());
    for c in &cs {
        let t_end = secs(c["t"].as_f64().unwrap() + 11.0);
        let rule = c["rule"].as_str().unwrap_or("");
        let idx = c["idx"].as_u64().unwrap();
        let fr = frames(c);
        let lens = c["fields"].as_array().unwrap().iter().map(|f| f["data_len"].as_u64().unwrap() as usize);
        pay.push(fr.iter().zip(lens).map(|(f, n)| f[..n].to_vec()).collect::<Vec<_>>());
        let lost = |i: usize| match rule {
            "zero@0.6-5.38" => i == 0,
            "zero@5.38-11" => i == 1,
            _ => false,
        };
        let frames = fr.into_iter().enumerate().map(|(i, bytes)| fm::FrameIn { ok: !lost(i), bytes, snr_db: 40.0 });
        let d = fm::DataIn {
            record: Record::from_index(c["record"].as_u64().unwrap() as usize).unwrap(),
            frames: frames.collect(),
            t_end,
        };
        if rule != "mute" {
            s.on_data(t_end, d);
        }
        let out = drain(&mut s);
        got.extend(delivered(&out));
        codes.extend(ctrl_codes(&out));
        s.on_tx_done(t_end + SECOND / 4);
        for &(_, code) in polls.iter().filter(|p| p.0 == idx) {
            let t = t_end + 2 * SECOND;
            s.on_ctrl(t, fm::CtrlIn { code, lead_row: 1, t_end: t });
            codes.extend(ctrl_codes(&drain(&mut s)));
            s.on_tx_done(t + SECOND / 4);
        }
    }
    assert_eq!(codes, acks, "{run}");
    (got, pay)
}

#[test]
fn f13_frame_1_resend_reassembles_like_vara() {
    let (got, p) = replay("f13_mute_f1", &[], &[7, 1, 1, 10, 1, 4]);
    let want =
        [&p[0][0][..], &p[1][0], &p[2][0], &p[4][0], &p[5][0], &p[5][1][..37], &p[3][1], &p[5][1][37..]].concat();
    assert_eq!(want.len(), 3000);
    assert!(got == want);
}

#[test]
fn f05_frame_2_resend_reassembles_like_vara() {
    let (got, p) = replay("f05_mute_f2", &[], &[7, 1, 1, 11, 1, 1, 4]);
    let want = [&p[0][0][..], &p[1][0], &p[2][0], &p[3][0], &p[4][0], &p[5][0], &p[6][0]].concat();
    assert_eq!(want.len(), 3000);
    assert!(got == want);
}

#[test]
fn f04_lost_ack_is_repeated_not_redelivered() {
    let (got, p) = replay("f04_mute_ack", &[(3, 13)], &[7, 1, 1, 1, 6, 4]);
    let want = [&p[0][0][..], &p[1][0], &p[2][0], &p[3][0], &p[3][1], &p[4][0]].concat();
    assert_eq!(want.len(), 3000);
    assert!(got == want);
}

#[test]
fn f03_lost_data_is_resent_once() {
    let (got, p) = replay("f03_mute_data", &[(4, 12)], &[7, 1, 2, 1, 1, 1, 1, 4]);
    let want = [&p[0][0][..], &p[1][0], &p[3][0], &p[4][0], &p[5][0], &p[5][1], &p[6][0], &p[6][1], &p[7][0]].concat();
    assert_eq!(want.len(), 3000);
    assert!(got == want);
}
