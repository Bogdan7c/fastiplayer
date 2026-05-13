//! Timeline UI и чистый mapper pointer events -> user actions.

use std::time::Duration;

use egui::{Color32, Pos2, Rect, Response, Sense, StrokeKind, Ui, Vec2};
use media_core::{MediaDuration, MediaTime, TimelineRange, TimelineSnapshot};

use crate::ui::skin::{PlayerSkin, TimelineStyle};

/// Transient UI-состояние timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TimelineUiState {
    /// Последняя позиция pointer drag, ещё не обязанная попасть в worker snapshot.
    transient_drag_position: Option<MediaTime>,
}

impl TimelineUiState {
    /// Возвращает `true`, если UI сейчас ведёт pointer scrub.
    #[must_use]
    pub const fn has_active_drag(&self) -> bool {
        self.transient_drag_position.is_some()
    }

    /// Сбрасывает transient pointer state после завершения scrub.
    pub fn clear_transient_drag(&mut self) {
        self.transient_drag_position = None;
    }

    /// Возвращает позицию, которую нужно показывать во время live scrub.
    #[must_use]
    pub fn display_position(&self, timeline: &TimelineSnapshot) -> MediaTime {
        self.transient_drag_position
            .or(timeline.target_position)
            .unwrap_or(timeline.current_position)
    }
}

/// Действие timeline, которое `AppState` конвертирует в `PlayerCommand`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineAction {
    /// Начало interactive scrub.
    BeginScrub,

    /// Обновление целевой позиции interactive scrub.
    UpdateScrub(MediaTime),

    /// Завершение scrub с commit latest policy.
    EndScrubCommitLatest,
}

/// Нормализованный input mapper-а без зависимости от `egui::Response` в тестах.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TimelinePointerInput {
    /// Click был завершён на timeline.
    pub clicked: bool,

    /// Pointer удерживается на timeline; это срабатывает раньше, чем egui подтвердит drag.
    pub pointer_down_on_timeline: bool,

    /// Drag начался на timeline.
    pub drag_started: bool,

    /// Pointer перемещается во время drag.
    pub dragged: bool,

    /// Drag завершился.
    pub drag_stopped: bool,

    /// Widget потерял focus во время scrub.
    pub lost_focus: bool,

    /// Пользователь нажал Escape во время scrub.
    pub escape_pressed: bool,

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
    let pointer_input = pointer_input_from_response(ui, &response, bounds, style);
    let interaction = map_timeline_interaction(timeline, state, bounds, pointer_input);

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
pub fn render_time_labels(ui: &mut Ui, timeline: &TimelineSnapshot, state: &TimelineUiState) {
    let display_position = state.display_position(timeline);
    let duration_text = format_media_duration(timeline.duration);
    let position_text = format_media_time(Some(display_position));

    ui.horizontal(|ui| {
        ui.monospace(position_text);
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
) -> TimelineInteraction {
    let Some(bounds) = bounds else {
        state.clear_transient_drag();
        return TimelineInteraction {
            actions: Vec::new(),
            display_position: timeline.current_position,
        };
    };

    let mut actions = Vec::new();
    let pointer_position = input
        .pointer_fraction
        .map(|fraction| bounds.position_from_fraction(fraction));

    let wants_scrub_position = input.pointer_down_on_timeline
        || input.drag_started
        || input.dragged
        || input.drag_stopped
        || input.clicked;
    let can_begin_scrub = input.pointer_down_on_timeline || input.drag_started || input.clicked;
    let should_begin_scrub =
        !state.has_active_drag() && can_begin_scrub && pointer_position.is_some();

    if should_begin_scrub {
        actions.push(TimelineAction::BeginScrub);
    }

    if wants_scrub_position
        && (should_begin_scrub || state.has_active_drag())
        && let Some(position) = pointer_position
    {
        let previous_position = state.transient_drag_position;
        state.transient_drag_position = Some(position);
        if should_begin_scrub || previous_position != Some(position) {
            actions.push(TimelineAction::UpdateScrub(position));
        }
    }

    let should_finish_scrub = state.has_active_drag()
        && (input.clicked || input.drag_stopped || input.lost_focus || input.escape_pressed);
    if should_finish_scrub {
        actions.push(TimelineAction::EndScrubCommitLatest);
        state.clear_transient_drag();
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
    ui: &Ui,
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
        escape_pressed: ui.input(|input| input.key_pressed(egui::Key::Escape)),
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
    let base_color = if enabled {
        style.track_fill
    } else {
        style.disabled_fill
    };

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
    use media_core::{MediaDuration, MediaTime, TimelineRange, TimelineSnapshot};

    use super::{
        TimelineAction, TimelineBounds, TimelinePointerInput, TimelineUiState, format_seconds,
        map_timeline_interaction,
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

    /// Проверяет, что pointer down сразу запускает preview scrub, а click только коммитит цель.
    #[test]
    fn pointer_down_starts_scrub_and_click_commits_latest_target() {
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
        assert_eq!(
            pointer_down.actions,
            vec![
                TimelineAction::BeginScrub,
                TimelineAction::UpdateScrub(MediaTime::from_secs(25))
            ]
        );
        assert!(state.has_active_drag());

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
        assert!(state.has_active_drag());

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

        assert_eq!(click.actions, vec![TimelineAction::EndScrubCommitLatest]);
        assert!(!state.has_active_drag());
    }

    /// Проверяет fallback для очень быстрого клика, где down/up попали в один UI frame.
    #[test]
    fn click_without_prior_pointer_down_uses_scrub_commit_sequence() {
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
            vec![
                TimelineAction::BeginScrub,
                TimelineAction::UpdateScrub(MediaTime::from_secs(25)),
                TimelineAction::EndScrubCommitLatest
            ]
        );
        assert!(!state.has_active_drag());
    }

    /// Проверяет последовательность drag start, move и end.
    #[test]
    fn drag_maps_to_begin_update_and_end() {
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
            start.actions,
            vec![
                TimelineAction::BeginScrub,
                TimelineAction::UpdateScrub(MediaTime::from_secs(20))
            ]
        );
        assert_eq!(
            update.actions,
            vec![TimelineAction::UpdateScrub(MediaTime::from_secs(70))]
        );
        assert_eq!(end.actions, vec![TimelineAction::EndScrubCommitLatest]);
        assert!(!state.has_active_drag());
    }

    /// Проверяет, что disabled timeline не отправляет seek/scrub commands.
    #[test]
    fn disabled_timeline_does_not_emit_seek_commands() {
        let timeline = TimelineSnapshot::default();
        let mut state = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(10)),
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

    /// Проверяет завершение scrub при focus loss и Escape.
    #[test]
    fn focus_loss_or_escape_ends_active_scrub() {
        let timeline = seekable_timeline();
        let mut focus_state = TimelineUiState {
            transient_drag_position: Some(MediaTime::from_secs(40)),
        };
        let mut escape_state = focus_state.clone();

        let focus_loss = map_timeline_interaction(
            &timeline,
            &mut focus_state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                lost_focus: true,
                ..TimelinePointerInput::default()
            },
        );
        let escape = map_timeline_interaction(
            &timeline,
            &mut escape_state,
            Some(seekable_bounds()),
            TimelinePointerInput {
                escape_pressed: true,
                ..TimelinePointerInput::default()
            },
        );

        assert_eq!(
            focus_loss.actions,
            vec![TimelineAction::EndScrubCommitLatest]
        );
        assert_eq!(escape.actions, vec![TimelineAction::EndScrubCommitLatest]);
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
