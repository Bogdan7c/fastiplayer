//! Владение привязкой освобождения при пересоздании рендера.

use super::*;

/// Рендер, который никому не передал привязку, отвязывает её при уничтожении.
#[test]
fn a_binding_nobody_adopted_is_detached_at_shutdown() {
    let owned = OwnedReleaseBinding::new();

    let report = owned
        .detach_for_shutdown()
        .expect("an unshared binding is detached by its owner");

    assert!(!report.already_detached);
}

/// Принятая преемником привязка предшественником больше не отвязывается: иначе уничтожение
/// старого рендера выключило бы освобождение кадров нового.
#[test]
fn a_binding_adopted_by_a_successor_is_not_detached_by_the_predecessor() {
    let predecessor = OwnedReleaseBinding::new();
    let mut successor = OwnedReleaseBinding::new();

    successor.adopt(predecessor.parts_for_successor());

    assert!(
        predecessor.detach_for_shutdown().is_none(),
        "the predecessor must leave the adopted binding alone"
    );
    // Преемник — настоящий владелец: его отвязка работает.
    let report = successor
        .detach_for_shutdown()
        .expect("the successor owns the binding now");
    assert!(!report.already_detached);
}

/// Пока преемник не принял привязку (кандидат отброшен), владелец остаётся прежним.
#[test]
fn offering_a_binding_without_adoption_changes_nothing() {
    let predecessor = OwnedReleaseBinding::new();
    let _offered = predecessor.parts_for_successor();

    assert!(predecessor.detach_for_shutdown().is_some());
}

/// Преемник и предшественник работают с ОДНОЙ привязкой: статистика видна обоим.
#[test]
fn successor_and_predecessor_share_one_binding() {
    let predecessor = OwnedReleaseBinding::new();
    let mut successor = OwnedReleaseBinding::new();
    successor.adopt(predecessor.parts_for_successor());

    // Отвязка преемника переводит общую привязку в режим «отвязана»: повтор у предшественника
    // это видит (no-op), хотя предшественник сам привязку не трогал.
    let _ = successor.detach_for_shutdown();
    let again = predecessor
        .binding()
        .renderer_detaching(RendererDetachGrace::DEFAULT);
    assert!(again.already_detached);
}
