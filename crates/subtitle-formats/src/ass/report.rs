//! Счётчики разбора ASS: что пропущено, что распознано, но не поддерживается.

use std::collections::BTreeMap;

use subtitle_core::AssUnsupportedTag;

use super::limits::AssParseLimits;

/// Максимальная длина запоминаемого имени неизвестного тега (в символах).
const MAX_UNKNOWN_TAG_NAME_CHARS: usize = 16;

/// Статистика тегов по всем разобранным репликам. Владеет лимитом числа запоминаемых имён,
/// поэтому растёт ограниченно.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssTagStats {
    max_tracked_unknown_names: usize,
    unsupported: BTreeMap<AssUnsupportedTag, usize>,
    unknown_total: usize,
    unknown_names: BTreeMap<String, usize>,
    malformed: usize,
    over_limit: usize,
}

impl AssTagStats {
    pub fn new(limits: &AssParseLimits) -> Self {
        Self {
            max_tracked_unknown_names: limits.max_tracked_unknown_tag_names,
            unsupported: BTreeMap::new(),
            unknown_total: 0,
            unknown_names: BTreeMap::new(),
            malformed: 0,
            over_limit: 0,
        }
    }

    /// Тег слоя 2–3: распознан, но не исполняется. Ключ — тип тега, поэтому карта ограничена
    /// размером перечисления.
    pub(super) fn record_unsupported(&mut self, tag: AssUnsupportedTag) {
        let counter = self.unsupported.entry(tag).or_insert(0);
        *counter = counter.saturating_add(1);
    }

    /// Совсем неизвестный тег: считается всегда, имя запоминается в пределах лимита.
    pub(super) fn record_unknown(&mut self, name: &str) {
        self.unknown_total = self.unknown_total.saturating_add(1);
        let short_name: String = name.chars().take(MAX_UNKNOWN_TAG_NAME_CHARS).collect();
        if let Some(counter) = self.unknown_names.get_mut(&short_name) {
            *counter = counter.saturating_add(1);
        } else if self.unknown_names.len() < self.max_tracked_unknown_names {
            self.unknown_names.insert(short_name, 1);
        }
    }

    /// Поддерживаемый тег с неверным числом аргументов (например, `\pos(1)`): проигнорирован.
    pub(super) fn record_malformed(&mut self) {
        self.malformed = self.malformed.saturating_add(1);
    }

    /// Тег отброшен из-за `max_override_tags_per_event`.
    pub(super) fn record_over_limit(&mut self) {
        self.over_limit = self.over_limit.saturating_add(1);
    }

    /// Сколько раз встретился каждый поддерживаемый в будущем, но пока не исполняемый тег.
    pub fn unsupported(&self) -> &BTreeMap<AssUnsupportedTag, usize> {
        &self.unsupported
    }

    /// Сколько раз встретился указанный тег слоя 2–3.
    pub fn unsupported_count(&self, tag: AssUnsupportedTag) -> usize {
        self.unsupported.get(&tag).copied().unwrap_or(0)
    }

    /// Общее число неизвестных тегов.
    pub fn unknown_total(&self) -> usize {
        self.unknown_total
    }

    /// Имена неизвестных тегов (ограниченный набор) и сколько раз каждое встретилось.
    pub fn unknown_names(&self) -> &BTreeMap<String, usize> {
        &self.unknown_names
    }

    /// Число поддерживаемых тегов с неверными аргументами.
    pub fn malformed(&self) -> usize {
        self.malformed
    }

    /// Число тегов, отброшенных лимитом `max_override_tags_per_event`.
    pub fn over_limit(&self) -> usize {
        self.over_limit
    }
}

/// Итог разбора заголовка (`[Script Info]` + стили).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AssHeaderReport {
    /// Стили, пропущенные из-за `max_styles`.
    pub skipped_styles: usize,
    /// Байты заголовка не были валидным UTF-8, часть заменена на U+FFFD.
    pub had_replacement_characters: bool,
}

/// Итог разбора файла `.ass/.ssa`: заголовок + теги всех реплик.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssParseReport {
    pub header: AssHeaderReport,
    pub tags: AssTagStats,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(max_names: usize) -> AssParseLimits {
        AssParseLimits {
            max_header_bytes: 1024,
            max_styles: 10,
            max_override_tags_per_event: 10,
            max_tracked_unknown_tag_names: max_names,
        }
    }

    #[test]
    fn unknown_names_are_bounded_but_total_is_not() {
        let mut stats = AssTagStats::new(&limits(2));
        for name in ["aa", "bb", "cc", "aa", "dd"] {
            stats.record_unknown(name);
        }
        assert_eq!(stats.unknown_total(), 5);
        assert_eq!(stats.unknown_names().len(), 2);
        assert_eq!(stats.unknown_names().get("aa"), Some(&2));
        assert_eq!(stats.unknown_names().get("cc"), None);
    }

    #[test]
    fn long_unknown_names_are_truncated() {
        let mut stats = AssTagStats::new(&limits(4));
        stats.record_unknown(&"x".repeat(100));
        let stored = stats.unknown_names().keys().next().map(String::len);
        assert_eq!(stored, Some(MAX_UNKNOWN_TAG_NAME_CHARS));
    }

    #[test]
    fn unsupported_tags_count_per_kind() {
        let mut stats = AssTagStats::new(&limits(1));
        stats.record_unsupported(AssUnsupportedTag::Blur);
        stats.record_unsupported(AssUnsupportedTag::Blur);
        stats.record_unsupported(AssUnsupportedTag::Move);
        assert_eq!(stats.unsupported_count(AssUnsupportedTag::Blur), 2);
        assert_eq!(stats.unsupported_count(AssUnsupportedTag::Move), 1);
        assert_eq!(stats.unsupported_count(AssUnsupportedTag::Clip), 0);
    }
}
