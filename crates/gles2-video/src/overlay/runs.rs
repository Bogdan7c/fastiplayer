//! Раскладка списка картинок оверлея по видам пикселей и «серии» рисования — чистый CPU.
//!
//! Порт `render-wgpu-shell/src/subtitle_overlay/runs.rs`. Картинки рисуются строго в порядке
//! списка, первая — самая нижняя. У двух видов пикселей разные программы и атласы, поэтому порядок
//! сохраняется так: идущие подряд картинки одного вида образуют серию ([`DrawRun`]), серия — один
//! draw-вызов своей программы. Внутри вида картинки лежат в порядке списка, значит серия — это
//! непрерывный диапазон картинок (и вершин) этого вида.

use std::ops::Range;

use super::vertices::VERTICES_PER_BITMAP;
use super::{OverlayBitmap, OverlayPixelFormat};

/// Вид пикселей: свой атлас, своя программа, свой буфер вершин.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum OverlayLayerKind {
    /// Маски покрытия.
    Coverage,
    /// RGBA-картинки.
    Picture,
}

impl OverlayLayerKind {
    /// Вид слоя для картинки с таким форматом пикселей.
    pub(super) const fn of_format(format: OverlayPixelFormat) -> Self {
        match format {
            OverlayPixelFormat::Coverage => Self::Coverage,
            OverlayPixelFormat::PremultipliedRgba => Self::Picture,
        }
    }
}

/// Непрерывная серия картинок одного вида, рисуемая одним draw-вызовом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DrawRun {
    /// Вид (а значит программа, атлас и буфер вершин) серии.
    pub(super) kind: OverlayLayerKind,
    /// Номер первой картинки серии среди картинок СВОЕГО вида.
    pub(super) first_bitmap: usize,
    /// Сколько картинок в серии (всегда больше нуля).
    pub(super) bitmap_count: usize,
}

impl DrawRun {
    /// Диапазон вершин серии в буфере своего вида; `None` — переполнение (недостижимо: буфер
    /// такого размера не собрать).
    pub(super) fn vertex_range(&self) -> Option<Range<usize>> {
        let start = self.first_bitmap.checked_mul(VERTICES_PER_BITMAP)?;
        let count = self.bitmap_count.checked_mul(VERTICES_PER_BITMAP)?;
        Some(start..start.checked_add(count)?)
    }
}

/// Картинки оверлея, разложенные по видам, и серии рисования в порядке списка.
pub(super) struct OverlayPartition<'a, B> {
    /// Маски, влезающие в атлас, в порядке списка.
    pub(super) coverage: Vec<&'a B>,
    /// RGBA-картинки, влезающие в атлас, в порядке списка.
    pub(super) pictures: Vec<&'a B>,
    /// Серии рисования всех влезающих картинок в порядке списка.
    pub(super) runs: Vec<DrawRun>,
    /// Сколько масок отброшено: сторона больше предела атласа.
    pub(super) dropped_coverage: usize,
    /// Сколько RGBA-картинок отброшено по той же причине.
    pub(super) dropped_pictures: usize,
}

/// Раскладывает список по видам; картинка со стороной больше `max_atlas_side` никогда не влезет в
/// атлас, поэтому она отбрасывается отдельно (остальные рисуются).
pub(super) fn partition_overlay<B: OverlayBitmap>(
    bitmaps: &[B],
    max_atlas_side: u32,
) -> OverlayPartition<'_, B> {
    let mut partition = OverlayPartition {
        coverage: Vec::new(),
        pictures: Vec::new(),
        runs: Vec::new(),
        dropped_coverage: 0,
        dropped_pictures: 0,
    };
    let mut kept_kinds = Vec::with_capacity(bitmaps.len());
    for bitmap in bitmaps {
        let kind = OverlayLayerKind::of_format(bitmap.pixels().format);
        let rect = bitmap.rect();
        let fits = rect.width <= max_atlas_side && rect.height <= max_atlas_side;
        match (kind, fits) {
            (OverlayLayerKind::Coverage, true) => partition.coverage.push(bitmap),
            (OverlayLayerKind::Picture, true) => partition.pictures.push(bitmap),
            (OverlayLayerKind::Coverage, false) => partition.dropped_coverage += 1,
            (OverlayLayerKind::Picture, false) => partition.dropped_pictures += 1,
        }
        if fits {
            kept_kinds.push(kind);
        }
    }
    partition.runs = build_runs(&kept_kinds);
    partition
}

/// Сворачивает последовательность видов (в порядке списка) в серии подряд идущих одного вида.
fn build_runs(kinds_in_list_order: &[OverlayLayerKind]) -> Vec<DrawRun> {
    let mut runs: Vec<DrawRun> = Vec::new();
    // Сколько картинок каждого вида уже выдано: следующая картинка вида лежит сразу за ними.
    let mut coverage_seen = 0_usize;
    let mut picture_seen = 0_usize;
    for &kind in kinds_in_list_order {
        let seen = match kind {
            OverlayLayerKind::Coverage => &mut coverage_seen,
            OverlayLayerKind::Picture => &mut picture_seen,
        };
        match runs.last_mut() {
            Some(last) if last.kind == kind => last.bitmap_count += 1,
            _ => runs.push(DrawRun {
                kind,
                first_bitmap: *seen,
                bitmap_count: 1,
            }),
        }
        *seen += 1;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::OverlayLayerKind::{Coverage, Picture};
    use super::*;
    use crate::overlay::test_support::FakeBitmap;

    fn run(kind: OverlayLayerKind, first_bitmap: usize, bitmap_count: usize) -> DrawRun {
        DrawRun {
            kind,
            first_bitmap,
            bitmap_count,
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
    fn interleaved_kinds_keep_list_order_and_per_layer_indexes() {
        // Порядок списка: текст, картинка, текст, текст, картинка. Номера считаются внутри вида.
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
    fn run_converts_to_a_vertex_range_and_rejects_overflow() {
        assert_eq!(run(Picture, 3, 4).vertex_range(), Some(18..42));
        assert_eq!(run(Picture, usize::MAX, 2).vertex_range(), None);
    }

    #[test]
    fn bitmaps_larger_than_the_atlas_limit_are_dropped_and_do_not_split_runs() {
        let bitmaps = [
            FakeBitmap::coverage(0, 0, 10, 10),
            FakeBitmap::picture(0, 0, 300, 4),
            FakeBitmap::coverage(0, 0, 10, 10),
            FakeBitmap::picture(0, 0, 4, 4),
        ];
        let partition = partition_overlay(&bitmaps, 256);
        assert_eq!(partition.coverage.len(), 2);
        assert_eq!(partition.pictures.len(), 1);
        assert_eq!(partition.dropped_coverage, 0);
        assert_eq!(partition.dropped_pictures, 1);
        // Отброшенная картинка не разрывает серию масок.
        assert_eq!(
            partition.runs,
            vec![run(Coverage, 0, 2), run(Picture, 0, 1)]
        );
    }
}
