//! Подписи аудиодорожек: «Russian — Dub (FLAC)», «Track 2 (AAC)».
//!
//! Формат: `<язык> — <название> (<кодек>[, commentary])`. Части без данных пропускаются; если
//! нет ни языка, ни названия, подпись начинается с «Track N» (N — порядковый номер среди
//! аудиодорожек, с единицы). Язык показывается английским названием из той же таблицы, что и
//! у субтитров, поэтому `rus` и `ru` дают одинаковое «Russian».

use media_core::TrackLabels;
use subtitle_core::SubtitleLanguage;

/// Код «язык не определён» (ISO 639-2): не показываем как язык.
const UNDETERMINED_LANGUAGE_CODE: &str = "und";
/// Разделитель языка и названия.
const LANGUAGE_TITLE_SEPARATOR: &str = " — ";
/// Префикс контейнерных идентификаторов Matroska для аудио (`A_FLAC`).
const MATROSKA_AUDIO_PREFIX: &str = "A_";
/// Пометка дорожки с комментариями.
const COMMENTARY_FLAG: &str = "commentary";

/// Порядковый номер аудиодорожки в меню (с единицы) для запасной подписи «Track N».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioTrackNumber(usize);

impl AudioTrackNumber {
    /// Из индекса строки меню (с нуля).
    pub(super) fn from_menu_index(menu_index: usize) -> Self {
        Self(menu_index + 1)
    }
}

/// Полная подпись дорожки для меню и уведомления.
pub(super) fn audio_track_label(
    labels: &TrackLabels,
    codec_id: &str,
    number: AudioTrackNumber,
) -> String {
    let language = language_text(labels);
    let title = labels
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let head = match (language, title) {
        (Some(language), Some(title)) => format!("{language}{LANGUAGE_TITLE_SEPARATOR}{title}"),
        (Some(language), None) => language,
        (None, Some(title)) => title.to_owned(),
        (None, None) => format!("Track {}", number.0),
    };

    let mut details: Vec<String> = Vec::new();
    if let Some(codec) = short_codec_name(codec_id) {
        details.push(codec);
    }
    if labels.disposition.is_commentary {
        details.push(COMMENTARY_FLAG.to_owned());
    }
    if details.is_empty() {
        head
    } else {
        format!("{head} ({})", details.join(", "))
    }
}

/// Текст toast-а после переключения клавишей `B`.
pub(super) fn audio_selected_toast(label: &str) -> String {
    format!("Audio: {label}")
}

/// Язык для показа: английское название, а если языка нет в таблице — сам тег.
fn language_text(labels: &TrackLabels) -> Option<String> {
    language_display_name(labels.language.as_ref()?.as_str())
}

/// Английское название языка по тегу (`rus`, `en-US`); `None` — тега нет или он «und».
///
/// Общая часть для встроенных дорожек и web-версий звука (`web_versions.rs`): один и тот же
/// язык должен называться в меню одинаково, откуда бы ни пришла строка.
pub(super) fn language_display_name(raw_tag: &str) -> Option<String> {
    let raw = raw_tag.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case(UNDETERMINED_LANGUAGE_CODE) {
        return None;
    }
    match SubtitleLanguage::parse(raw) {
        Ok(language) => Some(
            language
                .english_display_name()
                .map_or_else(|| language.as_str().to_owned(), str::to_owned),
        ),
        Err(error) => {
            // Сырой тег показываем как есть: контейнер так назвал, а пользователю лучше что-то.
            tracing::debug!(%error, "Audio track has an unparseable language tag");
            Some(raw.to_owned())
        }
    }
}

/// Короткое имя кодека для подписи; `None` — идентификатор пустой.
///
/// Известные семейства получают привычное написание, остальное — идентификатор без префикса
/// `A_` в верхнем регистре, чтобы новая дорожка не оставалась без кодека в подписи.
fn short_codec_name(codec_id: &str) -> Option<String> {
    let trimmed = codec_id.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase();
    let bare = upper.strip_prefix(MATROSKA_AUDIO_PREFIX).unwrap_or(&upper);
    let name = match bare {
        "FLAC" => "FLAC",
        "OPUS" => "Opus",
        "VORBIS" => "Vorbis",
        "AC3" => "AC-3",
        "EAC3" => "E-AC-3",
        "TRUEHD" => "TrueHD",
        "ALAC" => "ALAC",
        "MP1" => "MP1",
        "MP2" => "MP2",
        "MP3" | "MPEG/L3" => "MP3",
        "MPEG/L2" => "MP2",
        "MPEG/L1" => "MP1",
        family if family.starts_with("AAC") || family.starts_with("MP4A") => "AAC",
        family if family.starts_with("DTS") => "DTS",
        family if family.starts_with("PCM") => "PCM",
        other => return Some(other.to_owned()),
    };
    Some(name.to_owned())
}
