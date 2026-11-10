//! Строки меню субтитров страницы в меню CC (сессия 15a2, решения 292–293).
//!
//! # Что здесь
//! - [`SubtitleOwner::menu_view_with_page_menu`] — снимок меню CC с учётом меню страницы:
//!   строки страницы идут перед дорожками, а дорожки, которые скачала сама страница
//!   (`page_download`), из списка убираются: их представляет строка меню, дубль не нужен.
//!   Свои файлы, встроенные дорожки и дорожки страницы другого происхождения остаются.
//! - [`SubtitleOwner::expect_page_document`] / [`SubtitleOwner::cancel_page_document_wait`] —
//!   «жду документ выбранной строки» (`page_line_wait`): читает его `attach_page_tracks`.
//! - [`next_native_line_step`] — чистое правило клавиши V по родным строкам страницы.
//!
//! # Отметка строки
//! Строка отмечена, когда наши субтитры показываются, страница отметила именно её и показана
//! не другая (видимая) дорожка списка: свой файл или встроенная дорожка, выбранные зрителем,
//! отметку забирают. Две радио-отметки сразу в одном меню были бы враньём.
//!
//! # Автоперевод
//! Протокол несёт у строки только идентификатор, подпись и отметку, а автопереводы идут после
//! родных дорожек. Различаем по подписи: адаптер страницы подписывает перевод «(auto-translated)»
//! (общая функция подписей экстеншена, план 15a2). Подпись без такой отметки — родная строка.

use std::collections::HashSet;

use subtitle_core::SubtitleTrackId;

use super::SubtitleOwner;
use super::intents::SubtitleVisibility;
use super::menu_view::{PageSubtitleLine, SubtitleMenuView};
use crate::page_variants::{PageVariantAvailability, PageVariantMenuView, PageVariantSelection};

/// Отметка подписи автоперевода (регистр не важен).
const AUTO_TRANSLATED_MARK: &str = "(auto-translated)";

/// Подпись страницы называет строку автопереводом.
pub(super) fn is_auto_translated_label(label: &str) -> bool {
    label.to_lowercase().contains(AUTO_TRANSLATED_MARK)
}

/// Что делает клавиша V при меню страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeLineStep {
    /// Выбрать родную строку с этим номером в списке строк.
    Select(usize),
    /// Родные строки кончились: выключить субтитры (дальше цикл начнётся заново).
    TurnOff,
    /// Выбирать нечего (строк нет).
    NothingToSelect,
}

/// Следующий шаг клавиши V по строкам страницы: родные строки по порядку, затем «выкл», затем
/// снова первая. Автопереводы в цикле не участвуют (их 150+, они только в меню). Если родных
/// строк нет вовсе (меню из одних переводов), ходим по всем: иначе V ничего бы не делала.
pub(crate) fn next_native_line_step(lines: &[PageSubtitleLine]) -> NativeLineStep {
    if lines.is_empty() {
        return NativeLineStep::NothingToSelect;
    }
    let native_count = lines
        .iter()
        .position(|line| line.is_auto_translation)
        .filter(|count| *count > 0)
        .unwrap_or(lines.len());
    match lines.iter().position(|line| line.is_selected) {
        None => NativeLineStep::Select(0),
        Some(current) if current + 1 < native_count => NativeLineStep::Select(current + 1),
        // Последняя родная строка или уже автоперевод: круг закончен.
        Some(_) => NativeLineStep::TurnOff,
    }
}

/// Клавиша C при меню страницы, пока у медиа нет ни одной дорожки (YouTube ещё ничего не скачал):
/// показывать и прятать нечего, поэтому C включает субтитры страницы — первым шагом V (первая родная
/// строка). Иначе тост «No subtitles for this video» соврал бы: субтитры у ролика есть, просто не
/// скачаны. `None` — C работает как раньше (дорожки есть или меню страницы нет).
pub(crate) fn page_toggle_step(
    lines: &[PageSubtitleLine],
    has_tracks: bool,
) -> Option<NativeLineStep> {
    if has_tracks || lines.is_empty() {
        return None;
    }
    Some(next_native_line_step(lines))
}

impl SubtitleOwner {
    /// Снимок меню CC вместе с меню страницы (`None` — страница меню не прислала: снимок
    /// такой же, как без страницы). Нет media — строки некому показывать.
    pub(super) fn menu_view_with_page_menu(
        &self,
        page_menu: Option<&PageVariantMenuView>,
        availability: PageVariantAvailability,
    ) -> SubtitleMenuView {
        let mut view = self.menu_view();
        let (Some(page_menu), Some(media)) = (page_menu, self.media.as_ref()) else {
            return view;
        };
        // Дорожки, которые скачала страница, представляет её меню.
        let downloaded_by_page: HashSet<SubtitleTrackId> = media
            .tracks
            .entries()
            .iter()
            .filter(|entry| {
                entry
                    .description
                    .page()
                    .is_some_and(|page| page.is_page_download())
            })
            .map(|entry| entry.id)
            .collect();
        view.tracks
            .retain(|track| !downloaded_by_page.contains(&track.id));
        let listed_track_is_selected = view.tracks.iter().any(|track| track.is_selected);
        let marks_page_line =
            media.visibility == SubtitleVisibility::Shown && !listed_track_is_selected;
        view.page_lines = page_menu
            .options()
            .filter_map(|option| {
                Some(PageSubtitleLine {
                    choice: page_menu.choice(option.index)?,
                    label: option.label.into(),
                    is_selected: marks_page_line
                        && option.selection == PageVariantSelection::Selected,
                    is_auto_translation: is_auto_translated_label(option.label),
                })
            })
            .collect();
        view.page_lines_availability = availability;
        view
    }

    /// Зритель выбрал строку меню страницы и просьба ушла: ждём её документ (правила —
    /// `page_line_wait`). Показ до прихода документа не трогается.
    pub(super) fn expect_page_document(&mut self, line_label: &str) {
        self.page_viewer.expect_page_document(line_label);
    }

    /// Снять ожидание документа строки (зритель запросил следующую серию).
    pub(super) fn cancel_page_document_wait(&mut self) {
        self.page_viewer.cancel_document_wait();
    }

    /// Ждёт ли владелец документ выбранной строки (для тестов и журнала).
    #[cfg(test)]
    pub(super) fn is_waiting_for_page_document(&self) -> bool {
        self.page_viewer.is_waiting_for_document()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::page_variants::{
        PageVariantChoice, PageVariantIndex, PageVariantKind, PageVariantMenuRevision,
    };

    fn line(label: &str, is_selected: bool) -> PageSubtitleLine {
        PageSubtitleLine {
            choice: PageVariantChoice {
                kind: PageVariantKind::Subtitles,
                revision: PageVariantMenuRevision::new(1),
                index: PageVariantIndex::new(0),
            },
            label: Arc::from(label),
            is_selected,
            is_auto_translation: is_auto_translated_label(label),
        }
    }

    #[test]
    fn auto_translation_is_recognised_by_the_mark_in_any_case() {
        assert!(is_auto_translated_label("German (auto-translated)"));
        assert!(is_auto_translated_label("german (Auto-Translated)"));
        assert!(!is_auto_translated_label("English (auto-generated)"));
        assert!(!is_auto_translated_label("English"));
    }

    #[test]
    fn v_walks_native_lines_then_off_then_starts_again() {
        let mut lines = vec![
            line("English", false),
            line("English (auto-generated)", false),
            line("German (auto-translated)", false),
        ];
        assert_eq!(next_native_line_step(&lines), NativeLineStep::Select(0));
        lines[0].is_selected = true;
        assert_eq!(next_native_line_step(&lines), NativeLineStep::Select(1));
        lines[0].is_selected = false;
        lines[1].is_selected = true;
        assert_eq!(
            next_native_line_step(&lines),
            NativeLineStep::TurnOff,
            "translations are not part of the V cycle"
        );
        lines[1].is_selected = false;
        lines[2].is_selected = true;
        assert_eq!(next_native_line_step(&lines), NativeLineStep::TurnOff);
    }

    #[test]
    fn c_without_tracks_turns_on_the_first_native_page_line() {
        let lines = vec![
            line("English", false),
            line("German (auto-translated)", false),
        ];
        assert_eq!(
            page_toggle_step(&lines, false),
            Some(NativeLineStep::Select(0))
        );
    }

    #[test]
    fn c_with_tracks_or_without_page_lines_keeps_its_old_meaning() {
        let lines = vec![line("English", true)];
        assert_eq!(
            page_toggle_step(&lines, true),
            None,
            "tracks exist: C shows and hides them"
        );
        assert_eq!(
            page_toggle_step(&[], false),
            None,
            "no page menu: the old toast stays"
        );
    }

    #[test]
    fn v_without_native_lines_walks_everything_and_without_lines_does_nothing() {
        let mut lines = vec![
            line("German (auto-translated)", false),
            line("French (auto-translated)", false),
        ];
        assert_eq!(next_native_line_step(&lines), NativeLineStep::Select(0));
        lines[0].is_selected = true;
        assert_eq!(next_native_line_step(&lines), NativeLineStep::Select(1));
        assert_eq!(next_native_line_step(&[]), NativeLineStep::NothingToSelect);
    }
}
