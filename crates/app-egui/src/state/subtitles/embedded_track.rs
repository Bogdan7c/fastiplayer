//! Описание встроенной дорожки субтитров (внутри контейнера видео): то, что известно из
//! снимка плеера без чтения пакетов.
//!
//! Модуль чистый (без состояния и ввода-вывода): переводит данные снимка плеера
//! (`TrackSummarySnapshot`) в типы владельца субтитров — формат разбора пакета, подпись для
//! меню, язык для выбора шрифта и признаки автовыбора.
//!
//! # Поддержка кодеков
//! Показываются только текстовые кодеки (`SubtitleCodecFamily::TextRenderable`): SRT, WebVTT,
//! ASS/SSA (пока стилем по умолчанию, настоящие стили — сессия 08) и mov_text. Картинки (PGS,
//! VobSub, DVB) и нераспознанные кодеки остаются в меню серыми: «not supported yet».

use media_core::subtitle_codec::{self, SubtitleCodecFamily};
use media_core::{TrackId, TrackLabels};
use player_core::TrackSummarySnapshot;
use playlist_state::TrackHintCandidate;
use subtitle_core::{SubtitleFormat, SubtitleLanguage};
use subtitle_formats::EmbeddedCueFormat;

use super::menu_view::{SubtitleFileOrigin, SubtitleTrackLabel};

/// Код «язык не определён» в контейнерах (MKV по умолчанию пишет `eng`, MP4 — `und`).
const UNDETERMINED_LANGUAGE_CODE: &str = "und";

/// Встроенная дорожка: идентификатор плеера и метки контейнера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EmbeddedTrackDescription {
    /// Идентификатор дорожки у плеера: по нему приходят пакеты трубы.
    player_track_id: TrackId,
    /// Идентификатор кодека контейнера (`S_TEXT/UTF8`…).
    codec_id: String,
    /// Язык, название и флаги из контейнера.
    labels: TrackLabels,
    /// Язык в нормализованном виде (разобран один раз при создании описания).
    language: Option<SubtitleLanguage>,
}

impl EmbeddedTrackDescription {
    /// Описание по строке снимка плеера.
    pub(super) fn from_snapshot(track: &TrackSummarySnapshot) -> Self {
        Self {
            player_track_id: track.id,
            codec_id: track.codec_id.clone(),
            labels: track.labels.clone(),
            language: normalized_language(&track.labels),
        }
    }

    /// Тот же кодек: дорожку можно обновить на месте, не теряя прочитанных реплик.
    pub(super) fn has_same_codec(&self, other: &Self) -> bool {
        self.codec_id == other.codec_id
    }

    /// Совпадает ли описание со строкой снимка (без аллокаций): так владелец понимает,
    /// что список дорожек не менялся, и не перестраивает его на каждом кадре.
    pub(super) fn matches_snapshot(&self, track: &TrackSummarySnapshot) -> bool {
        self.player_track_id == track.id
            && self.codec_id == track.codec_id
            && self.labels == track.labels
    }

    /// Идентификатор дорожки у плеера.
    pub(super) fn player_track_id(&self) -> TrackId {
        self.player_track_id
    }

    /// Как разбирать пакеты дорожки; `None` — кодек показать нельзя.
    pub(super) fn cue_format(&self) -> Option<EmbeddedCueFormat> {
        cue_format_for_codec(&self.codec_id)
    }

    /// Этот кодек умеем показывать сейчас.
    pub(super) fn is_renderable_now(&self) -> bool {
        self.cue_format().is_some()
    }

    /// Дорожка помечена контейнером как «по умолчанию».
    pub(super) fn is_default(&self) -> bool {
        self.labels.disposition.is_default
    }

    /// Дорожка принудительная (только иностранная речь).
    pub(super) fn is_forced(&self) -> bool {
        self.labels.disposition.is_forced
    }

    /// Дорожка для слабослышащих.
    pub(super) fn is_hearing_impaired(&self) -> bool {
        self.labels.disposition.is_hearing_impaired
    }

    /// Язык в нормализованном виде; `None` — не указан, «und» или невалидный тег.
    pub(super) fn language(&self) -> Option<&SubtitleLanguage> {
        self.language.as_ref()
    }

    /// Описание дорожки для сверки с запомненным выбором «для этого видео».
    ///
    /// `ordinal_among_kind` — номер среди встроенных субтитровых дорожек в порядке снимка.
    /// Язык берётся «сырым» (как записал контейнер), чтобы описание не зависело от
    /// нормализации, которая может измениться между версиями.
    pub(super) fn track_hint_candidate(&self, ordinal_among_kind: u32) -> TrackHintCandidate {
        crate::state::track_memory::hint_candidate_from_labels(
            &self.labels,
            &self.codec_id,
            ordinal_among_kind,
        )
    }

    /// Формат для подписи в меню; `None` — формат без названия в модели.
    pub(super) fn display_format(&self) -> Option<SubtitleFormat> {
        match self.codec_id.as_str() {
            subtitle_codec::TEXT_UTF8 => Some(SubtitleFormat::Srt),
            subtitle_codec::WEBVTT => Some(SubtitleFormat::WebVtt),
            subtitle_codec::ASS => Some(SubtitleFormat::Ass),
            subtitle_codec::SSA => Some(SubtitleFormat::Ssa),
            subtitle_codec::MOV_TEXT => Some(SubtitleFormat::MovText),
            subtitle_codec::HDMV_PGS => Some(SubtitleFormat::Pgs),
            _ => None,
        }
    }

    /// Части подписи для меню.
    pub(super) fn label(&self) -> SubtitleTrackLabel {
        SubtitleTrackLabel {
            language: self.language.clone(),
            format: self.display_format(),
            origin: SubtitleFileOrigin::Embedded,
            is_forced: self.is_forced(),
            is_hearing_impaired: self.is_hearing_impaired(),
            title: self.labels.title.clone(),
            // У встроенной дорожки файла нет: без языка меню напишет «Track N».
            file_name: None,
        }
    }
}

/// Язык из меток контейнера: `None` для пустого, «und» и невалидного значения.
fn normalized_language(labels: &TrackLabels) -> Option<SubtitleLanguage> {
    let raw = labels.language.as_ref()?.as_str().trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case(UNDETERMINED_LANGUAGE_CODE) {
        return None;
    }
    match SubtitleLanguage::parse(raw) {
        Ok(language) => Some(language),
        Err(error) => {
            // Сырой тег в лог не пишем: достаточно факта.
            tracing::debug!(%error, "Embedded subtitle track has an unparseable language tag");
            None
        }
    }
}

/// Формат разбора пакета по идентификатору кодека; `None` — кодек не текстовый или неизвестен.
///
/// Единственное место, где решается «можно ли показать встроенный кодек»: меню, автовыбор и
/// разбор пакетов используют его же, поэтому расходиться не могут.
pub(super) fn cue_format_for_codec(codec_id: &str) -> Option<EmbeddedCueFormat> {
    if subtitle_codec::classify(codec_id) != SubtitleCodecFamily::TextRenderable {
        return None;
    }
    match codec_id {
        subtitle_codec::TEXT_UTF8 => Some(EmbeddedCueFormat::SrtText),
        subtitle_codec::WEBVTT => Some(EmbeddedCueFormat::WebVttCueText),
        subtitle_codec::ASS | subtitle_codec::SSA => Some(EmbeddedCueFormat::AssEventLine),
        subtitle_codec::MOV_TEXT => Some(EmbeddedCueFormat::MovText),
        // Семейство «текстовое», а разбора нет: новый кодек добавили в `media-core`, а сюда нет.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use media_core::{TrackDisposition, TrackKind, TrackLanguage};

    use super::*;

    fn snapshot_row(codec_id: &str, labels: TrackLabels) -> TrackSummarySnapshot {
        TrackSummarySnapshot {
            id: TrackId::new(3),
            kind: TrackKind::Subtitle,
            codec_id: codec_id.to_owned(),
            sample_rate: None,
            channels: None,
            duration: None,
            video: None,
            video_color_summary: None,
            labels,
        }
    }

    #[test]
    fn text_codecs_have_a_parse_format_and_bitmap_codecs_do_not() {
        let supported = [
            (subtitle_codec::TEXT_UTF8, EmbeddedCueFormat::SrtText),
            (subtitle_codec::WEBVTT, EmbeddedCueFormat::WebVttCueText),
            (subtitle_codec::ASS, EmbeddedCueFormat::AssEventLine),
            (subtitle_codec::SSA, EmbeddedCueFormat::AssEventLine),
            (subtitle_codec::MOV_TEXT, EmbeddedCueFormat::MovText),
        ];
        for (codec, expected) in supported {
            assert_eq!(cue_format_for_codec(codec), Some(expected), "{codec}");
        }
        for codec in [
            subtitle_codec::HDMV_PGS,
            subtitle_codec::VOBSUB,
            subtitle_codec::DVBSUB,
            subtitle_codec::KATE,
            subtitle_codec::UNKNOWN,
        ] {
            assert_eq!(cue_format_for_codec(codec), None, "{codec}");
        }
    }

    #[test]
    fn label_carries_language_title_flags_and_embedded_origin() {
        let labels = TrackLabels {
            language: Some(TrackLanguage::new("rus")),
            title: Some("Dub".to_owned()),
            disposition: TrackDisposition {
                is_forced: true,
                is_hearing_impaired: true,
                ..TrackDisposition::default()
            },
        };
        let description = EmbeddedTrackDescription::from_snapshot(&snapshot_row(
            subtitle_codec::TEXT_UTF8,
            labels,
        ));

        let label = description.label();

        assert_eq!(
            label.language.as_ref().map(SubtitleLanguage::as_str),
            Some("rus")
        );
        assert_eq!(label.title.as_deref(), Some("Dub"));
        assert!(label.is_forced && label.is_hearing_impaired);
        assert_eq!(label.origin, SubtitleFileOrigin::Embedded);
        assert_eq!(label.format, Some(SubtitleFormat::Srt));
    }

    #[test]
    fn undetermined_or_broken_language_is_treated_as_missing() {
        for raw in ["und", "UND", "", "  ", "not a tag!!"] {
            let labels = TrackLabels {
                language: Some(TrackLanguage::new(raw)),
                ..TrackLabels::default()
            };
            let description =
                EmbeddedTrackDescription::from_snapshot(&snapshot_row(subtitle_codec::ASS, labels));
            assert_eq!(description.language(), None, "raw tag {raw:?}");
        }
    }

    #[test]
    fn matches_snapshot_notices_any_label_codec_or_id_change() {
        let row = snapshot_row(subtitle_codec::TEXT_UTF8, TrackLabels::default());
        let description = EmbeddedTrackDescription::from_snapshot(&row);
        assert!(description.matches_snapshot(&row));

        let mut changed = row.clone();
        changed.labels.title = Some("renamed".to_owned());
        assert!(!description.matches_snapshot(&changed));
        let mut changed = row.clone();
        changed.codec_id = subtitle_codec::ASS.to_owned();
        assert!(!description.matches_snapshot(&changed));
        let mut changed = row;
        changed.id = TrackId::new(9);
        assert!(!description.matches_snapshot(&changed));
    }
}
