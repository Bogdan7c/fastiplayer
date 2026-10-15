//! Результат поиска: кандидаты и отчёт о том, что было пропущено или обрезано.

use std::io;
use std::path::PathBuf;

use subtitle_core::{SubtitleFormat, SubtitleLanguage, SubtitleTrackDisposition};

/// Формат файла субтитров по расширению.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SidecarSubtitleFormat {
    Srt,
    WebVtt,
    Ass,
    Ssa,
}

impl SidecarSubtitleFormat {
    /// Определяет формат по расширению без учёта регистра; прочие расширения — `None`.
    pub(crate) fn from_extension(extension: &str) -> Option<Self> {
        match extension.to_ascii_lowercase().as_str() {
            "srt" => Some(Self::Srt),
            "vtt" => Some(Self::WebVtt),
            "ass" => Some(Self::Ass),
            "ssa" => Some(Self::Ssa),
            _ => None,
        }
    }

    /// Можно ли показать этот формат уже сейчас (текстовые SRT/WebVTT). ASS/SSA найдены,
    /// но рендер появится в сессии 08 — до тех пор вызывающий код их пропускает.
    pub fn is_renderable_now(self) -> bool {
        matches!(self, Self::Srt | Self::WebVtt)
    }

    /// Соответствующий формат нейтральной модели.
    pub fn to_subtitle_format(self) -> SubtitleFormat {
        match self {
            Self::Srt => SubtitleFormat::Srt,
            Self::WebVtt => SubtitleFormat::WebVtt,
            Self::Ass => SubtitleFormat::Ass,
            Self::Ssa => SubtitleFormat::Ssa,
        }
    }
}

/// Где найден файл; определяет группу порядка (кроме точного совпадения имени).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SidecarLocation {
    /// Каталог видео.
    VideoDirectory,
    /// `Subs`/`Sub`/`Subtitles`/`Subtitle` рядом с видео.
    SubtitleDirectory,
    /// `Subs/<основа видео>/`.
    MovieNamedDirectory,
}

/// Один найденный файл субтитров.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarSubtitleCandidate {
    /// Точный путь к файлу (без потерь для не-UTF-8 имён).
    pub path: PathBuf,
    pub format: SidecarSubtitleFormat,
    pub location: SidecarLocation,
    /// Язык из имени файла, если распознан.
    pub language: Option<SubtitleLanguage>,
    /// Только `is_forced` может быть выставлен (токен `forced`).
    pub disposition: SubtitleTrackDisposition,
    /// Токен `sdh`/`cc`/`hi`. Только запись подсказки; на выбор дорожки пока не влияет.
    pub is_hearing_impaired: bool,
    /// Нераспознанные токены имени, склеенные пробелом. Строка только для показа
    /// (с заменой невалидных байтов), пути из неё не строятся.
    pub title: Option<String>,
}

/// Каталог, который не удалось прочитать; путь хранится, но не печатается в `Display`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreadableDirectory {
    pub path: PathBuf,
    pub error_kind: io::ErrorKind,
}

/// Что произошло при поиске, кроме самих кандидатов. Все счётчики — «молча не теряем».
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidecarSearchReport {
    /// Сколько каталогов было открыто для просмотра.
    pub directories_scanned: usize,
    /// Сколько записей каталогов просмотрено суммарно.
    pub entries_scanned: usize,
    /// В скольких каталогах просмотр остановлен лимитом записей.
    pub directories_truncated_by_entry_limit: usize,
    /// Сколько кандидатов отброшено сверх `max_candidates` (после упорядочивания).
    pub candidates_dropped_by_limit: usize,
    /// Сколько подходящих подкаталогов не открыто из-за `max_subdirectory_depth`.
    pub subdirectories_skipped_by_depth_limit: usize,
    /// Записи каталога, которые не удалось прочитать/проверить (ошибка I/O).
    pub unreadable_entries: usize,
    /// Каталоги, которые не удалось открыть.
    pub unreadable_directories: Vec<UnreadableDirectory>,
}

impl SidecarSearchReport {
    /// Был ли результат неполным из-за лимитов (но не из-за ошибок I/O).
    pub fn is_truncated(&self) -> bool {
        self.directories_truncated_by_entry_limit > 0
            || self.candidates_dropped_by_limit > 0
            || self.subdirectories_skipped_by_depth_limit > 0
    }
}

/// Итог поиска: кандидаты в порядке предпочтения и отчёт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarSearchOutcome {
    pub candidates: Vec<SidecarSubtitleCandidate>,
    pub report: SidecarSearchReport,
}
