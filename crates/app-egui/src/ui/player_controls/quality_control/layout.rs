//! Чистая раскладка кнопки качества в нижней строке controls.
//!
//! Строка: `Open … Shuffle — Prev (Play) [rate] Next — Repeat  [качество] [♪ ▾] [CC ▾] [Fullscreen]`.
//! Кнопка стоит сразу левее того, что стоит левее Fullscreen (♪, CC или сам Fullscreen), с
//! обычным зазором соседей. Появляясь, она двигает только Shuffle/Repeat: они сжимают
//! расстояние ровно настолько, сколько вместе занимают CC, «♪» и она сама.
//!
//! # Режимы
//! 1. [`QualityControlMode::Shown`] — кнопка с подписью.
//! 2. [`QualityControlMode::Hidden`] — места нет: кнопки нет.
//!
//! # Приоритет
//! CC и «♪» выбирают режим, не оглядываясь на кнопку качества; она занимает только то, что
//! осталось, — поэтому при сужении окна исчезает первой.

use egui::{Rect, pos2, vec2};

use super::super::queue_mode_controls::{QueueModeControlLayout, TrailingReservedWidth};
use super::super::subtitle_control::{APPEAR_SLIDE_DISTANCE, SubtitleLayoutGeometry};

/// Ширина кнопки относительно `subtitles_button_size` скина: подпись «1080p» шире значка CC.
/// Ширина фиксирована (не зависит от текста), чтобы строка не прыгала при смене подписи.
const BUTTON_WIDTH_TO_SUBTITLES_BUTTON: f32 = 1.75;

/// Что кнопке качества нужно знать о соседях справа, чтобы встать левее и не наехать на них.
#[derive(Debug, Clone, Copy)]
pub(in crate::ui::player_controls) struct QualityNeighbor {
    /// Геометрия строки (общая для всех кнопок цепочки).
    geometry: SubtitleLayoutGeometry,
    /// Сколько места справа заняли CC и «♪» на этом кадре (с учётом их анимаций).
    reserve_now: TrailingReservedWidth,
    /// Сколько места они займут при полном появлении (то, что видно при целевых режимах).
    reserve_fully_shown: TrailingReservedWidth,
}

impl QualityNeighbor {
    /// Соседи справа: `reserve_now` — занято сейчас, `reserve_fully_shown` — будет занято
    /// после завершения анимаций.
    pub(in crate::ui::player_controls) fn new(
        geometry: SubtitleLayoutGeometry,
        reserve_now: TrailingReservedWidth,
        reserve_fully_shown: TrailingReservedWidth,
    ) -> Self {
        Self {
            geometry,
            reserve_now,
            reserve_fully_shown,
        }
    }

    /// Геометрия строки.
    pub(in crate::ui::player_controls) const fn geometry(&self) -> &SubtitleLayoutGeometry {
        &self.geometry
    }
}

/// Как показывается кнопка качества при данной ширине строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui::player_controls) enum QualityControlMode {
    /// Кнопка с подписью.
    Shown,
    /// Места нет: кнопка не показывается.
    Hidden,
}

/// Итоговая раскладка одного кадра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::ui::player_controls) struct QualityControlLayout {
    /// Режим, выбранный по ширине.
    pub(in crate::ui::player_controls) mode: QualityControlMode,
    /// Hit-area кнопки (`None` — не показывается в этом кадре).
    pub(in crate::ui::player_controls) button_rect: Option<Rect>,
    /// Shuffle/Repeat с учётом места, занятого CC, «♪» и кнопкой качества.
    pub(in crate::ui::player_controls) queue_mode: QueueModeControlLayout,
}

/// Ширина кнопки (без зазоров).
fn button_width(geometry: &SubtitleLayoutGeometry) -> f32 {
    geometry.controls_style.subtitles_button_size * BUTTON_WIDTH_TO_SUBTITLES_BUTTON
}

/// Резерв кнопки между Repeat и соседями справа: сама кнопка и её дальний зазор.
fn own_reserve(
    geometry: &SubtitleLayoutGeometry,
    mode: QualityControlMode,
    reveal_progress: f32,
) -> TrailingReservedWidth {
    if mode == QualityControlMode::Hidden {
        return TrailingReservedWidth::NONE;
    }
    let progress = reveal_progress.clamp(0.0, 1.0);
    TrailingReservedWidth::of_width(progress * (button_width(geometry) + geometry.neighbor_gap()))
}

/// Помещается ли кнопка при полном появлении вместе с соседями справа.
fn fits(neighbor: &QualityNeighbor) -> bool {
    let geometry = &neighbor.geometry;
    let reserve =
        neighbor
            .reserve_fully_shown
            .plus(own_reserve(geometry, QualityControlMode::Shown, 1.0));
    let queue_mode = geometry.queue_mode_with_reserve(reserve);
    // Next в самом тесном положении должен остаться на положенном расстоянии от Repeat.
    geometry.base_next_button_rect.right() + geometry.neighbor_gap()
        <= queue_mode.repeat_rect.left()
}

/// Выбирает режим по ширине строки.
pub(in crate::ui::player_controls) fn choose_mode(
    neighbor: &QualityNeighbor,
) -> QualityControlMode {
    if fits(neighbor) {
        QualityControlMode::Shown
    } else {
        QualityControlMode::Hidden
    }
}

/// Раскладка кадра: `reveal_progress` (`0.0..=1.0`) — анимация появления/скрытия кнопки.
///
/// При `reveal_progress == 0` Shuffle/Repeat совпадают с раскладкой без кнопки качества.
pub(in crate::ui::player_controls) fn layout(
    neighbor: &QualityNeighbor,
    mode: QualityControlMode,
    reveal_progress: f32,
) -> QualityControlLayout {
    let progress = reveal_progress.clamp(0.0, 1.0);
    let geometry = &neighbor.geometry;
    let queue_mode = geometry.queue_mode_with_reserve(
        neighbor
            .reserve_now
            .plus(own_reserve(geometry, mode, progress)),
    );
    if mode == QualityControlMode::Hidden || progress <= 0.0 {
        return QualityControlLayout {
            mode,
            button_rect: None,
            queue_mode,
        };
    }
    // Правый край — на зазор левее того, что стоит справа (♪/CC с их зазором или Fullscreen).
    // Slide сдвигает кнопку вправо только на время появления.
    let slide = (1.0 - progress) * APPEAR_SLIDE_DISTANCE;
    let right_edge = geometry.fullscreen_button_rect.left()
        - geometry.neighbor_gap()
        - neighbor.reserve_now.width()
        + slide;
    let height = geometry.controls_style.subtitles_button_size;
    let top = geometry.fullscreen_button_rect.center().y - height * 0.5;
    let width = button_width(geometry);
    QualityControlLayout {
        mode,
        button_rect: Some(Rect::from_min_size(
            pos2(right_edge - width, top),
            vec2(width, height),
        )),
        queue_mode,
    }
}
