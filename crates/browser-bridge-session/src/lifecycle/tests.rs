use super::*;

fn key(text: &str) -> SessionRef {
    instance_of(text, 1)
}

fn instance_of(text: &str, instance: u64) -> SessionRef {
    SessionRef {
        key: browser_bridge_protocol::SessionKey::new(text).unwrap(),
        instance: crate::registry::SessionInstance(instance),
    }
}

fn ended(text: &str, reason: SessionEndReason) -> SessionLifecycleEvent {
    SessionLifecycleEvent::Ended {
        session: key(text),
        reason,
    }
}

const IDLE: Duration = Duration::from_secs(30);

#[test]
fn fresh_process_keeps_running_until_the_idle_deadline_then_exits() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    assert_eq!(
        lifecycle.verdict(start + Duration::from_secs(29)),
        LifecycleVerdict::KeepRunning {
            wake_at: Some(start + IDLE)
        }
    );
    assert_eq!(
        lifecycle.verdict(start + IDLE),
        LifecycleVerdict::Exit(ExitCause::IdleWithoutSession)
    );
}

#[test]
fn opened_session_cancels_idle_exit_and_needs_no_wakeup() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    lifecycle.on_session_opened(&key("a"));
    assert_eq!(
        lifecycle.verdict(start + Duration::from_secs(3600)),
        LifecycleVerdict::KeepRunning { wake_at: None }
    );
}

#[test]
fn end_of_the_active_session_exits_with_its_reason() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    lifecycle.on_session_opened(&key("a"));
    let reason = SessionEndReason::ClosedByPage { code: Some(1000) };
    lifecycle.on_lifecycle_event(&ended("a", reason));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
}

#[test]
fn connection_loss_and_attach_do_not_exit() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    lifecycle.on_session_opened(&key("a"));
    lifecycle.on_lifecycle_event(&SessionLifecycleEvent::Attached { session: key("a") });
    lifecycle.on_lifecycle_event(&SessionLifecycleEvent::ConnectionLost { session: key("a") });
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::KeepRunning { wake_at: None }
    );
}

#[test]
fn replaced_session_end_never_exits_even_if_it_arrives_before_the_new_open() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    lifecycle.on_session_opened(&key("a"));
    // Порядок «гонки»: Ended старого пришёл раньше on_session_opened нового.
    lifecycle.on_lifecycle_event(&ended(
        "a",
        SessionEndReason::ClosedByPlayer(SessionCloseReason::ReplacedByAnotherTab),
    ));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::KeepRunning { wake_at: None }
    );
    lifecycle.on_session_opened(&key("b"));
    // А потом конец нового — уже выход.
    lifecycle.on_lifecycle_event(&ended("b", SessionEndReason::ReconnectGraceExpired));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(
            SessionEndReason::ReconnectGraceExpired
        ))
    );
}

#[test]
fn end_of_a_non_active_key_is_ignored() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    lifecycle.on_session_opened(&key("b"));
    lifecycle.on_lifecycle_event(&ended("a", SessionEndReason::ReconnectGraceExpired));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::KeepRunning { wake_at: None }
    );
}

#[test]
fn exit_decision_is_final_even_if_a_new_session_is_opened_afterwards() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    lifecycle.on_session_opened(&key("a"));
    let reason = SessionEndReason::FirstAttachDeadlineExpired;
    lifecycle.on_lifecycle_event(&ended("a", reason));
    lifecycle.on_session_opened(&key("c"));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
}

#[test]
fn late_end_of_an_old_instance_with_the_same_key_does_not_exit_the_new_instance() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    // Грейс сеанса K (экземпляр 1) истёк, экстеншен открыл K заново (экземпляр 2).
    lifecycle.on_session_opened(&instance_of("K", 2));
    lifecycle.on_lifecycle_event(&SessionLifecycleEvent::Ended {
        session: instance_of("K", 1),
        reason: SessionEndReason::ReconnectGraceExpired,
    });
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::KeepRunning { wake_at: None }
    );
    // А конец именно экземпляра 2 — выход.
    let reason = SessionEndReason::ClosedByPage { code: None };
    lifecycle.on_lifecycle_event(&SessionLifecycleEvent::Ended {
        session: instance_of("K", 2),
        reason,
    });
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
}

#[test]
fn end_of_a_new_instance_arriving_before_its_open_still_exits_when_the_open_is_seen() {
    let start = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(start, IDLE);
    lifecycle.on_session_opened(&instance_of("K", 1));
    // Экземпляр 2 успел открыться и закончиться, а вызов входа ещё не обработан.
    let reason = SessionEndReason::ReconnectGraceExpired;
    lifecycle.on_lifecycle_event(&ended_instance("K", 2, reason));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::KeepRunning { wake_at: None }
    );
    lifecycle.on_session_opened(&instance_of("K", 2));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
}

fn ended_instance(text: &str, instance: u64, reason: SessionEndReason) -> SessionLifecycleEvent {
    SessionLifecycleEvent::Ended {
        session: instance_of(text, instance),
        reason,
    }
}
