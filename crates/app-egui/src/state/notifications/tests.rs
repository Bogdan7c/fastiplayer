//! Тесты владельца уведомлений: жизненный цикл по фальшивому времени и сквозные сценарии
//! с настоящим `PlayerSession` (его собственные recoverable/fatal ошибки).

use std::time::{Duration, Instant};

use media_core::MediaTime;
use player_core::{
    PlaybackState, PlayerCommand, PlayerError, PlayerErrorKind, PlayerSession, PlayerSnapshot,
    SeekRequest,
};

use super::*;

/// Проекция без идущего открытия; reduced motion не влияет на состав уведомлений.
fn idle_frame(center: &mut NotificationCenter, now: Instant) -> NotificationsFrame {
    center.frame(OpenProgress::Idle, UiMotion::Standard, now)
}

/// Тексты видимых toast-ов, самый новый первым.
fn toast_messages(frame: &NotificationsFrame) -> Vec<String> {
    frame
        .toasts
        .iter()
        .map(|toast| toast.message.to_string())
        .collect()
}

/// Текст фатальной ошибки в центре, если она там.
fn center_failure_message(frame: &NotificationsFrame) -> Option<String> {
    match &frame.center {
        Some(CenterNotice::MediaFailure(failure)) => Some(failure.message.to_string()),
        _ => None,
    }
}

/// Id фатальной ошибки в центре (для кнопки ×).
fn center_failure_id(frame: &NotificationsFrame) -> Option<NotificationId> {
    match &frame.center {
        Some(CenterNotice::MediaFailure(failure)) => Some(failure.id),
        _ => None,
    }
}

#[test]
fn transient_toast_disappears_exactly_after_its_lifetime() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_transient("Перемотка недоступна", started_at);

    // Будильник указывает ровно на момент исчезновения — на паузе окно проснётся вовремя.
    assert_eq!(
        center.next_wake_deadline(),
        Some(started_at + TRANSIENT_NOTIFICATION_LIFETIME)
    );
    let just_before = started_at + TRANSIENT_NOTIFICATION_LIFETIME - Duration::from_millis(1);
    assert_eq!(
        toast_messages(&idle_frame(&mut center, just_before)),
        ["Перемотка недоступна"]
    );

    let expired = idle_frame(&mut center, started_at + TRANSIENT_NOTIFICATION_LIFETIME);
    assert!(expired.toasts.is_empty());
    assert_eq!(expired.center, None);
    assert_eq!(center.next_wake_deadline(), None);
}

#[test]
fn info_toast_lives_longer_than_transient() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_info("Позиция недоступна", started_at);

    let after_transient_lifetime = started_at + TRANSIENT_NOTIFICATION_LIFETIME;
    assert_eq!(
        toast_messages(&idle_frame(&mut center, after_transient_lifetime)),
        ["Позиция недоступна"]
    );
    let frame = idle_frame(&mut center, started_at + INFO_NOTIFICATION_LIFETIME);
    assert!(frame.toasts.is_empty());
}

#[test]
fn media_failure_never_expires_but_closes_by_dismiss() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    let failure_id = center.show_media_failure(
        "Не удалось открыть «clip.mkv»",
        MediaFailureOrigin::MediaOpen,
    );

    // Через час ошибка на месте: таймера у фатальной ошибки нет, будильник не нужен.
    let much_later = started_at + Duration::from_secs(3600);
    let frame = idle_frame(&mut center, much_later);
    assert_eq!(
        center_failure_message(&frame).as_deref(),
        Some("Не удалось открыть «clip.mkv»")
    );
    assert_eq!(center_failure_id(&frame), Some(failure_id));
    assert_eq!(center.next_wake_deadline(), None);

    assert_eq!(
        center.dismiss(failure_id),
        NotificationDismissOutcome::Dismissed
    );
    assert_eq!(idle_frame(&mut center, much_later).center, None);
    // Повторный × по уже закрытой ошибке — штатный no-op.
    assert_eq!(
        center.dismiss(failure_id),
        NotificationDismissOutcome::AlreadyGone
    );
}

#[test]
fn successful_or_new_open_resolves_media_failure() {
    let now = Instant::now();
    let mut center =
        NotificationCenter::with_startup_messages(Some("Файл не найден".to_string()), None, now);
    assert_eq!(
        center_failure_message(&idle_frame(&mut center, now)).as_deref(),
        Some("Файл не найден")
    );

    center.resolve_media_failure();
    assert_eq!(idle_frame(&mut center, now).center, None);
}

#[test]
fn open_progress_is_not_hidden_by_old_failure() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.show_media_failure("Старая ошибка", MediaFailureOrigin::MediaOpen);

    let frame = center.frame(
        OpenProgress::InProgress("Открываем «next.mkv»…"),
        UiMotion::Standard,
        now,
    );
    assert_eq!(
        frame.center,
        Some(CenterNotice::Progress(Arc::from("Открываем «next.mkv»…")))
    );
    // Прогресс закончился (например, открытие отменили) — старая ошибка снова видна.
    assert_eq!(
        center_failure_message(&idle_frame(&mut center, now)).as_deref(),
        Some("Старая ошибка")
    );
}

#[test]
fn repeated_message_extends_single_toast_instead_of_duplicating() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    let first_id = center.notify_transient("Перемотка недоступна", started_at);
    let repeated_at = started_at + Duration::from_secs(3);
    let repeated_id = center.notify_transient("Перемотка недоступна", repeated_at);

    assert_eq!(first_id, repeated_id);
    let frame = idle_frame(&mut center, repeated_at);
    assert_eq!(toast_messages(&frame), ["Перемотка недоступна"]);
    // Возраст считается от первого появления: плашка не мигает повторным проявлением.
    assert_eq!(frame.toasts[0].age, Duration::from_secs(3));

    // Срок продлён от повтора: жива после исходных 5 с, исчезает через 5 с после повтора.
    let after_original_expiry = started_at + TRANSIENT_NOTIFICATION_LIFETIME;
    assert_eq!(
        idle_frame(&mut center, after_original_expiry).toasts.len(),
        1
    );
    let after_repeat_expiry = repeated_at + TRANSIENT_NOTIFICATION_LIFETIME;
    assert!(
        idle_frame(&mut center, after_repeat_expiry)
            .toasts
            .is_empty()
    );
}

#[test]
fn same_text_of_different_kind_is_separate_toast() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_transient("Текст", now);
    center.notify_info("Текст", now);

    let kinds: Vec<ToastKind> = idle_frame(&mut center, now)
        .toasts
        .iter()
        .map(|toast| toast.kind)
        .collect();
    assert_eq!(kinds, [ToastKind::Info, ToastKind::Transient]);
}

#[test]
fn toast_stack_keeps_three_newest_first() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    for message in ["первое", "второе", "третье", "четвёртое"] {
        center.notify_transient(message, now);
    }

    assert_eq!(MAX_VISIBLE_TOASTS, 3);
    assert_eq!(
        toast_messages(&idle_frame(&mut center, now)),
        ["четвёртое", "третье", "второе"]
    );
}

#[test]
fn dismissing_one_toast_keeps_others_and_failure() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.show_media_failure("Фатальная", MediaFailureOrigin::MediaOpen);
    let first_id = center.notify_transient("первое", now);
    center.notify_transient("второе", now);

    assert_eq!(
        center.dismiss(first_id),
        NotificationDismissOutcome::Dismissed
    );
    let frame = idle_frame(&mut center, now);
    assert_eq!(toast_messages(&frame), ["второе"]);
    assert_eq!(center_failure_message(&frame).as_deref(), Some("Фатальная"));
}

/// Сквозной сценарий: настоящий player отклоняет seek без seekable timeline.
///
/// Раньше `last_error` с этим текстом висел красным в центре до следующего файла.
#[test]
fn real_player_seek_rejection_becomes_transient_toast_and_overlay_clears() {
    let mut session = PlayerSession::new();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .expect("seek command must be accepted by state machine");
    let snapshot_before = format!("{:?}", session.snapshot());
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(&PlayerErrorKind::SeekUnavailable),
        "player обязан сам отклонить seek, иначе сценарий ничего не проверяет"
    );
    assert_ne!(session.snapshot().playback_state, PlaybackState::Failed);

    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    for event in session.take_events() {
        center.record_player_event(&event, started_at);
    }
    center.observe_player_snapshot(session.snapshot());

    let frame = idle_frame(&mut center, started_at);
    assert_eq!(frame.toasts.len(), 1);
    assert_eq!(frame.toasts[0].kind, ToastKind::Transient);
    assert!(
        frame.toasts[0].message.contains("Seek невозможен"),
        "{:?}",
        frame.toasts[0].message
    );
    // Старый `last_error` без `Failed` больше не становится вечной ошибкой в центре.
    assert_eq!(frame.center, None);

    let expired = idle_frame(&mut center, started_at + TRANSIENT_NOTIFICATION_LIFETIME);
    assert!(expired.toasts.is_empty());
    assert_eq!(expired.center, None);
    // Владелец уведомлений только читал player: snapshot не изменился.
    assert_eq!(format!("{:?}", session.snapshot()), snapshot_before);
}

/// Сквозной сценарий: настоящий player переходит в `Failed`.
#[test]
fn real_player_fatal_failure_stays_until_dismissed_and_is_not_resurrected() {
    let mut session = PlayerSession::new();
    session.mark_fatal_error(PlayerError::new(
        PlayerErrorKind::DemuxError,
        "поток повреждён",
    ));
    assert_eq!(session.snapshot().playback_state, PlaybackState::Failed);
    let snapshot_before = format!("{:?}", session.snapshot());

    let now = Instant::now();
    let mut center = NotificationCenter::default();
    for event in session.take_events() {
        center.record_player_event(&event, now);
    }
    center.observe_player_snapshot(session.snapshot());

    let frame = idle_frame(&mut center, now + Duration::from_secs(3600));
    let message = center_failure_message(&frame).expect("Failed должен показать ошибку в центре");
    assert!(message.contains("поток повреждён"), "{message}");
    // Фатальная ошибка не дублируется временным toast-ом.
    assert!(frame.toasts.is_empty());

    let failure_id = center_failure_id(&frame).expect("ошибка в центре имеет id");
    assert_eq!(
        center.dismiss(failure_id),
        NotificationDismissOutcome::Dismissed
    );
    // Тот же `Failed` на следующих кадрах не возвращает закрытую ошибку.
    center.observe_player_snapshot(session.snapshot());
    center.observe_player_snapshot(session.snapshot());
    assert_eq!(idle_frame(&mut center, now).center, None);
    assert_eq!(format!("{:?}", session.snapshot()), snapshot_before);
}

/// Сессия 16: player остановился, потому что сеть не вернулась за бюджет ожидания.
/// В центре — человеческая причина, без технической цепочки HTTP-ошибок.
#[test]
fn network_failure_during_playback_shows_human_reason() {
    let mut session = PlayerSession::new();
    session.mark_fatal_error(PlayerError::new(
        PlayerErrorKind::NetworkError,
        "Ошибка чтения packet: HTTP request `range-read` не удался: Connection refused",
    ));
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.observe_player_snapshot(session.snapshot());

    let message = center_failure_message(&idle_frame(&mut center, now))
        .expect("Failed по сети показывает ошибку в центре");

    assert_eq!(message, player_feed::NETWORK_LOST_DURING_PLAYBACK_MESSAGE);
    assert!(!message.contains("range-read"), "{message}");
}

#[test]
fn player_recovery_clears_player_failure_but_keeps_open_failure() {
    let now = Instant::now();
    let failed = PlayerSnapshot {
        playback_state: PlaybackState::Failed,
        last_error: Some(PlayerError::new(PlayerErrorKind::DemuxError, "сбой")),
        ..Default::default()
    };
    let playing = PlayerSnapshot {
        playback_state: PlaybackState::Playing,
        ..Default::default()
    };

    let mut center = NotificationCenter::default();
    center.observe_player_snapshot(&failed);
    assert!(center_failure_message(&idle_frame(&mut center, now)).is_some());
    // Player снова играет (новый файл установился) — его ошибка больше не актуальна.
    center.observe_player_snapshot(&playing);
    assert_eq!(idle_frame(&mut center, now).center, None);

    // Не удалось открыть новый файл, пока играет старый: старый играет, ошибка остаётся.
    center.show_media_failure(
        "Не удалось открыть «broken.mkv»",
        MediaFailureOrigin::MediaOpen,
    );
    center.observe_player_snapshot(&playing);
    center.observe_player_snapshot(&playing);
    assert_eq!(
        center_failure_message(&idle_frame(&mut center, now)).as_deref(),
        Some("Не удалось открыть «broken.mkv»")
    );
}

#[test]
fn failed_without_error_text_shows_nothing_and_clears_stale_player_failure() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.observe_player_snapshot(&PlayerSnapshot {
        playback_state: PlaybackState::Failed,
        last_error: Some(PlayerError::new(PlayerErrorKind::DemuxError, "первая")),
        ..Default::default()
    });
    center.observe_player_snapshot(&PlayerSnapshot {
        playback_state: PlaybackState::Failed,
        last_error: None,
        ..Default::default()
    });

    assert_eq!(idle_frame(&mut center, now).center, None);
}

#[test]
fn reduced_motion_is_passed_to_frame_projection() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    let frame = center.frame(OpenProgress::Idle, UiMotion::Reduced, now);
    assert_eq!(frame.motion, UiMotion::Reduced);
}

/// Предупреждение о config-е (сессия 05): не истекает и не будит окно, уходит только по ×.
#[test]
fn startup_config_warning_stays_until_dismissed() {
    let now = Instant::now();
    let mut center = NotificationCenter::with_startup_messages(
        None,
        Some("Файл настроек был повреждён".to_string()),
        now,
    );

    let first_frame = idle_frame(&mut center, now);
    assert_eq!(first_frame.center, None);
    assert_eq!(
        toast_messages(&first_frame),
        vec!["Файл настроек был повреждён"]
    );
    assert_eq!(first_frame.toasts[0].kind, ToastKind::Warning);
    // Без таймера окну незачем просыпаться ради этой плашки.
    assert_eq!(center.next_wake_deadline(), None);

    // Час спустя предупреждение всё ещё на месте.
    let much_later = now + Duration::from_secs(3_600);
    let later_frame = idle_frame(&mut center, much_later);
    assert_eq!(
        toast_messages(&later_frame),
        vec!["Файл настроек был повреждён"]
    );

    let warning_id = later_frame.toasts[0].id;
    assert_eq!(
        center.dismiss(warning_id),
        NotificationDismissOutcome::Dismissed
    );
    assert!(idle_frame(&mut center, much_later).toasts.is_empty());
}

/// Ошибка CLI и предупреждение config-а при старте живут в своих местах одновременно.
#[test]
fn startup_messages_keep_failure_in_center_and_warning_in_corner() {
    let now = Instant::now();
    let mut center = NotificationCenter::with_startup_messages(
        Some("Файл не найден".to_string()),
        Some("Файл настроек был повреждён".to_string()),
        now,
    );

    let frame = idle_frame(&mut center, now);

    assert_eq!(
        center_failure_message(&frame).as_deref(),
        Some("Файл не найден")
    );
    assert_eq!(toast_messages(&frame), vec!["Файл настроек был повреждён"]);
}

/// Поток временных toast-ов не вытесняет предупреждение до ×: уходят временные.
#[test]
fn transient_burst_does_not_evict_warning_until_dismissed() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_until_dismissed("Файл настроек был повреждён", now);
    for index in 0..5 {
        center.notify_transient(format!("Временное {index}"), now);
    }

    let frame = idle_frame(&mut center, now);

    assert_eq!(frame.toasts.len(), MAX_VISIBLE_TOASTS);
    assert_eq!(
        toast_messages(&frame),
        vec!["Временное 4", "Временное 3", "Файл настроек был повреждён"]
    );
    // Временные истекают, предупреждение остаётся.
    let after_expiry = now + TRANSIENT_NOTIFICATION_LIFETIME + Duration::from_millis(1);
    assert_eq!(
        toast_messages(&idle_frame(&mut center, after_expiry)),
        vec!["Файл настроек был повреждён"]
    );
}

/// Если на экране одни предупреждения до ×, вытесняется самое старое из них.
#[test]
fn oldest_warning_is_evicted_when_only_warnings_overflow() {
    let now = Instant::now();
    let mut center = NotificationCenter::default();
    for index in 0..=MAX_VISIBLE_TOASTS {
        center.notify_until_dismissed(format!("Предупреждение {index}"), now);
    }

    let frame = idle_frame(&mut center, now);

    assert_eq!(
        toast_messages(&frame),
        vec!["Предупреждение 3", "Предупреждение 2", "Предупреждение 1"]
    );
}
