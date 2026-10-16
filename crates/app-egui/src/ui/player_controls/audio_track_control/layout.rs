//! Чистая раскладка кнопки аудиодорожек «♪» в нижней строке controls.
//!
//! Строка: `Open … Shuffle — Prev (Play) [rate] Next — Repeat  [♪ ▾] [CC ▾] [Fullscreen]`.
//! «♪» стоит сразу левее CC (а если у media нет субтитров — левее Fullscreen) с обычным
//! зазором соседей. Появляясь, она двигает только Shuffle/Repeat: они сжимают расстояние `d`
//! ровно настолько, сколько вместе занимают CC и «♪».
//!
//! # Режимы
//! 1. [`AudioTrackControlMode::Full`] — «♪» и отдельная стрелка меню «▾».
//! 2. [`AudioTrackControlMode::Compact`] — только «♪»; клик по ней открывает меню.
//! 3. [`AudioTrackControlMode::Hidden`] — кнопки нет (клавиша `B` работает).
//!
//! # Приоритет CC
//! Режим CC уже выбран без оглядки на «♪» и остаётся как есть. Режим «♪» — самый богатый из
//! тех, что помещаются рядом с CC в её режиме: так при сужении окна «♪» исчезает раньше CC.
//! Проверка «помещается» использует ту же функцию раскладки Shuffle/Repeat, что и отрисовка,
//! поэтому формулы не расходятся.

use egui::{Rect, pos2, vec2};

use super::super::queue_mode_controls::{QueueModeControlLayout, TrailingReservedWidth};
use super::super::subtitle_control::{APPEAR_SLIDE_DISTANCE, SubtitleNeighbor};

/// Как показывается кнопка аудиодорожек при данной ширине строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui::player_controls) enum AudioTrackControlMode {
    /// «♪» и отдельная стрелка меню.
    Full,
    /// Только «♪»: клик открывает меню.
    Compact,
    /// Места нет: кнопка не показывается.
    Hidden,
}

/// Итоговая раскладка одного кадра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::ui::player_controls) struct AudioTrackControlLayout {
    /// Режим, выбранный по ширине.
    pub(in crate::ui::player_controls) mode: AudioTrackControlMode,
    /// Hit-area кнопки «♪» (`None` — не показывается в этом кадре).
    pub(in crate::ui::player_controls) button_rect: Option<Rect>,
    /// Hit-area стрелки меню (только в режиме `Full`).
    pub(in crate::ui::player_controls) arrow_rect: Option<Rect>,
    /// Shuffle/Repeat с учётом места, занятого CC и «♪».
    pub(in crate::ui::player_controls) queue_mode: QueueModeControlLayout,
}

/// Ширина «♪» в режиме (без зазоров).
fn footprint_width(neighbor: &SubtitleNeighbor, mode: AudioTrackControlMode) -> f32 {
    let style = neighbor.geometry.controls_style;
    match mode {
        AudioTrackControlMode::Full => {
            style.subtitles_button_size + style.subtitles_menu_arrow_width
        }
        AudioTrackControlMode::Compact => style.subtitles_button_size,
        AudioTrackControlMode::Hidden => 0.0,
    }
}

/// Резерв «♪» между Repeat и CC: сама кнопка и её дальний зазор до CC (или до Fullscreen).
fn audio_reserve(
    neighbor: &SubtitleNeighbor,
    mode: AudioTrackControlMode,
    reveal_progress: f32,
) -> TrailingReservedWidth {
    if mode == AudioTrackControlMode::Hidden {
        return TrailingReservedWidth::NONE;
    }
    let progress = reveal_progress.clamp(0.0, 1.0);
    TrailingReservedWidth::of_width(
        progress * (footprint_width(neighbor, mode) + neighbor.geometry.neighbor_gap()),
    )
}

/// Резерв CC на кадре при заданном прогрессе её появления.
fn subtitle_reserve(neighbor: &SubtitleNeighbor, reveal_progress: f32) -> TrailingReservedWidth {
    neighbor
        .geometry
        .reserve_for(neighbor.mode, reveal_progress)
}

/// Резерв CC при её полном появлении; ноль, если CC не будет показана.
fn subtitle_reserve_when_fully_shown(neighbor: &SubtitleNeighbor) -> TrailingReservedWidth {
    if neighbor.target_visible {
        subtitle_reserve(neighbor, 1.0)
    } else {
        TrailingReservedWidth::NONE
    }
}

/// Помещается ли «♪» в режиме `mode` при полном появлении рядом с CC.
fn fits(neighbor: &SubtitleNeighbor, mode: AudioTrackControlMode) -> bool {
    let reserve =
        subtitle_reserve_when_fully_shown(neighbor).plus(audio_reserve(neighbor, mode, 1.0));
    let queue_mode = neighbor.geometry.queue_mode_with_reserve(reserve);
    // Next в самом тесном положении должен остаться на положенном расстоянии от Repeat.
    neighbor.geometry.base_next_button_rect.right() + neighbor.geometry.neighbor_gap()
        <= queue_mode.repeat_rect.left()
}

/// Выбирает режим по ширине строки: самый богатый из тех, что помещаются рядом с CC.
pub(in crate::ui::player_controls) fn choose_mode(
    neighbor: &SubtitleNeighbor,
) -> AudioTrackControlMode {
    [AudioTrackControlMode::Full, AudioTrackControlMode::Compact]
        .into_iter()
        .find(|mode| fits(neighbor, *mode))
        .unwrap_or(AudioTrackControlMode::Hidden)
}

/// Раскладка кадра: `reveal_progress` (`0.0..=1.0`) — анимация появления/скрытия «♪».
///
/// При `reveal_progress == 0` результат Shuffle/Repeat совпадает с раскладкой, в которой
/// есть только CC (то есть с тем, что было до появления «♪»).
pub(in crate::ui::player_controls) fn layout(
    neighbor: &SubtitleNeighbor,
    mode: AudioTrackControlMode,
    reveal_progress: f32,
) -> AudioTrackControlLayout {
    let progress = reveal_progress.clamp(0.0, 1.0);
    let geometry = &neighbor.geometry;
    let subtitle_reserve_now = subtitle_reserve(neighbor, neighbor.reveal_progress);
    let queue_mode = geometry.queue_mode_with_reserve(
        subtitle_reserve_now.plus(audio_reserve(neighbor, mode, progress)),
    );
    if mode == AudioTrackControlMode::Hidden || progress <= 0.0 {
        return AudioTrackControlLayout {
            mode,
            button_rect: None,
            arrow_rect: None,
            queue_mode,
        };
    }

    let style = geometry.controls_style;
    // Правый край «♪» — на зазор левее того, что стоит справа (CC с её зазором или Fullscreen).
    // Slide сдвигает кнопку вправо только на время появления.
    let slide = (1.0 - progress) * APPEAR_SLIDE_DISTANCE;
    let footprint_right = geometry.fullscreen_button_rect.left()
        - geometry.neighbor_gap()
        - subtitle_reserve_now.width()
        + slide;
    let center_y = geometry.fullscreen_button_rect.center().y;
    let button_height = style.subtitles_button_size;
    let top = center_y - button_height * 0.5;
    let (button_rect, arrow_rect) = match mode {
        AudioTrackControlMode::Full => {
            let arrow_left = footprint_right - style.subtitles_menu_arrow_width;
            let button_left = arrow_left - style.subtitles_button_size;
            (
                Rect::from_min_size(
                    pos2(button_left, top),
                    vec2(style.subtitles_button_size, button_height),
                ),
                Some(Rect::from_min_size(
                    pos2(arrow_left, top),
                    vec2(style.subtitles_menu_arrow_width, button_height),
                )),
            )
        }
        AudioTrackControlMode::Compact | AudioTrackControlMode::Hidden => (
            Rect::from_min_size(
                pos2(footprint_right - style.subtitles_button_size, top),
                vec2(style.subtitles_button_size, button_height),
            ),
            None,
        ),
    };
    AudioTrackControlLayout {
        mode,
        button_rect: Some(button_rect),
        arrow_rect,
        queue_mode,
    }
}
