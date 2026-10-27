//! Видеослой в кадре с частичной перерисовкой: отсечение прямоугольников областью кадра.
//!
//! Сборщик кадра (`gles2-egui::FrameCompositor`) может ограничить кадр областью
//! ([`ActiveFrame::repaint_area`]): остальные пиксели заднего буфера уже верны, и слой не имеет
//! права их менять (инвариант на `ActiveFrame`). Видеослой рисует видео и заливает чёрным всё
//! вне картинки только внутри этой области.

use gles2_egui::{ActiveFrame, PixelRect as FramePixelRect};

use crate::geometry::PixelRect;

/// Части `rects`, лежащие в области перерисовки кадра. Прямоугольники без пересечения
/// выпадают; без ограничения (вся цель) результат равен входу.
pub(crate) fn clip_to_repaint_area(frame: &ActiveFrame<'_>, rects: &[PixelRect]) -> Vec<PixelRect> {
    rects
        .iter()
        .filter_map(|rect| {
            let clipped = frame.clip_to_repaint(FramePixelRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })?;
            Some(PixelRect {
                x: clipped.x,
                y: clipped.y,
                width: clipped.width,
                height: clipped.height,
            })
        })
        .collect()
}
