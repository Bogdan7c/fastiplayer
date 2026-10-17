//! Счётчики пропущенных элементов при отрисовке одной реплики.
//!
//! Глубокие стадии конвейера (контуры, обводка) не знают про счётчики растеризатора: они
//! возвращают этот небольшой отчёт вместе с результатом, а владелец (растеризатор) добавляет
//! его к общей статистике. Так сбой одного глифа не молчит, но и не ломает реплику.

/// Что пропущено при отрисовке реплики.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RenderDiagnostics {
    /// Глифы без контура из-за ошибки чтения (сам глиф не нарисован).
    pub(crate) skipped_glyphs: u32,
    /// Глифы, нарисованные без обводки: обводку построить не удалось.
    pub(crate) skipped_borders: u32,
    /// Глифы, не нарисованные, потому что после преобразования (3D-перспектива у плоскости
    /// камеры) их битмап превысил предел стороны `RasterInputLimits::max_canvas_side`.
    pub(crate) skipped_oversized_bitmaps: u32,
    /// Рисунки ASS (`\p`), не давшие ни одного контура: команды не распознаны или точка вышла
    /// за допустимый диапазон. Рисунок остаётся в строке пустым псевдоглифом (как в libass).
    pub(crate) invalid_ass_drawings: u32,
    /// Слои, оставшиеся неразмытыми из-за нарушенного внутреннего инварианта размытия (не из-за
    /// предела размера: тот считается в `skipped_oversized_bitmaps`).
    pub(crate) failed_blur_layers: u32,
}

impl RenderDiagnostics {
    /// Суммирует два отчёта (насыщение вместо переполнения: счётчик не должен ронять рендер).
    pub(crate) fn combined(self, other: Self) -> Self {
        Self {
            skipped_glyphs: self.skipped_glyphs.saturating_add(other.skipped_glyphs),
            skipped_borders: self.skipped_borders.saturating_add(other.skipped_borders),
            skipped_oversized_bitmaps: self
                .skipped_oversized_bitmaps
                .saturating_add(other.skipped_oversized_bitmaps),
            invalid_ass_drawings: self
                .invalid_ass_drawings
                .saturating_add(other.invalid_ass_drawings),
            failed_blur_layers: self
                .failed_blur_layers
                .saturating_add(other.failed_blur_layers),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_blur_layers_are_summed_and_do_not_leak_into_other_counters() {
        let first = RenderDiagnostics {
            failed_blur_layers: 2,
            ..RenderDiagnostics::default()
        };
        let second = RenderDiagnostics {
            failed_blur_layers: u32::MAX,
            skipped_oversized_bitmaps: 1,
            ..RenderDiagnostics::default()
        };
        let total = first.combined(second);
        assert_eq!(total.failed_blur_layers, u32::MAX);
        assert_eq!(total.skipped_oversized_bitmaps, 1);
        assert_eq!(total.skipped_glyphs, 0);
    }
}
