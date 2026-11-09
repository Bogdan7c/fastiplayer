//! Дорожка субтитров страницы (мост, сессия 13b1) в списке владельца: описание, подпись меню,
//! признаки автовыбора.
//!
//! # Владение
//! Модуль чистый: документ (байты и метаданные) принадлежит складу сеанса и приходит сюда
//! общим `Arc` — описание его не копирует. Разбор байтов — фоновая задача `page_load_job`.
//!
//! # Подпись
//! Имя строки меню: подпись страницы («English (auto-generated)»), иначе название языка из
//! таблицы, иначе сам тег, иначе «Track N». Двойники (две дорожки с одним именем) различает
//! общий модуль `language_label_disambiguation`, как у web-дорожек; если и после него имена
//! совпали (страница назвала разные дорожки одинаково при одном языке), добавляется номер
//! дорожки страницы `#N` — номера внутри сеанса различны всегда.

use std::collections::HashMap;
use std::sync::Arc;

use subtitle_core::SubtitleLanguage;

use super::menu_view::{SubtitleFileOrigin, SubtitleTrackLabel};
use crate::page_subtitles::{PageSubtitleDisplay, PageSubtitleDocument, PageSubtitleTrackNumber};
use crate::state::language_label_disambiguation::{LanguageNameInput, distinguish_language_names};

/// Описание дорожки страницы в списке владельца.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PageTrackDescription {
    /// Документ со склада (общий `Arc`, байты не копируются).
    document: Arc<PageSubtitleDocument>,
    /// Язык в нормализованном виде (разобран один раз); `None` — не назван или не разбирается.
    language: Option<SubtitleLanguage>,
    /// Различимое имя, если у нескольких дорожек страницы совпало имя; выставляет
    /// [`distinguish_page_track_names`].
    distinguishing_name: Option<String>,
}

impl PageTrackDescription {
    /// Описание документа со склада.
    pub(super) fn new(document: Arc<PageSubtitleDocument>) -> Self {
        let language = document
            .language
            .tag()
            .and_then(|tag| SubtitleLanguage::parse(tag).ok());
        Self {
            document,
            language,
            distinguishing_name: None,
        }
    }

    /// Номер дорожки страницы.
    pub(super) fn number(&self) -> PageSubtitleTrackNumber {
        self.document.track
    }

    /// Документ для задачи разбора.
    pub(super) fn document(&self) -> &Arc<PageSubtitleDocument> {
        &self.document
    }

    /// Язык для автовыбора, кодировки и шрифта.
    pub(super) fn language(&self) -> Option<&SubtitleLanguage> {
        self.language.as_ref()
    }

    /// Страница показывала эту дорожку, когда её предложила.
    pub(super) fn was_showing_on_page(&self) -> bool {
        self.document.display == PageSubtitleDisplay::ShowingOnPage
    }

    /// Части подписи для меню; текст собирает UI (`subtitle_message::track_text`).
    ///
    /// Формата в подписи нет: подсказка страницы — не обещание, а настоящий формат узнаётся
    /// только разбором.
    pub(super) fn label(&self) -> SubtitleTrackLabel {
        SubtitleTrackLabel {
            language: self.language.clone(),
            format: None,
            origin: SubtitleFileOrigin::Page,
            is_forced: false,
            is_hearing_impaired: false,
            title: None,
            file_name: None,
            // Подпись страницы — главное имя строки (главнее таблицы языков).
            fallback_name: self.document.label.text().map(str::to_owned),
            distinguishing_name: self.distinguishing_name.clone(),
        }
    }

    /// Имя строки без уточнений: подпись страницы, иначе название языка, иначе тег, иначе
    /// `None` (тогда UI пишет «Track N»).
    fn base_name(&self) -> Option<String> {
        self.document
            .label
            .text()
            .map(str::to_owned)
            .or_else(|| {
                self.language
                    .as_ref()
                    .and_then(SubtitleLanguage::english_display_name)
                    .map(str::to_owned)
            })
            .or_else(|| self.document.language.tag().map(str::to_owned))
    }
}

/// Даёт различимые имена дорожкам страницы с одинаковым именем (см. документацию модуля).
/// Дорожка без двойника имени не меняет (`distinguishing_name = None`).
pub(super) fn distinguish_page_track_names(descriptions: &mut [&mut PageTrackDescription]) {
    // Дорожки без имени получают «Track N» по месту в меню и двойников не имеют.
    let named: Vec<usize> = (0..descriptions.len())
        .filter(|&index| descriptions[index].base_name().is_some())
        .collect();
    let inputs: Vec<LanguageNameInput> = named
        .iter()
        .map(|&index| {
            let description = &descriptions[index];
            LanguageNameInput {
                base_name: description.base_name().unwrap_or_default(),
                language_tag: description
                    .document
                    .language
                    .tag()
                    .map_or_else(|| format!("#{}", description.number().get()), str::to_owned),
                source_name: None,
            }
        })
        .collect();
    let mut names = distinguish_language_names(&inputs);
    append_track_numbers_to_remaining_twins(&mut names, &named, descriptions);
    for description in descriptions.iter_mut() {
        description.distinguishing_name = None;
    }
    for ((&index, name), input) in named.iter().zip(names).zip(&inputs) {
        descriptions[index].distinguishing_name = (name != input.base_name).then_some(name);
    }
}

/// Имена, которые и после различения совпали, получают номер дорожки страницы `#N`.
fn append_track_numbers_to_remaining_twins(
    names: &mut [String],
    named: &[usize],
    descriptions: &[&mut PageTrackDescription],
) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for name in names.iter() {
        *counts.entry(name.to_lowercase()).or_default() += 1;
    }
    for (name, &index) in names.iter_mut().zip(named) {
        if counts
            .get(&name.to_lowercase())
            .copied()
            .unwrap_or_default()
            > 1
        {
            *name = format!("{name} #{}", descriptions[index].number().get());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page_subtitles::{PageSubtitleFormatHint, PageSubtitleLabel, PageSubtitleLanguage};

    fn description(
        track: u32,
        language: Option<&str>,
        label: Option<&str>,
    ) -> PageTrackDescription {
        PageTrackDescription::new(Arc::new(PageSubtitleDocument {
            track: PageSubtitleTrackNumber::new(track),
            language: language.map_or(PageSubtitleLanguage::NotNamed, |tag| {
                PageSubtitleLanguage::Tagged(tag.to_owned())
            }),
            label: label.map_or(PageSubtitleLabel::NotGiven, |text| {
                PageSubtitleLabel::Text(text.to_owned())
            }),
            format_hint: PageSubtitleFormatHint::WebVtt,
            display: PageSubtitleDisplay::NotShowingOnPage,
            bytes: Arc::from(b"WEBVTT".as_slice()),
        }))
    }

    fn names_after_distinguishing(
        mut descriptions: Vec<PageTrackDescription>,
    ) -> Vec<Option<String>> {
        let mut refs: Vec<&mut PageTrackDescription> = descriptions.iter_mut().collect();
        distinguish_page_track_names(&mut refs);
        descriptions
            .iter()
            .map(|description| description.label().distinguishing_name)
            .collect()
    }

    #[test]
    fn the_page_label_names_the_row_and_the_language_is_parsed_for_auto_select() {
        let page = description(1, Some("ru"), Some("Русские (от автора)"));
        let label = page.label();
        assert_eq!(label.fallback_name.as_deref(), Some("Русские (от автора)"));
        assert_eq!(label.origin, SubtitleFileOrigin::Page);
        assert_eq!(page.language().map(SubtitleLanguage::as_str), Some("ru"));
    }

    #[test]
    fn unique_names_stay_short_and_twins_get_told_apart() {
        let names = names_after_distinguishing(vec![
            description(1, Some("en"), None),
            description(2, Some("ru"), None),
        ]);
        assert_eq!(names, vec![None, None], "no twins, no change");

        let names = names_after_distinguishing(vec![
            description(1, Some("pt-BR"), None),
            description(2, Some("pt-PT"), None),
        ]);
        assert_eq!(
            names,
            vec![
                Some("Portuguese (pt-BR)".to_owned()),
                Some("Portuguese (pt-PT)".to_owned())
            ]
        );
    }

    #[test]
    fn identical_labels_with_the_same_language_get_the_page_track_number() {
        let names = names_after_distinguishing(vec![
            description(3, Some("en"), Some("English")),
            description(7, Some("en"), Some("English")),
            description(9, None, None),
        ]);
        assert_eq!(
            names,
            vec![
                Some("English (en) #3".to_owned()),
                Some("English (en) #7".to_owned()),
                None
            ]
        );
    }
}
