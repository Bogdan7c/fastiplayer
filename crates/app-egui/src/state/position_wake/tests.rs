use std::time::{Duration, Instant};

use media_core::{MediaDuration, MediaTime, TimelineMode, TimelinePreviewState, TimelineSnapshot};
use player_core::{PlaybackRate, PlaybackState, PlayerSnapshot};

use super::*;
use crate::state::ui_frame_model::quantize_timeline_position;

/// Ширина дорожки ползунка в тестах: 128 с на 1024 px → пиксель = 125 мс (точно
/// представимо в `f64`, поэтому дедлайны сравниваются на равенство без допусков).
const TRACK_WIDTH_PX: Option<u32> = Some(1024);
/// Длительность кадра в snapshot-е (30 fps), от неё считается backoff.
const FRAME_DURATION: Duration = Duration::from_millis(33);

/// Играющий snapshot 128-секундного файла в позиции `position_ms`.
fn playing_snapshot(position_ms: u64) -> PlayerSnapshot {
    let mut snapshot = PlayerSnapshot::empty();
    snapshot.playback_state = PlaybackState::Playing;
    snapshot.playback_rate = PlaybackRate::new(1.0).expect("valid rate");
    snapshot.current_position = Duration::from_millis(position_ms);
    snapshot.video_frame_duration_estimate = FRAME_DURATION;
    snapshot.timeline = TimelineSnapshot {
        mode: TimelineMode::default(),
        current_position: MediaTime::from_millis(position_ms),
        target_position: None,
        duration: Some(MediaDuration::from_duration(Duration::from_secs(128))),
        live_edge: None,
        live_epoch: None,
        live_revision: None,
        seekable_range: None,
        seekable: true,
        not_seekable_reason: None,
        seeking: false,
        scrubbing: false,
        stale_frame: false,
        preview_state: TimelinePreviewState::Inactive,
    };
    snapshot
}

/// Позиция snapshot-а так, как её видно на показанной нижней панели.
fn shown(snapshot: &PlayerSnapshot) -> VisibleTimelinePosition {
    VisibleTimelinePosition::Shown(quantize_timeline_position(
        &snapshot.timeline,
        TRACK_WIDTH_PX,
    ))
}

fn facts(
    snapshot: &PlayerSnapshot,
    position: VisibleTimelinePosition,
    now: Instant,
) -> PositionWakeFacts<'_> {
    PositionWakeFacts {
        snapshot,
        position,
        track_width_px: TRACK_WIDTH_PX,
        now,
    }
}

/// Видимая панель: дедлайн ровно тот же, что в сессии 04, — ближайшая граница пикселя.
#[test]
fn visible_position_wakes_at_the_next_pixel_boundary_as_before() {
    let now = Instant::now();
    let snapshot = playing_snapshot(12_000);
    let mut tracker = PositionWakeTracker::default();

    let deadline = tracker.next_deadline(facts(&snapshot, shown(&snapshot), now));

    // 12.000 с → следующий пиксель на 12.125 с: через 125 мс настенного времени.
    assert_eq!(deadline, Some(now + Duration::from_millis(125)));
    assert_eq!(
        deadline,
        next_visible_position_change(&snapshot, TRACK_WIDTH_PX, now),
        "the tracker adds nothing to the pure boundary on a fresh visible position"
    );
}

/// Скрытая панель: позиция меняется, а дедлайна нет — UI не будится ради невидимого.
#[test]
fn hidden_position_never_schedules_a_wake_while_playback_runs() {
    let now = Instant::now();
    let mut tracker = PositionWakeTracker::default();

    for position_ms in [12_000, 12_125, 13_000, 40_000] {
        let snapshot = playing_snapshot(position_ms);
        assert_eq!(
            tracker.next_deadline(facts(&snapshot, VisibleTimelinePosition::OffScreen, now)),
            None,
            "off-screen position at {position_ms} ms must not wake the UI"
        );
    }
}

/// Показ панели после скрытия: первый дедлайн — свежая граница, а не backoff по памяти,
/// записанной до скрытия (иначе время на только что показанной панели отставало бы).
#[test]
fn showing_the_panel_again_starts_from_a_fresh_boundary() {
    let start = Instant::now();
    let snapshot = playing_snapshot(12_000);
    let mut tracker = PositionWakeTracker::default();
    let first = tracker
        .next_deadline(facts(&snapshot, shown(&snapshot), start))
        .expect("visible playing position schedules a wake");

    // Дедлайн прошёл, позиция в snapshot-е та же: без скрытия это был бы backoff.
    let after_deadline = first + Duration::from_millis(1);
    assert_eq!(
        tracker.next_deadline(facts(
            &snapshot,
            VisibleTimelinePosition::OffScreen,
            after_deadline
        )),
        None
    );
    let reshown = tracker.next_deadline(facts(&snapshot, shown(&snapshot), after_deadline));

    assert_eq!(
        reshown,
        next_visible_position_change(&snapshot, TRACK_WIDTH_PX, after_deadline),
        "hiding forgets the stale memory, so the first visible deadline is the pure boundary"
    );
    assert_ne!(reshown, Some(after_deadline + FRAME_DURATION));
}

/// Без скрытия память по-прежнему защищает от спина на не догнавшем границу snapshot-е.
#[test]
fn visible_position_still_backs_off_while_the_snapshot_lags() {
    let start = Instant::now();
    let snapshot = playing_snapshot(12_000);
    let mut tracker = PositionWakeTracker::default();
    let first = tracker
        .next_deadline(facts(&snapshot, shown(&snapshot), start))
        .expect("visible playing position schedules a wake");

    let after_deadline = first + Duration::from_millis(1);
    assert_eq!(
        tracker.next_deadline(facts(&snapshot, shown(&snapshot), after_deadline)),
        Some(after_deadline + FRAME_DURATION),
        "same position after the deadline waits for the worker tick"
    );
}

/// Дедлайн позиции: обычно — вычисленная граница; если прошлый дедлайн прошёл, а snapshot
/// не изменился, — ожидание тика worker-а, не меньше 8 мс (иначе спин по 1 мс).
#[test]
fn position_wake_backs_off_while_snapshot_lags_behind_the_boundary() {
    let now = Instant::now();
    let position = QuantizedTimelinePosition {
        whole_seconds: 12,
        slider_step: Some(120),
    };
    let other_position = QuantizedTimelinePosition {
        whole_seconds: 12,
        slider_step: Some(121),
    };
    let computed = now + Duration::from_millis(1);
    let frame_duration = Duration::from_millis(33);

    assert_eq!(
        position_wake_with_backoff(None, position, computed, frame_duration, now),
        computed,
        "first deadline is the computed boundary"
    );
    let pending = PositionWakeMemory {
        position,
        deadline: now + Duration::from_millis(20),
    };
    assert_eq!(
        position_wake_with_backoff(Some(pending), position, computed, frame_duration, now),
        computed,
        "a deadline still in the future keeps the computed boundary"
    );
    let expired = PositionWakeMemory {
        position,
        deadline: now - Duration::from_millis(1),
    };
    assert_eq!(
        position_wake_with_backoff(Some(expired), position, computed, frame_duration, now),
        now + frame_duration,
        "expired deadline with the same position waits for the worker tick"
    );
    assert_eq!(
        position_wake_with_backoff(
            Some(expired),
            position,
            computed,
            Duration::from_millis(2),
            now
        ),
        now + POSITION_STALE_BACKOFF,
        "backoff never drops below the floor"
    );
    assert_eq!(
        position_wake_with_backoff(Some(expired), other_position, computed, frame_duration, now),
        computed,
        "a changed position is a fresh boundary, not a stale one"
    );
}
