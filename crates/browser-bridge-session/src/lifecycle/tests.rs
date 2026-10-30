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
    let reason = SessionEndReason::ClosedByPage { code: None };
    lifecycle.on_lifecycle_event(&ended("b", reason));
    assert_eq!(
        lifecycle.verdict(start),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
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
    let reason = SessionEndReason::FirstAttachDeadlineExpired;
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

const TAB_GONE: SessionEndReason = SessionEndReason::ClosedByPlayer(SessionCloseReason::TabGone);
const KEEP: LifecycleVerdict = LifecycleVerdict::KeepRunning { wake_at: None };

/// Машина с активным сеансом `a` (экземпляр 1), у которого случился конец `reason`.
fn lifecycle_after_end(reason: SessionEndReason) -> ProcessLifecycle {
    let mut lifecycle = ProcessLifecycle::new(Instant::now(), IDLE);
    lifecycle.on_session_opened(&key("a"));
    lifecycle.on_lifecycle_event(&ended("a", reason));
    lifecycle
}

#[test]
fn tab_gone_awaits_drain_then_exits_with_tab_gone_on_drained() {
    let now = Instant::now();
    let mut lifecycle = lifecycle_after_end(TAB_GONE);
    assert_eq!(lifecycle.verdict(now), LifecycleVerdict::AwaitPlaybackDrain);
    // Время ничего не меняет: сроки доигрывания знает не машина.
    assert_eq!(
        lifecycle.verdict(now + Duration::from_secs(3600)),
        LifecycleVerdict::AwaitPlaybackDrain
    );
    assert_eq!(
        lifecycle.on_playback_drained(),
        PlaybackDrainOutcome::ExitDecided
    );
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(TAB_GONE))
    );
}

#[test]
fn reconnect_grace_expiry_is_also_played_out() {
    let now = Instant::now();
    let reason = SessionEndReason::ReconnectGraceExpired;
    let mut lifecycle = lifecycle_after_end(reason);
    assert_eq!(lifecycle.verdict(now), LifecycleVerdict::AwaitPlaybackDrain);
    assert_eq!(
        lifecycle.on_playback_drained(),
        PlaybackDrainOutcome::ExitDecided
    );
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
}

#[test]
fn other_ends_exit_immediately_without_draining() {
    let now = Instant::now();
    for reason in [
        SessionEndReason::ClosedByPlayer(SessionCloseReason::PlayerClosed),
        SessionEndReason::ClosedByPlayer(SessionCloseReason::ProtocolViolation),
        SessionEndReason::ClosedByPlayer(SessionCloseReason::PlayerShuttingDown),
        SessionEndReason::ClosedByPage { code: Some(1000) },
        SessionEndReason::FirstAttachDeadlineExpired,
    ] {
        let mut lifecycle = lifecycle_after_end(reason);
        assert_eq!(
            lifecycle.verdict(now),
            LifecycleVerdict::Exit(ExitCause::SessionEnded(reason)),
            "{reason:?}"
        );
    }
}

#[test]
fn drain_rule_is_defined_in_one_place() {
    assert!(ends_with_playback_drain(TAB_GONE));
    assert!(ends_with_playback_drain(
        SessionEndReason::ReconnectGraceExpired
    ));
    assert!(!ends_with_playback_drain(SessionEndReason::ClosedByPage {
        code: None
    }));
}

#[test]
fn drained_outside_of_draining_is_a_noop() {
    let now = Instant::now();
    let mut fresh = ProcessLifecycle::new(now, IDLE);
    assert_eq!(
        fresh.on_playback_drained(),
        PlaybackDrainOutcome::NotDraining
    );
    assert_eq!(
        fresh.verdict(now),
        LifecycleVerdict::KeepRunning {
            wake_at: Some(now + IDLE)
        }
    );
    let mut active = ProcessLifecycle::new(now, IDLE);
    active.on_session_opened(&key("a"));
    assert_eq!(
        active.on_playback_drained(),
        PlaybackDrainOutcome::NotDraining
    );
    assert_eq!(active.verdict(now), KEEP);
}

#[test]
fn drained_after_finished_changes_nothing() {
    let now = Instant::now();
    let reason = SessionEndReason::ClosedByPage { code: None };
    let mut lifecycle = lifecycle_after_end(reason);
    assert_eq!(
        lifecycle.on_playback_drained(),
        PlaybackDrainOutcome::NotDraining
    );
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
    // Повторный «доиграно» после решения «выход» из доигрывания — тоже no-op.
    let mut drained = lifecycle_after_end(TAB_GONE);
    assert_eq!(
        drained.on_playback_drained(),
        PlaybackDrainOutcome::ExitDecided
    );
    assert_eq!(
        drained.on_playback_drained(),
        PlaybackDrainOutcome::NotDraining
    );
    assert_eq!(
        drained.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(TAB_GONE))
    );
}

#[test]
fn new_session_during_drain_replaces_it_and_its_own_end_works() {
    let now = Instant::now();
    let mut lifecycle = lifecycle_after_end(TAB_GONE);
    lifecycle.on_session_opened(&instance_of("b", 2));
    assert_eq!(lifecycle.verdict(now), KEEP);
    // Старое доигрывание забыто: «доиграно» ничего не решает.
    assert_eq!(
        lifecycle.on_playback_drained(),
        PlaybackDrainOutcome::NotDraining
    );
    // Конец нового сеанса, не доигрываемый, — выход сразу.
    let reason = SessionEndReason::ClosedByPage { code: Some(1001) };
    lifecycle.on_lifecycle_event(&ended_instance("b", 2, reason));
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
}

#[test]
fn new_session_may_itself_end_with_a_drain() {
    let now = Instant::now();
    let mut lifecycle = lifecycle_after_end(SessionEndReason::ReconnectGraceExpired);
    lifecycle.on_session_opened(&instance_of("b", 2));
    lifecycle.on_lifecycle_event(&ended_instance("b", 2, TAB_GONE));
    assert_eq!(lifecycle.verdict(now), LifecycleVerdict::AwaitPlaybackDrain);
    lifecycle.on_playback_drained();
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(TAB_GONE))
    );
}

#[test]
fn old_or_same_instance_during_drain_is_ignored() {
    let now = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(now, IDLE);
    lifecycle.on_session_opened(&instance_of("a", 5));
    lifecycle.on_lifecycle_event(&ended_instance("a", 5, TAB_GONE));
    // Открытие того же и более старого экземпляра не возвращает в Active.
    lifecycle.on_session_opened(&instance_of("a", 5));
    lifecycle.on_session_opened(&instance_of("z", 3));
    assert_eq!(lifecycle.verdict(now), LifecycleVerdict::AwaitPlaybackDrain);
    // Конец более старого экземпляра и повторный конец того же не меняют причину.
    lifecycle.on_lifecycle_event(&ended_instance(
        "z",
        3,
        SessionEndReason::ClosedByPage { code: None },
    ));
    lifecycle.on_lifecycle_event(&ended_instance(
        "a",
        5,
        SessionEndReason::ReconnectGraceExpired,
    ));
    assert_eq!(lifecycle.verdict(now), LifecycleVerdict::AwaitPlaybackDrain);
    lifecycle.on_playback_drained();
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(TAB_GONE))
    );
}

#[test]
fn replaced_end_is_still_ignored_during_drain() {
    let now = Instant::now();
    let mut lifecycle = lifecycle_after_end(TAB_GONE);
    lifecycle.on_lifecycle_event(&ended_instance(
        "b",
        2,
        SessionEndReason::ClosedByPlayer(SessionCloseReason::ReplacedByAnotherTab),
    ));
    lifecycle.on_session_opened(&instance_of("b", 2));
    assert_eq!(lifecycle.verdict(now), KEEP);
}

#[test]
fn early_drainable_end_of_a_new_instance_leads_to_draining_not_exit() {
    let now = Instant::now();
    let mut lifecycle = ProcessLifecycle::new(now, IDLE);
    lifecycle.on_session_opened(&instance_of("K", 1));
    // Новый экземпляр закончился (tab_gone) раньше, чем приложение узнало об открытии.
    lifecycle.on_lifecycle_event(&ended_instance("K", 2, TAB_GONE));
    assert_eq!(lifecycle.verdict(now), KEEP);
    lifecycle.on_session_opened(&instance_of("K", 2));
    assert_eq!(lifecycle.verdict(now), LifecycleVerdict::AwaitPlaybackDrain);
    lifecycle.on_playback_drained();
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(TAB_GONE))
    );
}

#[test]
fn early_end_newer_than_the_draining_one_is_remembered() {
    let now = Instant::now();
    let mut lifecycle = lifecycle_after_end(TAB_GONE);
    let reason = SessionEndReason::ClosedByPage { code: None };
    lifecycle.on_lifecycle_event(&ended_instance("b", 2, reason));
    assert_eq!(lifecycle.verdict(now), LifecycleVerdict::AwaitPlaybackDrain);
    lifecycle.on_session_opened(&instance_of("b", 2));
    assert_eq!(
        lifecycle.verdict(now),
        LifecycleVerdict::Exit(ExitCause::SessionEnded(reason))
    );
}
