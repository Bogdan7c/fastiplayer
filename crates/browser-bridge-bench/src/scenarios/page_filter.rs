//! Выбор страниц для прогона: флаг `--page <id>` (повторяемый) сужает `pages`, `capture` и
//! `page-ends` до перечисленных страниц, чтобы гонять одну страницу, а не весь список.

/// Какие страницы прогонять. Пустой фильтр = все страницы команды.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageFilter {
    /// Идентификаторы в порядке указания, без повторов.
    ids: Vec<String>,
}

impl PageFilter {
    /// Без фильтра: прогонять всё.
    pub fn all() -> Self {
        Self::default()
    }

    /// Только перечисленные страницы (повторы отбрасываются).
    pub fn only(ids: impl IntoIterator<Item = String>) -> Self {
        let mut unique: Vec<String> = Vec::new();
        for id in ids {
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        Self { ids: unique }
    }

    /// Пройдёт ли страница `id` через фильтр.
    pub fn admits(&self, id: &str) -> bool {
        self.ids.is_empty() || self.ids.iter().any(|selected| selected == id)
    }

    /// Фильтр не задан (прогоняется всё).
    pub fn selects_everything(&self) -> bool {
        self.ids.is_empty()
    }

    /// Выбранные идентификаторы (пусто, если фильтра нет).
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// Пересекается ли выбор с `candidates`. Без фильтра — всегда да.
    pub fn matches_any_of<'a>(&self, candidates: impl IntoIterator<Item = &'a str>) -> bool {
        self.selects_everything() || candidates.into_iter().any(|id| self.admits(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(ids: &[&str]) -> PageFilter {
        PageFilter::only(ids.iter().map(|id| (*id).to_owned()))
    }

    #[test]
    fn an_empty_filter_admits_everything() {
        let filter = PageFilter::all();
        assert!(filter.selects_everything());
        assert!(filter.admits("mse-fmp4") && filter.admits("anything"));
    }

    #[test]
    fn a_filter_admits_only_the_listed_pages() {
        let filter = only(&["page-freeze", "mse-fmp4"]);
        assert!(!filter.selects_everything());
        assert!(filter.admits("page-freeze") && filter.admits("mse-fmp4"));
        assert!(!filter.admits("mse-webm"));
    }

    #[test]
    fn repeated_ids_are_kept_once_in_the_given_order() {
        let filter = only(&["b", "a", "b"]);
        assert_eq!(filter.ids(), ["b", "a"]);
    }

    #[test]
    fn matching_candidates_needs_an_actual_intersection() {
        let filter = only(&["page-freeze"]);
        assert!(filter.matches_any_of(["source-withdrawn", "page-freeze"]));
        assert!(!filter.matches_any_of(["mse-fmp4"]));
        assert!(PageFilter::all().matches_any_of(["mse-fmp4"]));
    }
}
