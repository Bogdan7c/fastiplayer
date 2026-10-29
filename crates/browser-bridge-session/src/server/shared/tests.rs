use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

#[test]
fn open_gate_runs_the_delivery_and_closed_gate_does_not() {
    let gate = DeliveryGate::new();
    let ran = Mutex::new(0);
    assert!(gate.deliver(|| *ran.lock().unwrap() += 1));
    gate.close();
    assert!(!gate.deliver(|| *ran.lock().unwrap() += 1));
    assert_eq!(*ran.lock().unwrap(), 1, "после close выдача не выполняется");
}

#[test]
fn close_waits_for_a_delivery_that_is_already_running() {
    let gate = Arc::new(DeliveryGate::new());
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let order = Arc::new(Mutex::new(Vec::new()));

    let delivering_gate = Arc::clone(&gate);
    let delivering_order = Arc::clone(&order);
    let delivering = std::thread::spawn(move || {
        delivering_gate.deliver(|| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            delivering_order.lock().unwrap().push("выдача закончена");
        });
    });
    started_rx.recv().unwrap();

    let closing_gate = Arc::clone(&gate);
    let closing_order = Arc::clone(&order);
    let closing = std::thread::spawn(move || {
        closing_gate.close();
        closing_order.lock().unwrap().push("калитка закрыта");
    });
    // close не может завершиться, пока выдача не отпущена.
    std::thread::sleep(Duration::from_millis(50));
    assert!(order.lock().unwrap().is_empty());
    release_tx.send(()).unwrap();
    delivering.join().unwrap();
    closing.join().unwrap();
    assert_eq!(
        *order.lock().unwrap(),
        vec!["выдача закончена", "калитка закрыта"]
    );
}
