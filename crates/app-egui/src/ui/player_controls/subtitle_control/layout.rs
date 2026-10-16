//! Чистая раскладка кнопки субтитров (CC) в нижней строке controls.
//!
//! Строка: `Open … Shuffle — Prev (Play) [rate] Next — Repeat  [CC ▾]  Fullscreen`.
//! CC стоит между Repeat и Fullscreen и прижата к Fullscreen. Появляясь, она не двигает
//! никого, кроме Shuffle/Repeat: они остаются симметричными относительно Play и сжимают
//! расстояние `d` ровно настолько, насколько CC занимает места.
//!
//! # Режимы
//! 1. [`SubtitleControlMode::Full`] — CC и отдельная стрелка меню «▾».
//! 2. [`SubtitleControlMode::Compact`] — только CC; меню открывается правым кликом.
//! 3. [`SubtitleControlMode::Hidden`] — кнопки нет (горячие клавиши работают).
//!
//! Режим выбирается по ширине, а не по анимации: «помещается» значит, что при полностью
//! показанной CC расстояние `d` не меньше минимума, при котором Next (с закрытой кнопкой
//! скорости) всё ещё отстоит от Repeat на положенные 12 pt. Проверка использует ту же
//! функцию раскладки Shuffle/Repeat, что и отрисовка, поэтому формулы не расходятся.

use egui::{Rect, pos2, vec2};

use super::super::queue_mode_controls::{
    QueueModeControlLayout, TrailingReservedWidth, control_layout_reserving_trailing_space,
};
use crate::ui::skin::ControlsStyle;

/// Насколько CC «выезжает» вбок при появлении (points): лёгкий slide вместе с fade.
pub(in crate::ui::player_controls) const APPEAR_SLIDE_DISTANCE: f32 = 6.0;

/// Как показывается кнопка субтитров при данной ширине строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui::player_controls) enum SubtitleControlMode {
    /// CC и отдельная стрелка меню.
    Full,
    /// Только CC: клик переключает, правый клик открывает меню.
    Compact,
    /// Места нет: кнопка не показывается.
    Hidden,
}

/// Есть ли у видео дорожки субтитров (решение владельца: нет дорожек — нет кнопки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui::player_controls) enum SubtitleControlAvailability {
    /// У видео нет дорожек: CC не показывается совсем.
    NoTracks,
    /// Дорожки есть: CC показывается, если хватает места.
    HasTracks,
}

/// Статическая геометрия строки, от которой зависит раскладка.
#[derive(Debug, Clone, Copy)]
pub(in crate::ui::player_controls) struct SubtitleLayoutGeometry {
    /// Rect Play/Pause (центр строки).
    pub(in crate::ui::player_controls) playback_button_rect: Rect,
    /// Next в закрытом состоянии кнопки скорости (самое тесное положение).
    pub(in crate::ui::player_controls) base_next_button_rect: Rect,
    /// Open у левого края.
    pub(in crate::ui::player_controls) open_file_button_rect: Rect,
    /// Fullscreen у правого края.
    pub(in crate::ui::player_controls) fullscreen_button_rect: Rect,
    /// Токены размеров skin-а.
    pub(in crate::ui::player_controls) controls_style: ControlsStyle,
    /// Общий горизонтальный spacing egui.
    pub(in crate::ui::player_controls) item_spacing: f32,
}

/// Итоговая раскладка одного кадра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::ui::player_controls) struct SubtitleControlLayout {
    /// Режим, выбранный по ширине.
    pub(in crate::ui::player_controls) mode: SubtitleControlMode,
    /// Hit-area кнопки CC (`None` — не показывается в этом кадре).
    pub(in crate::ui::player_controls) button_rect: Option<Rect>,
    /// Hit-area стрелки меню (только в режиме `Full`).
    pub(in crate::ui::player_controls) arrow_rect: Option<Rect>,
    /// Shuffle/Repeat с учётом места, занятого CC.
    pub(in crate::ui::player_controls) queue_mode: QueueModeControlLayout,
}

impl SubtitleLayoutGeometry {
    /// Зазор между соседними controls: не меньше общего spacing и skin-овых 12 pt.
    pub(in crate::ui::player_controls) fn neighbor_gap(&self) -> f32 {
        self.item_spacing
            .max(self.controls_style.queue_mode_neighbor_gap)
    }

    /// Ширина, которую занимает CC в режиме (без зазоров).
    pub(in crate::ui::player_controls) fn footprint_width(&self, mode: SubtitleControlMode) -> f32 {
        let style = self.controls_style;
        match mode {
            SubtitleControlMode::Full => {
                style.subtitles_button_size + style.subtitles_menu_arrow_width
            }
            SubtitleControlMode::Compact => style.subtitles_button_size,
            SubtitleControlMode::Hidden => 0.0,
        }
    }

    /// Резерв между Repeat и Fullscreen: сама CC и её дальний зазор до Fullscreen.
    /// Ближний зазор до Repeat добавляет раскладка Shuffle/Repeat.
    pub(in crate::ui::player_controls) fn reserve_for(
        &self,
        mode: SubtitleControlMode,
        reveal_progress: f32,
    ) -> TrailingReservedWidth {
        if mode == SubtitleControlMode::Hidden {
            return TrailingReservedWidth::NONE;
        }
        let progress = reveal_progress.clamp(0.0, 1.0);
        TrailingReservedWidth::of_width(
            progress * (self.footprint_width(mode) + self.neighbor_gap()),
        )
    }

    /// Раскладка Shuffle/Repeat при данном резерве.
    pub(in crate::ui::player_controls) fn queue_mode_with_reserve(
        &self,
        reserve: TrailingReservedWidth,
    ) -> QueueModeControlLayout {
        control_layout_reserving_trailing_space(
            self.playback_button_rect,
            self.open_file_button_rect,
            self.fullscreen_button_rect,
            self.controls_style,
            self.item_spacing,
            reserve,
        )
    }

    /// Помещается ли CC в режиме `mode` при полном появлении.
    fn fits(&self, mode: SubtitleControlMode) -> bool {
        let queue_mode = self.queue_mode_with_reserve(self.reserve_for(mode, 1.0));
        // Next в самом тесном положении должен остаться на положенном расстоянии от Repeat.
        self.base_next_button_rect.right() + self.neighbor_gap() <= queue_mode.repeat_rect.left()
    }
}

/// Выбирает режим по ширине строки: самый богатый из тех, что помещаются.
pub(in crate::ui::player_controls) fn choose_mode(
    geometry: &SubtitleLayoutGeometry,
) -> SubtitleControlMode {
    [SubtitleControlMode::Full, SubtitleControlMode::Compact]
        .into_iter()
        .find(|mode| geometry.fits(*mode))
        .unwrap_or(SubtitleControlMode::Hidden)
}

/// Раскладка кадра: `reveal_progress` (`0.0..=1.0`) — анимация появления/скрытия CC.
///
/// Тот же progress двигает Shuffle/Repeat и проявляет CC, поэтому ничто не прыгает. При
/// `reveal_progress == 0` результат совпадает с раскладкой до появления CC.
pub(in crate::ui::player_controls) fn layout(
    geometry: &SubtitleLayoutGeometry,
    mode: SubtitleControlMode,
    reveal_progress: f32,
) -> SubtitleControlLayout {
    let progress = reveal_progress.clamp(0.0, 1.0);
    let queue_mode = geometry.queue_mode_with_reserve(geometry.reserve_for(mode, progress));
    if mode == SubtitleControlMode::Hidden || progress <= 0.0 {
        return SubtitleControlLayout {
            mode,
            button_rect: None,
            arrow_rect: None,
            queue_mode,
        };
    }

    let style = geometry.controls_style;
    // Конечное положение: правый край футпринта на зазор левее Fullscreen. Slide сдвигает
    // кнопку к Fullscreen только на время появления.
    let slide = (1.0 - progress) * APPEAR_SLIDE_DISTANCE;
    let footprint_right = geometry.fullscreen_button_rect.left() - geometry.neighbor_gap() + slide;
    let center_y = geometry.fullscreen_button_rect.center().y;
    let button_height = style.subtitles_button_size;
    let (button_rect, arrow_rect) = match mode {
        SubtitleControlMode::Full => {
            let arrow_left = footprint_right - style.subtitles_menu_arrow_width;
            let button_left = arrow_left - style.subtitles_button_size;
            (
                Rect::from_min_size(
                    pos2(button_left, center_y - button_height * 0.5),
                    vec2(style.subtitles_button_size, button_height),
                ),
                Some(Rect::from_min_size(
                    pos2(arrow_left, center_y - button_height * 0.5),
                    vec2(style.subtitles_menu_arrow_width, button_height),
                )),
            )
        }
        SubtitleControlMode::Compact | SubtitleControlMode::Hidden => (
            Rect::from_min_size(
                pos2(
                    footprint_right - style.subtitles_button_size,
                    center_y - button_height * 0.5,
                ),
                vec2(style.subtitles_button_size, button_height),
            ),
            None,
        ),
    };
    SubtitleControlLayout {
        mode,
        button_rect: Some(button_rect),
        arrow_rect,
        queue_mode,
    }
}

/// Раскладка Shuffle/Repeat без CC: ровно то, что было до появления кнопки.
#[cfg(test)]
pub(in crate::ui::player_controls) fn layout_without_subtitles(
    geometry: &SubtitleLayoutGeometry,
) -> QueueModeControlLayout {
    super::super::queue_mode_controls::control_layout(
        geometry.playback_button_rect,
        geometry.open_file_button_rect,
        geometry.fullscreen_button_rect,
        geometry.controls_style,
        geometry.item_spacing,
    )
}
