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
}

impl RenderDiagnostics {
    /// Суммирует два отчёта (насыщение вместо переполнения: счётчик не должен ронять рендер).
    pub(crate) fn combined(self, other: Self) -> Self {
        Self {
            skipped_glyphs: self.skipped_glyphs.saturating_add(other.skipped_glyphs),
            skipped_borders: self.skipped_borders.saturating_add(other.skipped_borders),
        }
    }
}
