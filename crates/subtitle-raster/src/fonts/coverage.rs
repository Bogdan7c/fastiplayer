//! Компактное покрытие шрифта: какие кодпоинты есть в `cmap`.
//!
//! Нужно, чтобы fallback по системным шрифтам не открывал файлы на каждом промахе: покрытие
//! строится один раз при сканировании ([`GlyphCoverage::from_mappings`]) и дальше отвечает на
//! «есть ли символ в лице?» только из памяти. Хранятся отсортированные непересекающиеся
//! диапазоны кодпоинтов: у типичного шрифта их сотни-тысячи вместо десятков тысяч символов.

use std::mem::size_of;

/// Отсортированные непересекающиеся диапазоны кодпоинтов `[first, last]` (границы включены).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct GlyphCoverage {
    ranges: Vec<(u32, u32)>,
}

impl GlyphCoverage {
    /// Строит покрытие из кодпоинтов (порядок не важен, повторы допустимы).
    ///
    /// Обычно `cmap` отдаёт кодпоинты по возрастанию, тогда сортировка не нужна; если порядок
    /// нарушен, диапазоны сортируются и склеиваются в конце.
    pub(super) fn from_mappings(codepoints: impl IntoIterator<Item = u32>) -> Self {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        let mut previous: Option<u32> = None;
        let mut sorted = true;
        for codepoint in codepoints {
            // Порядок нарушен: диапазоны придётся отсортировать и склеить в конце.
            if previous.is_some_and(|before| codepoint < before) {
                sorted = false;
            }
            previous = Some(codepoint);
            match ranges.last_mut() {
                // Продолжение текущего диапазона.
                Some((_, last)) if codepoint == last.saturating_add(1) => *last = codepoint,
                // Повтор внутри текущего диапазона: ничего не меняется.
                Some((first, last)) if (*first..=*last).contains(&codepoint) => {}
                _ => ranges.push((codepoint, codepoint)),
            }
        }
        if !sorted {
            ranges = merge_unsorted(ranges);
        }
        ranges.shrink_to_fit();
        Self { ranges }
    }

    /// Есть ли кодпоинт в покрытии.
    pub(super) fn contains(&self, codepoint: u32) -> bool {
        // Первый диапазон, чей конец >= codepoint; кодпоинт внутри, если начало не больше.
        let index = self.ranges.partition_point(|(_, last)| *last < codepoint);
        self.ranges
            .get(index)
            .is_some_and(|(first, _)| *first <= codepoint)
    }

    /// Число диапазонов (для тестов и диагностики).
    #[cfg(test)]
    pub(super) fn range_count(&self) -> usize {
        self.ranges.len()
    }

    /// Память, занимаемая диапазонами.
    pub(super) fn memory_bytes(&self) -> usize {
        self.ranges.len().saturating_mul(size_of::<(u32, u32)>())
    }
}

/// Сортирует диапазоны и склеивает пересекающиеся и соседние.
fn merge_unsorted(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        match merged.last_mut() {
            Some((_, merged_last)) if first <= merged_last.saturating_add(1) => {
                *merged_last = (*merged_last).max(last);
            }
            _ => merged.push((first, last)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_codepoints_collapse_into_ranges() {
        let coverage = GlyphCoverage::from_mappings([0x41, 0x42, 0x43, 0x61, 0x62, 0x400]);
        assert_eq!(coverage.range_count(), 3);
        assert!(coverage.contains(0x42) && coverage.contains(0x62) && coverage.contains(0x400));
        assert!(!coverage.contains(0x44), "gap after a range");
        assert!(!coverage.contains(0x40), "just before a range");
        assert!(!coverage.contains(0x401), "just after the last range");
        assert_eq!(coverage.memory_bytes(), 3 * 8);
    }

    #[test]
    fn empty_coverage_contains_nothing() {
        let coverage = GlyphCoverage::from_mappings([]);
        assert!(!coverage.contains(0) && !coverage.contains(0x10FFFF));
        assert_eq!(coverage.range_count(), 0);
    }

    #[test]
    fn unsorted_and_duplicated_input_gives_the_same_coverage_as_sorted() {
        let sorted = GlyphCoverage::from_mappings([1, 2, 3, 10, 11, 20]);
        let shuffled = GlyphCoverage::from_mappings([20, 3, 11, 1, 10, 2, 2, 11]);
        assert_eq!(shuffled, sorted);
        assert!(shuffled.contains(3) && shuffled.contains(10) && !shuffled.contains(12));
    }

    #[test]
    fn range_boundaries_are_inclusive() {
        let coverage = GlyphCoverage::from_mappings([5, 6, 7]);
        assert!(coverage.contains(5) && coverage.contains(7));
        assert!(!coverage.contains(4) && !coverage.contains(8));
    }
}
