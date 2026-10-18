//! Раскладка списка картинок overlay-а по слоям (маски текста / RGBA-картинки) и «серии» рисования.
//!
//! Чистая CPU-логика без wgpu. Контракт overlay-а: картинки рисуются строго в порядке списка,
//! первая — самая нижняя. У двух видов пикселей разные pipeline и атласы, поэтому порядок
//! сохраняется так: идущие подряд картинки одного вида образуют серию (`DrawRun`), серия — один
//! draw-вызов своего pipeline. Внутри слоя инстансы лежат в порядке списка, значит серия — это
//! непрерывный диапазон инстансов этого слоя.

use std::ops::Range;

use subtitle_raster::{BitmapPixelFormat, SubtitleBitmap};

use super::pipeline::LayerKind;

/// Вид слоя, в котором рисуется картинка с таким форматом пикселей.
pub(super) fn layer_kind_of(bitmap: &SubtitleBitmap) -> LayerKind {
    match bitmap.pixel_format() {
        BitmapPixelFormat::Coverage => LayerKind::Coverage,
        BitmapPixelFormat::PremultipliedRgba => LayerKind::Picture,
    }
}

/// Непрерывная серия инстансов одного слоя, рисуемая одним draw-вызовом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DrawRun {
    /// Слой (а значит pipeline, атлас и instance buffer), которому принадлежит серия.
    pub(super) kind: LayerKind,
    /// Индекс первого инстанса серии в instance buffer-е СВОЕГО слоя.
    pub(super) first_instance: usize,
    /// Сколько инстансов в серии (всегда больше нуля).
    pub(super) instance_count: usize,
}

impl DrawRun {
    /// Диапазон инстансов для `draw`; `None`, если индекс не помещается в `u32`
    /// (слой такой размер не принимает, так что в рабочем коде это недостижимо).
    pub(super) fn instance_range(&self) -> Option<Range<u32>> {
        let start = u32::try_from(self.first_instance).ok()?;
        let end = u32::try_from(self.first_instance.checked_add(self.instance_count)?).ok()?;
        Some(start..end)
    }
}

/// Картинки overlay-а, разложенные по слоям, и серии рисования в порядке списка.
pub(super) struct OverlayPartition<'a> {
    /// Маски текста, влезающие в GPU, в порядке списка.
    pub(super) coverage: Vec<&'a SubtitleBitmap>,
    /// RGBA-картинки, влезающие в GPU, в порядке списка.
    pub(super) pictures: Vec<&'a SubtitleBitmap>,
    /// Серии рисования всех влезающих картинок в порядке списка.
    pub(super) runs: Vec<DrawRun>,
    /// Сколько масок текста отброшено: одна сторона больше лимита текстуры GPU.
    pub(super) dropped_coverage: usize,
    /// Сколько RGBA-картинок отброшено по той же причине.
    pub(super) dropped_pictures: usize,
}

/// Раскладывает список картинок по слоям; картинка со стороной больше `max_texture_side`
/// никогда не влезет в атлас, поэтому она отбрасывается отдельно (остальные рисуются).
pub(super) fn partition_overlay(
    bitmaps: &[SubtitleBitmap],
    max_texture_side: u32,
) -> OverlayPartition<'_> {
    let mut partition = OverlayPartition {
        coverage: Vec::new(),
        pictures: Vec::new(),
        runs: Vec::new(),
        dropped_coverage: 0,
        dropped_pictures: 0,
    };
    let mut kept_kinds = Vec::with_capacity(bitmaps.len());
    for bitmap in bitmaps {
        let kind = layer_kind_of(bitmap);
        let fits = bitmap.width() <= max_texture_side && bitmap.height() <= max_texture_side;
        match (kind, fits) {
            (LayerKind::Coverage, true) => partition.coverage.push(bitmap),
            (LayerKind::Picture, true) => partition.pictures.push(bitmap),
            (LayerKind::Coverage, false) => partition.dropped_coverage += 1,
            (LayerKind::Picture, false) => partition.dropped_pictures += 1,
        }
        if fits {
            kept_kinds.push(kind);
        }
    }
    partition.runs = build_runs(&kept_kinds);
    partition
}

/// Сворачивает последовательность видов (в порядке списка) в серии подряд идущих одного вида.
fn build_runs(kinds_in_list_order: &[LayerKind]) -> Vec<DrawRun> {
    let mut runs: Vec<DrawRun> = Vec::new();
    // Сколько инстансов каждого слоя уже выдано: следующий инстанс слоя лежит сразу за ними.
    let mut coverage_seen = 0_usize;
    let mut picture_seen = 0_usize;
    for &kind in kinds_in_list_order {
        let seen = match kind {
            LayerKind::Coverage => &mut coverage_seen,
            LayerKind::Picture => &mut picture_seen,
        };
        match runs.last_mut() {
            Some(last) if last.kind == kind => last.instance_count += 1,
            _ => runs.push(DrawRun {
                kind,
                first_instance: *seen,
                instance_count: 1,
            }),
        }
        *seen += 1;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::LayerKind::{Coverage, Picture};
    use super::*;

    fn run(kind: LayerKind, first_instance: usize, instance_count: usize) -> DrawRun {
        DrawRun {
            kind,
            first_instance,
            instance_count,
        }
    }

    #[test]
    fn consecutive_bitmaps_of_one_kind_form_a_single_run() {
        assert_eq!(
            build_runs(&[Coverage, Coverage, Picture, Picture, Picture]),
            vec![run(Coverage, 0, 2), run(Picture, 0, 3)]
        );
    }

    #[test]
    fn interleaved_kinds_keep_list_order_and_per_layer_instance_indexes() {
        // Порядок списка: текст, картинка, текст, текст, картинка. Индексы считаются внутри слоя.
        assert_eq!(
            build_runs(&[Coverage, Picture, Coverage, Coverage, Picture]),
            vec![
                run(Coverage, 0, 1),
                run(Picture, 0, 1),
                run(Coverage, 1, 2),
                run(Picture, 1, 1),
            ]
        );
    }

    #[test]
    fn empty_list_has_no_runs() {
        assert!(build_runs(&[]).is_empty());
    }

    #[test]
    fn run_converts_to_a_draw_range_and_rejects_u32_overflow() {
        assert_eq!(run(Picture, 3, 4).instance_range(), Some(3..7));
        assert_eq!(run(Picture, usize::MAX, 2).instance_range(), None);
    }
}
