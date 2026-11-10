//! Субтитры страницы: нейтральные типы между сеансом моста, окном и владельцем субтитров
//! (мост, сессия 13b1, решения 270–274).
//!
//! Путь документа:
//!
//! ```text
//! страница ── subtitle_track_offered + рамки вида 3 ──► сборщик (browser_session, поток приёма)
//!          ──► готовый [`PageSubtitleDocument`] ──► склад сеанса (живёт весь сеанс)
//!          ──► [`PageSubtitleShelfView`] каждый проход окна ──► владелец субтитров (дорожки
//!              ТЕКУЩЕГО медиа; новое медиа получает весь склад заново)
//! ```
//!
//! Модуль ничего не знает ни о протоколе, ни о сеансе, ни о владельце субтитров — только
//! словарь и строки журнала. Протокольные типы переводит в эти сборщик сеанса.
//!
//! ПРИВАТНОСТЬ (инвариант 7 моста): байты документа и подпись страницы — содержимое
//! страницы, в журнал они не попадают никогда. `Debug` документа печатает только номер,
//! язык, подсказку формата и длину.

use std::fmt;
use std::sync::Arc;

/// Строка журнала: склад сеанса принял готовый документ (поля `track`, `bytes`,
/// `format_hint`, `page_display`, `replaced`).
pub(crate) const PAGE_SUBTITLE_DOCUMENT_LINE: &str = "Page subtitle document received";

/// Строка журнала: дорожка страницы разобрана и готова к показу (поля `track`, `language`,
/// `format`, `cues`, `selected`).
pub(crate) const PAGE_SUBTITLE_TRACK_READY_LINE: &str = "Page subtitle track ready";

/// Строка журнала: дорожка страницы не разобралась (поля `track`, `reason`); остальные живут.
pub(crate) const PAGE_SUBTITLE_TRACK_FAILED_LINE: &str = "Page subtitle track failed";

/// Номер дорожки страницы (его выдаёт экстеншен на сеанс; тот же номер = замена документа).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PageSubtitleTrackNumber(u32);

impl PageSubtitleTrackNumber {
    /// Номер как его назвала страница (протокол уже проверил, что он ≥ 1).
    pub(crate) const fn new(number: u32) -> Self {
        Self(number)
    }

    /// Число для журнала и тестов.
    pub(crate) const fn get(self) -> u32 {
        self.0
    }
}

/// Язык дорожки по словам страницы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PageSubtitleLanguage {
    /// Страница язык не назвала.
    NotNamed,
    /// Тег BCP-47 (форму уже проверил протокол: ASCII, буквы, цифры и дефисы).
    Tagged(String),
}

impl PageSubtitleLanguage {
    /// Тег, если страница его назвала.
    pub(crate) fn tag(&self) -> Option<&str> {
        match self {
            Self::NotNamed => None,
            Self::Tagged(tag) => Some(tag),
        }
    }
}

/// Подпись дорожки для человека (текст страницы — только в меню, не в журнал).
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum PageSubtitleLabel {
    /// Подписи нет: меню назовёт дорожку по языку.
    NotGiven,
    /// Подпись страницы.
    Text(String),
}

impl PageSubtitleLabel {
    /// Текст подписи, если он есть и не пуст после обрезки пробелов.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Self::NotGiven => None,
            Self::Text(text) => Some(text.trim()).filter(|text| !text.is_empty()),
        }
    }
}

impl fmt::Debug for PageSubtitleLabel {
    /// Только факт подписи: её текст — содержимое страницы.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotGiven => formatter.write_str("NotGiven"),
            Self::Text(_) => formatter.write_str("Text(..)"),
        }
    }
}

/// Формат документа по мнению экстеншена. Это подсказка: плеер верит содержимому, если оно
/// уверенно опознаётся.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageSubtitleFormatHint {
    /// WebVTT.
    WebVtt,
    /// SubRip.
    Srt,
    /// TTML / IMSC / DFXP.
    Ttml,
    /// XML-формат YouTube srv3.
    Srv3,
    /// JSON-формат YouTube json3.
    Json3,
    /// Экстеншен формата не знает: решает только содержимое.
    Unknown,
}

impl PageSubtitleFormatHint {
    /// Стабильная метка для журнала.
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::WebVtt => "web_vtt",
            Self::Srt => "srt",
            Self::Ttml => "ttml",
            Self::Srv3 => "srv3",
            Self::Json3 => "json3",
            Self::Unknown => "unknown",
        }
    }
}

/// Была ли дорожка включена на странице, когда её предложили (решение 271: «скачала — значит
/// включено»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageSubtitleDisplay {
    /// Страница показывала эту дорожку: плеер её выбирает и включает, пока зритель сам не
    /// выбирал.
    ShowingOnPage,
    /// Дорожка была скачана или видна, но не показывалась.
    NotShowingOnPage,
}

impl PageSubtitleDisplay {
    /// Стабильная метка для журнала.
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::ShowingOnPage => "showing",
            Self::NotShowingOnPage => "not_showing",
        }
    }
}

/// Откуда экстеншен взял документ (15a2): важно меню CC, чтобы не дублировать строки.
///
/// Документ, который скачала сама страница (YouTube качает субтитры выбранного языка), в меню
/// CC представляет строка меню страницы; остальные документы (элемент `<track>`, реплики
/// `TextTrack`) строки не имеют и остаются обычными дорожками.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageSubtitleSource {
    /// Страница сама скачала файл субтитров (протокол `page_download`).
    PageDownload,
    /// Элемент `<track>` или реплики `TextTrack`, собранные скриптом страницы.
    PageElement,
}

/// Готовый документ дорожки страницы: метаданные предложения и все его байты.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PageSubtitleDocument {
    /// Номер дорожки.
    pub(crate) track: PageSubtitleTrackNumber,
    /// Язык.
    pub(crate) language: PageSubtitleLanguage,
    /// Подпись страницы.
    pub(crate) label: PageSubtitleLabel,
    /// Подсказка формата.
    pub(crate) format_hint: PageSubtitleFormatHint,
    /// Показывалась ли дорожка на странице.
    pub(crate) display: PageSubtitleDisplay,
    /// Откуда экстеншен взял документ.
    pub(crate) source: PageSubtitleSource,
    /// Байты документа. `Arc`: склад, окно и задача разбора делят одну копию.
    pub(crate) bytes: Arc<[u8]>,
}

impl fmt::Debug for PageSubtitleDocument {
    /// Без байтов и без текста подписи (содержимое страницы).
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PageSubtitleDocument")
            .field("track", &self.track.get())
            .field("language", &self.language.tag())
            .field("label", &self.label)
            .field("format_hint", &self.format_hint)
            .field("display", &self.display)
            .field("source", &self.source)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Какой это склад: новый сеанс в том же окне — новый склад с новым номером.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PageSubtitleShelfId(u64);

impl PageSubtitleShelfId {
    /// Номер первого склада процесса.
    pub(crate) const FIRST: Self = Self(0);

    /// Номер следующего склада.
    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Порядок поступления документа на склад (больше — позже). Замена дорожки получает новый
/// номер: по нему потребитель видит, что документ сменился, и знает, какой `showing` новее.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct PageSubtitleArrival(u64);

impl PageSubtitleArrival {
    /// Самое первое поступление склада.
    pub(crate) const FIRST: Self = Self(0);

    /// Следующее поступление.
    pub(crate) const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Подсказка страницы о субтитрах этой серии (`subtitle_preference`, протокол 1.7, 14a):
/// экстеншен помнит, что зритель выбрал в прошлых сериях, и напоминает плееру.
///
/// Порядок силы у владельца субтитров: действие зрителя в этом сеансе > подсказка страницы >
/// `showing` страницы (13b) > наш автовыбор. Последнее слово страницы главнее и действует на
/// текущее и следующие поколения сеанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PageSubtitlePreference {
    /// Страница ничего не подсказывала.
    #[default]
    NotGiven,
    /// Включить дорожку страницы с этим номером (ждёт, пока дорожка придёт).
    PageTrack(PageSubtitleTrackNumber),
    /// Субтитры выключены.
    Off,
}

impl PageSubtitlePreference {
    /// Слово для журнала (`preference=page_track`).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::NotGiven => "not_given",
            Self::PageTrack(_) => "page_track",
            Self::Off => "off",
        }
    }
}

/// Выбор зрителя, который плеер сообщает странице для её памяти (`subtitle_choice`, 1.7).
///
/// Только то, что экстеншен может запомнить: дорожка страницы или «выключено». Свой файл и
/// встроенная дорожка сюда не попадают — у страницы их нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageSubtitleViewerChoice {
    /// Зритель выбрал (или включил) дорожку страницы с этим номером.
    PageTrack(PageSubtitleTrackNumber),
    /// Зритель выключил субтитры.
    Off,
}

impl PageSubtitleViewerChoice {
    /// Слово для журнала (`choice=off`).
    pub(crate) const fn log_label(self) -> &'static str {
        match self {
            Self::PageTrack(_) => "page_track",
            Self::Off => "off",
        }
    }
}

/// Документ на складе и когда он поступил.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShelvedPageSubtitle {
    /// Порядок поступления.
    pub(crate) arrival: PageSubtitleArrival,
    /// Документ (общий для всех читателей склада).
    pub(crate) document: Arc<PageSubtitleDocument>,
}

/// Снимок склада для окна: дешёвая копия (`Arc`), сравнивается по номеру склада,
/// последнему поступлению и подсказке страницы.
#[derive(Debug, Clone)]
pub(crate) struct PageSubtitleShelfView {
    shelf: PageSubtitleShelfId,
    entries: Arc<[ShelvedPageSubtitle]>,
    /// Последняя подсказка страницы этого сеанса (14a). Едет вместе со складом: у них одна
    /// жизнь (сеанс) и один номер, поэтому подсказка нового сеанса не достанется старому.
    preference: PageSubtitlePreference,
}

impl PageSubtitleShelfView {
    /// Снимок склада `shelf` с этими документами (по дорожке — не больше одного, в порядке
    /// первого появления дорожки).
    pub(crate) fn new(shelf: PageSubtitleShelfId, entries: Vec<ShelvedPageSubtitle>) -> Self {
        Self {
            shelf,
            entries: entries.into(),
            preference: PageSubtitlePreference::NotGiven,
        }
    }

    /// Тот же снимок с подсказкой страницы `preference`.
    #[must_use]
    pub(crate) const fn with_preference(mut self, preference: PageSubtitlePreference) -> Self {
        self.preference = preference;
        self
    }

    /// Последняя подсказка страницы о субтитрах.
    pub(crate) const fn preference(&self) -> PageSubtitlePreference {
        self.preference
    }

    /// Пустой снимок склада `shelf`.
    pub(crate) fn empty(shelf: PageSubtitleShelfId) -> Self {
        Self::new(shelf, Vec::new())
    }

    /// Чей это склад.
    pub(crate) const fn shelf(&self) -> PageSubtitleShelfId {
        self.shelf
    }

    /// Документы в порядке первого появления их дорожек.
    pub(crate) fn entries(&self) -> &[ShelvedPageSubtitle] {
        &self.entries
    }

    /// Самое позднее поступление (`None` — склад пуст).
    pub(crate) fn latest_arrival(&self) -> Option<PageSubtitleArrival> {
        self.entries.iter().map(|entry| entry.arrival).max()
    }
}

/// Что владелец субтитров сделал со снимком склада.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageTracksUpdate {
    /// Ничего нового для текущего медиа (или медиа нет): кадр перерисовывать не нужно.
    Unchanged,
    /// Дорожки страницы добавлены или заменены: меню и, возможно, кадр изменились.
    Changed,
}

/// Два снимка равны, если это один склад в одном и том же состоянии: на складе только
/// добавляют и заменяют, и каждое изменение даёт новое поступление; подсказка страницы
/// сравнивается отдельно (она меняется без нового документа).
impl PartialEq for PageSubtitleShelfView {
    fn eq(&self, other: &Self) -> bool {
        self.shelf == other.shelf
            && self.entries.len() == other.entries.len()
            && self.latest_arrival() == other.latest_arrival()
            && self.preference == other.preference
    }
}

impl Eq for PageSubtitleShelfView {}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(track: u32) -> Arc<PageSubtitleDocument> {
        Arc::new(PageSubtitleDocument {
            track: PageSubtitleTrackNumber::new(track),
            language: PageSubtitleLanguage::Tagged("en".to_owned()),
            label: PageSubtitleLabel::Text("Secret page words".to_owned()),
            format_hint: PageSubtitleFormatHint::WebVtt,
            display: PageSubtitleDisplay::ShowingOnPage,
            source: PageSubtitleSource::PageDownload,
            bytes: Arc::from(b"WEBVTT\n\n00:01.000 --> 00:02.000\nHidden cue text".as_slice()),
        })
    }

    #[test]
    fn debug_of_a_document_never_prints_its_bytes_or_label_text() {
        let printed = format!("{:?}", document(3));
        assert!(printed.contains("track: 3"), "{printed}");
        assert!(!printed.contains("Hidden cue text"), "{printed}");
        assert!(!printed.contains("WEBVTT"), "{printed}");
        assert!(!printed.contains("Secret page words"), "{printed}");
    }

    #[test]
    fn a_blank_label_counts_as_no_label() {
        assert_eq!(PageSubtitleLabel::Text("  ".to_owned()).text(), None);
        assert_eq!(
            PageSubtitleLabel::Text(" English ".to_owned()).text(),
            Some("English")
        );
        assert_eq!(PageSubtitleLabel::NotGiven.text(), None);
    }

    #[test]
    fn views_differ_when_the_shelf_or_its_latest_arrival_differs() {
        let first = PageSubtitleShelfId::FIRST;
        let entry = |arrival: u64| ShelvedPageSubtitle {
            arrival: (0..arrival).fold(PageSubtitleArrival::FIRST, |at, _| at.next()),
            document: document(1),
        };
        let view = PageSubtitleShelfView::new(first, vec![entry(0)]);
        assert_eq!(view, PageSubtitleShelfView::new(first, vec![entry(0)]));
        assert_ne!(view, PageSubtitleShelfView::new(first, vec![entry(1)]));
        assert_ne!(
            view,
            PageSubtitleShelfView::new(first.next(), vec![entry(0)])
        );
        assert_ne!(view, PageSubtitleShelfView::empty(first));
        assert_eq!(view.latest_arrival(), Some(PageSubtitleArrival::FIRST));
    }
}
