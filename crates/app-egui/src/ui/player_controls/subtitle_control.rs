//! Кнопка субтитров CC в нижней строке controls и её меню.
//!
//! # Владение
//! - Состояние субтитров (включены ли, дорожки, сдвиг) принадлежит `AppState`; сюда оно
//!   приходит снимком [`SubtitleMenuView`], а намерения уходят типизированными
//!   [`SubtitleControlAction`]. Модуль сам ничего не применяет.
//! - `layout` — чистая раскладка и выбор режима по ширине (Full / Compact / Hidden).
//! - `button` — отрисовка и ввод CC и стрелки «▾», `menu` — содержимое выпадающего меню.
//! - Анимация появления CC (250 мс, как у кнопки скорости) принадлежит этому модулю; тот же
//!   progress сдвигает Shuffle/Repeat, поэтому ничего не прыгает.
//!
//! # Видимость
//! У видео без дорожек кнопки нет вовсе: ни виджета, ни rect-а (решение владельца).

mod button;
mod layout;
mod menu;
#[cfg(test)]
mod tests;
mod translation_menu;

use egui::{Id, Ui};
use subtitle_core::SubtitleTrackId;

use super::ControlAction;
use crate::state::{SubtitleMenuView, SyncNudge};
pub(super) use layout::APPEAR_SLIDE_DISTANCE;
pub(super) use layout::SubtitleLayoutGeometry;
use layout::{
    SubtitleControlAvailability, SubtitleControlLayout, SubtitleControlMode, choose_mode,
};
/// Раскладка CC для тестов соседней кнопки «♪»: она сверяется с реальной раскладкой CC.
#[cfg(test)]
pub(super) use layout::{
    SubtitleControlMode as TestSubtitleControlMode, choose_mode as choose_subtitle_mode,
    layout as subtitle_layout, layout_without_subtitles,
};

/// Полный переход «появилась/исчезла» занимает 250 мс (как кнопка скорости).
const REVEAL_TRANSITION_SECONDS: f32 = 0.250;
/// Суффикс Id анимации появления: отделяет её от Id виджетов.
const REVEAL_TRANSITION_ID_SUFFIX: &str = "subtitle_control_reveal";

/// Действие субтитров, которое shell применит после egui pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleControlAction {
    /// Включить/выключить показ (клик по CC, клавиша C).
    ToggleVisibility,
    /// Выбрать дорожку из меню (и включить показ).
    SelectTrack(SubtitleTrackId),
    /// Выключить субтитры (пункт «Off»).
    Disable,
    /// Следующая дорожка по кругу, включая «выкл» (клавиша V).
    SelectNextTrack,
    /// Сдвинуть синхронизацию на шаг (клавиши Z/X).
    NudgeSync(SyncNudge),
    /// Сбросить сдвиг синхронизации.
    ResetSync,
    /// Открыть диалог выбора файла субтитров.
    OpenLoadFileDialog,
}

/// Подготовленная на кадр раскладка и прогресс появления CC.
pub(super) struct SubtitleControlFrame {
    layout: SubtitleControlLayout,
    reveal_progress: f32,
    geometry: SubtitleLayoutGeometry,
    /// CC должна быть видна при полном появлении (есть дорожки и хватает места).
    target_visible: bool,
}

/// Что соседней кнопке «♪» нужно знать о CC, чтобы встать левее и не наехать на неё.
#[derive(Debug, Clone, Copy)]
pub(super) struct SubtitleNeighbor {
    /// Геометрия строки (общая для обеих кнопок).
    pub(super) geometry: SubtitleLayoutGeometry,
    /// Режим CC (у CC приоритет: «♪» подстраивается под него, а не наоборот).
    pub(super) mode: SubtitleControlMode,
    /// CC будет видна при полном появлении.
    pub(super) target_visible: bool,
    /// Текущий прогресс появления CC.
    pub(super) reveal_progress: f32,
}

impl SubtitleControlFrame {
    /// Выбирает режим по ширине, продвигает анимацию появления и считает раскладку.
    pub(super) fn prepare(
        ui: &Ui,
        geometry: SubtitleLayoutGeometry,
        view: &SubtitleMenuView,
        reduced_motion: bool,
    ) -> Self {
        let mode = choose_mode(&geometry);
        let availability = if view.has_tracks {
            SubtitleControlAvailability::HasTracks
        } else {
            SubtitleControlAvailability::NoTracks
        };
        let target_visible = availability == SubtitleControlAvailability::HasTracks
            && mode != SubtitleControlMode::Hidden;
        let reveal_progress = reveal_progress(ui, target_visible, reduced_motion);
        Self {
            layout: layout::layout(&geometry, mode, reveal_progress),
            reveal_progress,
            geometry,
            target_visible,
        }
    }

    /// Данные CC для кнопки аудиодорожек: она стоит левее и занимает место вместе с CC.
    pub(super) fn neighbor_for_audio_tracks(&self) -> SubtitleNeighbor {
        SubtitleNeighbor {
            geometry: self.geometry,
            mode: self.layout.mode,
            target_visible: self.target_visible,
            reveal_progress: self.reveal_progress,
        }
    }

    /// Рисует CC и стрелку (если они видны) и добавляет действия пользователя.
    pub(super) fn render(
        &self,
        ui: &mut Ui,
        view: &SubtitleMenuView,
        reduced_motion: bool,
        actions: &mut Vec<ControlAction>,
    ) {
        button::render(
            ui,
            &self.layout,
            view,
            self.geometry.controls_style,
            button::RevealState {
                progress: self.reveal_progress,
                reduced_motion,
            },
            actions,
        );
    }
}

/// Прогресс появления CC; при reduced motion переход мгновенный и без repaint.
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
