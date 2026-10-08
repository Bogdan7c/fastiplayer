//! Тесты спиннера ожидания данных на фальшивом времени: задержка появления, исчезновение
//! в том же кадре, приоритет сообщений центра и будильник окна.

use std::time::{Duration, Instant};

use player_core::{PlaybackState, PlayerError, PlayerErrorKind, PlayerSnapshot};

use super::super::{
    BUFFERING_INDICATOR_APPEAR_DELAY, BufferingIndicator, CenterNotice, MediaFailureOrigin,
    NotificationCenter, NotificationsFrame, OpenProgress,
};
use crate::ui::animation::UiMotion;

const ALL_PLAYBACK_STATES: [PlaybackState; 11] = [
    PlaybackState::Idle,
    PlaybackState::Opening,
    PlaybackState::Paused,
    PlaybackState::Playing,
    PlaybackState::Buffering,
    PlaybackState::Seeking,
    PlaybackState::Scrubbing,
    PlaybackState::Draining,
    PlaybackState::Ended,
    PlaybackState::Stopped,
    PlaybackState::Failed,
];

/// Один кадр приложения: наблюдение состояния player-а и проекция без идущего открытия
/// (тот же порядок, что в `AppState::notifications_frame`).
fn frame_at(
    center: &mut NotificationCenter,
    playback_state: PlaybackState,
    now: Instant,
) -> NotificationsFrame {
    center.observe_playback_waiting(playback_state, now);
    center.frame(OpenProgress::Idle, UiMotion::Standard, now)
}

#[test]
fn waiting_longer_than_delay_shows_spinner_and_shorter_wait_never_does() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();

    // Короткая буферизация (старт локального файла): кадры каждые 100 мс, спиннера нет.
    for step_ms in [0, 100, 200, 300, 400] {
        let frame = frame_at(
            &mut center,
            PlaybackState::Buffering,
            started_at + Duration::from_millis(step_ms),
        );
        assert_eq!(frame.buffering, BufferingIndicator::Hidden, "{step_ms} мс");
        assert!(!frame.shows_buffering_indicator());
    }
    let resumed = frame_at(
        &mut center,
        PlaybackState::Playing,
        started_at + Duration::from_millis(450),
    );
    assert_eq!(resumed.buffering, BufferingIndicator::Hidden);
    // После Playing время идёт дальше, но эпизод закончился — спиннер так и не появился.
    let later = frame_at(
        &mut center,
        PlaybackState::Playing,
        started_at + Duration::from_secs(5),
    );
    assert_eq!(later.buffering, BufferingIndicator::Hidden);

    // Долгая буферизация: ровно на границе задержки спиннер появляется.
    let stalled_at = started_at + Duration::from_secs(10);
    let just_before = frame_at(
        &mut center,
        PlaybackState::Buffering,
        stalled_at + BUFFERING_INDICATOR_APPEAR_DELAY - Duration::from_millis(1),
    );
    assert_eq!(just_before.buffering, BufferingIndicator::Hidden);
    // Первое наблюдение было на `just_before`, поэтому отсчёт идёт от него.
    let first_seen = stalled_at + BUFFERING_INDICATOR_APPEAR_DELAY - Duration::from_millis(1);
    let visible = frame_at(
        &mut center,
        PlaybackState::Buffering,
        first_seen + BUFFERING_INDICATOR_APPEAR_DELAY,
    );
    assert_eq!(visible.buffering, BufferingIndicator::Visible);
    assert!(visible.shows_buffering_indicator());
}

#[test]
fn leaving_waiting_hides_spinner_in_the_same_frame() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    frame_at(&mut center, PlaybackState::Buffering, started_at);
    let shown_at = started_at + Duration::from_secs(2);
    assert!(frame_at(&mut center, PlaybackState::Buffering, shown_at).shows_buffering_indicator());

    for next_state in [
        PlaybackState::Playing,
        PlaybackState::Paused,
        PlaybackState::Failed,
        PlaybackState::Idle,
    ] {
        let mut center = NotificationCenter::default();
        frame_at(&mut center, PlaybackState::Seeking, started_at);
        assert!(
            frame_at(&mut center, PlaybackState::Seeking, shown_at).shows_buffering_indicator()
        );
        // Тот же момент времени: никакой задержки исчезновения.
        let hidden = frame_at(&mut center, next_state, shown_at);
        assert_eq!(
            hidden.buffering,
            BufferingIndicator::Hidden,
            "{next_state:?}"
        );
    }
}

#[test]
fn seeking_then_buffering_is_one_wait_and_a_new_wait_restarts_the_delay() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    frame_at(&mut center, PlaybackState::Seeking, started_at);
    // Seek закончился, player добирает preroll: ожидание то же, таймер не сброшен.
    let continued = frame_at(
        &mut center,
        PlaybackState::Buffering,
        started_at + Duration::from_millis(300),
    );
    assert_eq!(continued.buffering, BufferingIndicator::Hidden);
    let shown = frame_at(
        &mut center,
        PlaybackState::Buffering,
        started_at + BUFFERING_INDICATOR_APPEAR_DELAY,
    );
    assert_eq!(shown.buffering, BufferingIndicator::Visible);

    // Короткое Playing между ожиданиями начинает новый эпизод со своей задержкой.
    let playing_at = started_at + Duration::from_secs(1);
    frame_at(&mut center, PlaybackState::Playing, playing_at);
    let new_wait = frame_at(
        &mut center,
        PlaybackState::Buffering,
        playing_at + Duration::from_millis(10),
    );
    assert_eq!(new_wait.buffering, BufferingIndicator::Hidden);
}

#[test]
fn only_buffering_and_seeking_count_as_waiting_for_data() {
    let started_at = Instant::now();
    let long_after = started_at + Duration::from_secs(30);
    for state in ALL_PLAYBACK_STATES {
        let mut center = NotificationCenter::default();
        frame_at(&mut center, state, started_at);
        let frame = frame_at(&mut center, state, long_after);
        let expected = if matches!(state, PlaybackState::Buffering | PlaybackState::Seeking) {
            BufferingIndicator::Visible
        } else {
            BufferingIndicator::Hidden
        };
        assert_eq!(frame.buffering, expected, "{state:?}");
    }
}

#[test]
fn center_error_or_open_progress_wins_over_spinner() {
    let started_at = Instant::now();
    let long_after = started_at + Duration::from_secs(3);

    // Новый файл не открылся, а старый ещё буферизуется: в центре ошибка, не спиннер.
    let mut center = NotificationCenter::default();
    center.show_media_failure(
        "Не удалось открыть «clip.mkv»",
        MediaFailureOrigin::MediaOpen,
    );
    frame_at(&mut center, PlaybackState::Buffering, started_at);
    let failed = frame_at(&mut center, PlaybackState::Buffering, long_after);
    assert!(matches!(failed.center, Some(CenterNotice::MediaFailure(_))));
    assert_eq!(failed.buffering, BufferingIndicator::Hidden);
    assert!(!failed.shows_buffering_indicator());

    // Идёт новое открытие: прогресс в центре, спиннер не рисуется.
    let mut center = NotificationCenter::default();
    center.observe_playback_waiting(PlaybackState::Seeking, started_at);
    center.observe_playback_waiting(PlaybackState::Seeking, long_after);
    let opening = center.frame(
        OpenProgress::InProgress("Открываем…"),
        UiMotion::Standard,
        long_after,
    );
    assert!(matches!(opening.center, Some(CenterNotice::Progress(_))));
    assert_eq!(opening.buffering, BufferingIndicator::Hidden);

    // Player умер: snapshot `Failed` даёт ошибку в центре и не является ожиданием.
    let mut center = NotificationCenter::default();
    frame_at(&mut center, PlaybackState::Buffering, started_at);
    let snapshot = PlayerSnapshot {
        playback_state: PlaybackState::Failed,
        last_error: Some(PlayerError::new(
            PlayerErrorKind::DemuxError,
            "connection reset",
        )),
        ..PlayerSnapshot::empty()
    };
    center.observe_player_snapshot(&snapshot);
    let dead = frame_at(&mut center, snapshot.playback_state, long_after);
    assert!(matches!(dead.center, Some(CenterNotice::MediaFailure(_))));
    assert_eq!(dead.buffering, BufferingIndicator::Hidden);
}

#[test]
fn wake_deadline_brings_spinner_on_time_and_stops_after_it_appeared() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    assert_eq!(center.next_wake_deadline(), None);

    frame_at(&mut center, PlaybackState::Seeking, started_at);
    let appear_at = started_at + BUFFERING_INDICATOR_APPEAR_DELAY;
    assert_eq!(center.next_wake_deadline(), Some(appear_at));

    // Toast, который истекает позже, не откладывает появление спиннера.
    center.notify_transient("Перемотка недоступна", started_at);
    assert_eq!(center.next_wake_deadline(), Some(appear_at));

    // Спиннер показан: дальше будильник ведёт только toast, прошедший срок не возвращается.
    assert!(frame_at(&mut center, PlaybackState::Seeking, appear_at).shows_buffering_indicator());
    assert_eq!(
        center.next_wake_deadline(),
        Some(started_at + super::super::TRANSIENT_NOTIFICATION_LIFETIME)
    );

    // Ожидание кончилось до срока: будильник снят.
    let mut center = NotificationCenter::default();
    frame_at(&mut center, PlaybackState::Buffering, started_at);
    frame_at(
        &mut center,
        PlaybackState::Playing,
        started_at + Duration::from_millis(100),
    );
    assert_eq!(center.next_wake_deadline(), None);
}

#[test]
fn deadline_passed_under_center_message_does_not_keep_waking_the_window() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    center.show_media_failure(
        "Не удалось открыть «clip.mkv»",
        MediaFailureOrigin::MediaOpen,
    );
    frame_at(&mut center, PlaybackState::Buffering, started_at);
    let hidden = frame_at(
        &mut center,
        PlaybackState::Buffering,
        started_at + Duration::from_secs(1),
    );
    assert_eq!(hidden.buffering, BufferingIndicator::Hidden);
    assert_eq!(center.next_wake_deadline(), None);

    // Ошибку закрыли ×: спиннер появляется сразу, без новой задержки.
    let failure_id = match hidden.center {
        Some(CenterNotice::MediaFailure(failure)) => failure.id,
        other => panic!("в центре должна быть ошибка: {other:?}"),
    };
    center.dismiss(failure_id);
    let after_dismiss = frame_at(
        &mut center,
        PlaybackState::Buffering,
        started_at + Duration::from_millis(1100),
    );
    assert_eq!(after_dismiss.buffering, BufferingIndicator::Visible);
}

#[test]
fn waiting_observation_does_not_touch_messages_it_does_not_own() {
    let started_at = Instant::now();
    let mut center = NotificationCenter::default();
    center.notify_info("Звук переключён на системное устройство", started_at);
    center.show_media_failure(
        "Не удалось открыть «clip.mkv»",
        MediaFailureOrigin::MediaOpen,
    );
    let before = center.frame(OpenProgress::Idle, UiMotion::Standard, started_at);

    for (offset_ms, state) in [
        (0, PlaybackState::Buffering),
        (700, PlaybackState::Seeking),
        (900, PlaybackState::Playing),
    ] {
        center.observe_playback_waiting(state, started_at + Duration::from_millis(offset_ms));
    }
    let after = center.frame(OpenProgress::Idle, UiMotion::Standard, started_at);
    assert_eq!(after.center, before.center);
    assert_eq!(after.toasts, before.toasts);
}
