//! Кнопка выбора аудиодорожки «♪» и её меню в нижней строке controls.
//!
//! # Владение
//! - Список дорожек и выбор принадлежат player-у; сюда они приходят снимком
//!   [`AudioTrackMenuView`], намерения уходят типизированными [`AudioTrackControlAction`].
//!   Модуль сам ничего не применяет.
//! - `layout` — чистая раскладка и выбор режима по ширине (Full / Compact / Hidden).
//! - `button` — отрисовка и ввод «♪» и стрелки «▾», `menu` — радио-список дорожек.
//! - Анимация появления (250 мс, как у CC) принадлежит этому модулю; тот же progress сдвигает
//!   Shuffle/Repeat, поэтому ничего не прыгает.
//!
//! # Видимость
//! Кнопка есть только у media с двумя и более аудиодорожками; иначе её нет вовсе (ни
//! виджета, ни rect-а), и строка раскладывается так же, как до появления кнопки. При нехватке
//! места «♪» скрывается раньше CC: у субтитров приоритет.

mod button;
mod layout;
mod menu;
#[cfg(test)]
mod tests;

use egui::{Id, Ui};
use media_core::TrackId;

use super::ControlAction;
use super::queue_mode_controls::QueueModeControlLayout;
use super::subtitle_control::SubtitleNeighbor;
use crate::state::AudioTrackMenuView;
use crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction;
use layout::{AudioTrackControlLayout, AudioTrackControlMode, choose_mode};

/// Полный переход «появилась/исчезла» занимает 250 мс (как CC и кнопка скорости).
const REVEAL_TRANSITION_SECONDS: f32 = 0.250;
/// Суффикс Id анимации появления: отделяет её от Id виджетов.
const REVEAL_TRANSITION_ID_SUFFIX: &str = "audio_track_control_reveal";

/// Действие выбора аудио, которое shell применит после egui pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTrackControlAction {
    /// Выбрать встроенную дорожку из меню.
    SelectTrack(TrackId),
    /// Выбрать другую web-версию звука (HLS/DASH audio rendition) из того же меню.
    SwitchWebAudioVersion(ComponentVariantSelectionAction),
    /// Следующая дорожка по кругу (клавиша `B`).
    SelectNextTrack,
}

/// Подготовленная на кадр раскладка и прогресс появления «♪».
pub(super) struct AudioTrackControlFrame {
    layout: AudioTrackControlLayout,
    reveal_progress: f32,
    neighbor: SubtitleNeighbor,
}

impl AudioTrackControlFrame {
    /// Выбирает режим по ширине, продвигает анимацию появления и считает раскладку.
    pub(super) fn prepare(
        ui: &Ui,
        neighbor: SubtitleNeighbor,
        view: &AudioTrackMenuView,
        reduced_motion: bool,
    ) -> Self {
        let mode = choose_mode(&neighbor);
        let target_visible = view.has_choice() && mode != AudioTrackControlMode::Hidden;
        let reveal_progress = reveal_progress(ui, target_visible, reduced_motion);
        Self {
            layout: layout::layout(&neighbor, mode, reveal_progress),
            reveal_progress,
            neighbor,
        }
    }

    /// Раскладка Shuffle/Repeat этого кадра (с учётом места под CC и «♪»).
    pub(super) fn queue_mode_layout(&self) -> QueueModeControlLayout {
        self.layout.queue_mode
    }

    /// Рисует «♪» и стрелку (если они видны) и добавляет действия пользователя.
    pub(super) fn render(
        &self,
        ui: &mut Ui,
        view: &AudioTrackMenuView,
        reduced_motion: bool,
        actions: &mut Vec<ControlAction>,
    ) {
        button::render(
            ui,
            &self.layout,
            view,
            self.neighbor.geometry.controls_style,
            button::RevealState {
                progress: self.reveal_progress,
                reduced_motion,
            },
            actions,
        );
    }
}

/// Прогресс появления «♪»; при reduced motion переход мгновенный и без repaint.
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
