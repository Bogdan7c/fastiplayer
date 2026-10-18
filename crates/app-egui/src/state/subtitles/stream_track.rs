//! Дорожки субтитров потокового источника (HLS-рендиции): описание, подпись для меню,
//! признаки автовыбора и связь с памятью выбора.
//!
//! # Владение
//! Модуль чистый: ничего не скачивает и не хранит состояния ленты (им владеет
//! `owner_stream`). Он переводит безопасный список дорожек `media-source-open` (без ссылок) в
//! типы владельца субтитров.
//!
//! # Инварианты
//! * Идентификатор дорожки в источнике (`StreamSubtitleTrackId`) нестабилен между открытиями,
//!   поэтому в память выбора идёт не он, а описание «язык + название + forced»
//!   ([`playlist_state::RememberedStreamSubtitle`]).
//! * Ссылок нет ни в описании, ни в `Debug`: источник печатает только метаданные дорожек.
//! * Автовыбор уважает манифест: `DEFAULT=YES` ведёт себя как default встроенной дорожки,
//!   `AUTOSELECT=NO` запрещает выбор без участия пользователя (ручной выбор остаётся).

use media_source_open::stream_subtitles::{
    StreamSubtitleSource, StreamSubtitleTrackId, StreamSubtitleTrackInfo,
};
use playlist_state::{RememberedStreamSubtitle, RememberedStreamSubtitleError};
use subtitle_core::SubtitleLanguage;

use super::SubtitleMediaSource;
use super::menu_view::{SubtitleFileOrigin, SubtitleTrackLabel};
use super::web_track::WebSubtitleSource;
use crate::media_open::WebMediaSourceIntent;

/// Код «язык не определён» (как у встроенных дорожек).
const UNDETERMINED_LANGUAGE_CODE: &str = "und";

/// Источник субтитров для web-media: дорожки extractor-а (YouTube) и/или рендиции потока
/// (HLS). У одного media могут быть оба списка: например, yt-dlp-субтитры и HLS-рендиции.
pub(super) fn subtitle_source_of_web_intent(intent: &WebMediaSourceIntent) -> SubtitleMediaSource {
    match (
        WebSubtitleSource::from_intent(intent),
        intent.stream_subtitle_source(),
    ) {
        (Some(web), Some(stream)) => SubtitleMediaSource::WebAndStream { web, stream },
        (Some(web), None) => SubtitleMediaSource::Web(web),
        (None, Some(stream)) => SubtitleMediaSource::Stream(stream),
        (None, None) => SubtitleMediaSource::NotLocal,
    }
}

/// Описание дорожки потока в списке владельца.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StreamTrackDescription {
    /// Сведения источника (идентификатор для ленты, язык, название, флаги).
    info: StreamSubtitleTrackInfo,
    /// Язык в нормализованном виде (разобран один раз); `None`, если тег пуст, «und» или
    /// не разбирается.
    language: Option<SubtitleLanguage>,
}

impl StreamTrackDescription {
    /// Описание по строке источника.
    pub(super) fn new(info: StreamSubtitleTrackInfo) -> Self {
        let language = normalized_language(info.language.as_deref());
        Self { info, language }
    }

    /// Описания всех дорожек источника в порядке манифеста.
    pub(super) fn all_of(source: &StreamSubtitleSource) -> Vec<Self> {
        source.tracks().iter().cloned().map(Self::new).collect()
    }

    /// Идентификатор дорожки для запуска ленты.
    pub(super) fn feed_track_id(&self) -> StreamSubtitleTrackId {
        self.info.id
    }

    /// Язык для автовыбора и выбора шрифта.
    pub(super) fn language(&self) -> Option<&SubtitleLanguage> {
        self.language.as_ref()
    }

    /// Дорожка «только форсированные» (иностранная речь, надписи).
    pub(super) fn is_forced(&self) -> bool {
        self.info.forced
    }

    /// Манифест выбрал дорожку по умолчанию.
    pub(super) fn is_default(&self) -> bool {
        self.info.default
    }

    /// Автовыбор вправе рассматривать дорожку: `AUTOSELECT=YES` либо `DEFAULT=YES` (по
    /// спецификации default подразумевает autoselect). Ручной выбор разрешён всегда.
    pub(super) fn is_eligible_for_auto_select(&self) -> bool {
        self.info.autoselect || self.info.default
    }

    /// Части подписи для меню; текст собирает UI (`subtitle_message`).
    ///
    /// Название рендиции показывается рядом с языком, только когда оно что-то добавляет
    /// (`track_text` не повторяет название, совпавшее с именем языка). Без языка название
    /// становится основным именем дорожки.
    pub(super) fn label(&self) -> SubtitleTrackLabel {
        let name = self.info.name.trim();
        let has_language = self.language.is_some();
        SubtitleTrackLabel {
            language: self.language.clone(),
            format: None,
            origin: SubtitleFileOrigin::Stream,
            is_forced: self.info.forced,
            is_hearing_impaired: false,
            title: (has_language && !name.is_empty()).then(|| name.to_owned()),
            file_name: (!has_language && !name.is_empty()).then(|| name.to_owned()),
            fallback_name: None,
            distinguishing_name: None,
        }
    }

    /// Выбор в виде записи памяти «для этого видео».
    pub(super) fn to_remembered(
        &self,
    ) -> Result<RememberedStreamSubtitle, RememberedStreamSubtitleError> {
        RememberedStreamSubtitle::new(
            self.info.language.as_deref().map(str::to_owned),
            self.info.name.to_string(),
            self.info.forced,
        )
    }

    /// Это та самая дорожка, что запомнена (язык, название и forced совпадают точно).
    pub(super) fn matches_remembered(&self, remembered: &RememberedStreamSubtitle) -> bool {
        self.info.language.as_deref() == remembered.language()
            && &*self.info.name == remembered.name()
            && self.info.forced == remembered.forced()
    }
}

/// Язык из манифеста: `None` для пустого, «und» и неразбираемого значения.
fn normalized_language(raw: Option<&str>) -> Option<SubtitleLanguage> {
    let raw = raw?.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case(UNDETERMINED_LANGUAGE_CODE) {
        return None;
    }
    match SubtitleLanguage::parse(raw) {
        Ok(language) => Some(language),
        Err(error) => {
            // Сырой тег в лог не пишем: достаточно факта.
            tracing::debug!(%error, "A stream subtitle track has an unparseable language tag");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(
        language: Option<&str>,
        name: &str,
        flags: (bool, bool, bool),
    ) -> StreamSubtitleTrackInfo {
        let (forced, default, autoselect) = flags;
        StreamSubtitleTrackInfo {
            id: StreamSubtitleTrackId::new(0),
            language: language.map(Into::into),
            name: name.into(),
            forced,
            default,
            autoselect,
        }
    }

    #[test]
    fn label_shows_the_rendition_name_only_next_to_a_known_language() {
        let with_language =
            StreamTrackDescription::new(info(Some("en"), "English SDH", (false, false, true)));
        let label = with_language.label();
        assert_eq!(label.origin, SubtitleFileOrigin::Stream);
        assert_eq!(label.title.as_deref(), Some("English SDH"));
        assert_eq!(label.file_name, None);

        let without_language =
            StreamTrackDescription::new(info(None, "Commentary", (false, false, true)));
        let label = without_language.label();
        assert_eq!(label.language, None);
        assert_eq!(label.title, None);
        assert_eq!(label.file_name.as_deref(), Some("Commentary"));

        let undetermined =
            StreamTrackDescription::new(info(Some("und"), "Extra", (false, false, true)));
        assert_eq!(undetermined.language(), None, "`und` is not a language");
    }

    #[test]
    fn forced_default_and_autoselect_flags_come_from_the_manifest() {
        let forced = StreamTrackDescription::new(info(Some("ru"), "Forced", (true, false, true)));
        assert!(forced.is_forced() && forced.label().is_forced);
        assert!(forced.is_eligible_for_auto_select());

        let manual_only =
            StreamTrackDescription::new(info(Some("fr"), "Francais", (false, false, false)));
        assert!(!manual_only.is_eligible_for_auto_select(), "AUTOSELECT=NO");

        let default_without_autoselect =
            StreamTrackDescription::new(info(Some("en"), "English", (false, true, false)));
        assert!(default_without_autoselect.is_default());
        assert!(default_without_autoselect.is_eligible_for_auto_select());
    }

    #[test]
    fn remembered_description_roundtrips_and_matches_only_the_exact_track() {
        let track = StreamTrackDescription::new(info(Some("en"), "English", (false, false, true)));
        let remembered = track.to_remembered().expect("rememberable");
        assert!(track.matches_remembered(&remembered));

        let forced_twin =
            StreamTrackDescription::new(info(Some("en"), "English", (true, false, true)));
        assert!(
            !forced_twin.matches_remembered(&remembered),
            "forced differs"
        );
        let renamed =
            StreamTrackDescription::new(info(Some("en"), "English SDH", (false, false, true)));
        assert!(!renamed.matches_remembered(&remembered), "name differs");
        let other_language =
            StreamTrackDescription::new(info(Some("de"), "English", (false, false, true)));
        assert!(
            !other_language.matches_remembered(&remembered),
            "language differs"
        );
    }

    #[test]
    fn a_track_with_an_empty_name_cannot_be_remembered() {
        let nameless = StreamTrackDescription::new(info(Some("en"), "", (false, false, true)));
        assert_eq!(
            nameless.to_remembered(),
            Err(RememberedStreamSubtitleError::EmptyName)
        );
    }
}
