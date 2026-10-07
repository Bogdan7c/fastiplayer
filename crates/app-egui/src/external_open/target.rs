//! Hit-test места броска: попала ли точка на панель плейлиста.
//!
//! Источник событий сообщает позицию в **физических пикселях**, а UI публикует прямоугольник
//! панели в **точках egui** (`physical / pixels_per_point`). Пересчёт делается только здесь.
//! Прямоугольник публикует UI каждый кадр через intent-метод [`PlaylistPanelHitTest::record_panel_rect`];
//! если панели плейлиста нет на экране, публикуется `None` и любой бросок идёт на видео.

use super::event::PhysicalDropPosition;
use super::request::DropTarget;

/// Последний опубликованный прямоугольник панели плейлиста.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PlaylistPanelHitTest {
    panel_rect: Option<egui::Rect>,
}

impl PlaylistPanelHitTest {
    /// Запоминает прямоугольник панели этого кадра (`None` — панели плейлиста нет).
    pub(crate) fn record_panel_rect(&mut self, panel_rect: Option<egui::Rect>) {
        self.panel_rect = panel_rect;
    }

    /// Прямоугольник панели плейлиста, если она сейчас видна.
    pub(crate) fn panel_rect(&self) -> Option<egui::Rect> {
        self.panel_rect
    }

    /// Определяет место назначения по позиции и масштабу окна.
    ///
    /// Нет позиции, нет панели или масштаб некорректен — видео: это безопасное значение
    /// по умолчанию (замена очереди требует подтверждения, добавление — нет).
    pub(crate) fn resolve(
        &self,
        position: Option<PhysicalDropPosition>,
        pixels_per_point: f32,
    ) -> DropTarget {
        let (Some(position), Some(panel_rect)) = (position, self.panel_rect) else {
            return DropTarget::Video;
        };
        if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
            return DropTarget::Video;
        }
        let scale = f64::from(pixels_per_point);
        // Сужение до f32 осознанное: координаты окна на порядки меньше предела точности f32.
        let point_in_points = egui::pos2((position.x / scale) as f32, (position.y / scale) as f32);
        if panel_rect.contains(point_in_points) {
            DropTarget::Playlist
        } else {
            DropTarget::Video
        }
    }
}

#[cfg(test)]
mod tests;
