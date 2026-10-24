use xara_arq::compression::Compression;
use xara_arq::hf::frames::parse;
use xara_arq::hf::session::{Config, CtrlIn, DataIn, FrameIn, Output, Session, State, Tx};
use xara_arq::host::HostEvent;
use xara_arq::time::{SECOND, Time};
use xara_phy::hf::ctrl::Fmt;
use xara_phy::hf::frame::term_marker;
use xara_phy::hf::level::{Bandwidth, Record};

struct Sim {
    s: Session,
    now: Time,
    host: Vec<HostEvent>,
    txq: Vec<Tx>,
}

impl Sim {
    fn caller() -> Self {
        let mut cfg = Config::new(Bandwidth::Hz2300, "N0CALL");
        cfg.cwid = true;
        Self { s: Session::new(cfg), now: 0, host: Vec::new(), txq: Vec::new() }
    }

    fn drain(&mut self) -> Vec<Tx> {
        while let Some(o) = self.s.pop() {
            match o {
                Output::Tx(t) => self.txq.push(t),
                Output::Host(h) => self.host.push(h),
                Output::Listen(_) => {}
            }
        }
        std::mem::take(&mut self.txq)
    }

    fn tx(&mut self) -> Tx {
        let mut t = self.drain();
        assert_eq!(t.len(), 1, "{t:?}");
        self.now += SECOND;
        self.s.on_tx_done(self.now);
        t.remove(0)
    }

    fn none(&mut self) {
        let t = self.drain();
        assert!(t.is_empty(), "{t:?}");
    }

    fn ctrl(&mut self, code: u32, fmt: usize) {
        self.now += SECOND / 5;
        let hits = [31, 15, 14][fmt];
        let fmt = Fmt::from_index(fmt).unwrap();
        self.s.on_ctrl(self.now, CtrlIn { code, fmt, hits, snr_norm: 20.0, t_end: self.now });
    }

    fn wait(&mut self, secs: f64) {
        let end = self.now + (secs * SECOND as f64) as Time;
        while let Some(t) = self.s.next_deadline().filter(|&t| t <= end) {
            self.now = self.now.max(t);
            self.s.poll(self.now);
            let t = self.drain();
            if !t.is_empty() {
                self.txq = t;
                return;
            }
        }
        self.now = end;
        self.s.poll(self.now);
    }
}

fn rec(n: usize) -> Record {
    Record::new(n).unwrap()
}

fn ctrl(code: u32, fmt: usize) -> Tx {
    Tx::Ctrl { code, fmt: Fmt::from_index(fmt).unwrap(), extra: 0 }
}

fn handshake(sim: &mut Sim) {
    let n = if sim.s.max_start_level() > rec(4) { 56 } else { 54 };
    sim.s.connect(0, None, "N1CALL");
    assert_eq!(sim.tx(), Tx::Ctrl { code: 26, fmt: Fmt::F0, extra: 9 });
    sim.ctrl(18, 1);
    let Tx::Data { level, frames } = sim.tx() else { panic!() };
    assert_eq!(level, rec(4));
    assert_eq!(frames[0][..7], [0x39, 0xb0, 0xc1, 0x30, 0xc0, 0x00, 0x00]);
    assert_eq!(frames[0][7] & 0xfe, 0x80);
    let p = parse(&frames[0], term_marker(b"N0CALL")).unwrap();
    assert!(p.last);
    assert_eq!(frames[0][frames[0].len() - 3], 0x82);
    assert_eq!(sim.s.state(), State::HeaderSent);
    sim.ctrl(49, 2);
    assert_eq!(sim.tx(), ctrl(n, 1));
    assert!(matches!(sim.host[..], [.., HostEvent::Connected { .. }]));
    assert_eq!(sim.s.state(), State::Receiving);
    sim.ctrl(49, 2);
    assert_eq!(sim.tx(), ctrl(n, 0));
}

#[test]
fn caller_handshake_follows_session_2300() {
    let mut sim = Sim::caller();
    handshake(&mut sim);
    sim.ctrl(50, 2);
    assert_eq!(sim.tx(), ctrl(21, 0));
    assert_eq!(sim.s.state(), State::Idle);

    let mut sim = Sim::caller();
    handshake(&mut sim);
    sim.s.send(sim.now, b"hello");
    sim.ctrl(50, 2);
    assert_eq!(sim.tx(), ctrl(49, 0));
    assert_eq!(sim.s.state(), State::Break);
    sim.ctrl(14, 2);
    let Tx::Data { level, frames } = sim.tx() else { panic!() };
    assert_eq!((level, frames.len()), (rec(4), 1));
    let p = parse(&frames[0], term_marker(b"N0CALL")).unwrap();
    assert_eq!(p.data.as_deref(), Some(&b"hello"[..]));
    assert!(p.last);
    assert_eq!((p.ctrl.reply, p.ctrl.hint), (2, 0));
}

#[test]
fn break_backoff_uses_the_supplied_draw() {
    for r in 0..3 {
        let mut sim = Sim::caller();
        handshake(&mut sim);
        sim.s.send(sim.now, b"hello");
        sim.ctrl(50, 2);
        assert_eq!(sim.drain(), vec![ctrl(49, 0)]);
        sim.s.set_break_draw(r);
        sim.now += SECOND;
        sim.s.on_tx_done(sim.now);
        assert_eq!(sim.s.next_deadline(), Some(sim.now + (657 + (i64::from(r) + 1) * 1024) * 64));
    }
}

#[test]
fn bulk_transfer_end_and_disconnect_follow_text_2300_chatoff() {
    let mut sim = Sim::caller();
    handshake(&mut sim);
    let data: Vec<u8> = (0..145u32).map(|i| b'a' + (i % 26) as u8).collect();
    sim.s.send(sim.now, &data);
    sim.ctrl(50, 2);
    assert_eq!(sim.tx(), ctrl(49, 0));
    sim.ctrl(14, 2);
    let marker = term_marker(b"N0CALL");
    let Tx::Data { level, frames } = sim.tx() else { panic!() };
    assert_eq!(level, rec(4));
    let p = parse(&frames[0], marker).unwrap();
    assert_eq!((p.full, p.last, p.ctrl.hint), (true, false, 2));
    assert_eq!(frames[0][frames[0].len() - 3], 0x89);
    assert_eq!(p.data.as_ref().unwrap()[..], data[..89]);
    sim.ctrl(1, 2);
    let Tx::Data { level, frames } = sim.tx() else { panic!() };
    assert_eq!(level, rec(4));
    let p = parse(&frames[0], marker).unwrap();
    assert_eq!(frames[0][frames[0].len() - 3], 0x82);
    assert_eq!((p.last, p.next_level), (true, Some(4)));
    assert_eq!(p.data.as_ref().unwrap()[..], data[89..]);
    sim.ctrl(49, 2);
    assert_eq!(sim.tx(), ctrl(14, 1));
    assert!(sim.host.contains(&HostEvent::Buffer(0)));
    sim.ctrl(50, 2);
    assert_eq!(sim.tx(), ctrl(21, 1));
    sim.wait(10.0);
    sim.none();
    sim.ctrl(50, 2);
    assert_eq!(sim.tx(), ctrl(21, 0));
    sim.s.disconnect(sim.now);
    sim.wait(0.5);
    assert_eq!(sim.tx(), ctrl(49, 0));
    sim.ctrl(14, 2);
    assert_eq!(sim.tx(), ctrl(3, 0));
    assert!(matches!(sim.tx(), Tx::CwId { blanks: 0, .. }));
    sim.none();
    assert_eq!(sim.host.last(), Some(&HostEvent::Disconnected));
    assert_eq!(sim.s.state(), State::Listen);
}

#[test]
fn connect_retries_then_gives_up() {
    let mut cfg = Config::new(Bandwidth::Hz2300, "N0CALL");
    cfg.host.retries = 3;
    let mut sim = Sim { s: Session::new(cfg), now: 0, host: Vec::new(), txq: Vec::new() };
    sim.s.connect(0, None, "N1CALL");
    assert_eq!(sim.tx(), Tx::Ctrl { code: 26, fmt: Fmt::F0, extra: 9 });
    let t0 = sim.now;
    sim.wait(1.26);
    sim.none();
    sim.wait(0.02);
    assert_eq!(sim.tx(), ctrl(26, 0));
    assert_eq!(sim.now - SECOND - t0, 950 * 64);
    sim.wait(1.3);
    assert_eq!(sim.tx(), ctrl(26, 0));
    sim.wait(1.3);
    assert!(matches!(sim.tx(), Tx::CwId { .. }));
    sim.none();
    assert!(sim.host.contains(&HostEvent::UnableToConnect));
    assert_eq!(sim.s.state(), State::Listen);
    assert_eq!(sim.host.last(), Some(&HostEvent::Disconnected));
}

fn data_session(registered: bool) -> Sim {
    let mut cfg = Config::new(Bandwidth::Hz2300, "N0CALL");
    cfg.host.registered = registered;
    cfg.host.compression = Compression::Off;
    let mut sim = Sim { s: Session::new(cfg), now: 0, host: Vec::new(), txq: Vec::new() };
    handshake(&mut sim);
    sim.s.send(sim.now, &vec![0x55; 200_000]);
    sim.ctrl(50, 2);
    assert_eq!(sim.tx(), ctrl(49, 0));
    sim.ctrl(14, 2);
    sim
}

fn frames(t: Tx) -> (usize, Vec<Vec<u8>>) {
    match t {
        Tx::Data { level, frames } => (level.get(), frames),
        t => panic!("{t:?}"),
    }
}

#[test]
fn sender_follows_ack_codes() {
    let mut sim = data_session(true);
    let (l, f) = frames(sim.tx());
    assert_eq!((l, f.len(), f[0][f[0].len() - 3]), (4, 1, 0x99));
    sim.ctrl(7, 2);
    assert_eq!(frames(sim.tx()).0, 5);
    sim.ctrl(9, 2);
    assert_eq!(frames(sim.tx()).0, 9);
    sim.ctrl(6, 2);
    assert_eq!(frames(sim.tx()).0, 8);
    for i in 0..6 {
        sim.ctrl(1, 2);
        assert_eq!(frames(sim.tx()).1.len(), if i < 5 { 1 } else { 2 }, "after {} stays", i + 1);
    }
    let before = sim.s.buffered();
    let pay = Bandwidth::Hz2300.level(rec(8)).payload() as usize;
    sim.ctrl(10, 2);
    let (l, f) = frames(sim.tx());
    assert_eq!((l, f.len()), (6, 1), "code 10: L8 -> L7 has skip_on_step_down -> L6, frame 1 resent alone");
    assert_eq!(sim.s.buffered(), before - pay);
    let pay6 = Bandwidth::Hz2300.level(rec(6)).payload() as usize;
    assert_eq!(parse(&f[0], term_marker(b"N0CALL")).unwrap().data.unwrap().len(), pay.min(pay6));
    sim.ctrl(2, 2);
    assert_eq!(frames(sim.tx()).0, 5);
    sim.ctrl(19, 2);
    assert_eq!(frames(sim.tx()).0, 3);
}

#[test]
fn sender_polls_with_alternating_req() {
    let mut sim = data_session(false);
    frames(sim.tx());
    sim.wait(5.0);
    assert_eq!(sim.tx(), ctrl(12, 0));
    sim.wait(5.0);
    assert_eq!(sim.tx(), ctrl(12, 0));
    sim.ctrl(1, 2);
    frames(sim.tx());
    sim.wait(5.0);
    assert_eq!(sim.tx(), ctrl(13, 0));
}

fn data_frame(level: Record, data: &[u8], hint: u8, last: bool) -> Vec<u8> {
    let len = Bandwidth::Hz2300.level(level).frame_bytes() as usize;
    let ctrl = xara_phy::hf::frame::Control { reply: 2, compressed: false, hint };
    xara_phy::hf::frame::build_frame(len, data, ctrl, last.then_some(4), term_marker(b"N0CALL"))
}

#[test]
fn receiver_acks_and_takes_the_turn() {
    let mut cfg = Config::new(Bandwidth::Hz2300, "N0CALL");
    cfg.host.registered = true;
    cfg.host.compression = Compression::Off;
    let mut sim = Sim { s: Session::new(cfg), now: 0, host: Vec::new(), txq: Vec::new() };
    handshake(&mut sim);
    assert_eq!(sim.s.level(), rec(6));
    let mut got = Vec::new();
    let sns = [5.0, 9.0, 12.0];
    for (i, &sn) in sns.iter().enumerate() {
        let lv = sim.s.level();
        let body = vec![i as u8; Bandwidth::Hz2300.level(lv).payload() as usize];
        got.extend_from_slice(&body);
        sim.now += SECOND;
        let fr = FrameIn { ok: true, bytes: data_frame(lv, &body, 6, false), snr_db: sn };
        sim.s.on_data(sim.now, DataIn { level: lv, frames: vec![fr], t_end: sim.now });
        let Tx::Ctrl { code, fmt: Fmt::F2, .. } = sim.tx() else { panic!() };
        let step = match code {
            1 => 0,
            7 => 1,
            5 => 2,
            8 => 3,
            9 => 4,
            6 => -1,
            c => panic!("frame {i}: code {c}"),
        };
        assert_eq!(sim.s.level().get() as i64, lv.get() as i64 + step);
    }
    sim.now += SECOND;
    let lv = sim.s.level();
    let fr = FrameIn { ok: true, bytes: data_frame(lv, b"bye", 0, true), snr_db: 12.0 };
    sim.s.on_data(sim.now, DataIn { level: lv, frames: vec![fr], t_end: sim.now });
    assert_eq!(sim.tx(), ctrl(49, 2));
    assert_eq!(sim.s.state(), State::Break);
    got.extend_from_slice(b"bye");
    let rx: Vec<u8> = sim
        .host
        .iter()
        .filter_map(|h| if let HostEvent::Data(d) = h { Some(d.clone()) } else { None })
        .flatten()
        .collect();
    assert_eq!(rx, got);
    sim.ctrl(14, 2);
    assert_eq!(sim.tx(), ctrl(50, 2));
    assert_eq!(sim.s.state(), State::IdleSent);
}

#[test]
fn receiver_nacks_and_answers_polls() {
    let mut sim = Sim::caller();
    handshake(&mut sim);
    sim.now += SECOND;
    let fr = FrameIn { ok: false, bytes: Vec::new(), snr_db: 0.0 };
    sim.s.on_data(sim.now, DataIn { level: rec(4), frames: vec![fr], t_end: sim.now });
    assert_eq!(sim.tx(), ctrl(2, 0));
    assert_eq!(sim.s.level(), rec(3));
    sim.ctrl(12, 0);
    assert_eq!(sim.tx(), ctrl(2, 0));
    sim.ctrl(13, 0);
    assert_eq!(sim.tx(), ctrl(19, 0));
    assert_eq!(sim.s.level(), rec(1));
}

#[test]
fn receiver_answers_unseen_req_with_19_one_format_lower() {
    let mut sim = Sim::caller();
    handshake(&mut sim);
    sim.ctrl(12, 2);
    assert_eq!(sim.tx(), ctrl(19, 0));
}

#[test]
fn receiver_abort_waits_for_the_frame_window() {
    let mut sim = Sim::caller();
    handshake(&mut sim);
    sim.s.abort(sim.now);
    sim.wait(4.0);
    sim.none();
    sim.wait(4.0);
    let Tx::Ctrl { code: 3, .. } = sim.tx() else { panic!() };
}

#[test]
fn receiver_abort_answers_the_next_frame_with_qrt() {
    let mut sim = Sim::caller();
    handshake(&mut sim);
    sim.s.abort(sim.now);
    sim.now += 2 * SECOND;
    let fr = FrameIn { ok: true, bytes: data_frame(rec(4), b"hello", 6, false), snr_db: 10.0 };
    sim.s.on_data(sim.now, DataIn { level: rec(4), frames: vec![fr], t_end: sim.now });
    let Tx::Ctrl { code: 3, .. } = sim.tx() else { panic!() };
    assert!(!sim.host.iter().any(|h| matches!(h, HostEvent::Data(_))));
}

#[test]
fn compressed_frame_with_a_lost_frame_2_is_still_expanded() {
    let mut cfg = Config::new(Bandwidth::Hz2300, "N0CALL");
    (cfg.host.registered, cfg.host.compression) = (true, Compression::Text);
    let mut sim = Sim { s: Session::new(cfg), now: 0, host: Vec::new(), txq: Vec::new() };
    sim.s.connect(0, None, "N1CALL");
    sim.tx();
    sim.ctrl(18, 1);
    sim.tx();
    sim.ctrl(49, 2);
    sim.tx();
    assert_eq!(sim.s.state(), State::Receiving);
    let lv = sim.s.level();
    let len = Bandwidth::Hz2300.level(lv).frame_bytes() as usize;
    let e = xara_arq::textcomp::compress(b"[WL2K-5.0-B2FWIHJM$]\r", len - 8, 1);
    let ctrl = xara_phy::hf::frame::Control { reply: 2, compressed: true, hint: 6 };
    let bytes = xara_phy::hf::frame::build_frame(len, &e.bytes, ctrl, None, term_marker(b"N0CALL"));
    let lost = FrameIn { ok: false, bytes: Vec::new(), snr_db: 0.0 };
    sim.now += SECOND;
    sim.s.on_data(
        sim.now,
        DataIn { level: lv, frames: vec![FrameIn { ok: true, bytes, snr_db: 20.0 }, lost], t_end: sim.now },
    );
    sim.drain();
    let rx: Vec<u8> = sim
        .host
        .iter()
        .filter_map(|h| if let HostEvent::Data(d) = h { Some(d.clone()) } else { None })
        .flatten()
        .collect();
    assert_eq!(rx, &b"[WL2K-5.0-B2FWIHJM$]\r"[..e.consumed]);
}
