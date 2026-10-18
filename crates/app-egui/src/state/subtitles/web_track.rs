//! Web-дорожки субтитров (YouTube и другие extractor-ы): описание, порядок в меню, источник.
//!
//! # Владение
//! Модуль чистый: ничего не скачивает и не хранит состояния загрузки. Он переводит безопасный
//! список дорожек `media-source-open` (без ссылок) в типы владельца субтитров и решает, В КАКОМ
//! ПОРЯДКЕ дорожки стоят в меню: сначала ручные дорожки автора, затем распознанная речь
//! («English (auto)»), затем автопереводы.
//!
//! Внутри группы сверху языки из предпочтений (в порядке предпочтений), ниже остальные по
//! алфавиту отображаемого имени. Меню потом просто режет список на «основной» и «Auto-translate».
//!
//! # Инварианты
//! * Идентификатор дорожки — `WebSubtitleTrackId` (вид + язык): стабилен между открытиями и
//!   годится как ключ памяти выбора.
//! * В описании нет ссылок; `Debug` источника показывает только число дорожек.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use media_source_open::web_subtitle_fetch::{WebSubtitleTrack, WebSubtitleTrackId};
use playlist_state::{
    RememberedWebSubtitle, RememberedWebSubtitleError, RememberedWebSubtitleKind,
};
use subtitle_core::SubtitleLanguage;
use web_media_core::SubtitleKind;

use super::auto_select::{PreferredLanguages, TrackOrigin};
use super::menu_view::{SubtitleFileOrigin, SubtitleTrackLabel};
use super::web_download::WebSubtitleDownloader;
use crate::media_open::WebMediaSourceIntent;
use crate::state::language_label_disambiguation::{LanguageNameInput, distinguish_language_names};

/// Безопасные сведения о дорожке: всё, что нужно меню и памяти выбора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebTrackInfo {
    /// Стабильный идентификатор (вид + язык).
    pub(crate) id: WebSubtitleTrackId,
    /// Название от источника («Russian»); запасное имя, если языка нет в таблице.
    pub(crate) display_name: Option<String>,
}

impl WebTrackInfo {
    /// Проекция дорожки `media-source-open` (ссылок в ней нет по построению).
    fn from_track(track: &WebSubtitleTrack) -> Self {
        Self {
            id: track.id().clone(),
            display_name: track.display_name().map(str::to_owned),
        }
    }
}

/// Дорожки web-видео и тот, кто умеет их скачивать.
#[derive(Clone)]
pub(crate) struct WebSubtitleSource {
    /// Дорожки в порядке источника (порядок меню строит [`ordered_for_menu`]).
    tracks: Vec<WebTrackInfo>,
    /// Загрузчик; для всех копий источника один и тот же.
    downloader: Arc<dyn WebSubtitleDownloader>,
}

impl WebSubtitleSource {
    /// Собирает источник из готовых дорожек и загрузчика (тесты подставляют фальшивый).
    pub(crate) fn new(
        tracks: Vec<WebTrackInfo>,
        downloader: Arc<dyn WebSubtitleDownloader>,
    ) -> Self {
        Self { tracks, downloader }
    }

    /// Источник субтитров открытого web-видео; `None` — у него субтитров этого вида нет
    /// (нативные HLS/DASH/progressive) либо список дорожек пуст.
    pub(crate) fn from_intent(intent: &WebMediaSourceIntent) -> Option<Self> {
        let handle = intent.web_subtitle_fetch_handle()?;
        let tracks: Vec<WebTrackInfo> = intent
            .web_subtitle_tracks()
            .iter()
            .map(WebTrackInfo::from_track)
            .collect();
        if tracks.is_empty() {
            return None;
        }
        Some(Self::new(tracks, Arc::new(handle)))
    }

    /// Загрузчик для задач чтения.
    pub(super) fn downloader(&self) -> Arc<dyn WebSubtitleDownloader> {
        Arc::clone(&self.downloader)
    }

    /// Дорожки в порядке меню.
    pub(super) fn descriptions_for_menu(
        &self,
        preferred: &PreferredLanguages,
    ) -> Vec<WebTrackDescription> {
        ordered_for_menu(&self.tracks, preferred)
    }
}

impl fmt::Debug for WebSubtitleSource {
    /// Только число дорожек: ни ссылок, ни названий.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebSubtitleSource")
            .field("track_count", &self.tracks.len())
            .finish_non_exhaustive()
    }
}

/// Два источника равны, если это один и тот же загрузчик с теми же дорожками.
impl PartialEq for WebSubtitleSource {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.downloader, &other.downloader) && self.tracks == other.tracks
    }
}

impl Eq for WebSubtitleSource {}

/// Описание web-дорожки в списке владельца.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WebTrackDescription {
    /// Сведения источника.
    info: WebTrackInfo,
    /// Язык в нормализованном виде (разобран один раз); `None`, если тег не разбирается.
    language: Option<SubtitleLanguage>,
    /// Различимое название, когда у нескольких дорожек одного вида совпало название языка
    /// (`Chinese (Taiwan)`); считает [`ordered_for_menu`], пока видит весь список.
    distinguishing_name: Option<String>,
}

impl WebTrackDescription {
    fn new(info: WebTrackInfo) -> Self {
        let language = SubtitleLanguage::parse(info.id.language().as_str()).ok();
        Self {
            info,
            language,
            distinguishing_name: None,
        }
    }

    /// Идентификатор для задачи загрузки.
    pub(super) fn track_id(&self) -> &WebSubtitleTrackId {
        &self.info.id
    }

    /// Вид дорожки.
    pub(super) fn kind(&self) -> SubtitleKind {
        self.info.id.kind()
    }

    /// Язык для автовыбора и выбора шрифта.
    pub(super) fn language(&self) -> Option<&SubtitleLanguage> {
        self.language.as_ref()
    }

    /// Автоперевод: живёт в подменю и никогда не выбирается автоматически.
    pub(super) fn is_auto_translation(&self) -> bool {
        self.kind() == SubtitleKind::AutoTranslated
    }

    /// Откуда дорожка для автовыбора (ручная важнее распознанной речи при равенстве).
    /// `None` — автовыбор такую дорожку не рассматривает (автоперевод).
    pub(super) fn auto_select_origin(&self) -> Option<TrackOrigin> {
        match self.kind() {
            SubtitleKind::Manual => Some(TrackOrigin::WebManual),
            SubtitleKind::AutoGenerated => Some(TrackOrigin::WebAutoGenerated),
            SubtitleKind::AutoTranslated => None,
        }
    }

    /// Части подписи для меню; текст собирает UI (`subtitle_message`).
    pub(super) fn label(&self) -> SubtitleTrackLabel {
        SubtitleTrackLabel {
            language: self.language.clone(),
            format: None,
            origin: SubtitleFileOrigin::Web(self.kind()),
            is_forced: false,
            is_hearing_impaired: false,
            title: None,
            file_name: None,
            distinguishing_name: self.distinguishing_name.clone(),
            // Запасное имя: название источника, иначе сам код языка.
            fallback_name: Some(
                self.info
                    .display_name
                    .clone()
                    .unwrap_or_else(|| self.info.id.language().as_str().to_owned()),
            ),
        }
    }

    /// Выбор в виде записи памяти «для этого видео».
    pub(super) fn to_remembered(
        &self,
    ) -> Result<RememberedWebSubtitle, RememberedWebSubtitleError> {
        RememberedWebSubtitle::new(
            self.info.id.language().as_str().to_owned(),
            remembered_kind_of(self.kind()),
        )
    }

    /// Это та самая дорожка, что запомнена (вид и язык совпадают точно).
    pub(super) fn matches_remembered(&self, remembered: &RememberedWebSubtitle) -> bool {
        remembered_kind_of(self.kind()) == remembered.kind()
            && self.info.id.language().as_str() == remembered.language()
    }

    /// Вход для различения двойников: базовое название, тег и подробное имя от источника.
    fn name_input(&self) -> LanguageNameInput {
        LanguageNameInput {
            base_name: self.display_base_name(),
            language_tag: self.info.id.language().as_str().to_owned(),
            source_name: self.info.display_name.clone(),
        }
    }

    /// Название языка так, как его показывает подпись без уточнений: таблица, иначе имя
    /// источника, иначе код.
    fn display_base_name(&self) -> String {
        self.language
            .as_ref()
            .and_then(SubtitleLanguage::english_display_name)
            .map(str::to_owned)
            .or_else(|| self.info.display_name.clone())
            .unwrap_or_else(|| self.info.id.language().as_str().to_owned())
    }

    /// Ключ сортировки по алфавиту: имя из таблицы языков, иначе имя источника, иначе код.
    fn sort_name(&self) -> String {
        self.display_base_name().to_lowercase()
    }
}

fn remembered_kind_of(kind: SubtitleKind) -> RememberedWebSubtitleKind {
    match kind {
        SubtitleKind::Manual => RememberedWebSubtitleKind::Manual,
        SubtitleKind::AutoGenerated => RememberedWebSubtitleKind::AutoGenerated,
        SubtitleKind::AutoTranslated => RememberedWebSubtitleKind::AutoTranslated,
    }
}

/// Порядок групп в меню: ручные, затем распознанная речь, затем автопереводы.
const fn group_rank(kind: SubtitleKind) -> u8 {
    match kind {
        SubtitleKind::Manual => 0,
        SubtitleKind::AutoGenerated => 1,
        SubtitleKind::AutoTranslated => 2,
    }
}

/// Дорожки в порядке меню (см. документацию модуля).
fn ordered_for_menu(
    tracks: &[WebTrackInfo],
    preferred: &PreferredLanguages,
) -> Vec<WebTrackDescription> {
    let mut descriptions: Vec<WebTrackDescription> = tracks
        .iter()
        .cloned()
        .map(WebTrackDescription::new)
        .collect();
    descriptions.sort_by(|left, right| compare_for_menu(left, right, preferred));
    distinguish_same_named_languages(&mut descriptions);
    descriptions
}

/// Даёт различимые названия дорожкам одного вида с одинаковым названием языка
/// (`zh-CN`/`zh-TW`, `pt-BR`/`pt-PT`): подпись строки включает вид («(auto)»), поэтому группы
/// считаются отдельно по каждому виду. Единственная дорожка названия не меняет.
fn distinguish_same_named_languages(descriptions: &mut [WebTrackDescription]) {
    for kind in [
        SubtitleKind::Manual,
        SubtitleKind::AutoGenerated,
        SubtitleKind::AutoTranslated,
    ] {
        let group: Vec<usize> = (0..descriptions.len())
            .filter(|&index| descriptions[index].kind() == kind)
            .collect();
        let inputs: Vec<LanguageNameInput> = group
            .iter()
            .map(|&index| descriptions[index].name_input())
            .collect();
        let names = distinguish_language_names(&inputs);
        for (&index, (name, input)) in group.iter().zip(names.into_iter().zip(&inputs)) {
            // Название без двойника остаётся прежним: подпись соберёт таблица языков.
            descriptions[index].distinguishing_name = (name != input.base_name).then_some(name);
        }
    }
}

fn compare_for_menu(
    left: &WebTrackDescription,
    right: &WebTrackDescription,
    preferred: &PreferredLanguages,
) -> Ordering {
    // `None` (язык вне предпочтений) должен идти ПОСЛЕ любого `Some`: сравниваем по флагу.
    let rank_key = |description: &WebTrackDescription| {
        let rank = preferred.rank_of(description.language());
        (rank.is_none(), rank)
    };
    group_rank(left.kind())
        .cmp(&group_rank(right.kind()))
        .then_with(|| rank_key(left).cmp(&rank_key(right)))
        .then_with(|| left.sort_name().cmp(&right.sort_name()))
        .then_with(|| {
            left.info
                .id
                .language()
                .as_str()
                .cmp(right.info.id.language().as_str())
        })
}
