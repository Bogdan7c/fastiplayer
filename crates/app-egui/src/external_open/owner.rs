//! Владелец состояния внешнего открытия внутри `AppState`.
//!
//! Объединяет состояние жеста и hit-test панели плейлиста и отдаёт остальному коду только
//! intent-методы: «опубликуй прямоугольник панели», «примени событие жеста», «что рисовать
//! поверх». Поля наружу не торчат.

use super::event::DropGestureEvent;
use super::gesture::DropGestureTracker;
use super::request::{DropTarget, ExternalOpenRequest};
use super::target::PlaylistPanelHitTest;

#[cfg(not(target_os = "linux"))]
use super::winit_source::LegacyFileDropCollector;

/// Что подсветить, пока над окном тащат данные.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DropOverlay {
    /// Выбранное место назначения (определяет текст подсказки).
    pub(crate) target: DropTarget,
    /// Область подсветки в точках egui.
    pub(crate) rect: egui::Rect,
}

/// Состояние внешнего открытия, привязанное к окну.
#[derive(Debug, Default)]
pub(crate) struct ExternalOpenOwner {
    gesture: DropGestureTracker,
    hit_test: PlaylistPanelHitTest,
    /// Устаревший источник событий нужен только вне Linux (см. `winit_source`).
    #[cfg(not(target_os = "linux"))]
    legacy_drops: LegacyFileDropCollector,
}

impl ExternalOpenOwner {
    /// UI публикует прямоугольник панели плейлиста этого кадра (`None` — панели нет).
    pub(crate) fn record_playlist_panel_rect(&mut self, panel_rect: Option<egui::Rect>) {
        self.hit_test.record_panel_rect(panel_rect);
    }

    /// Применяет событие жеста; на броске возвращает ровно один запрос открытия.
    pub(crate) fn apply_gesture_event(
        &mut self,
        event: DropGestureEvent,
        pixels_per_point: f32,
    ) -> Option<ExternalOpenRequest> {
        let hit_test = self.hit_test;
        self.gesture.apply_event(event, |position| {
            hit_test.resolve(position, pixels_per_point)
        })
    }

    /// Что подсвечивать в этом кадре; `None`, если над окном ничего не тащат.
    ///
    /// `video_rect` — область видео этого кадра; для броска на панель берётся прямоугольник
    /// панели, опубликованный UI.
    pub(crate) fn drop_overlay(
        &self,
        video_rect: egui::Rect,
        pixels_per_point: f32,
    ) -> Option<DropOverlay> {
        let hover_position = self.gesture.hovering_position()?;
        let target = self.hit_test.resolve(hover_position, pixels_per_point);
        let rect = match target {
            DropTarget::Playlist => self.hit_test.panel_rect().unwrap_or(video_rect),
            DropTarget::Video => video_rect,
        };
        Some(DropOverlay { target, rect })
    }

    /// Принимает устаревшее оконное событие файла (только вне Linux).
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn ingest_legacy_window_event(
        &mut self,
        event: &winit::event::WindowEvent,
    ) -> Option<DropGestureEvent> {
        self.legacy_drops.on_window_event(event)
    }

    /// Забирает накопленный за проход event loop бросок устаревшего источника (только вне Linux).
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn take_legacy_completed_drop(&mut self) -> Option<DropGestureEvent> {
        self.legacy_drops.take_completed_drop()
    }
}

#[cfg(test)]
mod tests;
