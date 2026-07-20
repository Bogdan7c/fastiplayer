//! Публичная bounded CUE preview model без queue/player authority.

use std::fmt;
use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

use media_core::MediaTime;
use playlist_core::{DurableReopenLocator, PlaylistSingleImportDraft};

/// CUE использует ровно 75 frames в одной секунде.
pub const CUE_FRAMES_PER_SECOND: u64 = 75;

/// One-based физическая строка CUE document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CueLineNumber(NonZeroUsize);

impl CueLineNumber {
    /// Создаёт line number из zero-based iterator index.
    pub(crate) fn from_zero_based(index: usize) -> Option<Self> {
        index.checked_add(1).and_then(NonZeroUsize::new).map(Self)
    }

    /// Возвращает one-based номер строки.
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

/// Доказанная text encoding входного CUE.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueTextEncoding {
    /// Strict UTF-8 без BOM.
    Utf8,
    /// Strict UTF-8 с optional marker, который был реально найден.
    Utf8WithBom,
    /// UTF-16 little-endian с обязательным BOM.
    Utf16LittleEndianWithBom,
    /// UTF-16 big-endian с обязательным BOM.
    Utf16BigEndianWithBom,
}

/// Поддержанный FILE type, совместимый с текущим audio demux profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueFileTypeKind {
    /// RIFF/WAVE.
    Wave,
    /// AIFF.
    Aiff,
    /// MPEG audio.
    Mp3,
    /// FLAC.
    Flac,
}

/// Explicit FILE type с сохранением исходного token case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueFileType {
    kind: CueFileTypeKind,
    declared_token: String,
}

impl CueFileType {
    pub(crate) fn new(kind: CueFileTypeKind, declared_token: String) -> Self {
        Self {
            kind,
            declared_token,
        }
    }

    /// Возвращает semantic type.
    pub const fn kind(&self) -> CueFileTypeKind {
        self.kind
    }

    /// Возвращает exact case-preserved FILE type token.
    pub fn declared_token(&self) -> &str {
        &self.declared_token
    }
}

/// Exact CUE timestamp и его checked total-frame identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CueTimestamp {
    minutes: u64,
    seconds: u8,
    frames: u8,
    total_frames: u64,
}

impl CueTimestamp {
    pub(crate) fn new(minutes: u64, seconds: u8, frames: u8, total_frames: u64) -> Self {
        Self {
            minutes,
            seconds,
            frames,
            total_frames,
        }
    }

    /// Возвращает исходное поле MM без artificial two-digit cap.
    pub const fn minutes(self) -> u64 {
        self.minutes
    }

    /// Возвращает validated SS в `00..=59`.
    pub const fn seconds(self) -> u8 {
        self.seconds
    }

    /// Возвращает validated FF в `00..=74`.
    pub const fn frames(self) -> u8 {
        self.frames
    }

    /// Возвращает exact rational timeline identity в 1/75 секунды.
    pub const fn total_frames(self) -> u64 {
        self.total_frames
    }

    /// Переводит frame identity в нейтральный timeline без overflow.
    pub fn media_time(self) -> MediaTime {
        let whole_seconds = self.total_frames / CUE_FRAMES_PER_SECOND;
        let remaining_frames = self.total_frames % CUE_FRAMES_PER_SECOND;
        let subsecond_nanos = (remaining_frames * 1_000_000_000 / CUE_FRAMES_PER_SECOND) as u32;
        MediaTime::from_duration(Duration::new(whole_seconds, subsecond_nanos))
    }
}

/// Один retained INDEX marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CueIndex {
    number: u8,
    timestamp: CueTimestamp,
    line: CueLineNumber,
}

impl CueIndex {
    pub(crate) fn new(number: u8, timestamp: CueTimestamp, line: CueLineNumber) -> Self {
        Self {
            number,
            timestamp,
            line,
        }
    }

    /// Возвращает INDEX number.
    pub const fn number(self) -> u8 {
        self.number
    }

    /// Возвращает exact 75-fps timestamp.
    pub const fn timestamp(self) -> CueTimestamp {
        self.timestamp
    }

    /// Возвращает source line.
    pub const fn line(self) -> CueLineNumber {
        self.line
    }
}

/// Одна explicit FILE section CUE document.
#[derive(Clone, PartialEq, Eq)]
pub struct CueFile {
    declared_path: String,
    resolved_locator: DurableReopenLocator,
    file_type: CueFileType,
    line: CueLineNumber,
}

impl CueFile {
    pub(crate) fn new(
        declared_path: String,
        resolved_locator: DurableReopenLocator,
        file_type: CueFileType,
        line: CueLineNumber,
    ) -> Self {
        Self {
            declared_path,
            resolved_locator,
            file_type,
            line,
        }
    }

    /// Возвращает decoded case-preserved FILE payload.
    pub fn declared_path(&self) -> &str {
        &self.declared_path
    }

    /// Возвращает exact durable local locator.
    pub fn resolved_locator(&self) -> &DurableReopenLocator {
        &self.resolved_locator
    }

    /// Возвращает explicit FILE type.
    pub const fn file_type(&self) -> &CueFileType {
        &self.file_type
    }

    /// Возвращает source line FILE command.
    pub const fn line(&self) -> CueLineNumber {
        self.line
    }
}

impl fmt::Debug for CueFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CueFile")
            .field("declared_path", &"<redacted>")
            .field("resolved_locator", &self.resolved_locator)
            .field("file_type", &self.file_type)
            .field("line", &self.line)
            .finish()
    }
}

/// Retained unknown command, который запрещает lossless export.
#[derive(Clone, PartialEq, Eq)]
pub struct CueUnknownCommand {
    command: String,
    arguments: String,
    line: CueLineNumber,
}

impl fmt::Debug for CueUnknownCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CueUnknownCommand")
            .field("command", &self.command)
            .field("arguments", &"<redacted>")
            .field("line", &self.line)
            .finish()
    }
}

impl CueUnknownCommand {
    pub(crate) fn new(command: String, arguments: String, line: CueLineNumber) -> Self {
        Self {
            command,
            arguments,
            line,
        }
    }

    /// Возвращает case-preserved command token.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// Возвращает case-preserved payload без leading indentation.
    pub fn arguments(&self) -> &str {
        &self.arguments
    }

    /// Возвращает source line.
    pub const fn line(&self) -> CueLineNumber {
        self.line
    }
}

/// Typed причина, почему будущий CUE exporter не может обещать exact preservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueExportIneligibility {
    /// Parser retained unknown command.
    UnknownCommand {
        /// Source line unknown command.
        line: CueLineNumber,
    },
    /// Track retained semantic sub-index, который текущий export subset не обещает.
    RetainedSubIndex {
        /// CUE track number.
        track_number: u8,
        /// INDEX number из `02..=99`.
        index_number: u8,
    },
}

/// Один AUDIO track preview и готовый ID-less domain draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueTrack {
    number: u8,
    file_index: usize,
    title: Option<String>,
    performer: Option<String>,
    indexes: Box<[CueIndex]>,
    import_draft: PlaylistSingleImportDraft,
}

impl CueTrack {
    pub(crate) fn new(
        number: u8,
        file_index: usize,
        title: Option<String>,
        performer: Option<String>,
        indexes: Vec<CueIndex>,
        import_draft: PlaylistSingleImportDraft,
    ) -> Self {
        Self {
            number,
            file_index,
            title,
            performer,
            indexes: indexes.into_boxed_slice(),
            import_draft,
        }
    }

    /// Возвращает original CUE TRACK number.
    pub const fn number(&self) -> u8 {
        self.number
    }

    /// Возвращает zero-based FILE section index.
    pub const fn file_index(&self) -> usize {
        self.file_index
    }

    /// Возвращает case-preserved track TITLE.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Возвращает case-preserved track PERFORMER.
    pub fn performer(&self) -> Option<&str> {
        self.performer.as_deref()
    }

    /// Возвращает validated INDEX markers в source order.
    pub fn indexes(&self) -> &[CueIndex] {
        &self.indexes
    }

    /// Возвращает ID-less domain draft без queue allocation.
    pub const fn import_draft(&self) -> &PlaylistSingleImportDraft {
        &self.import_draft
    }
}

/// Полный bounded CUE preview.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueDocument {
    source: super::CueDocumentSource,
    encoding: CueTextEncoding,
    title: Option<String>,
    performer: Option<String>,
    files: Box<[CueFile]>,
    tracks: Box<[CueTrack]>,
    unknown_commands: Box<[CueUnknownCommand]>,
    export_ineligibilities: Box<[CueExportIneligibility]>,
}

impl CueDocument {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source: super::CueDocumentSource,
        encoding: CueTextEncoding,
        title: Option<String>,
        performer: Option<String>,
        files: Vec<CueFile>,
        tracks: Vec<CueTrack>,
        unknown_commands: Vec<CueUnknownCommand>,
        export_ineligibilities: Vec<CueExportIneligibility>,
    ) -> Self {
        Self {
            source,
            encoding,
            title,
            performer,
            files: files.into_boxed_slice(),
            tracks: tracks.into_boxed_slice(),
            unknown_commands: unknown_commands.into_boxed_slice(),
            export_ineligibilities: export_ineligibilities.into_boxed_slice(),
        }
    }

    /// Возвращает exact local CUE source identity.
    pub const fn source(&self) -> &super::CueDocumentSource {
        &self.source
    }

    /// Возвращает доказанную input encoding.
    pub const fn encoding(&self) -> CueTextEncoding {
        self.encoding
    }

    /// Возвращает case-preserved document TITLE.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Возвращает case-preserved document PERFORMER.
    pub fn performer(&self) -> Option<&str> {
        self.performer.as_deref()
    }

    /// Возвращает FILE sections в source order.
    pub fn files(&self) -> &[CueFile] {
        &self.files
    }

    /// Возвращает AUDIO tracks в source order.
    pub fn tracks(&self) -> &[CueTrack] {
        &self.tracks
    }

    /// Возвращает retained unknown commands.
    pub fn unknown_commands(&self) -> &[CueUnknownCommand] {
        &self.unknown_commands
    }

    /// Возвращает все deterministic причины запрета CUE export.
    pub fn export_ineligibilities(&self) -> &[CueExportIneligibility] {
        &self.export_ineligibilities
    }

    /// Сообщает, доказана ли semantic eligibility будущего exact CUE export.
    pub fn is_export_eligible(&self) -> bool {
        self.export_ineligibilities.is_empty()
    }

    /// Возвращает исходный local path без lossy conversion.
    pub fn source_path(&self) -> &Path {
        self.source.path()
    }
}

/// Fatal CUE parse/validation error с bounded public taxonomy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CueParseError {
    kind: CueParseErrorKind,
}

impl CueParseError {
    pub(crate) fn new(kind: CueParseErrorKind) -> Self {
        Self { kind }
    }

    /// Возвращает typed cause без locator/raw document leakage.
    pub const fn kind(&self) -> &CueParseErrorKind {
        &self.kind
    }
}

impl fmt::Display for CueParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "CUE document отклонён: {:?}", self.kind)
    }
}

impl std::error::Error for CueParseError {}

/// Fatal grammar/budget/domain taxonomy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CueParseErrorKind {
    /// Byte slice превышает request budget.
    DocumentLimitExceeded,
    /// BOM/bytes не соответствуют UTF-8 или разрешённому BOM-marked UTF-16.
    UnsupportedOrInvalidEncoding,
    /// Декодированная строка превышает request budget.
    LineLimitExceeded {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// Retained metadata/unknown-command text превысил общий budget.
    RetainedTextLimitExceeded {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// FILE section count превысил request budget.
    FileLimitExceeded {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// Unknown-command count превысил request budget.
    UnknownCommandLimitExceeded {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// Command grammar malformed.
    MalformedCommand {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// FILE command отсутствует перед TRACK.
    TrackWithoutFile {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// FILE type отсутствует либо не входит в доказанный audio demux profile.
    UnsupportedFileType {
        /// One-based source line.
        line: CueLineNumber,
        /// Bounded case-preserved token.
        declared_type: String,
    },
    /// TRACK number вне `01..=99`.
    InvalidTrackNumber {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// Следующий TRACK number не равен предыдущему + 1.
    NonSequentialTrackNumber {
        /// One-based source line.
        line: CueLineNumber,
        /// Ожидаемый следующий номер.
        expected: u8,
        /// Фактически объявленный номер.
        actual: u8,
    },
    /// TRACK mode не равен AUDIO.
    DataTrackUnsupported {
        /// One-based source line.
        line: CueLineNumber,
        /// Bounded case-preserved mode token.
        declared_mode: String,
    },
    /// INDEX объявлен вне TRACK.
    IndexWithoutTrack {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// INDEX number не представим в supported `00..=99`.
    InvalidIndexNumber {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// AUDIO track не содержит ровно один INDEX 01.
    MissingIndex01 {
        /// TRACK number.
        track_number: u8,
    },
    /// INDEX number grammar нарушена.
    InvalidIndexSequence {
        /// One-based source line.
        line: CueLineNumber,
        /// TRACK number.
        track_number: u8,
        /// Фактический INDEX number.
        actual: u8,
    },
    /// MM:SS:FF malformed либо выходит за `SS/FF` ranges.
    InvalidTimestamp {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// Checked conversion MM:SS:FF → total frames переполнена.
    TimestampOverflow {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// INDEX timestamp уменьшился внутри одной FILE section.
    TimestampMovedBackwards {
        /// One-based source line.
        line: CueLineNumber,
    },
    /// Два соседних playable tracks образовали пустой span.
    EmptyPlaybackSpan {
        /// Предыдущий TRACK number.
        track_number: u8,
    },
    /// playlist-core отверг ID-less payload после parser validation.
    DomainDraftRejected {
        /// TRACK number.
        track_number: u8,
    },
    /// Документ не содержит ни одного AUDIO track.
    NoAudioTracks,
}
