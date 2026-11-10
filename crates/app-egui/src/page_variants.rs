//! Варианты страницы для окна браузерного режима: озвучки, качества и «следующая серия»
//! (сессия 14a, решения 279–283).
//!
//! Нейтральные типы между медиа-частью сеанса (`browser_session::session_media::page_variants`)
//! и окном (`state::window_mode::page_variants`, меню ♪, кнопка качества, кнопка Next): ни
//! моста, ни протокола, ни egui. Медиа-часть каждый проход собирает [`PageVariantFacts`], окно
//! хранит последнее слово и по нему рисует пункты. Выбор зрителя окно отдаёт обратно
//! [`PageVariantChoice`]: вид меню, его ревизия и номер пункта. Идентификаторы вариантов
//! страницы окну не нужны — по номеру пункта и ревизии медиа-часть сама найдёт, что слать.
//!
//! **Подписи — текст страницы** (инвариант 1): протокол уже не пускает управляющие символы и
//! знаки смены направления, а [`sanitize_page_variant_label`] — второй рубеж перед показом.
//! Длинные подписи обрезает интерфейс. В журнал подписи не попадают: только вид и номер.

use std::sync::Arc;

/// Вид меню вариантов, который показывает окно.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PageVariantKind {
    /// Озвучки (меню ♪).
    Audio,
    /// Качества (кнопка качества).
    Quality,
    /// Субтитры страницы (строки меню CC, сессия 15a2). Сам документ субтитров приходит
    /// отдельно, дорожкой 13b: меню лишь просит страницу включить язык.
    Subtitles,
}

impl PageVariantKind {
    /// Слово для журнала (`kind=audio`).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Quality => "quality",
            Self::Subtitles => "subtitles",
        }
    }

    /// Начало тоста и запасной подписи («Audio: …», «Quality 2»).
    pub(crate) const fn viewer_title(self) -> &'static str {
        match self {
            Self::Audio => "Audio",
            Self::Quality => "Quality",
            Self::Subtitles => "Subtitles",
        }
    }

    /// Рождает ли выбор пункта этого вида новое поколение медиа (и значит, нужен возврат
    /// секунды зрителя, решение 280). Озвучка и качество переоткрывают поток; субтитры —
    /// нет: страница просто скачивает ещё один документ, видео идёт как шло.
    pub(crate) const fn selection_restarts_media(self) -> bool {
        match self {
            Self::Audio | Self::Quality => true,
            Self::Subtitles => false,
        }
    }
}

/// Ревизия меню вариантов: растёт при каждой смене содержимого любого меню страницы.
///
/// Выбор, сделанный по меню старой ревизии, — устаревший: меню сменилось после того, как
/// зритель его открыл, и номер пункта мог уже значить другое (инвариант договора 14a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PageVariantMenuRevision(u64);

impl PageVariantMenuRevision {
    /// Ревизия с этим номером.
    pub(crate) const fn new(number: u64) -> Self {
        Self(number)
    }

    /// Номер ревизии.
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

/// Номер пункта меню в порядке страницы (с нуля). В журнал идёт он, а не подпись.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PageVariantIndex(usize);

impl PageVariantIndex {
    /// Пункт с этим номером.
    pub(crate) const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Номер пункта.
    pub(crate) const fn get(self) -> usize {
        self.0
    }
}

/// Выбрал ли этот пункт плеер страницы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageVariantSelection {
    /// Сейчас выбран на странице (отметка «играет»).
    Selected,
    /// Не выбран.
    Available,
}

/// Один пункт меню глазами окна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageVariantOptionView<'menu> {
    /// Номер пункта (его окно отдаёт обратно в [`PageVariantChoice`]).
    pub(crate) index: PageVariantIndex,
    /// Подпись для показа (уже очищена, но ещё не обрезана).
    pub(crate) label: &'menu str,
    /// Выбран ли на странице.
    pub(crate) selection: PageVariantSelection,
}

/// Меню одного вида глазами окна (дешёвая копия: подписи под `Arc`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PageVariantMenuView {
    kind: PageVariantKind,
    revision: PageVariantMenuRevision,
    labels: Arc<[Arc<str>]>,
    selected: Option<PageVariantIndex>,
}

impl PageVariantMenuView {
    /// Меню из уже очищенных подписей; `selected` — номер пункта, отмеченного страницей.
    /// Номер за пределами списка не отмечает ничего.
    pub(crate) fn new(
        kind: PageVariantKind,
        revision: PageVariantMenuRevision,
        labels: Vec<Arc<str>>,
        selected: Option<PageVariantIndex>,
    ) -> Self {
        let selected = selected.filter(|index| index.get() < labels.len());
        Self {
            kind,
            revision,
            labels: labels.into(),
            selected,
        }
    }

    /// Ревизия меню.
    pub(crate) const fn revision(&self) -> PageVariantMenuRevision {
        self.revision
    }

    /// Сколько пунктов.
    pub(crate) fn option_count(&self) -> usize {
        self.labels.len()
    }

    /// Пункт, отмеченный страницей (`None` — страница ничего не отметила).
    pub(crate) const fn selected(&self) -> Option<PageVariantIndex> {
        self.selected
    }

    /// Подпись пункта (`None` — такого номера нет).
    pub(crate) fn label(&self, index: PageVariantIndex) -> Option<&str> {
        self.labels.get(index.get()).map(AsRef::as_ref)
    }

    /// Пункты в порядке страницы.
    pub(crate) fn options(&self) -> impl Iterator<Item = PageVariantOptionView<'_>> {
        self.labels
            .iter()
            .enumerate()
            .map(|(position, label)| PageVariantOptionView {
                index: PageVariantIndex::new(position),
                label,
                selection: if self.selected == Some(PageVariantIndex::new(position)) {
                    PageVariantSelection::Selected
                } else {
                    PageVariantSelection::Available
                },
            })
    }

    /// Выбор пункта по этому меню (`None` — такого номера нет).
    pub(crate) fn choice(&self, index: PageVariantIndex) -> Option<PageVariantChoice> {
        (index.get() < self.labels.len()).then_some(PageVariantChoice {
            kind: self.kind,
            revision: self.revision,
            index,
        })
    }
}

/// Можно ли сейчас выбирать пункты меню страницы (меню ♪ и кнопка качества).
///
/// Пока на экране реклама, выбор озвучки и качества заблокирован, как перемотка (решения 206,
/// 283): окно рисует пункты неактивными с подсказкой, а не прячет меню.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageVariantAvailability {
    /// Выбирать можно.
    #[default]
    Selectable,
    /// На экране реклама: пункты видны, но неактивны.
    BlockedDuringAd,
}

impl PageVariantAvailability {
    /// Можно ли нажать пункт.
    pub(crate) const fn is_selectable(self) -> bool {
        matches!(self, Self::Selectable)
    }
}

/// Выбор зрителя: какой пункт какого меню и по какой ревизии меню он сделан.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PageVariantChoice {
    /// Вид меню.
    pub(crate) kind: PageVariantKind,
    /// Ревизия меню, которое видел зритель.
    pub(crate) revision: PageVariantMenuRevision,
    /// Номер пункта.
    pub(crate) index: PageVariantIndex,
}

/// Можно ли предложить зрителю «следующую серию» (кнопка Next, клавиша N, MPRIS Next).
///
/// Причины разведены, а не слиты в `bool`: окну важно лишь «предложено или нет», а журнал и
/// тесты различают «страница сказала нет», «экстеншен устарел» и «страницы нет».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NextEpisodeOffer {
    /// Предложено: страница сказала `page_navigation{available}`, она слушает, версия ≥ 1.7.
    Offered,
    /// Страница сказала «следующей серии нет» (или ещё ничего не сказала).
    PageSaysUnavailable,
    /// Общая версия протокола ниже 1.7: экстеншен не умеет `page_navigation`.
    ExtensionTooOld,
    /// Страница ещё не подключилась: версия неизвестна.
    #[default]
    NotAttached,
    /// Страницы сейчас нет (досмотр после ухода вкладки).
    PageAway,
}

impl NextEpisodeOffer {
    /// Предложена ли следующая серия.
    pub(crate) const fn is_offered(self) -> bool {
        matches!(self, Self::Offered)
    }
}

/// Что сеанс знает о вариантах страницы для окна на этот проход.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PageVariantFacts {
    /// Меню озвучек (`None` — страница его не прислала или оно пропало).
    pub(crate) audio: Option<PageVariantMenuView>,
    /// Меню качеств.
    pub(crate) quality: Option<PageVariantMenuView>,
    /// Меню субтитров страницы: родные дорожки ролика, затем автопереводы (15a2).
    pub(crate) subtitles: Option<PageVariantMenuView>,
    /// Можно ли предложить следующую серию.
    pub(crate) next_episode: NextEpisodeOffer,
}

impl PageVariantFacts {
    /// Меню этого вида.
    pub(crate) const fn menu(&self, kind: PageVariantKind) -> Option<&PageVariantMenuView> {
        match kind {
            PageVariantKind::Audio => self.audio.as_ref(),
            PageVariantKind::Quality => self.quality.as_ref(),
            PageVariantKind::Subtitles => self.subtitles.as_ref(),
        }
    }
}

/// Что сказать зрителю тостом после принятого действия (одно слово за шаг).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageVariantNotice {
    /// Выбор ушёл странице: «Audio: <подпись>» / «Quality: <подпись>».
    Selected {
        /// Вид меню.
        kind: PageVariantKind,
        /// Подпись выбранного пункта (очищенная).
        label: Arc<str>,
    },
    /// Просьба о следующей серии ушла странице: «Next episode».
    NextEpisodeRequested,
}

/// Текст тоста о следующей серии.
pub(crate) const NEXT_EPISODE_TOAST: &str = "Next episode";

impl PageVariantNotice {
    /// Текст тоста для зрителя (английский, как весь интерфейс).
    pub(crate) fn toast_text(&self) -> String {
        match self {
            Self::Selected { kind, label } => format!("{}: {label}", kind.viewer_title()),
            Self::NextEpisodeRequested => NEXT_EPISODE_TOAST.to_owned(),
        }
    }
}

/// Убирает из подписи страницы то, что нельзя показывать как есть: управляющие символы,
/// разделители строк и абзацев Unicode и знаки смены направления (встраивание, подмена,
/// изоляция и метки LRM/RLM/ALM). Подменой направления подпись могла бы «перевернуть»
/// соседний текст меню. Края без пробелов; обрезку длинных подписей делает интерфейс.
pub(crate) fn sanitize_page_variant_label(raw: &str) -> String {
    raw.chars()
        .filter(|character| !is_hidden_from_viewer(*character))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Символ, которому не место в подписи меню (см. [`sanitize_page_variant_label`]).
const fn is_hidden_from_viewer(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            // Разделители строки и абзаца.
            '\u{2028}' | '\u{2029}'
            // Встраивание и подмена направления (LRE, RLE, PDF, LRO, RLO).
            | '\u{202A}'..='\u{202E}'
            // Изоляция направления (LRI, RLI, FSI, PDI).
            | '\u{2066}'..='\u{2069}'
            // Метки направления (LRM, RLM, ALM).
            | '\u{200E}' | '\u{200F}' | '\u{061C}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(texts: &[&str]) -> Vec<Arc<str>> {
        texts.iter().map(|text| Arc::from(*text)).collect()
    }

    #[test]
    fn control_and_direction_characters_are_removed_and_the_rest_is_kept() {
        assert_eq!(
            sanitize_page_variant_label("\u{202E}Укр\u{0007}аїнська\u{2066} \u{200F}"),
            "Українська"
        );
        assert_eq!(sanitize_page_variant_label("1080p\nHDR"), "1080pHDR");
        assert_eq!(
            sanitize_page_variant_label("English (auto)"),
            "English (auto)"
        );
        assert_eq!(sanitize_page_variant_label("\u{2028}\u{202A}"), "");
    }

    #[test]
    fn options_mark_only_the_selected_index_and_choices_carry_the_revision() {
        let menu = PageVariantMenuView::new(
            PageVariantKind::Audio,
            PageVariantMenuRevision::new(7),
            labels(&["Original", "Dub"]),
            Some(PageVariantIndex::new(1)),
        );
        let marks: Vec<_> = menu.options().map(|option| option.selection).collect();
        assert_eq!(
            marks,
            [
                PageVariantSelection::Available,
                PageVariantSelection::Selected
            ]
        );
        assert_eq!(menu.label(PageVariantIndex::new(1)), Some("Dub"));
        assert_eq!(
            menu.choice(PageVariantIndex::new(0)),
            Some(PageVariantChoice {
                kind: PageVariantKind::Audio,
                revision: PageVariantMenuRevision::new(7),
                index: PageVariantIndex::new(0),
            })
        );
        assert_eq!(menu.choice(PageVariantIndex::new(2)), None);
    }

    #[test]
    fn a_selected_index_outside_the_list_marks_nothing() {
        let menu = PageVariantMenuView::new(
            PageVariantKind::Quality,
            PageVariantMenuRevision::new(1),
            labels(&["720p"]),
            Some(PageVariantIndex::new(3)),
        );
        assert_eq!(menu.selected(), None);
    }

    #[test]
    fn toasts_name_the_kind_and_the_label() {
        let audio = PageVariantNotice::Selected {
            kind: PageVariantKind::Audio,
            label: Arc::from("Dub"),
        };
        assert_eq!(audio.toast_text(), "Audio: Dub");
        let quality = PageVariantNotice::Selected {
            kind: PageVariantKind::Quality,
            label: Arc::from("720p"),
        };
        assert_eq!(quality.toast_text(), "Quality: 720p");
        assert_eq!(
            PageVariantNotice::NextEpisodeRequested.toast_text(),
            "Next episode"
        );
    }

    #[test]
    fn the_default_facts_offer_nothing() {
        let facts = PageVariantFacts::default();
        assert_eq!(facts.menu(PageVariantKind::Audio), None);
        assert_eq!(facts.next_episode, NextEpisodeOffer::NotAttached);
        assert!(!facts.next_episode.is_offered());
    }
}
