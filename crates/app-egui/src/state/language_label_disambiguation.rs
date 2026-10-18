//! Различимые подписи языков: два пункта меню не должны называться одинаково.
//!
//! # Зачем
//! Название языка берётся из таблицы по основному подтегу, поэтому `zh-CN` и `zh-TW`, `pt-BR` и
//! `pt-PT`, `es-419` и `es` оба получали бы «Chinese», «Portuguese», «Spanish». Пользователь не
//! может отличить строки друг от друга.
//!
//! # Правило
//! - Название без двойника остаётся коротким («English», «Russian»): регион не нужен.
//! - Если у нескольких пунктов одно базовое название, каждый получает уточнённое:
//!   подробное имя от источника (`Chinese (Taiwan)`), когда оно начинается с базового названия и
//!   длиннее его; иначе базовое имя с тегом (`Chinese (zh-TW)`).
//! - Если и после этого имена в группе совпали (источник назвал разные дорожки одинаково),
//!   вся группа получает форму «имя (тег)»: теги внутри одного меню различны.
//!
//! Модуль чистый: не знает ни про субтитры, ни про звук; им пользуются оба меню.

use std::collections::HashMap;

/// Один пункт меню на входе: что известно о его языке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LanguageNameInput {
    /// Базовое название (из таблицы языков либо запасное); по нему ищутся двойники.
    pub(crate) base_name: String,
    /// Нормализованный языковой тег пункта (`zh-TW`); различим в пределах одного меню.
    pub(crate) language_tag: String,
    /// Подробное имя от источника (`Chinese (Taiwan)`), если оно есть.
    pub(crate) source_name: Option<String>,
}

/// Итоговые подписи в том же порядке, что и вход; все различны, если различны теги.
pub(crate) fn distinguish_language_names(inputs: &[LanguageNameInput]) -> Vec<String> {
    let mut group_sizes: HashMap<String, usize> = HashMap::new();
    for input in inputs {
        *group_sizes.entry(group_key(input)).or_default() += 1;
    }
    let mut names: Vec<String> = inputs
        .iter()
        .map(|input| {
            if group_sizes[&group_key(input)] > 1 {
                detailed_name(input)
            } else {
                input.base_name.clone()
            }
        })
        .collect();
    // Источник мог назвать разные дорожки одинаково: тогда в такой группе остаётся только тег.
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for name in &names {
        *name_counts.entry(name.to_lowercase()).or_default() += 1;
    }
    for (name, input) in names.iter_mut().zip(inputs) {
        if name_counts[&name.to_lowercase()] > 1 {
            *name = tagged_name(input);
        }
    }
    names
}

/// Ключ группы двойников: базовое название без учёта регистра.
fn group_key(input: &LanguageNameInput) -> String {
    input.base_name.to_lowercase()
}

/// Имя источника, если оно действительно уточняет базовое; иначе «имя (тег)».
fn detailed_name(input: &LanguageNameInput) -> String {
    let base_lower = input.base_name.to_lowercase();
    input
        .source_name
        .as_deref()
        .map(str::trim)
        .filter(|name| {
            let name_lower = name.to_lowercase();
            name_lower.starts_with(&base_lower)
                && name_lower.chars().count() > base_lower.chars().count()
        })
        .map_or_else(|| tagged_name(input), str::to_owned)
}

/// «Chinese (zh-TW)».
fn tagged_name(input: &LanguageNameInput) -> String {
    format!("{} ({})", input.base_name, input.language_tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(base: &str, tag: &str, source: Option<&str>) -> LanguageNameInput {
        LanguageNameInput {
            base_name: base.to_owned(),
            language_tag: tag.to_owned(),
            source_name: source.map(str::to_owned),
        }
    }

    #[test]
    fn unique_names_stay_short_even_with_region_and_source_name() {
        let names = distinguish_language_names(&[
            input("English", "en-US", Some("English (United States)")),
            input("Russian", "ru", None),
        ]);
        assert_eq!(names, ["English", "Russian"]);
    }

    #[test]
    fn twins_use_the_detailed_source_name() {
        let names = distinguish_language_names(&[
            input("Chinese", "zh-CN", Some("Chinese (China)")),
            input("Chinese", "zh-TW", Some("Chinese (Taiwan)")),
            input("French", "fr", None),
        ]);
        assert_eq!(names, ["Chinese (China)", "Chinese (Taiwan)", "French"]);
    }

    #[test]
    fn twins_without_a_useful_source_name_get_the_tag() {
        let names = distinguish_language_names(&[
            input("Portuguese", "pt-BR", None),
            // Имя на другом языке не уточняет базовое: берём тег.
            input("Portuguese", "pt-PT", Some("Português (Portugal)")),
            // Имя не длиннее базового: ничего не уточняет.
            input("Spanish", "es", Some("Spanish")),
            input("Spanish", "es-419", Some("Spanish (Latin America)")),
        ]);
        assert_eq!(
            names,
            [
                "Portuguese (pt-BR)",
                "Portuguese (pt-PT)",
                "Spanish (es)",
                "Spanish (Latin America)"
            ]
        );
    }

    #[test]
    fn identical_source_names_fall_back_to_tags_for_the_whole_group() {
        let names = distinguish_language_names(&[
            input("Chinese", "zh-Hans", Some("Chinese (Chinese)")),
            input("Chinese", "zh-Hant", Some("chinese (chinese)")),
        ]);
        assert_eq!(names, ["Chinese (zh-Hans)", "Chinese (zh-Hant)"]);
    }

    #[test]
    fn empty_input_gives_empty_output() {
        assert!(distinguish_language_names(&[]).is_empty());
    }
}
