use std::sync::mpsc;
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use super::{RESOURCE_LOOKUP_LOCK_BUDGET, lock_within_budget};

/// Сколько тестовый «декодер» держит замок, прежде чем отпустить.
const BRIEF_HOLD: Duration = Duration::from_millis(20);

/// Бюджет в тестах с запасом на загруженный CI.
const GENEROUS_BUDGET: Duration = Duration::from_secs(5);

/// Держит замок в другом потоке; возвращает, когда замок точно взят, и поток-держатель.
fn hold_lock_for(mutex: &Arc<Mutex<u32>>, hold: Duration) -> thread::JoinHandle<()> {
    let (locked_tx, locked_rx) = mpsc::channel();
    let holder_mutex = Arc::clone(mutex);
    let holder = thread::spawn(move || {
        let _guard = holder_mutex.lock().expect("holder takes the lock");
        locked_tx.send(()).expect("test waits for the holder");
        thread::sleep(hold);
    });
    locked_rx.recv().expect("holder reports the lock");
    holder
}

/// Замок, который отпускают в пределах бюджета, берётся — а не отвечает `Busy`.
#[test]
fn briefly_held_lock_is_acquired_within_budget() {
    let mutex = Arc::new(Mutex::new(7));
    let holder = hold_lock_for(&mutex, BRIEF_HOLD);
    let started_at = Instant::now();

    let guard = lock_within_budget(&mutex, GENEROUS_BUDGET).expect("lock released within budget");

    assert_eq!(*guard, 7);
    assert!(
        started_at.elapsed() >= BRIEF_HOLD / 2,
        "the call really waited for the holder"
    );
    drop(guard);
    holder.join().expect("holder thread");
}

/// Замок держат дольше бюджета — ответ прежний `WouldBlock` (поиск вернёт `Busy`), и не раньше
/// исчерпания бюджета.
#[test]
fn lock_held_longer_than_budget_still_reports_would_block() {
    let mutex = Arc::new(Mutex::new(0));
    let holder = hold_lock_for(&mutex, Duration::from_millis(300));
    let budget = Duration::from_millis(30);
    let started_at = Instant::now();

    let outcome = lock_within_budget(&mutex, budget);

    assert!(matches!(outcome, Err(TryLockError::WouldBlock)));
    assert!(started_at.elapsed() >= budget);
    holder.join().expect("holder thread");
}

/// Свободный замок берётся сразу, отравленный — возвращается сразу, без ожидания бюджета.
#[test]
fn free_and_poisoned_locks_return_immediately() {
    let free = Mutex::new(1);
    assert_eq!(
        *lock_within_budget(&free, GENEROUS_BUDGET).expect("free lock"),
        1
    );

    let poisoned = Arc::new(Mutex::new(2));
    let poisoning = Arc::clone(&poisoned);
    let panic_result = thread::spawn(move || {
        let _guard = poisoning.lock().expect("lock before poisoning");
        panic!("poison the lock on purpose");
    })
    .join();
    assert!(panic_result.is_err());
    let started_at = Instant::now();

    let outcome = lock_within_budget(&poisoned, GENEROUS_BUDGET);

    assert!(matches!(outcome, Err(TryLockError::Poisoned(_))));
    assert!(started_at.elapsed() < Duration::from_secs(1));
}

/// Бюджет production — доли миллисекунды: на кадр 60 fps (16,7 мс) он не влияет заметно.
#[test]
fn production_budget_is_a_small_fraction_of_a_frame() {
    assert!(RESOURCE_LOOKUP_LOCK_BUDGET <= Duration::from_micros(500));
    assert!(RESOURCE_LOOKUP_LOCK_BUDGET >= Duration::from_micros(50));
}
