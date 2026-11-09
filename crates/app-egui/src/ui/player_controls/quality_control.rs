//! Кнопка качества «1080p» и её меню в нижней строке controls окна браузерного режима
//! (сессия 13c, решения 267, 278).
//!
//! # Владение
//! - Что показать, решает владелец состояния: сюда оно приходит снимком [`QualityMenuView`]
//!   (разрешение, битрейт, кодек, частота кадров, можно ли просить страницу), а намерение
//!   зрителя уходит типизированным [`QualityControlAction`]. Модуль сам ничего не применяет.
//! - `layout` — чистая раскладка и выбор режима по ширине (Shown / Hidden).
//! - `button` — кнопка с подписью-текстом, `menu` — содержимое выпадающего меню.
//! - Анимация появления (250 мс, как у CC и «♪») принадлежит этому модулю; тот же progress
//!   сдвигает Shuffle/Repeat, поэтому ничего не прыгает.
//!
//! # Место в строке
//! `… [качество] [♪ ▾] [CC ▾] [Fullscreen]`: кнопка стоит левее «♪» (а если «♪» нет — левее CC,
//! если и CC нет — левее Fullscreen). Цепочку соседей ведёт `QualityNeighbor`: «♪» собирает
//! его из того, сколько места заняли CC и она сама.
//!
//! # Видимость
//! Кнопки нет вовсе (ни виджета, ни rect-а), пока нечего показывать (нет кадра) и в обычном
//! окне (`WindowViewProfile::offers_page_quality_view`). При нехватке места кнопка скрывается
//! раньше «♪» и CC.

mod button;
mod layout;
mod menu;
#[cfg(test)]
mod tests;

use egui::{Id, Ui};

use super::ControlAction;
use super::queue_mode_controls::QueueModeControlLayout;
use crate::page_variants::PageVariantChoice;
use crate::state::QualityMenuView;
pub(super) use layout::QualityNeighbor;
use layout::{QualityControlLayout, QualityControlMode, choose_mode};

/// Полный переход «появилась/исчезла» занимает 250 мс (как CC и «♪»).
const REVEAL_TRANSITION_SECONDS: f32 = 0.250;
/// Суффикс Id анимации появления: отделяет её от Id виджетов.
const REVEAL_TRANSITION_ID_SUFFIX: &str = "quality_control_reveal";

/// Действие кнопки качества, которое shell применит после egui pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityControlAction {
    /// «Change quality on the page»: показать зрителю вкладку страницы.
    ShowPage,
    /// Выбрать качество из списка плеера страницы (14a): выбор уходит странице.
    SelectPageQuality(PageVariantChoice),
}

/// Подготовленная на кадр раскладка и прогресс появления кнопки качества.
pub(super) struct QualityControlFrame {
    layout: QualityControlLayout,
    reveal_progress: f32,
    neighbor: QualityNeighbor,
}

impl QualityControlFrame {
    /// Выбирает режим по ширине, продвигает анимацию появления и считает раскладку.
    pub(super) fn prepare(
        ui: &Ui,
        neighbor: QualityNeighbor,
        view: &QualityMenuView,
        reduced_motion: bool,
    ) -> Self {
        let mode = choose_mode(&neighbor);
        let target_visible = view.is_shown() && mode != QualityControlMode::Hidden;
        let reveal_progress = reveal_progress(ui, target_visible, reduced_motion);
        Self {
            layout: layout::layout(&neighbor, mode, reveal_progress),
            reveal_progress,
            neighbor,
        }
    }

    /// Раскладка Shuffle/Repeat этого кадра (с учётом места под CC, «♪» и кнопку качества).
    pub(super) fn queue_mode_layout(&self) -> QueueModeControlLayout {
        self.layout.queue_mode
    }

    /// Рисует кнопку (если она видна) и добавляет действия пользователя.
    pub(super) fn render(
        &self,
        ui: &mut Ui,
        view: &QualityMenuView,
        reduced_motion: bool,
        actions: &mut Vec<ControlAction>,
    ) {
        button::render(
            ui,
            &self.layout,
            view,
            self.neighbor.geometry().controls_style,
            button::RevealState {
                progress: self.reveal_progress,
                reduced_motion,
            },
            actions,
        );
    }
}

/// Прогресс появления кнопки; при reduced motion переход мгновенный и без repaint.
fn reveal_progress(ui: &Ui, target_visible: bool, reduced_motion: bool) -> f32 {
    if reduced_motion {
        return if target_visible { 1.0 } else { 0.0 };
    }
    let transition_id: Id = ui.make_persistent_id(REVEAL_TRANSITION_ID_SUFFIX);
    ui.ctx().animate_bool_with_time_and_easing(
        transition_id,
        target_visible,
        REVEAL_TRANSITION_SECONDS,
        egui::emath::easing::cubic_in_out,
    )
}
