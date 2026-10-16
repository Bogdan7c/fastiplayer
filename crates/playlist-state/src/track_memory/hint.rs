//! Подсказка о дорожке и чистый подбор дорожки по подсказке.
//!
//! Идентификаторы дорожек (индексы потоков) могут меняться между открытиями одного видео,
//! поэтому запоминаем не номер, а описание: язык, название, кодек и порядковый номер среди
//! дорожек того же типа. Этот модуль ничего не знает о контейнерах и о диске: он только
//! хранит ограниченное описание и сравнивает его с кандидатами.

use std::fmt;

/// Максимум байт в запомненном языковом теге (BCP-47/ISO 639 короче с большим запасом).
pub const MAX_HINT_LANGUAGE_BYTES: usize = 64;
/// Максимум байт в запомненном названии дорожки.
pub const MAX_HINT_TITLE_BYTES: usize = 256;
/// Максимум байт в запомненном идентификаторе кодека (`A_AC3`, `S_TEXT/UTF8`, ...).
pub const MAX_HINT_CODEC_ID_BYTES: usize = 64;

/// Почему описание дорожки отвергнуто.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackHintError {
    /// Языковой тег длиннее [`MAX_HINT_LANGUAGE_BYTES`].
    LanguageTooLong,
    /// Название длиннее [`MAX_HINT_TITLE_BYTES`].
    TitleTooLong,
    /// Идентификатор кодека пустой: по такой подсказке дорожку не найти.
    CodecIdEmpty,
    /// Идентификатор кодека длиннее [`MAX_HINT_CODEC_ID_BYTES`].
    CodecIdTooLong,
}

impl fmt::Display for TrackHintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::LanguageTooLong => "remembered track language tag is too long",
            Self::TitleTooLong => "remembered track title is too long",
            Self::CodecIdEmpty => "remembered track codec id is empty",
            Self::CodecIdTooLong => "remembered track codec id is too long",
        })
    }
}

impl std::error::Error for TrackHintError {}

/// Ограниченное описание дорожки, выбранной пользователем.
///
/// Инвариант: все строки в пределах лимитов, `codec_id` непустой, пустые язык/название
/// нормализованы в `None` (контейнер «не указал» и «указал пустую строку» для нас одно и то же).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RememberedTrackHint {
    language: Option<String>,
    title: Option<String>,
    codec_id: String,
    ordinal_among_kind: u32,
}

impl RememberedTrackHint {
    /// Проверяет лимиты и строит подсказку. `ordinal_among_kind` — индекс с нуля среди
    /// дорожек того же типа (аудио среди аудио, субтитры среди субтитров).
    pub fn new(
        language: Option<String>,
        title: Option<String>,
        codec_id: String,
        ordinal_among_kind: u32,
    ) -> Result<Self, TrackHintError> {
        let language = language.filter(|value| !value.is_empty());
        let title = title.filter(|value| !value.is_empty());
        if language
            .as_ref()
            .is_some_and(|value| value.len() > MAX_HINT_LANGUAGE_BYTES)
        {
            return Err(TrackHintError::LanguageTooLong);
        }
        if title
            .as_ref()
            .is_some_and(|value| value.len() > MAX_HINT_TITLE_BYTES)
        {
            return Err(TrackHintError::TitleTooLong);
        }
        if codec_id.is_empty() {
            return Err(TrackHintError::CodecIdEmpty);
        }
        if codec_id.len() > MAX_HINT_CODEC_ID_BYTES {
            return Err(TrackHintError::CodecIdTooLong);
        }
        Ok(Self {
            language,
            title,
            codec_id,
            ordinal_among_kind,
        })
    }

    /// Сырой языковой тег из контейнера, если он был.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Название дорожки из контейнера, если оно было.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Идентификатор кодека.
    #[must_use]
    pub fn codec_id(&self) -> &str {
        &self.codec_id
    }

    /// Индекс с нуля среди дорожек того же типа.
    #[must_use]
    pub const fn ordinal_among_kind(&self) -> u32 {
        self.ordinal_among_kind
    }
}

/// Дорожка текущего файла, с которой сравнивается подсказка.
///
/// Это простой набор данных без инвариантов: вызывающий собирает его из метаданных дорожки.
/// Индекс кандидата — его позиция в переданном срезе, вызывающий сам знает, какая дорожка
/// за ним стоит.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackHintCandidate {
    /// Сырой языковой тег контейнера.
    pub language: Option<String>,
    /// Название дорожки.
    pub title: Option<String>,
    /// Идентификатор кодека.
    pub codec_id: String,
    /// Индекс с нуля среди дорожек того же типа.
    pub ordinal_among_kind: u32,
}

/// Результат подбора дорожки по подсказке.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackHintMatch {
    /// Совпали все поля; число — позиция кандидата в срезе.
    Exact(usize),
    /// Дорожка перенумерована, но язык и кодек (и название, если совпало) те же.
    Approximate(usize),
    /// Подходящей дорожки нет: вызывающий откатывается на автовыбор.
    NotFound,
}

/// Подбирает среди `candidates` дорожку, которую описывает `hint`.
///
/// Правила, от строгого к мягкому (внутри уровня побеждает ближайший по порядковому номеру,
/// при равенстве — первый, поэтому результат детерминирован):
/// 1. `Exact`: совпадают язык, название, кодек и порядковый номер.
/// 2. `Approximate`: язык + название + кодек (порядковый номер изменился: дорожки
///    переставили или добавили/удалили другие).
/// 3. `Approximate`: язык + кодек (название поменяли, например при перекодировании).
///
/// Дорожка без языка подбирается только на уровне 1: «неизвестный язык + тот же кодек»
/// подходит почти любой безымянной дорожке, и молча подсунуть не ту хуже, чем откатиться на
/// автовыбор. По той же причине язык никогда не игнорируется: дорожку другого языка не берём.
/// Язык сравниваем без учёта регистра (`eng` == `ENG`), остальное — побайтово.
#[must_use]
pub fn find_matching_track(
    hint: &RememberedTrackHint,
    candidates: &[TrackHintCandidate],
) -> TrackHintMatch {
    if let Some(position) = candidates.iter().position(|candidate| {
        language_matches(hint, candidate)
            && title_matches(hint, candidate)
            && codec_matches(hint, candidate)
            && candidate.ordinal_among_kind == hint.ordinal_among_kind
    }) {
        return TrackHintMatch::Exact(position);
    }
    if hint.language.is_none() {
        return TrackHintMatch::NotFound;
    }
    let language_title_codec = closest_by_ordinal(hint, candidates, |candidate| {
        language_matches(hint, candidate)
            && title_matches(hint, candidate)
            && codec_matches(hint, candidate)
    });
    if let Some(position) = language_title_codec {
        return TrackHintMatch::Approximate(position);
    }
    let language_codec = closest_by_ordinal(hint, candidates, |candidate| {
        language_matches(hint, candidate) && codec_matches(hint, candidate)
    });
    match language_codec {
        Some(position) => TrackHintMatch::Approximate(position),
        None => TrackHintMatch::NotFound,
    }
}

/// Позиция подходящего кандидата с минимальным расстоянием по порядковому номеру
/// (`min_by_key` при равенстве возвращает первого).
fn closest_by_ordinal(
    hint: &RememberedTrackHint,
    candidates: &[TrackHintCandidate],
    is_acceptable: impl Fn(&TrackHintCandidate) -> bool,
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| is_acceptable(candidate))
        .min_by_key(|(_, candidate)| {
            candidate
                .ordinal_among_kind
                .abs_diff(hint.ordinal_among_kind)
        })
        .map(|(position, _)| position)
}

fn language_matches(hint: &RememberedTrackHint, candidate: &TrackHintCandidate) -> bool {
    match (&hint.language, &candidate.language) {
        (Some(remembered), Some(current)) => remembered.eq_ignore_ascii_case(current),
        (None, None) => true,
        _ => false,
    }
}

fn title_matches(hint: &RememberedTrackHint, candidate: &TrackHintCandidate) -> bool {
    hint.title == candidate.title
}

fn codec_matches(hint: &RememberedTrackHint, candidate: &TrackHintCandidate) -> bool {
    hint.codec_id == candidate.codec_id
}
