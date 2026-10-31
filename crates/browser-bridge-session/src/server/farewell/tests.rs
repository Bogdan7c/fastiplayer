use std::time::{Duration, Instant};

use browser_bridge_protocol::SessionKey;

use super::*;
use crate::registry::SessionInstance;

fn session(instance: u64) -> SessionRef {
    SessionRef {
        key: SessionKey::new("tab").unwrap(),
        instance: SessionInstance(instance),
    }
}

fn corridors(capacity: usize) -> FarewellCorridors {
    FarewellCorridors::new(&BrowserSessionPolicy {
        farewell_grace: Duration::from_secs(1),
        max_farewell_corridors: capacity,
        ..BrowserSessionPolicy::default()
    })
}

#[test]
fn slot_hands_out_the_farewell_once() {
    let slot = FarewellSlot::default();
    assert_eq!(slot.take(), None, "пустой слот ничего не отдаёт");
    slot.leave("bye".to_owned());
    assert_eq!(slot.take().as_deref(), Some("bye"));
    assert_eq!(slot.take(), None, "прощание пишется один раз");
}

#[test]
fn open_corridor_is_claimed_once_then_reported_closed() {
    let mut table = corridors(4);
    let now = Instant::now();
    assert_eq!(
        table.remember_replaced(session(1), Some(ConnectionId(7)), now),
        None
    );
    assert_eq!(
        table.claim(&session(1)),
        CorridorClaim::Claimed(ConnectionId(7))
    );
    assert_eq!(table.claim(&session(1)), CorridorClaim::AlreadyClosed);
    // Занятый коридор сроком больше не закрывается: соединение уже закрывает прощание.
    assert!(table.expire(now + Duration::from_secs(5)).is_empty());
}

#[test]
fn unknown_and_connectionless_instances_are_told_apart() {
    let mut table = corridors(4);
    let now = Instant::now();
    assert_eq!(table.remember_replaced(session(1), None, now), None);
    assert_eq!(table.claim(&session(1)), CorridorClaim::NoConnection);
    assert_eq!(table.claim(&session(2)), CorridorClaim::Unknown);
}

#[test]
fn corridor_expires_exactly_at_its_deadline() {
    let mut table = corridors(4);
    let now = Instant::now();
    let _ = table.remember_replaced(session(1), Some(ConnectionId(3)), now);
    assert!(
        table.expire(now + Duration::from_millis(999)).is_empty(),
        "до срока коридор открыт"
    );
    assert_eq!(
        table.expire(now + Duration::from_secs(1)),
        vec![ConnectionId(3)]
    );
    assert!(
        table.expire(now + Duration::from_secs(2)).is_empty(),
        "истекает один раз"
    );
    assert_eq!(table.claim(&session(1)), CorridorClaim::AlreadyClosed);
}

#[test]
fn released_connection_closes_only_its_own_corridor() {
    let mut table = corridors(4);
    let now = Instant::now();
    let _ = table.remember_replaced(session(1), Some(ConnectionId(1)), now);
    let _ = table.remember_replaced(session(2), Some(ConnectionId(2)), now);
    table.connection_released(ConnectionId(1));
    assert_eq!(table.claim(&session(1)), CorridorClaim::AlreadyClosed);
    assert_eq!(
        table.claim(&session(2)),
        CorridorClaim::Claimed(ConnectionId(2))
    );
}

#[test]
fn overflow_evicts_the_oldest_and_reports_its_open_connection() {
    let mut table = corridors(2);
    let now = Instant::now();
    assert_eq!(
        table.remember_replaced(session(1), Some(ConnectionId(1)), now),
        None
    );
    assert_eq!(table.remember_replaced(session(2), None, now), None);
    // Третий вытесняет первый, тот ещё ждал прощания — его надо закрыть сейчас.
    assert_eq!(
        table.remember_replaced(session(3), Some(ConnectionId(3)), now),
        Some(ConnectionId(1))
    );
    assert_eq!(table.claim(&session(1)), CorridorClaim::Unknown, "забыт");
    // Четвёртый вытесняет второй (без соединения) — закрывать нечего.
    assert_eq!(
        table.remember_replaced(session(4), Some(ConnectionId(4)), now),
        None
    );
    assert_eq!(
        table.claim(&session(3)),
        CorridorClaim::Claimed(ConnectionId(3))
    );
}

#[test]
fn shutdown_closes_every_open_corridor_and_nothing_else() {
    let mut table = corridors(4);
    let now = Instant::now();
    let _ = table.remember_replaced(session(1), Some(ConnectionId(1)), now);
    let _ = table.remember_replaced(session(2), None, now);
    let _ = table.remember_replaced(session(3), Some(ConnectionId(3)), now);
    assert_eq!(
        table.claim(&session(3)),
        CorridorClaim::Claimed(ConnectionId(3))
    );
    assert_eq!(table.close_all(), vec![ConnectionId(1)]);
    assert_eq!(table.claim(&session(1)), CorridorClaim::AlreadyClosed);
    assert_eq!(table.claim(&session(2)), CorridorClaim::NoConnection);
}
