//! Тесты `drain_playlist_persistence`: quarantine-имя строится только при реальном completion.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};

use playlist_state::{PlaylistStateStore, QuarantineFileName};

use super::*;
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::playlist_runtime::PlaylistStartupPhase;
use crate::playlist_runtime::persistence::QuarantineNamePolicy;

/// Policy-счётчик: считает, сколько раз runtime запросил quarantine-имя.
struct CountingQuarantineNamePolicy {
    calls: Rc<Cell<usize>>,
    timestamp: SystemTime,
}

impl QuarantineNamePolicy for CountingQuarantineNamePolicy {
    fn next_name(&mut self) -> QuarantineFileName {
        self.calls.set(self.calls.get() + 1);
        QuarantineFileName::from_timestamp(self.timestamp)
    }
}

/// Runtime со счётчиком вызовов quarantine-policy и фиксированным timestamp.
fn runtime_with_counting_policy(timestamp: SystemTime) -> (PlaylistRuntime, Rc<Cell<usize>>) {
    let mut runtime =
        PlaylistRuntime::new(AppWakePort::disconnected(AppWakeOwner::PlaylistRuntime));
    let calls = Rc::new(Cell::new(0));
    runtime
        .persistence
        .replace_quarantine_name_policy(Box::new(CountingQuarantineNamePolicy {
            calls: Rc::clone(&calls),
            timestamp,
        }));
    (runtime, calls)
}

/// Крутит drain, пока startup не станет Ready (inspection идёт в фоновом потоке).
fn drain_persistence_until_ready(runtime: &mut PlaylistRuntime) {
    for _ in 0..20_000 {
        runtime.drain_playlist_persistence().expect("startup drain");
        if runtime.playlist_startup_view().phase == PlaylistStartupPhase::Ready {
            return;
        }
        thread::yield_now();
    }
    panic!("startup did not become ready within bounded test spins");
}

#[test]
fn drain_without_completion_never_asks_for_quarantine_name() {
    let (mut runtime, calls) = runtime_with_counting_policy(SystemTime::UNIX_EPOCH);

    // Inspection ещё не запущена: completion-а нет ни на одном drain-е.
    for _ in 0..50 {
        let changed = runtime.drain_playlist_persistence().expect("idle drain");
        assert!(!changed, "idle drain must not report a change");
    }

    assert_eq!(calls.get(), 0, "name must not be built without completion");
}

#[test]
fn drain_with_completion_asks_for_quarantine_name_exactly_once() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let store = Arc::new(PlaylistStateStore::new(
        directory
            .path()
            .join(playlist_state::PLAYLIST_STATE_FILENAME),
    ));
    let (mut runtime, calls) = runtime_with_counting_policy(SystemTime::UNIX_EPOCH);
    runtime
        .begin_production_playlist_state_inspection(store)
        .expect("start inspection");

    drain_persistence_until_ready(&mut runtime);
    // Дополнительные drain-ы после Ready не должны снова запрашивать имя.
    for _ in 0..20 {
        runtime
            .drain_playlist_persistence()
            .expect("post-ready drain");
    }

    assert_eq!(calls.get(), 1);
}

#[test]
fn corrupt_state_is_quarantined_under_the_policy_name() {
    let directory = tempfile::tempdir().expect("temporary state directory");
    let state_path = directory
        .path()
        .join(playlist_state::PLAYLIST_STATE_FILENAME);
    std::fs::write(&state_path, br#"{"schema_version":1,"queue":{}}"#).expect("corrupt fixture");
    let timestamp = SystemTime::UNIX_EPOCH + Duration::from_secs(17);
    let (mut runtime, calls) = runtime_with_counting_policy(timestamp);
    runtime
        .begin_production_playlist_state_inspection(Arc::new(PlaylistStateStore::new(
            state_path.clone(),
        )))
        .expect("start inspection");

    drain_persistence_until_ready(&mut runtime);

    // Два completion-а (inspection, затем quarantine) -> по одному имени на каждый.
    assert_eq!(calls.get(), 2);
    assert!(!state_path.exists(), "corrupt source must be moved away");
    assert!(
        directory
            .path()
            .join("playlist-state.corrupt-17-000000000.json")
            .exists(),
        "quarantine file must carry the policy-provided name"
    );
}
