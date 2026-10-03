//! Безопасная для показа метка источника медиа.
//!
//! Перенесено из `app-egui/src/media_open/types.rs` (session-07 выноса web-media):
//! метка стоит в полях `Native*Url` подготовки native-протоколов, которая теперь
//! живёт в этом crate-е. Поведение не менялось; в `app-egui` остался re-export по
//! прежнему пути `crate::media_open::SafeMediaLabel`.

use std::fmt;
use std::path::Path;

/// Максимальная длина display-only label в Unicode scalar values.
const SAFE_MEDIA_LABEL_MAX_CHARS: usize = 160;

/// Bounded/redacted label, безопасный для UI, diagnostics и `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct SafeMediaLabel(String);

impl SafeMediaLabel {
    /// Принимает только уже redacted service-owned label и дополнительно ограничивает длину.
    #[must_use]
    pub fn from_service_safe_label(label: &str) -> Self {
        Self(label.chars().take(SAFE_MEDIA_LABEL_MAX_CHARS).collect())
    }

    /// Строит display label только из filename, не раскрывая parent path.
    #[must_use]
    pub fn from_local_path(path: &Path) -> Self {
        let filename = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        Self::from_service_safe_label(&filename)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SafeMediaLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("SafeMediaLabel")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for SafeMediaLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_label_is_bounded_by_named_unicode_limit() {
        let raw_label = "я".repeat(SAFE_MEDIA_LABEL_MAX_CHARS + 25);
        let label = SafeMediaLabel::from_service_safe_label(&raw_label);

        assert_eq!(label.as_str().chars().count(), SAFE_MEDIA_LABEL_MAX_CHARS);
    }
}
