//! Сборка картинок ASS-реплик кадра: порядок слоёв, столкновения, обрезка по холсту.

use std::sync::Arc;

use subtitle_core::SubtitleLayer;

use super::ass_collisions::{AssCollisionItem, AssCollisionMemory};
use super::cue_render::RasterizedCue;
use super::request::CueKey;
use crate::ass::{AssEventClock, AssRenderOptions};
use crate::overlay::{CanvasSize, SubtitleBitmap};

/// ASS-реплика кадра: готовая растеризация и данные порядка.
pub(super) struct AssFrameCue {
    pub(super) key: CueKey,
    pub(super) layer: SubtitleLayer,
    /// `ReadOrder` события: внутри слоя реплики идут в порядке чтения (libass: `cmp_event_layer`).
    pub(super) read_order: i32,
    pub(super) rasterized: Arc<RasterizedCue>,
    /// Часы реплики на момент кадра: по ним считается покадровый клип `\t(\clip(...))`.
    pub(super) clock: Option<AssEventClock>,
}

/// Картинки ASS-реплик в порядке отрисовки (libass: сортировка по слою, `fix_collisions` по
/// каждому слою, сдвиг картинок с обрезкой по холсту).
pub(super) fn assemble_ass_bitmaps(
    mut cues: Vec<AssFrameCue>,
    memory: &mut AssCollisionMemory,
    canvas: CanvasSize,
    options: &AssRenderOptions,
) -> Vec<SubtitleBitmap> {
    // Стабильная сортировка: равные (слой, ReadOrder) остаются в порядке запроса.
    cues.sort_by_key(|cue| (cue.layer, cue.read_order));
    let items: Vec<AssCollisionItem> = cues
        .iter()
        .map(|cue| AssCollisionItem {
            key: cue.key,
            layer: cue.layer,
            rect: cue
                .rasterized
                .ass_collision
                .filter(|collision| collision.detect_collisions)
                .map(|collision| collision.rect),
            direction: cue.rasterized.collision,
        })
        .collect();
    let shifts = memory.resolve(&items, canvas, options);

    let canvas_height = canvas.height();
    let mut bitmaps = Vec::new();
    for (cue, shift) in cues.iter().zip(shifts) {
        for bitmap in cue.rasterized.final_bitmaps(cue.clock.as_ref()).iter() {
            if shift == 0 {
                bitmaps.push(bitmap.clone());
            } else if let Some(moved) = bitmap.shifted_vertically_clipped(shift, canvas_height) {
                // Что вышло за кадр, обрезается (libass: `shift_event`).
                bitmaps.push(moved);
            }
        }
    }
    bitmaps
}
