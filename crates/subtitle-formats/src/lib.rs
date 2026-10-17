//! Загрузка текстовых форматов субтитров в нейтральную модель `subtitle-core`.
//!
//! Конвейер: байты -> [`decode_subtitle_bytes`] (кодировка) -> парсер формата ->
//! [`ParsedSubtitleDocument`] (события, регионы, отчёт). Точка входа — [`load_text_subtitle`].
//!
//! Фатальны только ошибки файла целиком ([`SubtitleLoadError`]: слишком большой вход,
//! слишком много событий). Проблемы отдельных реплик пропускаются и попадают
//! в [`SubtitleParseReport`] — плохая реплика не должна ломать весь фильм.

mod ass;
mod document;
mod embedded;
mod encoding;
mod error;
mod limits;
mod load;
mod report;
mod sniff;
mod srt;
mod webvtt;

pub use ass::{
    AssHeaderReport, AssParseError, AssParseLimits, AssParseReport, AssTagStats, parse_ass_script,
};
pub use document::ParsedSubtitleDocument;
pub use embedded::{
    EmbeddedAssEvent, EmbeddedCue, EmbeddedCueError, EmbeddedCueFormat, EmbeddedCueLimits,
    EmbeddedCueMalformation, EmbeddedCueOutcome, parse_embedded_ass_event, parse_embedded_cue,
};
pub use encoding::{
    DecodedSubtitleText, DetectedSubtitleEncoding, LegacyEncodingHint, decode_subtitle_bytes,
};
pub use error::SubtitleLoadError;
pub use limits::SubtitleParseLimits;
pub use load::{LoadedTextSubtitle, load_text_subtitle};
pub use report::{CueSettingName, LineNumber, ParseIssue, ParseIssueKind, SubtitleParseReport};
pub use sniff::{TextSubtitleFormat, sniff_text_subtitle_format};
