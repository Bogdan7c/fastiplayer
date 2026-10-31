//! Жест таймлайна целиком — от ввода указателя до команд плееру (без окна и без плеера).
//!
//! Цепочка та же, что в кадре окна: `map_timeline_interaction` (жест) → план по
//! охвату вида окна → [`timeline_step_commands`] (диспетчер жеста с троттлингом).
//! Проверяется результат — какие команды получил бы плеер окна (и какая перемотка
//! ушла бы зрительской перемоткой в сеанс).

use std::time::{Duration, Instant};

use media_core::{MediaDuration, MediaTime, TimelineRange, TimelineSnapshot};
use player_core::{PlayerCommand, ScrubCommitPolicy, SeekTarget};

use super::*;
use crate::downloaded_range::DownloadedRange;
use crate::downloaded_range::test_support::FakeDownloadedSpans;
use crate::ui::timeline::{
    TimelineBounds, TimelineLiveScrubDecodeMode, TimelinePointerInput, map_timeline_interaction,
};

/// Что получил плеер — без диагностики жеста, только смысл.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Sent {
    Begin,
    Preview(MediaTime),
    End(ScrubCommitPolicy),
    Seek(MediaTime),
}

fn sent(command: &PlayerCommand) -> Sent {
    let absolute = |target: SeekTarget| match target {
        SeekTarget::Absolute(position) => position,
        SeekTarget::Relative(_) => panic!("the timeline always seeks to absolute positions"),
    };
    match command {
        PlayerCommand::BeginScrub { .. } => Sent::Begin,
        PlayerCommand::PreviewScrub { request, .. } => Sent::Preview(absolute(request.target)),
        PlayerCommand::EndScrub { policy, .. } => Sent::End(*policy),
        PlayerCommand::Seek(request) => Sent::Seek(absolute(request.target)),
        other => panic!("unexpected timeline command {other:?}"),
    }
}

fn bounds() -> TimelineBounds {
    TimelineBounds::new(TimelineRange::from_bounds_saturating(
        MediaTime::ZERO,
        MediaTime::from_secs(100),
    ))
    .expect("seekable bounds")
}

fn at(fraction: f64) -> MediaTime {
    bounds().position_from_fraction(fraction)
}

/// Скачано 0–10 с и 20–30 с (дорожка — 100 с).
const ON_SHELF: f64 = 0.05;
const ALSO_ON_SHELF: f64 = 0.06;
const OFF_SHELF: f64 = 0.15;
const SECOND_PIECE: f64 = 0.25;

/// Один жест: ввод по кадрам → команды плееру.
struct Gesture {
    scope: LiveScrubScope,
    downloaded: DownloadedRange,
    timeline: TimelineUiState,
    settings: TimelineLiveScrubSettingsSnapshot,
    now: Instant,
}

impl Gesture {
    fn new(scope: LiveScrubScope, decode_mode: TimelineLiveScrubDecodeMode) -> Self {
        let (_spans, downloaded) = FakeDownloadedSpans::range(vec![
            Duration::from_secs(0)..Duration::from_secs(10),
            Duration::from_secs(20)..Duration::from_secs(30),
        ]);
        Self {
            scope,
            downloaded,
            timeline: TimelineUiState::default(),
            settings: TimelineLiveScrubSettingsSnapshot {
                decode_mode,
                max_hz: 60,
            },
            now: Instant::now(),
        }
    }

    fn browser() -> Self {
        Self::new(
            LiveScrubScope::DownloadedRangeOnly,
            TimelineLiveScrubDecodeMode::EveryDragEvent,
        )
    }

    /// Один кадр ввода; часы жеста идут на 100 мс за кадр (дальше любого троттлинга).
    fn frame(&mut self, input: TimelinePointerInput) -> Vec<Sent> {
        self.now += Duration::from_millis(100);
        let timeline = TimelineSnapshot::seekable_vod(MediaDuration::from_secs(100));
        let interaction =
            map_timeline_interaction(&timeline, &mut self.timeline, Some(bounds()), input, true);
        let mut commands = Vec::new();
        for action in interaction.actions {
            let steps = plan_timeline_action(
                action,
                &TimelinePlanContext {
                    scope: self.scope,
                    standing: player_scrub_standing(&self.timeline),
                    downloaded: &self.downloaded,
                },
            );
            commands.extend(
                timeline_step_commands(&steps, &mut self.timeline, self.settings, self.now)
                    .iter()
                    .map(|(command, _route)| sent(command)),
            );
        }
        commands
    }

    fn press(&mut self, fraction: f64) -> Vec<Sent> {
        self.frame(TimelinePointerInput {
            pointer_down_on_timeline: true,
            drag_started: true,
            pointer_fraction: Some(fraction),
            ..Default::default()
        })
    }

    fn drag(&mut self, fraction: f64) -> Vec<Sent> {
        self.frame(TimelinePointerInput {
            dragged: true,
            pointer_fraction: Some(fraction),
            ..Default::default()
        })
    }

    fn release(&mut self, fraction: f64) -> Vec<Sent> {
        self.frame(TimelinePointerInput {
            drag_stopped: true,
            pointer_fraction: Some(fraction),
            ..Default::default()
        })
    }
}

#[test]
fn browser_drag_shows_frames_only_on_the_shelf_and_seeks_once_on_release_outside() {
    let mut gesture = Gesture::browser();

    // Нажали вне полки: плеер о жесте не знает.
    assert_eq!(gesture.press(OFF_SHELF), []);
    // Первая цель на полке начинает скраб и сразу показывает кадр.
    assert_eq!(
        gesture.drag(ON_SHELF),
        [Sent::Begin, Sent::Preview(at(ON_SHELF))]
    );
    assert_eq!(
        gesture.drag(ALSO_ON_SHELF),
        [Sent::Preview(at(ALSO_ON_SHELF))]
    );
    // Вне полки: кадр стоит — плееру ничего.
    assert_eq!(gesture.drag(OFF_SHELF), []);
    // Второй кусок полки — кадры снова бегут.
    assert_eq!(
        gesture.drag(SECOND_PIECE),
        [Sent::Preview(at(SECOND_PIECE))]
    );
    assert_eq!(gesture.drag(OFF_SHELF), []);
    // Отпустили вне полки: скраб садится на показанное, и одна обычная перемотка.
    assert_eq!(
        gesture.release(OFF_SHELF),
        [
            Sent::End(ScrubCommitPolicy::CommitLatestTarget),
            Sent::Seek(at(OFF_SHELF)),
        ]
    );
    assert!(!gesture.timeline.has_live_scrub_dispatch());
}

#[test]
fn browser_release_on_the_shelf_never_produces_an_ordinary_seek() {
    let mut gesture = Gesture::browser();
    assert_eq!(
        gesture.press(ON_SHELF),
        [Sent::Begin, Sent::Preview(at(ON_SHELF))]
    );
    assert_eq!(gesture.drag(OFF_SHELF), []);
    // Отпустили снова на полке: финальная цель — плееру, политика drag — видимый кадр.
    assert_eq!(
        gesture.release(ALSO_ON_SHELF),
        [
            Sent::Preview(at(ALSO_ON_SHELF)),
            Sent::End(ScrubCommitPolicy::CommitVisiblePreview),
        ]
    );
}

#[test]
fn a_browser_gesture_entirely_off_the_shelf_is_one_seek_and_nothing_else() {
    let mut gesture = Gesture::browser();
    assert_eq!(gesture.press(OFF_SHELF), []);
    assert_eq!(gesture.drag(0.16), []);
    assert_eq!(gesture.release(0.17), [Sent::Seek(at(0.17))]);
}

#[test]
fn a_browser_click_on_the_shelf_lands_locally() {
    let mut gesture = Gesture::browser();
    let sent = gesture.frame(TimelinePointerInput {
        pointer_down_on_timeline: true,
        clicked: true,
        pointer_fraction: Some(ON_SHELF),
        ..Default::default()
    });
    assert_eq!(
        sent,
        [
            Sent::Begin,
            Sent::Preview(at(ON_SHELF)),
            Sent::End(ScrubCommitPolicy::CommitLatestTarget),
        ]
    );
}

#[test]
fn browser_focus_loss_ends_a_started_scrub_without_any_seek() {
    let mut gesture = Gesture::browser();
    let _begin = gesture.press(ON_SHELF);
    assert_eq!(gesture.drag(OFF_SHELF), []);
    let lost_focus = gesture.frame(TimelinePointerInput {
        lost_focus: true,
        pointer_fraction: Some(OFF_SHELF),
        ..Default::default()
    });
    assert_eq!(
        lost_focus,
        [Sent::End(ScrubCommitPolicy::CommitLatestTarget)]
    );
}

#[test]
fn an_off_shelf_target_does_not_hold_the_throttled_dispatcher() {
    let mut gesture = Gesture::new(
        LiveScrubScope::DownloadedRangeOnly,
        TimelineLiveScrubDecodeMode::ThrottledLatest,
    );
    let _begin = gesture.press(ON_SHELF);
    // Кадр первой цели показан — диспетчер свободен.
    gesture
        .timeline
        .note_live_scrub_landing_presented(at(ON_SHELF));
    assert_eq!(gesture.drag(OFF_SHELF), []);
    // Если бы цель вне полки ушла диспетчеру, он ждал бы её посадки (её не будет)
    // и придержал бы следующую цель на полке.
    assert_eq!(
        gesture.drag(ALSO_ON_SHELF),
        [Sent::Preview(at(ALSO_ON_SHELF))]
    );
    assert_eq!(
        gesture
            .timeline
            .live_scrub_diagnostics()
            .map(|diagnostics| diagnostics.throttled_latest_skip_count),
        Some(0)
    );
}

#[test]
fn the_standard_window_scrubs_everywhere_as_before() {
    let mut gesture = Gesture::new(
        LiveScrubScope::Anywhere,
        TimelineLiveScrubDecodeMode::EveryDragEvent,
    );
    assert_eq!(
        gesture.press(OFF_SHELF),
        [Sent::Begin, Sent::Preview(at(OFF_SHELF))]
    );
    assert_eq!(gesture.drag(ON_SHELF), [Sent::Preview(at(ON_SHELF))]);
    assert_eq!(
        gesture.release(0.4),
        [
            Sent::Preview(at(0.4)),
            Sent::End(ScrubCommitPolicy::CommitVisiblePreview),
        ]
    );
}
