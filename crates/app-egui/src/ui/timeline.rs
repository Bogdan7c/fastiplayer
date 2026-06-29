//! Timeline UI и чистый mapper pointer events -> user actions.

use std::time::Duration;

use egui::{Color32, Pos2, Rect, Response, Sense, StrokeKind, Ui, Vec2};
pub use frame_server_core::{
    DeferredLiveScrubSettingsChange, LiveScrubDecodeMode as TimelineLiveScrubDecodeMode,
    LiveScrubDiagnostics, LiveScrubSettingsSnapshot as TimelineLiveScrubSettingsSnapshot,
};
use media_core::{MediaDuration, MediaTime, TimelineRange, TimelineSnapshot};

use crate::ui::skin::{PlayerSkin, TimelineStyle};

/// Transient UI-состояние timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineUiState {
    /// Последняя позиция pointer drag, ещё не обязанная попасть в worker snapshot.
    transient_drag_position: Option<MediaTime>,

    /// Timeline pointer gesture уже владеет live scrub stream-ом.
    live_scrub_gesture_active: bool,

    /// Snapshot/decode-dispatch policy текущего live scrub gesture-а.
    live_scrub_dispatch: Option<TimelineLiveScrubDispatchState>,
}

/// UI-owned dispatch state одного live scrub gesture-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineLiveScrubDispatchState {
    /// Bounded diagnostics текущего drag-а, включая pointer-down settings snapshot.
    diagnostics: LiveScrubDiagnostics,

    /// Последний committed settings snapshot, замеченный во время active drag-а.
    last_observed_settings: TimelineLiveScrubSettingsSnapshot,

    /// Последний момент, когда app отправил target в player worker.
    last_decode_dispatch_at: Option<std::time::Instant>,

    /// Последний target, который реально ушёл в player worker.
    last_dispatched_target: Option<MediaTime>,

    /// Новейший target, пропущенный throttle-ом.
    pending_throttled_target: Option<MediaTime>,
}

impl TimelineUiState {
    /// Возвращает `true`, если UI сейчас ведёт локальный pointer drag.
    #[must_use]
    pub const fn has_active_drag(&self) -> bool {
        self.transient_drag_position.is_some()
    }

    /// Возвращает `true`, если текущий pointer gesture уже начал live scrub.
    #[must_use]
    pub const fn has_active_live_scrub_gesture(&self) -> bool {
        self.live_scrub_gesture_active
    }

    /// Сбрасывает transient pointer state после завершения drag.
    pub fn clear_transient_drag(&mut self) {
        self.transient_drag_position = None;
    }

    /// Завершает UI-owned live scrub gesture marker.
    pub fn clear_live_scrub_gesture(&mut self) {
        self.live_scrub_gesture_active = false;
    }

    /// Полностью очищает dispatch snapshot live scrub-а после отправки End/Cancel.
    pub fn clear_live_scrub_dispatch(&mut self) {
        self.live_scrub_dispatch = None;
    }

    /// Захватывает settings snapshot и помечает initial target уже отправленным.
    pub fn begin_live_scrub_dispatch(
        &mut self,
        settings: TimelineLiveScrubSettingsSnapshot,
        now: std::time::Instant,
        initial_target: MediaTime,
    ) {
        self.live_scrub_dispatch = Some(TimelineLiveScrubDispatchState {
            diagnostics: LiveScrubDiagnostics::from_settings_snapshot(settings),
            last_observed_settings: settings,
            last_decode_dispatch_at: Some(now),
            last_dispatched_target: Some(initial_target),
            pending_throttled_target: None,
        });
    }

    /// Возвращает текущий bounded diagnostics state active live-scrub dispatch-а.
    #[must_use]
    pub fn live_scrub_diagnostics(&self) -> Option<LiveScrubDiagnostics> {
        self.live_scrub_dispatch
            .as_ref()
            .map(|dispatch| dispatch.diagnostics)
    }

    /// Записывает settings change как deferred до следующего pointer-down.
    pub fn defer_live_scrub_settings_change(
        &mut self,
        new_snapshot: TimelineLiveScrubSettingsSnapshot,
    ) -> Option<LiveScrubDiagnostics> {
        let dispatch = self.live_scrub_dispatch.as_mut()?;
        if dispatch.last_observed_settings == new_snapshot {
            return Some(dispatch.diagnostics);
        }

        let change = DeferredLiveScrubSettingsChange {
            old_snapshot: dispatch.last_observed_settings,
            new_snapshot,
        };
        dispatch.last_observed_settings = new_snapshot;
        dispatch.diagnostics.record_deferred_settings_change(change);
        Some(dispatch.diagnostics)
    }

    /// Возвращает target, который можно отправить в decoder work для этого drag frame-а.
    pub fn live_scrub_preview_dispatch_target(
        &mut self,
        now: std::time::Instant,
        target: MediaTime,
    ) -> Option<MediaTime> {
        let Some(dispatch) = self.live_scrub_dispatch.as_mut() else {
            return Some(target);
        };

        match dispatch.diagnostics.settings_snapshot.decode_mode {
            TimelineLiveScrubDecodeMode::EveryDragEvent => {
                dispatch.last_decode_dispatch_at = Some(now);
                dispatch.last_dispatched_target = Some(target);
                dispatch.pending_throttled_target = None;
                Some(target)
            }
            TimelineLiveScrubDecodeMode::ThrottledLatest => {
                let min_period =
                    live_scrub_min_dispatch_period(dispatch.diagnostics.settings_snapshot.max_hz);
                let can_dispatch = dispatch
                    .last_decode_dispatch_at
                    .is_none_or(|last_dispatch| {
                        now.saturating_duration_since(last_dispatch) >= min_period
                    });

                if can_dispatch {
                    let newest_target = dispatch.pending_throttled_target.take().unwrap_or(target);
                    dispatch.last_decode_dispatch_at = Some(now);
                    dispatch.last_dispatched_target = Some(newest_target);
                    Some(newest_target)
                } else {
                    dispatch.pending_throttled_target = Some(target);
                    dispatch.diagnostics.record_throttled_latest_skip();
                    None
                }
            }
        }
    }

    /// Возвращает release target, если его ещё нужно отправить перед `EndScrub`.
    pub fn live_scrub_release_dispatch_target(
        &mut self,
        now: std::time::Instant,
        release_target: MediaTime,
    ) -> Option<MediaTime> {
        let dispatch = self.live_scrub_dispatch.as_mut()?;
        let newest_target = dispatch
            .pending_throttled_target
            .take()
            .unwrap_or(release_target);
        let exact_release_target = if newest_target == release_target {
            newest_target
        } else {
            release_target
        };

        if dispatch.last_dispatched_target == Some(exact_release_target) {
            return None;
        }

        dispatch.last_decode_dispatch_at = Some(now);
        dispatch.last_dispatched_target = Some(exact_release_target);
        Some(exact_release_target)
    }

    /// Возвращает позицию, которую нужно показывать во время локального drag preview.
    #[must_use]
    pub fn display_position(&self, timeline: &TimelineSnapshot) -> MediaTime {
        self.transient_drag_position
            .or(timeline.target_position)
            .unwrap_or(timeline.current_position)
    }
}

/// Переводит max Hz в минимальный период между decode attempts.
fn live_scrub_min_dispatch_period(max_hz: u16) -> Duration {
    let validated_max_hz = max_hz.clamp(1, 240);
    let nanos_per_second = 1_000_000_000u64;
    Duration::from_nanos(nanos_per_second / u64::from(validated_max_hz))
}

/// Действие timeline, которое `AppState` конвертирует в `PlayerCommand`.
///
/// Эти actions описывают pointer timeline UX без раскрытия worker-side drag state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineAction {
    /// Одиночный click-to-seek: точный final seek без interactive scrub release policy.
    ClickSeek(MediaTime),

    /// Завершение pointer drag как обычный final seek в выбранную позицию.
    CommitDragSeek(MediaTime),

    /// Pointer-down захватывает live scrub stream и отправляет initial exact target.
    BeginLiveScrub(MediaTime),

    /// Drag frame обновляет latest target live scrub-а.
    PreviewLiveScrub(MediaTime),

    /// Release завершает active live scrub без ordinary seek command.
    EndLiveScrub(MediaTime),

    /// Focus/cancel закрывает active live scrub без нового target-а.
    CancelLiveScrub,
}

/// Нормализованный input mapper-а без зависимости от `egui::Response` в тестах.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TimelinePointerInput {
    /// Click был завершён на timeline.
    pub clicked: bool,

    /// Pointer удерживается на timeline, но это не начинает scrub до реального drag.
    pub pointer_down_on_timeline: bool,

    /// Drag начался на timeline.
    pub drag_started: bool,

    /// Pointer перемещается во время drag.
    pub dragged: bool,

    /// Drag завершился.
    pub drag_stopped: bool,

    /// Widget потерял focus во время scrub.
    pub lost_focus: bool,

    /// Позиция pointer как доля seekable-диапазона `0.0..=1.0`.
    pub pointer_fraction: Option<f64>,
}

/// Результат обработки одного timeline frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineInteraction {
    /// Действия, которые shell должен отправить в worker.
    pub actions: Vec<TimelineAction>,

    /// Позиция, которую UI должен показывать в этот frame.
    pub display_position: MediaTime,
}

/// Рисует timeline и возвращает user actions без отправки команд в player.
#[must_use]
pub fn render_timeline(
    ui: &mut Ui,
    timeline: &TimelineSnapshot,
    state: &mut TimelineUiState,
    skin: &impl PlayerSkin,
    live_scrub_enabled: bool,
) -> TimelineInteraction {
    let style = skin.timeline_style();
    let bounds = timeline_bounds(timeline);
    let enabled = bounds.is_some();
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let desired_size = Vec2::new(ui.available_width(), style.hit_height);
    let (response, painter) = ui.allocate_painter(desired_size, sense);
    let pointer_input = pointer_input_from_response(&response, bounds, style);
    let interaction =
        map_timeline_interaction(timeline, state, bounds, pointer_input, live_scrub_enabled);

    if pointer_input.drag_started
        || pointer_input.dragged
        || state.has_active_drag()
        || state.has_active_live_scrub_gesture()
    {
        ui.ctx().request_repaint();
    }

    paint_timeline(
        &painter,
        response.rect,
        timeline,
        state,
        bounds,
        style,
        enabled,
    );

    interaction
}

/// Рисует строку времени вокруг timeline.
pub fn render_time_labels(
    ui: &mut Ui,
    timeline: &TimelineSnapshot,
    state: &TimelineUiState,
    inline_status: Option<&str>,
) {
    let display_position = state.display_position(timeline);
    let duration_text = format_media_duration(timeline.duration);
    let position_text = format_media_time(Some(display_position));

    ui.horizontal(|ui| {
        ui.monospace(position_text);
        if let Some(inline_status) = inline_status {
            ui.add_space(8.0);
            ui.colored_label(Color32::from_rgb(255, 180, 120), inline_status);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.monospace(duration_text);
        });
    });
}

/// Обрабатывает pointer input и обновляет только transient UI state.
#[must_use]
pub fn map_timeline_interaction(
    timeline: &TimelineSnapshot,
    state: &mut TimelineUiState,
    bounds: Option<TimelineBounds>,
    input: TimelinePointerInput,
    live_scrub_enabled: bool,
) -> TimelineInteraction {
    let Some(bounds) = bounds else {
        state.clear_transient_drag();
        state.clear_live_scrub_gesture();
        state.clear_live_scrub_dispatch();
        return TimelineInteraction {
            actions: Vec::new(),
            display_position: timeline.current_position,
        };
    };

    let mut actions = Vec::new();
    let pointer_position = input
        .pointer_fraction
        .map(|fraction| bounds.position_from_fraction(fraction));

    if input.lost_focus && state.has_active_live_scrub_gesture() {
        state.clear_transient_drag();
        state.clear_live_scrub_gesture();
        actions.push(TimelineAction::CancelLiveScrub);
        return TimelineInteraction {
            actions,
            display_position: state.display_position(timeline),
        };
    }

    if input.lost_focus && state.has_active_drag() {
        let commit_position = pointer_position.or(state.transient_drag_position);
        state.clear_transient_drag();
        if let Some(position) = commit_position {
            actions.push(TimelineAction::CommitDragSeek(position));
        }
        return TimelineInteraction {
            actions,
            display_position: state.display_position(timeline),
        };
    }

    let wants_drag_position = input.drag_started
        || input.dragged
        || input.drag_stopped
        || (live_scrub_enabled && (input.pointer_down_on_timeline || input.clicked));
    if wants_drag_position && let Some(position) = pointer_position {
        state.transient_drag_position = Some(position);
    }

    let mut began_live_scrub_this_frame = false;
    if live_scrub_enabled
        && input.pointer_down_on_timeline
        && !state.has_active_live_scrub_gesture()
        && let Some(position) = pointer_position
    {
        state.live_scrub_gesture_active = true;
        began_live_scrub_this_frame = true;
        actions.push(TimelineAction::BeginLiveScrub(position));
    }

    if live_scrub_enabled
        && state.has_active_live_scrub_gesture()
        && !began_live_scrub_this_frame
        && input.dragged
        && let Some(position) = pointer_position
    {
        actions.push(TimelineAction::PreviewLiveScrub(position));
    }

    if live_scrub_enabled
        && state.has_active_live_scrub_gesture()
        && (input.drag_stopped || input.clicked)
    {
        let release_position = pointer_position.or(state.transient_drag_position);
        state.clear_transient_drag();
        state.clear_live_scrub_gesture();
        if let Some(position) = release_position {
            actions.push(TimelineAction::EndLiveScrub(position));
        } else {
            actions.push(TimelineAction::CancelLiveScrub);
        }
    } else if input.drag_stopped {
        let commit_position = state.transient_drag_position;
        state.clear_transient_drag();
        if let Some(position) = commit_position {
            actions.push(TimelineAction::CommitDragSeek(position));
        }
    } else if input.clicked
        && !state.has_active_drag()
        && let Some(position) = pointer_position
    {
        actions.push(TimelineAction::ClickSeek(position));
    }

    TimelineInteraction {
        actions,
        display_position: state.display_position(timeline),
    }
}

/// Форматирует media time в `MM:SS`, `HH:MM:SS` или `--:--`.
#[must_use]
pub fn format_media_time(position: Option<MediaTime>) -> String {
    format_seconds(position.map(MediaTime::as_secs_f64))
}

/// Форматирует media duration в `MM:SS`, `HH:MM:SS` или `--:--`.
#[must_use]
pub fn format_media_duration(duration: Option<MediaDuration>) -> String {
    format_seconds(duration.map(MediaDuration::as_secs_f64))
}

/// Форматирует seconds в стабильную player-style строку.
#[must_use]
pub fn format_seconds(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds else {
        return "--:--".to_string();
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_string();
    }

    let total_seconds = seconds.floor() as u64;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds / 60) % 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// Seekable bounds timeline-а, удобные для mapper-а и renderer-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineBounds {
    /// Seekable-диапазон.
    range: TimelineRange,
}

impl TimelineBounds {
    /// Создаёт bounds только для ненулевого диапазона.
    #[must_use]
    pub fn new(range: TimelineRange) -> Option<Self> {
        (range.duration() > MediaDuration::ZERO).then_some(Self { range })
    }

    /// Возвращает позицию внутри диапазона по доле `0.0..=1.0`.
    #[must_use]
    pub fn position_from_fraction(self, fraction: f64) -> MediaTime {
        let clamped_fraction = fraction.clamp(0.0, 1.0);
        let offset = duration_mul_fraction(self.range.duration(), clamped_fraction);

        self.range.start.saturating_add(offset)
    }

    /// Возвращает долю позиции внутри диапазона.
    #[must_use]
    fn fraction_from_position(self, position: MediaTime) -> f32 {
        let clamped_position = self.range.clamp(position);
        let offset = clamped_position
            .as_duration()
            .saturating_sub(self.range.start.as_duration());
        let duration = self.range.duration().as_secs_f64();
        if duration <= 0.0 {
            return 0.0;
        }

        (offset.as_secs_f64() / duration).clamp(0.0, 1.0) as f32
    }
}

/// Возвращает seekable bounds, если timeline можно интерактивно seek-ать.
#[must_use]
fn timeline_bounds(timeline: &TimelineSnapshot) -> Option<TimelineBounds> {
    if !timeline.seekable {
        return None;
    }

    if let Some(range) = timeline.seekable_range {
        return TimelineBounds::new(range);
    }

    timeline.duration.and_then(|duration| {
        let end = MediaTime::from_duration(duration.as_duration());
        let range = TimelineRange::from_bounds_saturating(MediaTime::ZERO, end);
        TimelineBounds::new(range)
    })
}

/// Конвертирует `egui::Response` в тестируемый pointer input.
fn pointer_input_from_response(
    response: &Response,
    bounds: Option<TimelineBounds>,
    style: TimelineStyle,
) -> TimelinePointerInput {
    let pointer_fraction =
        bounds
            .zip(response.interact_pointer_pos())
            .map(|(_bounds, pointer_position)| {
                pointer_fraction(response.rect, style, pointer_position)
            });

    TimelinePointerInput {
        clicked: response.clicked(),
        pointer_down_on_timeline: response.is_pointer_button_down_on(),
        drag_started: response.drag_started(),
        dragged: response.dragged(),
        drag_stopped: response.drag_stopped(),
        lost_focus: response.lost_focus(),
        pointer_fraction,
    }
}

/// Возвращает долю pointer по ширине timeline track.
fn pointer_fraction(rect: Rect, style: TimelineStyle, pointer_position: Pos2) -> f64 {
    let track_rect = timeline_track_rect(rect, style);
    let width = track_rect.width().max(1.0);

    f64::from(((pointer_position.x - track_rect.left()) / width).clamp(0.0, 1.0))
}

/// Рисует фон, прогресс, target и thumb timeline.
fn paint_timeline(
    painter: &egui::Painter,
    rect: Rect,
    timeline: &TimelineSnapshot,
    state: &TimelineUiState,
    bounds: Option<TimelineBounds>,
    style: TimelineStyle,
    enabled: bool,
) {
    let track_rect = timeline_track_rect(rect, style);
    let track_radius = style.track_height / 2.0;
    let track_outline_rect = timeline_track_outline_rect(track_rect, style);
    let track_outline_radius = timeline_track_outline_radius(style);
    let base_color = if enabled {
        style.track_fill
    } else {
        style.disabled_fill
    };

    painter.rect_filled(
        track_outline_rect,
        track_outline_radius,
        style.track_outline_fill,
    );
    painter.rect_filled(track_rect, track_radius, base_color);

    let Some(bounds) = bounds else {
        painter.rect_stroke(
            track_rect,
            track_radius,
            (1.0, Color32::from_gray(72)),
            StrokeKind::Inside,
        );
        return;
    };

    let current_fraction = bounds.fraction_from_position(timeline.current_position);
    let display_position = state.display_position(timeline);
    let display_fraction = bounds.fraction_from_position(display_position);
    let played_rect = rect_from_fraction(track_rect, current_fraction);
    let target_rect = rect_from_fraction(track_rect, display_fraction);
    let thumb_center = Pos2::new(
        egui::lerp(track_rect.left()..=track_rect.right(), display_fraction),
        track_rect.center().y,
    );

    painter.rect_filled(played_rect, track_radius, style.played_fill);
    if state.has_active_drag() || timeline.scrubbing {
        painter.rect_filled(target_rect, track_radius, style.target_fill);
    }
    painter.circle_filled(
        thumb_center,
        thumb_outline_radius(style),
        style.thumb_outline_fill,
    );
    painter.circle_filled(thumb_center, style.thumb_radius, style.thumb_fill);
}

/// Создаёт track rect внутри hit area.
fn timeline_track_rect(rect: Rect, style: TimelineStyle) -> Rect {
    let horizontal_padding = style.horizontal_padding.min(rect.width() / 2.0);
    let left = rect.left() + horizontal_padding;
    let right = rect.right() - horizontal_padding;
    let center_y = rect.center().y;
    let half_height = style.track_height / 2.0;

    Rect::from_min_max(
        Pos2::new(left, center_y - half_height),
        Pos2::new(right.max(left), center_y + half_height),
    )
}

/// Создаёт только визуальный outline rect вокруг track-а.
fn timeline_track_outline_rect(track_rect: Rect, style: TimelineStyle) -> Rect {
    track_rect.expand(style.track_outline_width.max(0.0))
}

/// Возвращает радиус outline rect так, чтобы он повторял форму track-а.
fn timeline_track_outline_radius(style: TimelineStyle) -> f32 {
    style.track_height / 2.0 + style.track_outline_width.max(0.0)
}

/// Возвращает радиус тёмной подложки бегунка.
fn thumb_outline_radius(style: TimelineStyle) -> f32 {
    style.thumb_radius + style.thumb_outline_width.max(0.0)
}

/// Строит rect заполнения track-а от начала до указанной доли.
fn rect_from_fraction(track_rect: Rect, fraction: f32) -> Rect {
    let right = egui::lerp(
        track_rect.left()..=track_rect.right(),
        fraction.clamp(0.0, 1.0),
    );

    Rect::from_min_max(track_rect.left_top(), Pos2::new(right, track_rect.bottom()))
}

/// Умножает duration на долю, сохраняя saturating-поведение.
fn duration_mul_fraction(duration: MediaDuration, fraction: f64) -> MediaDuration {
    let seconds = duration.as_secs_f64() * fraction.clamp(0.0, 1.0);
    let duration = Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX);

    MediaDuration::from_duration(duration)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use egui::{Color32, Pos2, Rect, Vec2};
    use media_core::{MediaDuration, MediaTime, TimelineRange, TimelineSnapshot};

    use super::{
        DeferredLiveScrubSettingsChange, TimelineAction, TimelineBounds,
        TimelineLiveScrubDecodeMode, TimelineLiveScrubSettingsSnapshot, TimelinePointerInput,
        TimelineStyle, TimelineUiState, format_seconds, live_scrub_min_dispatch_period,
        pointer_fraction, thumb_outline_radius, timeline_track_outline_rect, timeline_track_rect,
    };

    /// Создаёт seekable VOD timeline для mapper tests.
    fn seekable_timeline() -> TimelineSnapshot {
        TimelineSnapshot::seekable_vod(MediaDuration::from_secs(100))
    }

    /// Возвращает bounds тестовой timeline.
    fn seekable_bounds() -> TimelineBounds {
        TimelineBounds::new(TimelineRange::from_bounds_saturating(
            MediaTime::ZERO,
            MediaTime::from_secs(100),
        ))
        .expect("test timeline is seekable")
    }

    /// Старый mapper mode: live scrub выключен, drag release идёт как exact seek.
    fn map_timeline_interaction(
        timeline: &TimelineSnapshot,
        state: &mut TimelineUiState,
        bounds: Option<TimelineBounds>,
        input: TimelinePointerInput,
    ) -> super::TimelineInteraction {
        super::map_timeline_interaction(timeline, state, bounds, input, false)
    }

    /// Возвращает стиль timeline с заметной outline-шириной для geometry tests.
    fn timeline_style_with_outline() -> TimelineStyle {
        TimelineStyle {
            hit_height: 28.0,
            track_height: 5.0,
            thumb_radius: 6.0,
            horizontal_padding: 8.0,
            track_fill: Color32::from_gray(64),
            played_fill: Color32::WHITE,
            target_fill: Color32::from_rgb(130, 190, 255),
            thumb_fill: Color32::WHITE,
            track_outline_width: 3.0,
            track_outline_fill: Color32::BLACK,
            thumb_outline_width: 2.0,
            thumb_outline_fill: Color32::BLACK,
            disabled_fill: Color32::from_gray(48),
        }
    }

    /// Проверяет, что outline расширяет только визуальный слой, а track rect стабилен.
    #[test]
    fn outline_rect_expands_visual_layer_without_changing_track_rect() {
        let style = timeline_style_with_outline();
        let hit_rect =
            Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(220.0, style.hit_height));
        let track_rect = timeline_track_rect(hit_rect, style);
        let outline_rect = timeline_track_outline_rect(track_rect, style);

        assert_eq!(timeline_track_rect(hit_rect, style), track_rect);
        assert_eq!(
            outline_rect.left(),
            track_rect.left() - style.track_outline_width
        );
        assert_eq!(
            outline_rect.right(),
            track_rect.right() + style.track_outline_width
        );
        assert_eq!(
            outline_rect.top(),
            track_rect.top() - style.track_outline_width
        );
        assert_eq!(
            outline_rect.bottom(),
            track_rect.bottom() + style.track_outline_width
        );
        assert_eq!(track_rect.height(), style.track_height);
    }

    /// Проверяет, что тёмная подложка бегунка больше основного белого круга.
    #[test]
    fn thumb_outline_radius_is_larger_than_thumb_radius() {
        let style = timeline_style_with_outline();

        assert_eq!(
            thumb_outline_radius(style),
            style.thumb_radius + style.thumb_outline_width
        );
        assert!(thumb_outline_radius(style) > style.thumb_radius);
    }

    /// Проверяет, что pointer mapping остаётся привязан к track rect, а не outline rect.
    #[test]
    fn pointer_fraction_ignores_visual_outline_width() {
        let style = timeline_style_with_outline();
        let hit_rect =
            Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(220.0, style.hit_height));
        let track_rect = timeline_track_rect(hit_rect, style);
        let pointer_at_track_left = Pos2::new(track_rect.left(), track_rect.center().y);
        let pointer_at_track_right = Pos2::new(track_rect.right(), track_rect.center().y);

        assert!(style.track_outline_width > 0.0);
        assert_eq!(
            pointer_fraction(hit_rect, style, pointer_at_track_left),
            0.0
        );
        assert_eq!(
            pointer_fraction(hit_rect, style, pointer_at_track_right),
            1.0
        );
    }

    /// Проверяет, что pointer down сам по себе не создаёт drag/seek action.
    #[test]
    fn pointer_down_before_click_does_not_create_drag_or_seek_action() {
        let timeline = seekable_timeline();
        let mut state = TimelineUiState::default();

        let pointer_down = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                pointer_down_on_timeline: true,
                pointer_fraction: Some(0.25),
                ..TimelinePointerInput::default()
            },
        );
        assert!(pointer_down.actions.is_empty());
        assert!(!state.has_active_drag());

        let repeated_pointer_down = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                pointer_down_on_timeline: true,
                pointer_fraction: Some(0.25),
                ..TimelinePointerInput::default()
            },
        );
        assert!(repeated_pointer_down.actions.is_empty());
        assert!(!state.has_active_drag());

        let click = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                clicked: true,
                pointer_fraction: Some(0.25),
                ..TimelinePointerInput::default()
            },
        );

        assert_eq!(
            click.actions,
            vec![TimelineAction::ClickSeek(MediaTime::from_secs(25))]
        );
        assert!(!state.has_active_drag());
    }

    /// Проверяет, что простой click выдаёт только exact seek action.
    #[test]
    fn simple_click_maps_to_click_seek_only() {
        let timeline = seekable_timeline();
        let mut state = TimelineUiState::default();

        let interaction = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                clicked: true,
                pointer_fraction: Some(0.25),
                ..TimelinePointerInput::default()
            },
        );

        assert_eq!(
            interaction.actions,
            vec![TimelineAction::ClickSeek(MediaTime::from_secs(25))]
        );
        assert!(!state.has_active_drag());
    }

    /// Проверяет последовательность drag start, move и end.
    #[test]
    fn drag_keeps_transient_position_and_release_maps_to_single_seek() {
        let timeline = seekable_timeline();
        let mut state = TimelineUiState::default();

        let start = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                drag_started: true,
                pointer_fraction: Some(0.20),
                ..TimelinePointerInput::default()
            },
        );
        assert!(start.actions.is_empty());
        assert_eq!(start.display_position, MediaTime::from_secs(20));
        assert!(state.has_active_drag());

        let update = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                dragged: true,
                pointer_fraction: Some(0.70),
                ..TimelinePointerInput::default()
            },
        );
        assert!(update.actions.is_empty());
        assert_eq!(update.display_position, MediaTime::from_secs(70));
        assert!(state.has_active_drag());

        let end = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                drag_stopped: true,
                pointer_fraction: Some(0.70),
                ..TimelinePointerInput::default()
            },
        );

        assert_eq!(
            end.actions,
            vec![TimelineAction::CommitDragSeek(MediaTime::from_secs(70))]
        );
        assert!(!state.has_active_drag());
    }

    /// Live mode: pointer-down starts scrub, drag updates preview, release ends scrub.
    #[test]
    fn live_scrub_drag_maps_to_begin_preview_end_without_seek_actions() {
        let timeline = seekable_timeline();
        let mut state = TimelineUiState::default();

        let start = super::map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                pointer_down_on_timeline: true,
                pointer_fraction: Some(0.20),
                ..TimelinePointerInput::default()
            },
            true,
        );
        assert_eq!(
            start.actions,
            vec![TimelineAction::BeginLiveScrub(MediaTime::from_secs(20))]
        );
        assert!(state.has_active_live_scrub_gesture());

        let update = super::map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                pointer_down_on_timeline: true,
                dragged: true,
                pointer_fraction: Some(0.70),
                ..TimelinePointerInput::default()
            },
            true,
        );
        assert_eq!(
            update.actions,
            vec![TimelineAction::PreviewLiveScrub(MediaTime::from_secs(70))]
        );

        let end = super::map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                drag_stopped: true,
                pointer_fraction: Some(0.90),
                ..TimelinePointerInput::default()
            },
            true,
        );
        assert_eq!(
            end.actions,
            vec![TimelineAction::EndLiveScrub(MediaTime::from_secs(90))]
        );
        assert!(!state.has_active_live_scrub_gesture());
        assert!(
            end.actions
                .iter()
                .all(|action| !matches!(action, TimelineAction::CommitDragSeek(_)))
        );
    }

    /// Pointer-down + dragged в одном frame-е не создаёт duplicate PreviewScrub.
    #[test]
    fn live_scrub_pointer_down_drag_frame_sends_only_initial_target() {
        let timeline = seekable_timeline();
        let mut state = TimelineUiState::default();

        let interaction = super::map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                pointer_down_on_timeline: true,
                dragged: true,
                pointer_fraction: Some(0.30),
                ..TimelinePointerInput::default()
            },
            true,
        );

        assert_eq!(
            interaction.actions,
            vec![TimelineAction::BeginLiveScrub(MediaTime::from_secs(30))]
        );
    }

    /// Throttled snapshot схлопывает skipped targets, но release exact target проходит всегда.
    #[test]
    fn live_scrub_throttled_snapshot_drops_intermediate_and_release_uses_exact_target() {
        let mut state = TimelineUiState::default();
        let started_at = Instant::now();
        state.begin_live_scrub_dispatch(
            TimelineLiveScrubSettingsSnapshot {
                decode_mode: TimelineLiveScrubDecodeMode::ThrottledLatest,
                max_hz: 60,
            },
            started_at,
            MediaTime::from_secs(10),
        );

        let too_soon = started_at + live_scrub_min_dispatch_period(60) / 2;
        assert_eq!(
            state.live_scrub_preview_dispatch_target(too_soon, MediaTime::from_secs(20)),
            None
        );
        assert_eq!(
            state
                .live_scrub_diagnostics()
                .expect("active live scrub diagnostics")
                .throttled_latest_skip_count,
            1
        );

        let on_time = started_at + live_scrub_min_dispatch_period(60);
        assert_eq!(
            state.live_scrub_preview_dispatch_target(on_time, MediaTime::from_secs(30)),
            Some(MediaTime::from_secs(20))
        );

        assert_eq!(
            state.live_scrub_release_dispatch_target(
                on_time + Duration::from_millis(1),
                MediaTime::from_secs(30),
            ),
            Some(MediaTime::from_secs(30))
        );
    }

    /// Mid-drag settings changes are diagnostics-only and apply to the next gesture.
    #[test]
    fn live_scrub_deferred_settings_change_keeps_pointer_down_policy_and_latest_only_record() {
        let mut state = TimelineUiState::default();
        let started_at = Instant::now();
        let pointer_down_snapshot = TimelineLiveScrubSettingsSnapshot {
            decode_mode: TimelineLiveScrubDecodeMode::ThrottledLatest,
            max_hz: 60,
        };
        let changed_snapshot = TimelineLiveScrubSettingsSnapshot {
            decode_mode: TimelineLiveScrubDecodeMode::EveryDragEvent,
            max_hz: 120,
        };
        let latest_changed_snapshot = TimelineLiveScrubSettingsSnapshot {
            decode_mode: TimelineLiveScrubDecodeMode::ThrottledLatest,
            max_hz: 30,
        };

        state.begin_live_scrub_dispatch(
            pointer_down_snapshot,
            started_at,
            MediaTime::from_secs(10),
        );
        state.defer_live_scrub_settings_change(changed_snapshot);
        state.defer_live_scrub_settings_change(latest_changed_snapshot);

        let too_soon = started_at + live_scrub_min_dispatch_period(60) / 2;
        assert_eq!(
            state.live_scrub_preview_dispatch_target(too_soon, MediaTime::from_secs(20)),
            None
        );

        let diagnostics = state
            .live_scrub_diagnostics()
            .expect("active live scrub diagnostics");
        assert_eq!(diagnostics.settings_snapshot, pointer_down_snapshot);
        assert_eq!(diagnostics.deferred_live_scrub_settings_change_count, 2);
        assert_eq!(
            diagnostics.latest_deferred_live_scrub_settings_change,
            Some(DeferredLiveScrubSettingsChange {
                old_snapshot: changed_snapshot,
                new_snapshot: latest_changed_snapshot,
            })
        );
        assert_eq!(diagnostics.throttled_latest_skip_count, 1);
    }

    /// Every-drag mode attempts each target and does not report throttle skips.
    #[test]
    fn live_scrub_every_drag_event_dispatches_each_target_without_throttle_skip() {
        let mut state = TimelineUiState::default();
        let started_at = Instant::now();
        state.begin_live_scrub_dispatch(
            TimelineLiveScrubSettingsSnapshot {
                decode_mode: TimelineLiveScrubDecodeMode::EveryDragEvent,
                max_hz: 1,
            },
            started_at,
            MediaTime::from_secs(10),
        );

        assert_eq!(
            state.live_scrub_preview_dispatch_target(started_at, MediaTime::from_secs(20)),
            Some(MediaTime::from_secs(20))
        );
        assert_eq!(
            state.live_scrub_preview_dispatch_target(started_at, MediaTime::from_secs(30)),
            Some(MediaTime::from_secs(30))
        );
        assert_eq!(
            state
                .live_scrub_diagnostics()
                .expect("active live scrub diagnostics")
                .throttled_latest_skip_count,
            0
        );
    }

    /// Проверяет, что disabled timeline не отправляет seek commands.
    #[test]
    fn disabled_timeline_does_not_emit_seek_commands() {
        let timeline = TimelineSnapshot::default();
        let mut state = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(10)),
            ..TimelineUiState::default()
        };

        let interaction = map_timeline_interaction(
            &timeline,
            &mut state,
            None,
            TimelinePointerInput {
                clicked: true,
                dragged: true,
                drag_stopped: true,
                pointer_fraction: Some(0.50),
                ..TimelinePointerInput::default()
            },
        );

        assert!(interaction.actions.is_empty());
        assert!(!state.has_active_drag());
    }

    /// Проверяет, что focus loss commit-ит последнюю локальную drag-позицию.
    #[test]
    fn focus_loss_commits_active_drag_as_single_seek() {
        let timeline = seekable_timeline();
        let mut state = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(40)),
            ..TimelineUiState::default()
        };

        let interaction = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                lost_focus: true,
                ..TimelinePointerInput::default()
            },
        );

        assert_eq!(
            interaction.actions,
            vec![TimelineAction::CommitDragSeek(MediaTime::from_secs(40))]
        );
        assert!(!state.has_active_drag());
    }

    /// Проверяет, что focus loss и drag stop одного frame-а дают ровно один seek.
    #[test]
    fn focus_loss_with_drag_stop_commits_final_pointer_once() {
        let timeline = seekable_timeline();
        let mut state = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(40)),
            ..TimelineUiState::default()
        };

        let interaction = map_timeline_interaction(
            &timeline,
            &mut state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                drag_stopped: true,
                lost_focus: true,
                pointer_fraction: Some(0.90),
                ..TimelinePointerInput::default()
            },
        );

        assert_eq!(
            interaction.actions,
            vec![TimelineAction::CommitDragSeek(MediaTime::from_secs(90))]
        );
        assert!(!state.has_active_drag());
    }

    /// Проверяет, что UI state хранит только transient pointer drag value.
    #[test]
    fn timeline_ui_state_stores_only_transient_pointer_drag_value() {
        let mut state = TimelineUiState::default();

        assert!(!state.has_active_drag());
        state.transient_drag_position = Some(MediaTime::from_secs(5));
        assert!(state.has_active_drag());
        state.clear_transient_drag();
        assert_eq!(state, TimelineUiState::default());
    }

    /// Проверяет порядок выбора отображаемой позиции: drag -> target -> current.
    #[test]
    fn display_position_prefers_transient_drag_then_target_then_current() {
        let mut timeline = seekable_timeline();
        timeline.current_position = MediaTime::from_secs(5);
        timeline.target_position = Some(MediaTime::from_secs(30));

        let mut state = TimelineUiState::default();
        assert_eq!(state.display_position(&timeline), MediaTime::from_secs(30));

        state.transient_drag_position = Some(MediaTime::from_secs(70));
        assert_eq!(state.display_position(&timeline), MediaTime::from_secs(70));

        state.clear_transient_drag();
        timeline.target_position = None;
        assert_eq!(state.display_position(&timeline), MediaTime::from_secs(5));
    }

    /// Проверяет формат времени для пустого, короткого и длинного media.
    #[test]
    fn time_formatter_uses_player_style_formats() {
        assert_eq!(format_seconds(None), "--:--");
        assert_eq!(format_seconds(Some(f64::NAN)), "--:--");
        assert_eq!(format_seconds(Some(65.9)), "01:05");
        assert_eq!(format_seconds(Some(3_661.0)), "01:01:01");
    }
}
