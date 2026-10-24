//! Набор имён расширений GL/EGL.

use std::collections::BTreeSet;

/// Имена расширений. Проверка — по ЦЕЛОМУ имени: `GL_EXT_texture_rg` не находится внутри
/// `GL_EXT_texture_rg_extra` (подстрочный поиск по строке расширений — классическая ошибка).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionSet {
    names: BTreeSet<String>,
}

impl ExtensionSet {
    /// Набор из строки, разделённой пробелами (`GL_EXTENSIONS`, `EGL_EXTENSIONS`).
    pub fn from_space_separated(extensions: &str) -> Self {
        Self::from_names(extensions.split_ascii_whitespace())
    }

    /// Набор из отдельных имён.
    pub fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        Self {
            names: names
                .into_iter()
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }

    /// Есть ли расширение.
    pub fn contains(&self, extension: &str) -> bool {
        self.names.contains(extension)
    }

    /// Есть ли хотя бы одно из расширений (например, KHR- и EXT-варианты одной платформы).
    pub fn contains_any(&self, extensions: &[&str]) -> bool {
        extensions.iter().any(|extension| self.contains(extension))
    }

    /// Имена в алфавитном порядке.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    /// Число расширений.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Пустой ли набор.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}
