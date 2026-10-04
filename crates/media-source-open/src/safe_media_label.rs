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

/// Метка для локального пути без последнего имени (`/`, `..`): путь целиком не показываем.
const LOCAL_PATH_WITHOUT_FILENAME_LABEL: &str = "(без имени)";

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
    ///
    /// Если у пути нет последнего имени (`/`, `..`), показываем нейтральную константу:
    /// раньше в этом случае подставлялся весь путь, что раскрывало родительские каталоги.
    /// Имя не в UTF-8 переводится lossy (нечитаемые байты → `�`), без паники.
    #[must_use]
    pub fn from_local_path(path: &Path) -> Self {
        match path.file_name() {
            Some(filename) => Self::from_service_safe_label(&filename.to_string_lossy()),
            None => Self::from_service_safe_label(LOCAL_PATH_WITHOUT_FILENAME_LABEL),
        }
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

    /// Метка локального файла — только последнее имя, без родительских каталогов.
    #[test]
    fn local_label_keeps_only_filename_without_parent_directories() {
        let label =
            SafeMediaLabel::from_local_path(Path::new("/home/private-owner/Videos/clip.mkv"));

        assert_eq!(label.as_str(), "clip.mkv");
    }

    /// Путь без имени (`/`, `..`) не должен раскрываться целиком.
    #[test]
    fn local_path_without_filename_uses_neutral_label_instead_of_full_path() {
        for path in ["/", "/home/private-owner/..", ".."] {
            let label = SafeMediaLabel::from_local_path(Path::new(path));

            assert_eq!(
                label.as_str(),
                LOCAL_PATH_WITHOUT_FILENAME_LABEL,
                "path: {path}"
            );
            assert!(!label.as_str().contains("private-owner"));
        }
    }

    /// Имя не в UTF-8 строится lossy и без паники; читаемая часть имени сохраняется.
    #[cfg(unix)]
    #[test]
    fn non_utf8_local_filename_is_rendered_lossy_without_panic() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let path = Path::new("/private-owner").join(OsStr::from_bytes(b"clip-\xff.mkv"));
        let label = SafeMediaLabel::from_local_path(&path);

        assert_eq!(label.as_str(), "clip-\u{fffd}.mkv");
    }
}
