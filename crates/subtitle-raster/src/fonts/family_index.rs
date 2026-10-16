//! Индекс «имя семейства -> лица» с нечувствительным к регистру сравнением имён.

use std::collections::HashMap;

use super::face::FaceId;

/// Индекс имён семейств одного источника (вложенные / системные / встроенные).
#[derive(Debug, Default)]
pub(super) struct FamilyIndex {
    faces_by_family: HashMap<String, Vec<FaceId>>,
}

impl FamilyIndex {
    /// Нормализация имени: libass/fontconfig сравнивают имена без учёта регистра.
    fn key(family: &str) -> String {
        family.trim().to_lowercase()
    }

    /// Регистрирует лицо под именем семейства (повтор того же лица игнорируется).
    pub(super) fn add(&mut self, family: &str, face: FaceId) {
        let faces = self.faces_by_family.entry(Self::key(family)).or_default();
        if !faces.contains(&face) {
            faces.push(face);
        }
    }

    /// Забывает все семейства.
    pub(super) fn clear(&mut self) {
        self.faces_by_family.clear();
    }

    /// Сколько различных имён семейств в индексе.
    pub(super) fn family_count(&self) -> usize {
        self.faces_by_family.len()
    }

    /// Лица семейства в порядке регистрации (пусто, если семейства нет).
    pub(super) fn faces_of(&self, family: &str) -> &[FaceId] {
        self.faces_by_family
            .get(&Self::key(family))
            .map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_ignores_case_and_surrounding_spaces_and_keeps_order() {
        let mut index = FamilyIndex::default();
        index.add("Noto Sans", FaceId(3));
        index.add("noto sans", FaceId(5));
        index.add("Noto Sans", FaceId(3));
        assert_eq!(index.faces_of("  NOTO SANS "), &[FaceId(3), FaceId(5)]);
        assert!(index.faces_of("Arial").is_empty());
    }
}
