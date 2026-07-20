//! Последовательная CUE state machine с checked timeline arithmetic.

use std::num::NonZeroU32;
use std::path::Path;

use media_core::TrackNumber;
use playlist_core::{
    CachedPlaylistMetadata, DurableReopenLocator, LocalLocator, PlaylistImportAvailability,
    PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistMediaKind, PlaylistPlaybackSpan,
    PlaylistSingleImportDraft,
};

use super::encoding::decode_cue_text;
use super::{
    CUE_FRAMES_PER_SECOND, CueDocument, CueDocumentSource, CueExportIneligibility, CueFile,
    CueFileType, CueFileTypeKind, CueIndex, CueLineNumber, CueParseError, CueParseErrorKind,
    CueParseRequest, CueParserLimits, CueTextEncoding, CueTimestamp, CueTrack, CueUnknownCommand,
};

/// Временное FILE-state хранит ordering invariant, не публикуя его как второй domain owner.
struct FileBuilder {
    file: CueFile,
    last_total_frames: Option<u64>,
}

/// Временное TRACK-state хранит grammar до построения immutable domain draft.
struct TrackBuilder {
    number: u8,
    file_index: usize,
    title: Option<String>,
    performer: Option<String>,
    indexes: Vec<CueIndex>,
}

impl TrackBuilder {
    /// Возвращает обязательный INDEX 01 после grammar validation.
    fn index01(&self) -> Option<CueIndex> {
        self.indexes
            .iter()
            .copied()
            .find(|index| index.number() == 1)
    }
}

/// Parser-owned mutable state одного bounded document.
struct ParserState {
    source: CueDocumentSource,
    limits: CueParserLimits,
    title: Option<String>,
    performer: Option<String>,
    files: Vec<FileBuilder>,
    tracks: Vec<TrackBuilder>,
    current_file_index: Option<usize>,
    current_track_index: Option<usize>,
    previous_track_number: Option<u8>,
    unknown_commands: Vec<CueUnknownCommand>,
    export_ineligibilities: Vec<CueExportIneligibility>,
    retained_text_bytes: usize,
}

impl ParserState {
    /// Создаёт empty state без queue IDs и I/O handles.
    fn new(source: CueDocumentSource, limits: CueParserLimits) -> Self {
        Self {
            source,
            limits,
            title: None,
            performer: None,
            files: Vec::new(),
            tracks: Vec::new(),
            current_file_index: None,
            current_track_index: None,
            previous_track_number: None,
            unknown_commands: Vec::new(),
            export_ineligibilities: Vec::new(),
            retained_text_bytes: 0,
        }
    }

    /// Учитывает retained text до изменения parser state.
    fn retain_text(&mut self, byte_count: usize, line: CueLineNumber) -> Result<(), CueParseError> {
        let next_total = self
            .retained_text_bytes
            .checked_add(byte_count)
            .ok_or_else(|| {
                CueParseError::new(CueParseErrorKind::RetainedTextLimitExceeded { line })
            })?;
        if next_total > self.limits.max_retained_text_bytes() {
            return Err(CueParseError::new(
                CueParseErrorKind::RetainedTextLimitExceeded { line },
            ));
        }
        self.retained_text_bytes = next_total;
        Ok(())
    }

    /// Добавляет FILE section после полного grammar/profile preflight.
    fn push_file(
        &mut self,
        declared_path: String,
        declared_type: String,
        line: CueLineNumber,
    ) -> Result<(), CueParseError> {
        self.ensure_current_track_has_index01()?;
        if self.files.len() >= self.limits.max_files() {
            return Err(CueParseError::new(CueParseErrorKind::FileLimitExceeded {
                line,
            }));
        }
        let file_type = parse_file_type(declared_type, line)?;
        let resolved_locator = resolve_cue_media_path(&self.source, &declared_path);
        self.retain_text(declared_path.len() + file_type.declared_token().len(), line)?;
        self.files.push(FileBuilder {
            file: CueFile::new(declared_path, resolved_locator, file_type, line),
            last_total_frames: None,
        });
        self.current_file_index = Some(self.files.len() - 1);
        self.current_track_index = None;
        Ok(())
    }

    /// Добавляет AUDIO track и проверяет global sequential numbering.
    fn push_track(
        &mut self,
        number: u8,
        declared_mode: String,
        line: CueLineNumber,
    ) -> Result<(), CueParseError> {
        self.ensure_current_track_has_index01()?;
        let file_index = self
            .current_file_index
            .ok_or_else(|| CueParseError::new(CueParseErrorKind::TrackWithoutFile { line }))?;
        if !declared_mode.eq_ignore_ascii_case("AUDIO") {
            return Err(CueParseError::new(
                CueParseErrorKind::DataTrackUnsupported {
                    line,
                    declared_mode,
                },
            ));
        }
        if !(1..=99).contains(&number) {
            return Err(CueParseError::new(CueParseErrorKind::InvalidTrackNumber {
                line,
            }));
        }
        if let Some(previous) = self.previous_track_number {
            let expected = previous.checked_add(1).ok_or_else(|| {
                CueParseError::new(CueParseErrorKind::NonSequentialTrackNumber {
                    line,
                    expected: 99,
                    actual: number,
                })
            })?;
            if number != expected {
                return Err(CueParseError::new(
                    CueParseErrorKind::NonSequentialTrackNumber {
                        line,
                        expected,
                        actual: number,
                    },
                ));
            }
        }

        self.tracks.push(TrackBuilder {
            number,
            file_index,
            title: None,
            performer: None,
            indexes: Vec::new(),
        });
        self.current_track_index = Some(self.tracks.len() - 1);
        self.previous_track_number = Some(number);
        Ok(())
    }

    /// Добавляет INDEX после grammar и cross-track FILE ordering checks.
    fn push_index(
        &mut self,
        number: u8,
        timestamp: CueTimestamp,
        line: CueLineNumber,
    ) -> Result<(), CueParseError> {
        let track_index = self
            .current_track_index
            .ok_or_else(|| CueParseError::new(CueParseErrorKind::IndexWithoutTrack { line }))?;
        let file_index = self.tracks[track_index].file_index;
        if !is_valid_next_index(&self.tracks[track_index].indexes, number) {
            return Err(CueParseError::new(
                CueParseErrorKind::InvalidIndexSequence {
                    line,
                    track_number: self.tracks[track_index].number,
                    actual: number,
                },
            ));
        }
        if self.files[file_index]
            .last_total_frames
            .is_some_and(|previous| timestamp.total_frames() < previous)
        {
            return Err(CueParseError::new(
                CueParseErrorKind::TimestampMovedBackwards { line },
            ));
        }

        self.files[file_index].last_total_frames = Some(timestamp.total_frames());
        self.tracks[track_index]
            .indexes
            .push(CueIndex::new(number, timestamp, line));
        if number >= 2 {
            self.export_ineligibilities
                .push(CueExportIneligibility::RetainedSubIndex {
                    track_number: self.tracks[track_index].number,
                    index_number: number,
                });
        }
        Ok(())
    }

    /// Применяет TITLE/PERFORMER к текущему track либо document scope.
    fn set_metadata(
        &mut self,
        command: &str,
        value: String,
        line: CueLineNumber,
    ) -> Result<(), CueParseError> {
        self.retain_text(value.len(), line)?;
        let target = if let Some(track_index) = self.current_track_index {
            let track = &mut self.tracks[track_index];
            if command.eq_ignore_ascii_case("TITLE") {
                &mut track.title
            } else {
                &mut track.performer
            }
        } else if command.eq_ignore_ascii_case("TITLE") {
            &mut self.title
        } else {
            &mut self.performer
        };
        if target.is_some() {
            return Err(CueParseError::new(CueParseErrorKind::MalformedCommand {
                line,
            }));
        }
        *target = Some(value);
        Ok(())
    }

    /// Retains unknown command и фиксирует exact-export prohibition.
    fn push_unknown(
        &mut self,
        command: String,
        arguments: String,
        line: CueLineNumber,
    ) -> Result<(), CueParseError> {
        if self.unknown_commands.len() >= self.limits.max_unknown_commands() {
            return Err(CueParseError::new(
                CueParseErrorKind::UnknownCommandLimitExceeded { line },
            ));
        }
        self.retain_text(command.len() + arguments.len(), line)?;
        self.unknown_commands
            .push(CueUnknownCommand::new(command, arguments, line));
        self.export_ineligibilities
            .push(CueExportIneligibility::UnknownCommand { line });
        Ok(())
    }

    /// Проверяет закрываемый TRACK до FILE/TRACK/EOF boundary.
    fn ensure_current_track_has_index01(&self) -> Result<(), CueParseError> {
        let Some(track_index) = self.current_track_index else {
            return Ok(());
        };
        let track = &self.tracks[track_index];
        if track.index01().is_none() {
            return Err(CueParseError::new(CueParseErrorKind::MissingIndex01 {
                track_number: track.number,
            }));
        }
        Ok(())
    }

    /// Завершает state machine и строит immutable previews/domain drafts.
    fn finish(self, encoding: CueTextEncoding) -> Result<CueDocument, CueParseError> {
        self.ensure_current_track_has_index01()?;
        if self.tracks.is_empty() {
            return Err(CueParseError::new(CueParseErrorKind::NoAudioTracks));
        }

        let mut finished_tracks = Vec::with_capacity(self.tracks.len());
        for (track_index, track) in self.tracks.iter().enumerate() {
            let start = track
                .index01()
                .expect("finish preflight proves INDEX 01")
                .timestamp()
                .media_time();
            let end_exclusive = self
                .tracks
                .get(track_index + 1)
                .filter(|next_track| next_track.file_index == track.file_index)
                .and_then(TrackBuilder::index01)
                .map(CueIndex::timestamp)
                .map(CueTimestamp::media_time);
            let playback_span = PlaylistPlaybackSpan::new(start, end_exclusive).map_err(|_| {
                CueParseError::new(CueParseErrorKind::EmptyPlaybackSpan {
                    track_number: track.number,
                })
            })?;
            let import_draft = build_track_import_draft(&self, track, playback_span, track_index)?;
            finished_tracks.push(CueTrack::new(
                track.number,
                track.file_index,
                track.title.clone(),
                track.performer.clone(),
                track.indexes.clone(),
                import_draft,
            ));
        }

        Ok(CueDocument::new(
            self.source,
            encoding,
            self.title,
            self.performer,
            self.files.into_iter().map(|file| file.file).collect(),
            finished_tracks,
            self.unknown_commands,
            self.export_ineligibilities,
        ))
    }
}

/// Единственная S12 entry point: bytes → bounded CUE document без hidden I/O.
pub fn parse_cue_document(request: CueParseRequest<'_>) -> Result<CueDocument, CueParseError> {
    let (document_bytes, source, limits) = request.into_parts();
    if document_bytes.len() > limits.max_document_bytes() {
        return Err(CueParseError::new(CueParseErrorKind::DocumentLimitExceeded));
    }
    let (decoded_text, encoding) = decode_cue_text(document_bytes)?;
    let mut parser = ParserState::new(source, limits);

    for (zero_based_line, raw_line) in decoded_text.split('\n').enumerate() {
        let line = CueLineNumber::from_zero_based(zero_based_line)
            .ok_or_else(|| CueParseError::new(CueParseErrorKind::DocumentLimitExceeded))?;
        let line_without_cr = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line_without_cr.len() > limits.max_line_bytes() {
            return Err(CueParseError::new(CueParseErrorKind::LineLimitExceeded {
                line,
            }));
        }
        parse_line(&mut parser, line_without_cr, line)?;
    }

    parser.finish(encoding)
}

/// Разбирает одну physical line и передаёт mutation владельцу state.
fn parse_line(
    parser: &mut ParserState,
    raw_line: &str,
    line: CueLineNumber,
) -> Result<(), CueParseError> {
    let trimmed = raw_line.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if trimmed.chars().any(|character| character.is_control()) {
        return Err(CueParseError::new(CueParseErrorKind::MalformedCommand {
            line,
        }));
    }
    let (command, arguments) = split_command(trimmed);
    if command.eq_ignore_ascii_case("FILE") {
        let (declared_path, declared_type) = parse_file_arguments(arguments, line)?;
        return parser.push_file(declared_path, declared_type, line);
    }
    if command.eq_ignore_ascii_case("TRACK") {
        let (number, mode) = parse_track_arguments(arguments, line)?;
        return parser.push_track(number, mode, line);
    }
    if command.eq_ignore_ascii_case("INDEX") {
        let (number, timestamp) = parse_index_arguments(arguments, line)?;
        return parser.push_index(number, timestamp, line);
    }
    if command.eq_ignore_ascii_case("TITLE") || command.eq_ignore_ascii_case("PERFORMER") {
        let metadata = parse_metadata(arguments, line)?;
        return parser.set_metadata(command, metadata, line);
    }
    parser.push_unknown(command.to_owned(), arguments.to_owned(), line)
}

/// Отделяет command token от case-preserved arguments.
fn split_command(line: &str) -> (&str, &str) {
    let command_end = line.find(char::is_whitespace).unwrap_or(line.len());
    let command = &line[..command_end];
    let arguments = line[command_end..].trim_start();
    (command, arguments)
}

/// Разбирает FILE path и mandatory type.
fn parse_file_arguments(
    arguments: &str,
    line: CueLineNumber,
) -> Result<(String, String), CueParseError> {
    if let Some(quoted) = arguments.strip_prefix('"') {
        let closing_quote = quoted.find('"').ok_or_else(|| malformed(line))?;
        let path = &quoted[..closing_quote];
        let remaining = quoted[closing_quote + 1..].trim();
        let mut type_tokens = remaining.split_whitespace();
        let declared_type = type_tokens.next().ok_or_else(|| malformed(line))?;
        if path.is_empty() || type_tokens.next().is_some() {
            return Err(malformed(line));
        }
        return Ok((path.to_owned(), declared_type.to_owned()));
    }

    let mut tokens = arguments.split_whitespace();
    let path = tokens.next().ok_or_else(|| malformed(line))?;
    let declared_type = tokens.next().ok_or_else(|| malformed(line))?;
    if tokens.next().is_some() {
        return Err(malformed(line));
    }
    Ok((path.to_owned(), declared_type.to_owned()))
}

/// Разбирает TRACK number и explicit mode.
fn parse_track_arguments(
    arguments: &str,
    line: CueLineNumber,
) -> Result<(u8, String), CueParseError> {
    let mut tokens = arguments.split_whitespace();
    let number_text = tokens.next().ok_or_else(|| malformed(line))?;
    let mode = tokens.next().ok_or_else(|| malformed(line))?;
    if tokens.next().is_some() {
        return Err(malformed(line));
    }
    let number = number_text
        .parse::<u8>()
        .map_err(|_| CueParseError::new(CueParseErrorKind::InvalidTrackNumber { line }))?;
    Ok((number, mode.to_owned()))
}

/// Разбирает INDEX number и exact 75-fps timestamp.
fn parse_index_arguments(
    arguments: &str,
    line: CueLineNumber,
) -> Result<(u8, CueTimestamp), CueParseError> {
    let mut tokens = arguments.split_whitespace();
    let number_text = tokens.next().ok_or_else(|| malformed(line))?;
    let timestamp_text = tokens.next().ok_or_else(|| malformed(line))?;
    if tokens.next().is_some() {
        return Err(malformed(line));
    }
    let number = number_text
        .parse::<u8>()
        .map_err(|_| CueParseError::new(CueParseErrorKind::InvalidIndexNumber { line }))?;
    if number > 99 {
        return Err(CueParseError::new(CueParseErrorKind::InvalidIndexNumber {
            line,
        }));
    }
    Ok((number, parse_timestamp(timestamp_text, line)?))
}

/// Разбирает quoted/unquoted metadata без изменения регистра.
fn parse_metadata(arguments: &str, line: CueLineNumber) -> Result<String, CueParseError> {
    let trimmed = arguments.trim();
    if let Some(quoted) = trimmed.strip_prefix('"') {
        let value = quoted.strip_suffix('"').ok_or_else(|| malformed(line))?;
        if value.contains('"') {
            return Err(malformed(line));
        }
        return Ok(value.to_owned());
    }
    if trimmed.is_empty() {
        return Err(malformed(line));
    }
    Ok(trimmed.to_owned())
}

/// Проверяет FILE type против текущего доказанного audio demux profile.
fn parse_file_type(
    declared_type: String,
    line: CueLineNumber,
) -> Result<CueFileType, CueParseError> {
    let kind = if declared_type.eq_ignore_ascii_case("WAVE") {
        CueFileTypeKind::Wave
    } else if declared_type.eq_ignore_ascii_case("AIFF") {
        CueFileTypeKind::Aiff
    } else if declared_type.eq_ignore_ascii_case("MP3") {
        CueFileTypeKind::Mp3
    } else if declared_type.eq_ignore_ascii_case("FLAC") {
        CueFileTypeKind::Flac
    } else {
        return Err(CueParseError::new(CueParseErrorKind::UnsupportedFileType {
            line,
            declared_type,
        }));
    };
    Ok(CueFileType::new(kind, declared_type))
}

/// Разрешает decoded FILE path относительно exact native parent CUE document.
fn resolve_cue_media_path(source: &CueDocumentSource, declared_path: &str) -> DurableReopenLocator {
    let declared_path = Path::new(declared_path);
    let native_path = if declared_path.is_absolute() {
        declared_path.to_path_buf()
    } else {
        source
            .path()
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(declared_path)
    };
    DurableReopenLocator::local(LocalLocator::Native(native_path))
}

/// Проверяет optional first INDEX 00/01 и strict sequential retained suffix.
fn is_valid_next_index(indexes: &[CueIndex], candidate: u8) -> bool {
    match indexes.last().map(|index| index.number()) {
        None => matches!(candidate, 0 | 1),
        Some(0) => candidate == 1,
        Some(1) => candidate == 2,
        Some(previous @ 2..=98) => previous.checked_add(1) == Some(candidate),
        Some(99) | Some(_) => false,
    }
}

/// Парсит MM:SS:FF и проверяет каждую arithmetic boundary.
fn parse_timestamp(timestamp: &str, line: CueLineNumber) -> Result<CueTimestamp, CueParseError> {
    let mut fields = timestamp.split(':');
    let minutes_text = fields.next().ok_or_else(|| invalid_timestamp(line))?;
    let seconds_text = fields.next().ok_or_else(|| invalid_timestamp(line))?;
    let frames_text = fields.next().ok_or_else(|| invalid_timestamp(line))?;
    if fields.next().is_some()
        || minutes_text.is_empty()
        || seconds_text.len() != 2
        || frames_text.len() != 2
        || !minutes_text.bytes().all(|byte| byte.is_ascii_digit())
        || !seconds_text.bytes().all(|byte| byte.is_ascii_digit())
        || !frames_text.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_timestamp(line));
    }
    let minutes = minutes_text
        .parse::<u64>()
        .map_err(|_| timestamp_overflow(line))?;
    let seconds = seconds_text
        .parse::<u8>()
        .map_err(|_| invalid_timestamp(line))?;
    let frames = frames_text
        .parse::<u8>()
        .map_err(|_| invalid_timestamp(line))?;
    if seconds > 59 || u64::from(frames) >= CUE_FRAMES_PER_SECOND {
        return Err(invalid_timestamp(line));
    }
    let total_frames = minutes
        .checked_mul(60)
        .and_then(|minutes_in_seconds| minutes_in_seconds.checked_add(u64::from(seconds)))
        .and_then(|total_seconds| total_seconds.checked_mul(CUE_FRAMES_PER_SECOND))
        .and_then(|whole_second_frames| whole_second_frames.checked_add(u64::from(frames)))
        .ok_or_else(|| timestamp_overflow(line))?;
    Ok(CueTimestamp::new(minutes, seconds, frames, total_frames))
}

/// Строит metadata/provenance/span payload одного track.
fn build_track_import_draft(
    parser: &ParserState,
    track: &TrackBuilder,
    playback_span: PlaylistPlaybackSpan,
    zero_based_track_index: usize,
) -> Result<PlaylistSingleImportDraft, CueParseError> {
    let fallback_title = track
        .title
        .clone()
        .unwrap_or_else(|| format!("Трек {:02}", track.number));
    let mut metadata = CachedPlaylistMetadata::new(fallback_title, PlaylistMediaKind::Audio)
        .with_title(track.title.clone())
        .with_album(parser.title.clone())
        .with_duration(playback_span.duration())
        .with_sequence(
            None,
            Some(TrackNumber::new(u64::from(track.number))),
            None,
            None,
        );
    let performer = track.performer.as_ref().or(parser.performer.as_ref());
    if let Some(performer) = performer {
        metadata = metadata
            .with_artists(vec![performer.clone()])
            .map_err(|_| domain_draft_rejected(track.number))?;
    }
    let source_ordinal = zero_based_track_index
        .checked_add(1)
        .and_then(|ordinal| u32::try_from(ordinal).ok())
        .and_then(NonZeroU32::new)
        .ok_or_else(|| domain_draft_rejected(track.number))?;
    let provenance = PlaylistImportProvenance::new(
        parser.source.durable_root(),
        PlaylistImportSourceKind::Cue,
        Some(source_ordinal),
    );
    PlaylistSingleImportDraft::new(
        parser.files[track.file_index]
            .file
            .resolved_locator()
            .clone(),
        metadata,
        Some(playback_span),
        Vec::new(),
        provenance,
        PlaylistImportAvailability::Available,
    )
    .map_err(|_| domain_draft_rejected(track.number))
}

/// Создаёт common malformed-command error.
fn malformed(line: CueLineNumber) -> CueParseError {
    CueParseError::new(CueParseErrorKind::MalformedCommand { line })
}

/// Создаёт invalid timestamp error.
fn invalid_timestamp(line: CueLineNumber) -> CueParseError {
    CueParseError::new(CueParseErrorKind::InvalidTimestamp { line })
}

/// Создаёт checked timestamp overflow.
fn timestamp_overflow(line: CueLineNumber) -> CueParseError {
    CueParseError::new(CueParseErrorKind::TimestampOverflow { line })
}

/// Создаёт fail-closed domain draft error.
fn domain_draft_rejected(track_number: u8) -> CueParseError {
    CueParseError::new(CueParseErrorKind::DomainDraftRejected { track_number })
}
