//! Поиск файлов субтитров рядом с локальным видео («sidecar»), решение владельца 13.
//!
//! Крейт нейтральный: знает только модель `subtitle-core` и файловую систему `std`.
//! Он НЕ читает содержимое файлов и НЕ парсит их — только находит кандидатов, определяет
//! формат по расширению и подсказки (язык, forced, SDH) по имени файла.
//!
//! Правила поиска (видео `Movie.mkv`, основа `Movie`):
//! * каталоги: папка видео; в ней подпапки `Subs`/`Sub`/`Subtitles`/`Subtitle`
//!   (ASCII без учёта регистра); в них подпапка с именем основы видео (`Subs/Movie/`);
//! * в папке видео и в `Subs`-подобных: основа файла начинается с основы видео (без учёта
//!   регистра), а следующий символ — конец имени или разделитель `. _ - пробел [ (`;
//!   `Movie2.srt` не подходит;
//! * в `Subs/<Movie>/` подходит любой файл субтитров;
//! * расширения `srt`, `vtt`, `ass`, `ssa` (без учёта регистра); ASS/SSA находятся, но
//!   помечены как пока не отображаемые — решает вызывающий код;
//! * пути всегда `PathBuf` без прогона через `String`; имена не в UTF-8 работают.
//!
//! Порядок результата: 1) точное `Movie.<ext>` в папке видео, 2) прочие совпадения там же,
//! 3) файлы в `Subs`-подобных папках, 4) файлы в `Subs/<Movie>/`. Внутри группы — по
//! порядку `OsStr` имени файла (на unix побайтовый).

mod chosen_file;
mod error;
mod language_hint;
mod limits;
mod matching;
mod outcome;
mod scan;
mod search;

pub use chosen_file::{describe_subtitle_file, is_subtitle_file_extension};
pub use error::SidecarSearchError;
pub use limits::SidecarSearchLimits;
pub use outcome::{
    SidecarLocation, SidecarSearchOutcome, SidecarSearchReport, SidecarSubtitleCandidate,
    SidecarSubtitleFormat, UnreadableDirectory,
};
pub use search::find_sidecar_subtitles;
