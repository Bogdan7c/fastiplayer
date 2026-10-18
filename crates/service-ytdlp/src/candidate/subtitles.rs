//! Субтитры из `--dump-single-json`: нейтральное описание и секретная сторона.
//!
//! Модуль владеет двумя вещами, которые нельзя смешивать:
//! * нейтральные [`SubtitleDescriptor`] (язык, вид, название, форматы) —
//!   безопасны для UI и логов;
//! * подписанные ссылки [`YtDlpSubtitleSource`] — секрет, живут только внутри
//!   сервиса и отдаются владельцу загрузки через явные методы.
//!
//! Разбор терпимый: кривая или неизвестная запись пропускается и никогда не
//! валит весь кандидат.

mod parse;
#[cfg(test)]
mod tests;

use std::fmt;
use std::time::SystemTime;

use source_core::HttpRequestTarget;
use web_media_core::{LanguageTag, SubtitleDescriptor, SubtitleKind};

pub(super) use parse::normalize_subtitle_inventory;

/// Формат субтитров, который умеет скачивать и разбирать плеер.
///
/// Порядок вариантов отражает предпочтение: `Srv3` богаче (пословные тайминги
/// автосубтитров), `WebVtt` служит запасным вариантом.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum YtDlpSubtitleFormat {
    /// YouTube srv3 (XML с окнами и словами).
    Srv3,
    /// WebVTT.
    WebVtt,
}

impl YtDlpSubtitleFormat {
    /// Значение поля `ext` у yt-dlp, оно же идентификатор формата в дескрипторе.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Srv3 => "srv3",
            Self::WebVtt => "vtt",
        }
    }

    /// Разбирает `ext`; все остальные форматы плеером не используются.
    pub(super) fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "srv3" => Some(Self::Srv3),
            "vtt" => Some(Self::WebVtt),
            _ => None,
        }
    }
}

/// Одна подписанная ссылка на один формат одной дорожки (секрет).
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpSubtitleSource {
    /// Формат, который отдаёт эта ссылка.
    format: YtDlpSubtitleFormat,
    /// Подписанная цель запроса; Debug у неё скрывает путь и query.
    target: HttpRequestTarget,
    /// Момент истечения подписи из параметра `expire`, если он был.
    expires_at: Option<SystemTime>,
}

impl YtDlpSubtitleSource {
    pub(super) const fn new(
        format: YtDlpSubtitleFormat,
        target: HttpRequestTarget,
        expires_at: Option<SystemTime>,
    ) -> Self {
        Self {
            format,
            target,
            expires_at,
        }
    }

    /// Формат этой ссылки.
    #[must_use]
    pub const fn format(&self) -> YtDlpSubtitleFormat {
        self.format
    }

    /// Раскрывает подписанную цель только владельцу HTTP-запроса.
    #[must_use]
    pub const fn target_for_request(&self) -> &HttpRequestTarget {
        &self.target
    }

    /// Момент истечения подписи, если сервер его указал.
    #[must_use]
    pub const fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }
}

impl fmt::Debug for YtDlpSubtitleSource {
    /// Не раскрывает ни путь, ни query, ни подпись.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpSubtitleSource")
            .field("format", &self.format)
            .field("has_expiry", &self.expires_at.is_some())
            .finish_non_exhaustive()
    }
}

/// Дорожка: нейтральное описание плюс ссылки по форматам (в порядке предпочтения).
#[derive(Clone, PartialEq, Eq)]
pub struct YtDlpSubtitleTrack {
    /// Безопасное описание.
    descriptor: SubtitleDescriptor,
    /// Ссылки; порядок совпадает с `descriptor.formats()`.
    sources: Vec<YtDlpSubtitleSource>,
}

impl YtDlpSubtitleTrack {
    pub(super) const fn new(
        descriptor: SubtitleDescriptor,
        sources: Vec<YtDlpSubtitleSource>,
    ) -> Self {
        Self {
            descriptor,
            sources,
        }
    }

    /// Нейтральное описание дорожки.
    #[must_use]
    pub const fn descriptor(&self) -> &SubtitleDescriptor {
        &self.descriptor
    }

    /// Секретные ссылки по форматам, предпочтительный формат первым.
    #[must_use]
    pub fn sources(&self) -> &[YtDlpSubtitleSource] {
        &self.sources
    }

    /// Вид дорожки (ручная, распознанная, переведённая).
    #[must_use]
    pub const fn kind(&self) -> SubtitleKind {
        self.descriptor.kind()
    }

    /// Язык дорожки.
    #[must_use]
    pub fn language(&self) -> Option<&LanguageTag> {
        self.descriptor.language()
    }
}

impl fmt::Debug for YtDlpSubtitleTrack {
    /// Показывает дескриптор (он безопасен) и только число ссылок.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpSubtitleTrack")
            .field("descriptor", &self.descriptor)
            .field("source_count", &self.sources.len())
            .finish()
    }
}

/// Все субтитры одного extraction snapshot-а.
///
/// Порядок: ручные, затем распознанная речь, затем автопереводы; внутри вида —
/// по языку. Число дорожек ограничено `MAX_SUBTITLE_DESCRIPTORS`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct YtDlpSubtitleInventory {
    /// Дорожки в описанном порядке.
    tracks: Box<[YtDlpSubtitleTrack]>,
    /// Сколько корректных дорожек отброшено лимитом (для безопасного лога).
    omitted_track_count: usize,
}

impl YtDlpSubtitleInventory {
    pub(super) fn new(tracks: Vec<YtDlpSubtitleTrack>, omitted_track_count: usize) -> Self {
        Self {
            tracks: tracks.into_boxed_slice(),
            omitted_track_count,
        }
    }

    /// Дорожки в порядке показа.
    #[must_use]
    pub fn tracks(&self) -> &[YtDlpSubtitleTrack] {
        &self.tracks
    }

    /// Число корректных дорожек, не поместившихся в лимит.
    #[must_use]
    pub const fn omitted_track_count(&self) -> usize {
        self.omitted_track_count
    }

    /// Ищет дорожку по смыслу (вид + язык), а не по идентичности snapshot-а.
    ///
    /// Нужен после повторного извлечения: идентичности нового snapshot-а другие,
    /// а вид и язык остаются прежними.
    #[must_use]
    pub fn find_track(
        &self,
        kind: SubtitleKind,
        language: &LanguageTag,
    ) -> Option<&YtDlpSubtitleTrack> {
        self.tracks
            .iter()
            .find(|track| track.kind() == kind && track.language() == Some(language))
    }
}

impl fmt::Debug for YtDlpSubtitleInventory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("YtDlpSubtitleInventory")
            .field("tracks", &self.tracks)
            .field("omitted_track_count", &self.omitted_track_count)
            .finish()
    }
}
