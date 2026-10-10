//! Подсказка со временем под курсором на таймлайне (UX edge cases 2, B3).
//!
//! Показывает время того места, куда попадёт перемотка по клику, в том же формате,
//! что и таймер (`MM:SS` / `HH:MM:SS`). Это только подсказка: перемотку, drag и
//! live-scrub она не трогает и состояния не хранит.
//!
//! Честность: для live-потока позиция в скользящем DVR-окне ничего не говорит о
//! времени «с начала», а для неперематываемого таймлайна клик ничего не делает —
//! в обоих случаях подсказки нет.

use egui::{Response, Tooltip};
use media_core::{TimelineMode, TimelineSnapshot};

use super::geometry::{TimelineBounds, format_media_time};

/// Зазор между курсором и подсказкой, чтобы она не закрывала место клика.
const TOOLTIP_GAP_FROM_POINTER: f32 = 12.0;

/// Текст подсказки для позиции курсора `pointer_fraction` (0.0..=1.0 по дорожке).
///
/// `None`, если подсказывать нечего: таймлайн не перематывается, либо это live.
#[must_use]
pub(super) fn hover_time_text(
    timeline: &TimelineSnapshot,
    bounds: Option<TimelineBounds>,
    pointer_fraction: f64,
) -> Option<String> {
    if timeline.mode == TimelineMode::Live {
        return None;
    }
    let bounds = bounds?;
    Some(format_media_time(Some(
        bounds.position_from_fraction(pointer_fraction),
    )))
}

/// Показывает подсказку рядом с курсором, пока он над таймлайном и кнопка не зажата.
///
/// Во время drag время уже видно в таймере и на дорожке, поэтому подсказка скрыта.
/// Задержки egui (`tooltip_delay`) здесь намеренно нет: `Tooltip::for_widget`
/// открыт, пока вызывается.
pub(super) fn show(response: &Response, hover_text: Option<String>) {
    let Some(hover_text) = hover_text else {
        return;
    };
    if !response.hovered() || response.dragged() {
        return;
    }
    Tooltip::for_widget(response)
        .at_pointer()
        .gap(TOOLTIP_GAP_FROM_POINTER)
        .show(|ui| {
            ui.monospace(hover_text);
        });
}

#[cfg(test)]
mod tests;
