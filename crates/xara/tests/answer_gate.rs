mod common;
use common::Lab;
use std::sync::atomic::Ordering;
use xara::signals::{ANSWER_GATE, GATE_CLOSED, GATE_NONE, GATE_OPEN};

fn call(lab: &mut Lab, a: &mut common::Client) -> bool {
    a.send("CONNECT XX0A XX0B");
    let al = a.lines.clone();
    lab.run_until(40.0, |_| al.lock().unwrap().iter().any(|l| l.starts_with("CONNECTED")))
}

/// X6100 XARA: with the gate closed the station with no host answers no
/// call; opening it (SIGRTMIN) lets the next call through. One test, since
/// the gate is process-wide.
#[test]
fn answer_gate_blocks_until_opened() {
    ANSWER_GATE.store(GATE_CLOSED, Ordering::Relaxed);
    let mut lab = Lab::new(2);
    let mut a = lab.attach(0);
    lab.settle();
    a.send("RETRIES 2");
    lab.settle();
    assert!(!call(&mut lab, &mut a), "answered with the gate closed: {:?}", a.since(0));
    lab.run_until(30.0, |l| l.tnc[0].station().is_free());
    lab.settle();
    ANSWER_GATE.store(GATE_OPEN, Ordering::Relaxed);
    assert!(call(&mut lab, &mut a), "no answer with the gate open: {:?}", a.since(0));
    ANSWER_GATE.store(GATE_NONE, Ordering::Relaxed);
}
