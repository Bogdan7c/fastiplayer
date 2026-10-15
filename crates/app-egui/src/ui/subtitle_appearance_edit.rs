//! Правка внешнего вида субтитров прямо на видео: поверхность жестов и панель настроек.
//!
//! # Кто чем владеет
//! - Состояние сессии (черновик, образец, «до»), позиция слоя и применение - у `SubtitleOwner`
//!   (`state/subtitles/owner_appearance_edit.rs`). Этот модуль только читает снимок
//!   [`AppearanceEditView`] и отдаёт типизированные намерения
//!   [`SubtitleAppearanceEditAction`]; применяет их `AppState` ПОСЛЕ прохода egui.
//! - Рисование рамки и маркеров - `ui-artwork-egui::ArtworkPainter::subtitle_edit_frame`
//!   (прямых примитивов Painter здесь нет).
//!
//! # Владение вводом (почему здесь нет `video_surface_input`)
//! Пока режим открыт, вместо `video_surface_input::show` вызывается [`show`] этого модуля
//! (выбор делает `ui::video_area_input`). Поэтому по определению: двойной клик по видео не
//! переключает полноэкранный режим, а колесо не меняет громкость - колесо теперь меняет размер
//! текста. Остальные клавиши (Space, C/V/Z/X, стрелки и т. д.) идут обычными путями: режим
//! работает вживую во время воспроизведения (решение владельца).
//!
//! # Фокус клавиатуры и Esc
//! Esc обрабатывает оболочка (`app_shell::escape_dismissal`), но только когда у egui нет
//! сфокусированного виджета: при фокусе на виджете первый Esc egui тратит на снятие фокуса.
//! Корневая причина «Esc не отменяет» - фокус, оставшийся на виджете панели после мыши,
//! поэтому: поверхность жестов вообще не фокусируема (`Sense::CLICK`/`Sense::DRAG` без
//! `FOCUSABLE`), а кнопки, слайдер и кнопки цвета после действия мышью отдают фокус
//! (`surrender_focus`, как остальные кнопки плеера). Клавиатурный (Tab) фокус сохраняется.

mod action;
mod gesture;
mod panel;
mod style;
mod surface;
#[cfg(test)]
mod tests;

pub(crate) use action::SubtitleAppearanceEditAction;
pub(crate) use style::SubtitleAppearanceEditStyle;

use egui::{Rect, Ui};

use crate::state::AppearanceEditView;

/// Всё, что нужно показать за один кадр режима правки.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SubtitleAppearanceEditInput<'a> {
    /// Снимок сессии: черновик и положение слоя на экране.
    pub(crate) view: &'a AppearanceEditView,
    /// Свободная область видео (та же, что у `video_surface_input`), точки egui.
    pub(crate) video_rect: Rect,
    /// Токены внешнего вида от skin-а.
    pub(crate) style: &'a SubtitleAppearanceEditStyle,
}

/// Рисует поверхность жестов с рамкой и панель; возвращает намерения кадра.
///
/// Вызывать вместо `video_surface_input::show`, после всех панелей кадра и до оверлеев.
#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    input: SubtitleAppearanceEditInput<'_>,
) -> Vec<SubtitleAppearanceEditAction> {
    let mut actions = surface::show(ui, input);
    actions.extend(panel::show(ui.ctx(), input));
    actions
}
