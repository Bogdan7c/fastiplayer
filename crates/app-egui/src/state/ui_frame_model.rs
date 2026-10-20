//! Явная «UI-модель кадра» (cpu-parity, сессия 04): что интерфейс реально показывает.
//!
//! Если модель не изменилась, не было ввода, событий плеера и анимаций, egui-проход можно
//! пропустить. Модель — не хэш всего `PlayerSnapshot`: позиция и буферы в нём меняются на
//! каждом тике, а на экране меняется только секунда текста и пиксель ползунка. Поэтому
//! позиция здесь квантуется ([`QuantizedTimelinePosition`]), а поля, не влияющие на картинку
//! (очереди, диагностика, счётчики), в модель не входят.
//!
//! Что сюда НЕ входит и почему это безопасно:
//! - содержимое дорожек, качества, ошибки, медиа — меняются только вместе с событиями плеера,
//!   а события всегда дают полный проход (`UiPassReasons::player_events`);
//! - содержимое меню и панелей, открываемых пользователем, — это ввод;
//! - анимации — отдельная причина прохода (`animation_in_flight` и запросы egui).
//!
//! Владелец модели — `AppState` (см. `state/redraw_runtime.rs`), здесь только чистые типы,
//! квантование и расчёт следующего видимого изменения позиции.

use std::time::{Duration, Instant};

use media_core::{
    MediaTime, TimelineMode, TimelineNotSeekableReason, TimelinePreviewState, TimelineRange,
    TimelineSnapshot,
};
use player_core::{MediaInstanceId, PlaybackState, PlayerSnapshot, TrackSelectionSnapshot};

use super::notifications::NotificationsFrame;
use super::sidebar_controller::{SidebarContentTransition, SidebarSection};
use crate::fullscreen_chrome::ChromePresentation;
use crate::playlist_runtime::{
    PendingPlaylistConfirmation, PlaylistImportPreview, PlaylistInteractionModel,
    PlaylistTransportUiModel, PlaylistUndoUiSnapshot, PlaylistViewRevision,
};
use crate::ui::timeline::TimelineUiState;

/// Факты об окне, которые интерфейс читает напрямую у winit на каждом проходе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowFacts {
    /// Размер surface в пикселях.
    pub(crate) inner_size: [u32; 2],
    /// Масштаб egui в битах `f32`: сравнение точное, как у `ScreenDescriptor` shell-а.
    pub(crate) pixels_per_point_bits: u32,
    pub(crate) is_maximized: bool,
    pub(crate) is_fullscreen: bool,
}

/// Позиция таймлайна, квантованная до того, что видно: секунда текста и пиксель ползунка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct QuantizedTimelinePosition {
    /// Целые секунды позиции — ровно то, что печатает `format_seconds` (`floor`).
    pub(crate) whole_seconds: u64,
    /// Номер пикселя ползунка (`floor(fraction × ширина дорожки)`); `None` без дорожки.
    pub(crate) slider_step: Option<u64>,
}

/// Поля `TimelineSnapshot`, влияющие на картинку (без непрерывной позиции).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimelineVisualKey {
    pub(crate) mode: TimelineMode,
    pub(crate) target_position: Option<MediaTime>,
    pub(crate) seekable_range: Option<TimelineRange>,
    pub(crate) seekable: bool,
    pub(crate) not_seekable_reason: Option<TimelineNotSeekableReason>,
    pub(crate) seeking: bool,
    pub(crate) scrubbing: bool,
    pub(crate) stale_frame: bool,
    pub(crate) preview_state: TimelinePreviewState,
    /// Live edge посекундно: справа от таймлайна печатается `DVR a–b · LIVE`.
    pub(crate) live_edge_whole_seconds: Option<u64>,
}

impl TimelineVisualKey {
    fn from_timeline(timeline: &TimelineSnapshot) -> Self {
        Self {
            mode: timeline.mode,
            target_position: timeline.target_position,
            seekable_range: timeline.seekable_range,
            seekable: timeline.seekable,
            not_seekable_reason: timeline.not_seekable_reason,
            seeking: timeline.seeking,
            scrubbing: timeline.scrubbing,
            stale_frame: timeline.stale_frame,
            preview_state: timeline.preview_state,
            live_edge_whole_seconds: timeline.live_edge.map(|edge| edge.as_duration().as_secs()),
        }
    }
}

/// Видимое состояние сайдбара: что показано и в какой фазе анимации.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SidebarVisualKey {
    pub(crate) displayed: Option<SidebarSection>,
    pub(crate) target: Option<SidebarSection>,
    pub(crate) open_progress: f32,
    pub(crate) content_transition: Option<SidebarContentTransition>,
    pub(crate) open_width_points: f32,
}

/// Видимое состояние уведомлений: `age` тоста заменён фактом «проявление закончено»,
/// иначе модель менялась бы на каждом кадре, пока тост на экране.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationsVisualKey {
    /// Центр, спиннер и motion — как в проекции кадра.
    pub(crate) frame_without_toast_ages: NotificationsFrame,
}

impl NotificationsVisualKey {
    /// Срок проявления тоста из `ui::notifications` (150 мс): после него картинка статична.
    fn from_frame(frame: &NotificationsFrame, fade_in: Duration) -> Self {
        let mut frame_without_toast_ages = frame.clone();
        for toast in &mut frame_without_toast_ages.toasts {
            // Квантование возраста: «ещё проявляется» (0) или «уже проявился» (fade_in).
            toast.age = if toast.age >= fade_in {
                fade_in
            } else {
                Duration::ZERO
            };
        }
        Self {
            frame_without_toast_ages,
        }
    }

    /// Пока хоть один тост проявляется, кадры нужны непрерывно (как у анимации).
    pub(crate) fn any_toast_fading_in(&self, fade_in: Duration) -> bool {
        self.frame_without_toast_ages
            .toasts
            .iter()
            .any(|toast| toast.age < fade_in)
    }
}

/// Модели плейлиста, которые UI получает от process-lifetime runtime-а на каждом кадре.
///
/// Без `Debug`: `PlaylistInteractionModel` намеренно не печатает сырой URL.
#[derive(Clone, PartialEq)]
pub(crate) struct PlaylistVisualKey {
    pub(crate) view_revision: Option<PlaylistViewRevision>,
    pub(crate) interaction: PlaylistInteractionModel,
    pub(crate) transport: PlaylistTransportUiModel,
    pub(crate) undo: PlaylistUndoUiSnapshot,
    pub(crate) import_preview: Option<PlaylistImportPreview>,
    pub(crate) confirmation: Option<PendingPlaylistConfirmation>,
}

/// Всё, что интерфейс показывает в кадре, в сравнимом виде (без `Debug`: см. `PlaylistVisualKey`).
#[derive(Clone, PartialEq)]
pub(crate) struct UiFrameModel {
    pub(crate) window: WindowFacts,
    pub(crate) playback_state: PlaybackState,
    pub(crate) playback_rate_bits: u32,
    pub(crate) volume_bits: u32,
    pub(crate) muted: bool,
    pub(crate) media_instance_id: Option<MediaInstanceId>,
    pub(crate) render_generation: u64,
    pub(crate) duration: Option<Duration>,
    pub(crate) position: QuantizedTimelinePosition,
    pub(crate) timeline: TimelineVisualKey,
    pub(crate) timeline_ui_state: TimelineUiState,
    pub(crate) timeline_inline_status: Option<&'static str>,
    pub(crate) selected_tracks: TrackSelectionSnapshot,
    pub(crate) track_count: usize,
    pub(crate) has_error: bool,
    pub(crate) startup_pending: bool,
    pub(crate) chrome: ChromePresentation,
    pub(crate) sidebar: SidebarVisualKey,
    pub(crate) notifications: NotificationsVisualKey,
    pub(crate) telemetry_open: bool,
    pub(crate) settings_window_open: bool,
    pub(crate) settings_ui_revision: u64,
    pub(crate) reduced_motion: bool,
    pub(crate) playlist: PlaylistVisualKey,
}

/// Факты плеера, нужные модели (вынесены, чтобы сборка была чистой функцией от snapshot-а).
pub(crate) struct PlayerVisualFacts<'snapshot> {
    pub(crate) snapshot: &'snapshot PlayerSnapshot,
    /// Ширина дорожки ползунка в пикселях с прошлого прохода; `None` до первого прохода.
    pub(crate) timeline_track_width_px: Option<u32>,
}

/// Факты app-слоя, которые читают владельцы состояния `AppState`.
pub(crate) struct AppVisualFacts<'frame> {
    pub(crate) window: WindowFacts,
    pub(crate) timeline_ui_state: &'frame TimelineUiState,
    pub(crate) timeline_inline_status: Option<&'static str>,
    pub(crate) startup_pending: bool,
    pub(crate) chrome: ChromePresentation,
    pub(crate) sidebar: SidebarVisualKey,
    pub(crate) notifications: &'frame NotificationsFrame,
    pub(crate) toast_fade_in: Duration,
    pub(crate) telemetry_open: bool,
    pub(crate) settings_window_open: bool,
    pub(crate) settings_ui_revision: u64,
    pub(crate) reduced_motion: bool,
    pub(crate) playlist: PlaylistVisualKey,
}

impl UiFrameModel {
    /// Собирает модель из фактов плеера и app-слоя. Чистая функция: ничего не мутирует.
    #[must_use]
    pub(crate) fn build(player: PlayerVisualFacts<'_>, app: AppVisualFacts<'_>) -> Self {
        let snapshot = player.snapshot;
        Self {
            window: app.window,
            playback_state: snapshot.playback_state,
            playback_rate_bits: snapshot.playback_rate.as_f32().to_bits(),
            volume_bits: snapshot.volume.to_bits(),
            muted: snapshot.muted,
            media_instance_id: snapshot.media_instance_id,
            render_generation: snapshot.render_generation,
            duration: snapshot.duration,
            position: quantize_timeline_position(
                &snapshot.timeline,
                player.timeline_track_width_px,
            ),
            timeline: TimelineVisualKey::from_timeline(&snapshot.timeline),
            timeline_ui_state: app.timeline_ui_state.clone(),
            timeline_inline_status: app.timeline_inline_status,
            selected_tracks: snapshot.selected_tracks.clone(),
            track_count: snapshot.tracks.len(),
            has_error: snapshot.last_error.is_some(),
            startup_pending: app.startup_pending,
            chrome: app.chrome,
            sidebar: app.sidebar,
            notifications: NotificationsVisualKey::from_frame(app.notifications, app.toast_fade_in),
            telemetry_open: app.telemetry_open,
            settings_window_open: app.settings_window_open,
            settings_ui_revision: app.settings_ui_revision,
            reduced_motion: app.reduced_motion,
            playlist: app.playlist,
        }
    }
}

/// Seekable-окно, по которому ползунок переводит позицию в долю дорожки
/// (та же логика, что `ui::timeline::geometry::timeline_bounds`).
fn slider_range(timeline: &TimelineSnapshot) -> Option<TimelineRange> {
    if !timeline.seekable {
        return None;
    }
    let range = match timeline.seekable_range {
        Some(range) => range,
        None => TimelineRange::from_bounds_saturating(
            MediaTime::ZERO,
            MediaTime::from_duration(timeline.duration?.as_duration()),
        ),
    };
    (range.duration().as_duration() > Duration::ZERO).then_some(range)
}

/// Квантует позицию до секунды текста и пикселя ползунка.
#[must_use]
pub(crate) fn quantize_timeline_position(
    timeline: &TimelineSnapshot,
    track_width_px: Option<u32>,
) -> QuantizedTimelinePosition {
    let position = timeline.current_position.as_duration();
    let slider_step = slider_range(timeline)
        .zip(track_width_px.filter(|width| *width > 0))
        .map(|(range, width)| {
            let offset = range
                .clamp(timeline.current_position)
                .as_duration()
                .saturating_sub(range.start.as_duration());
            let fraction = offset.as_secs_f64() / range.duration().as_duration().as_secs_f64();
            (fraction * f64::from(width)).floor() as u64
        });
    QuantizedTimelinePosition {
        whole_seconds: position.as_secs(),
        slider_step,
    }
}

/// Нижняя граница ожидания: короче ставить дедлайн бессмысленно, а ноль дал бы пустой цикл.
const MIN_POSITION_WAKE_DELAY: Duration = Duration::from_millis(1);

/// Когда позиция в следующий раз изменит картинку (секунда текста или пиксель ползунка).
///
/// Нужно, когда кадры не приходят (звук без видео, 24 fps фильм с длинной дорожкой): без этого
/// время на экране замирало бы до следующего кадра или страховочного таймера. Считается
/// только для `Playing` (часы идут) и положительной скорости; media-время переводится в
/// настенное делением на скорость.
#[must_use]
pub(crate) fn next_visible_position_change(
    snapshot: &PlayerSnapshot,
    track_width_px: Option<u32>,
    now: Instant,
) -> Option<Instant> {
    if snapshot.playback_state != PlaybackState::Playing {
        return None;
    }
    let rate = f64::from(snapshot.playback_rate.as_f32());
    // NaN не проходит ни одну из проверок ниже: `<=` с NaN ложно, `is_finite` — тоже.
    if rate <= 0.0 || !rate.is_finite() {
        return None;
    }
    let timeline = &snapshot.timeline;
    let position = timeline.current_position.as_duration();

    // До следующей целой секунды текста.
    let until_next_second =
        Duration::from_secs(1) - Duration::from_nanos(position.subsec_nanos().into());

    // До следующего пикселя ползунка (если дорожка известна).
    let until_next_slider_step = slider_range(timeline)
        .zip(track_width_px.filter(|width| *width > 0))
        .and_then(|(range, width)| {
            let range_duration = range.duration().as_duration().as_secs_f64();
            let step_seconds = range_duration / f64::from(width);
            let offset = position
                .saturating_sub(range.start.as_duration())
                .as_secs_f64();
            let next_step_seconds = ((offset / step_seconds).floor() + 1.0) * step_seconds;
            Duration::try_from_secs_f64((next_step_seconds - offset).max(0.0)).ok()
        });

    let media_delay = match until_next_slider_step {
        Some(step_delay) => until_next_second.min(step_delay),
        None => until_next_second,
    };
    let wall_delay = Duration::try_from_secs_f64(media_delay.as_secs_f64() / rate).ok()?;
    Some(now + wall_delay.max(MIN_POSITION_WAKE_DELAY))
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_core::MediaDuration;
    use player_core::PlaybackRate;

    fn timeline_with_position(position_ms: u64, duration_s: u64) -> TimelineSnapshot {
        TimelineSnapshot {
            mode: TimelineMode::default(),
            current_position: MediaTime::from_millis(position_ms),
            target_position: None,
            duration: Some(MediaDuration::from_duration(Duration::from_secs(
                duration_s,
            ))),
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
        }
    }

    fn playing_snapshot(position_ms: u64, duration_s: u64, rate: f32) -> PlayerSnapshot {
        let mut snapshot = PlayerSnapshot::empty();
        snapshot.playback_state = PlaybackState::Playing;
        snapshot.playback_rate = PlaybackRate::new(rate).expect("valid rate");
        snapshot.current_position = Duration::from_millis(position_ms);
        snapshot.timeline = timeline_with_position(position_ms, duration_s);
        snapshot
    }

    /// Позиции внутри одной секунды и одного пикселя дают одинаковый ключ.
    #[test]
    fn positions_within_the_same_second_and_pixel_quantize_equally() {
        // 100 с на 1000 px: пиксель = 100 мс; секунда 12.0..12.099 — тот же пиксель 120.
        let first = quantize_timeline_position(&timeline_with_position(12_000, 100), Some(1000));
        let second = quantize_timeline_position(&timeline_with_position(12_099, 100), Some(1000));
        assert_eq!(first, second);
        assert_eq!(first.whole_seconds, 12);
        assert_eq!(first.slider_step, Some(120));
    }

    /// Переход секунды и переход пикселя каждый по отдельности меняют ключ.
    #[test]
    fn crossing_a_second_or_a_pixel_changes_the_key() {
        // 1000 с на 100 px: пиксель = 10 с, поэтому 12.9 → 13.0 меняет только секунду.
        let before_second =
            quantize_timeline_position(&timeline_with_position(12_900, 1000), Some(100));
        let after_second =
            quantize_timeline_position(&timeline_with_position(13_000, 1000), Some(100));
        assert_ne!(before_second, after_second);
        assert_eq!(before_second.slider_step, after_second.slider_step);

        // 100 с на 1000 px: пиксель = 100 мс, поэтому 12.050 → 12.150 меняет только пиксель.
        let before_pixel =
            quantize_timeline_position(&timeline_with_position(12_050, 100), Some(1000));
        let after_pixel =
            quantize_timeline_position(&timeline_with_position(12_150, 100), Some(1000));
        assert_ne!(before_pixel, after_pixel);
        assert_eq!(before_pixel.whole_seconds, after_pixel.whole_seconds);
    }

    /// Без дорожки (нет длительности, не seekable, ширина неизвестна) пиксель не считается.
    #[test]
    fn slider_step_is_absent_without_a_track() {
        let mut no_duration = timeline_with_position(5_000, 100);
        no_duration.duration = None;
        assert_eq!(
            quantize_timeline_position(&no_duration, Some(1000)).slider_step,
            None
        );
        let mut not_seekable = timeline_with_position(5_000, 100);
        not_seekable.seekable = false;
        assert_eq!(
            quantize_timeline_position(&not_seekable, Some(1000)).slider_step,
            None
        );
        assert_eq!(
            quantize_timeline_position(&timeline_with_position(5_000, 100), None).slider_step,
            None
        );
        assert_eq!(
            quantize_timeline_position(&timeline_with_position(5_000, 100), Some(0)).slider_step,
            None
        );
    }

    /// Дедлайн — ближайшее из «следующая секунда» и «следующий пиксель», в настенном времени.
    #[test]
    fn next_change_is_the_nearest_of_second_and_pixel_boundaries() {
        let now = Instant::now();
        // 100 с на 1000 px: пиксель 100 мс. Позиция 12.030: до пикселя 70 мс, до секунды 970 мс.
        let deadline =
            next_visible_position_change(&playing_snapshot(12_030, 100, 1.0), Some(1000), now)
                .expect("playing media schedules a position wake");
        let delay = deadline - now;
        assert!(
            (Duration::from_millis(69)..=Duration::from_millis(71)).contains(&delay),
            "expected ~70 ms until the next slider pixel, got {delay:?}"
        );

        // Без дорожки остаётся только секунда: 12.250 → 750 мс.
        let deadline = next_visible_position_change(&playing_snapshot(12_250, 100, 1.0), None, now)
            .expect("playing media schedules a position wake");
        let delay = deadline - now;
        assert!(
            (Duration::from_millis(749)..=Duration::from_millis(751)).contains(&delay),
            "expected ~750 ms until the next whole second, got {delay:?}"
        );
    }

    /// Скорость 2× делит ожидание пополам; пауза и нулевая скорость ничего не планируют.
    #[test]
    fn rate_scales_the_wait_and_non_playing_states_do_not_schedule() {
        let now = Instant::now();
        let deadline = next_visible_position_change(&playing_snapshot(12_250, 100, 2.0), None, now)
            .expect("playing media schedules a position wake");
        let delay = deadline - now;
        assert!(
            (Duration::from_millis(374)..=Duration::from_millis(376)).contains(&delay),
            "expected ~375 ms at 2x, got {delay:?}"
        );

        let mut paused = playing_snapshot(12_250, 100, 1.0);
        paused.playback_state = PlaybackState::Paused;
        assert_eq!(next_visible_position_change(&paused, Some(1000), now), None);
    }

    /// Дедлайн никогда не в прошлом и не нулевой: на границе секунды ждём следующую.
    #[test]
    fn deadline_on_an_exact_boundary_targets_the_next_boundary() {
        let now = Instant::now();
        let deadline =
            next_visible_position_change(&playing_snapshot(12_000, 100, 1.0), Some(1000), now)
                .expect("playing media schedules a position wake");
        let delay = deadline - now;
        assert!(
            (Duration::from_millis(99)..=Duration::from_millis(101)).contains(&delay),
            "on a boundary the next pixel is a full step away, got {delay:?}"
        );
    }

    /// Возраст тостов квантуется: пока тост проявляется, ключ меняется, потом — нет.
    #[test]
    fn toast_age_is_quantized_to_fading_or_settled() {
        use super::super::notifications::BufferingIndicator;
        use super::super::notifications::{
            NotificationId, NotificationsFrame, ToastKind, ToastView,
        };
        use crate::ui::animation::UiMotion;

        let fade_in = Duration::from_millis(150);
        let frame = |age: Duration| NotificationsFrame {
            center: None,
            toasts: vec![ToastView {
                id: NotificationId::for_tests(1),
                kind: ToastKind::Info,
                message: "hello".into(),
                age,
            }],
            buffering: BufferingIndicator::Hidden,
            motion: UiMotion::Standard,
        };
        let fading = NotificationsVisualKey::from_frame(&frame(Duration::from_millis(40)), fade_in);
        let still_fading =
            NotificationsVisualKey::from_frame(&frame(Duration::from_millis(90)), fade_in);
        let settled =
            NotificationsVisualKey::from_frame(&frame(Duration::from_millis(400)), fade_in);
        let settled_later =
            NotificationsVisualKey::from_frame(&frame(Duration::from_secs(3)), fade_in);

        assert_eq!(
            fading, still_fading,
            "two fading ages collapse into one key"
        );
        assert_ne!(fading, settled, "finishing the fade changes the key once");
        assert_eq!(
            settled, settled_later,
            "a settled toast keeps the same key for its lifetime"
        );
        assert!(fading.any_toast_fading_in(fade_in));
        assert!(!settled.any_toast_fading_in(fade_in));
    }
}
