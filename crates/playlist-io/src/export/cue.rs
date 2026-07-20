//! Pure CUE scope analysis, preflight и exact 75-fps serializer.

use playlist_core::{
    DurableReopenLocator, PlaylistCueDocumentExportEligibility, PlaylistCueFileType,
    PlaylistCueFrameIndex, PlaylistCueTrackExportSemantics, PlaylistEntry,
    PlaylistImportSourceKind, PlaylistItem, PlaylistPlaybackSpan,
};

use super::locator::preflight_item_locator;
use super::{
    PlaylistExportAvailability, PlaylistExportDocumentTarget, PlaylistExportFormat,
    PlaylistExportIneligible, PlaylistExportLocatorPolicy, PlaylistExportPreflightError,
    PlaylistExportSecretClassification, PlaylistExportSnapshot, PlaylistExportSubject,
    PreparedCueTrack, PreparedPlaylistExport,
};

/// Safe причина, почему canonical scope нельзя представить exact CUE document-ом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueExportScopeIneligibility {
    /// CUE export требует хотя бы один playable item.
    EmptyScope,
    /// Compound boundary не является CUE AUDIO top-level Single.
    CompoundEntry,
    /// Item не имеет durable CUE track semantics.
    MissingCueSemantics,
    /// Scope смешивает разные исходные CUE documents.
    MixedSourceDocuments,
    /// Исходный CUE содержал unknown command либо retained sub-index.
    SourceDocumentIneligible,
    /// Track/file order больше не является последовательным CUE scope.
    NonSequentialTracks,
    /// Конечную границу span нельзя выразить следующим `INDEX 01` либо EOF.
    UnrepresentablePlaybackBoundary,
    /// CUE FILE поддерживает только exact local UTF-8 media path.
    NonLocalOrNonUtf8File,
    /// FILE path либо metadata содержат непредставимый CUE text.
    UnrepresentableText,
    /// Cached metadata больше не согласована внутри одного CUE document.
    InconsistentMetadata,
}

impl CueExportScopeIneligibility {
    /// Короткая privacy-safe причина для disabled UI.
    pub const fn safe_reason_ru(self) -> &'static str {
        match self {
            Self::EmptyScope => "В выбранной области нет треков",
            Self::CompoundEntry => "CUE поддерживает только отдельные CUE-треки",
            Self::MissingCueSemantics => "Треки не имеют точной CUE-разметки",
            Self::MixedSourceDocuments => "Треки относятся к разным CUE-файлам",
            Self::SourceDocumentIneligible => {
                "Исходный CUE содержит команды или индексы без точного экспорта"
            }
            Self::NonSequentialTracks => "Порядок треков больше не образует CUE-последовательность",
            Self::UnrepresentablePlaybackBoundary => {
                "Границы выбранных треков нельзя точно выразить в CUE"
            }
            Self::NonLocalOrNonUtf8File => "CUE export требует локальные UTF-8 пути",
            Self::UnrepresentableText => "Путь или metadata нельзя безопасно записать в CUE",
            Self::InconsistentMetadata => "Metadata выбранных CUE-треков несовместима",
        }
    }
}

/// Snapshot-only availability для UI до открытия save dialog.
pub fn cue_export_scope_availability(
    snapshot: &PlaylistExportSnapshot,
) -> PlaylistExportAvailability {
    match analyze_scope(snapshot) {
        Ok(_) => PlaylistExportAvailability::Available,
        Err((_, reason)) => PlaylistExportAvailability::Disabled(reason),
    }
}

/// Строит полностью проверенный CUE plan без filesystem access.
pub(super) fn preflight(
    snapshot: &PlaylistExportSnapshot,
    target: &PlaylistExportDocumentTarget,
    locator_policy: &impl PlaylistExportLocatorPolicy,
) -> Result<PreparedPlaylistExport, PlaylistExportPreflightError> {
    let analyzed_tracks =
        analyze_scope(snapshot).map_err(|(subject, reason)| PlaylistExportPreflightError {
            subject,
            reason: PlaylistExportIneligible::Cue(reason),
        })?;
    let mut prepared_tracks = Vec::with_capacity(analyzed_tracks.len());
    for analyzed in analyzed_tracks {
        let locator = preflight_item_locator(
            analyzed.item,
            PlaylistExportFormat::Cue,
            target,
            locator_policy,
        )
        .map_err(|reason| PlaylistExportPreflightError {
            subject: PlaylistExportSubject::Item(analyzed.item.item_id()),
            reason,
        })?;
        if !cue_file_path_representable(locator.as_str()) {
            return Err(PlaylistExportPreflightError {
                subject: PlaylistExportSubject::Item(analyzed.item.item_id()),
                reason: PlaylistExportIneligible::Cue(
                    CueExportScopeIneligibility::UnrepresentableText,
                ),
            });
        }
        prepared_tracks.push(PreparedCueTrack {
            locator,
            metadata: analyzed.item.cached_metadata().clone(),
            semantics: analyzed.semantics,
        });
    }
    Ok(PreparedPlaylistExport {
        format: PlaylistExportFormat::Cue,
        tracks: Box::new([]),
        cue_tracks: prepared_tracks.into_boxed_slice(),
        groups: Box::new([]),
        warnings: Box::new([]),
        secret_classification: PlaylistExportSecretClassification::NoSensitiveLocators,
    })
}

/// Сериализует preflighted CUE plan; fallible checks сюда не протекают.
pub(super) fn serialize(export: &PreparedPlaylistExport) -> String {
    let mut document = String::new();
    if let Some(album) = common_album(&export.cue_tracks) {
        push_metadata_command(&mut document, "", "TITLE", album);
    }
    let mut previous_locator: Option<&str> = None;
    for track in &export.cue_tracks {
        if previous_locator != Some(track.locator.as_str()) {
            document.push_str("FILE \"");
            document.push_str(track.locator.as_str());
            document.push_str("\" ");
            document.push_str(file_type_token(track.semantics.file_type()));
            document.push('\n');
            previous_locator = Some(track.locator.as_str());
        }
        document.push_str("  TRACK ");
        document.push_str(&format!("{:02}", track.semantics.track_number()));
        document.push_str(" AUDIO\n");
        if let Some(title) = track.metadata.title() {
            push_metadata_command(&mut document, "    ", "TITLE", title);
        }
        if let Some(performer) = track.metadata.artists().first() {
            push_metadata_command(&mut document, "    ", "PERFORMER", performer);
        }
        if let Some(index00) = track.semantics.index00() {
            push_index(&mut document, 0, index00);
        }
        push_index(&mut document, 1, track.semantics.index01());
    }
    document
}

struct AnalyzedCueTrack<'item> {
    item: &'item PlaylistItem,
    span: PlaylistPlaybackSpan,
    semantics: PlaylistCueTrackExportSemantics,
    root: &'item DurableReopenLocator,
    source: &'item DurableReopenLocator,
}

fn analyze_scope(
    snapshot: &PlaylistExportSnapshot,
) -> Result<Vec<AnalyzedCueTrack<'_>>, (PlaylistExportSubject, CueExportScopeIneligibility)> {
    let mut tracks = Vec::with_capacity(snapshot.retained_item_count());
    for entry in snapshot.entries() {
        let PlaylistEntry::Single(item) = entry else {
            let subject = match entry {
                PlaylistEntry::Compound(group) => PlaylistExportSubject::Compound(group.group_id()),
                PlaylistEntry::Single(_) => unreachable!("let-else proves compound"),
            };
            return Err((subject, CueExportScopeIneligibility::CompoundEntry));
        };
        tracks.push(analyze_item(item)?);
    }
    let Some(first) = tracks.first() else {
        return Err((
            PlaylistExportSubject::Scope,
            CueExportScopeIneligibility::EmptyScope,
        ));
    };
    for pair in tracks.windows(2) {
        validate_pair(&pair[0], &pair[1])?;
    }
    let Some(last) = tracks.last() else {
        return Err((
            PlaylistExportSubject::Scope,
            CueExportScopeIneligibility::EmptyScope,
        ));
    };
    if last.span.end_exclusive().is_some() {
        return Err((
            PlaylistExportSubject::Item(last.item.item_id()),
            CueExportScopeIneligibility::UnrepresentablePlaybackBoundary,
        ));
    }
    if tracks.iter().any(|track| track.root != first.root) {
        return Err((
            PlaylistExportSubject::Item(first.item.item_id()),
            CueExportScopeIneligibility::MixedSourceDocuments,
        ));
    }
    validate_metadata(&tracks)?;
    Ok(tracks)
}

fn analyze_item(
    item: &PlaylistItem,
) -> Result<AnalyzedCueTrack<'_>, (PlaylistExportSubject, CueExportScopeIneligibility)> {
    let subject = PlaylistExportSubject::Item(item.item_id());
    let payload = item
        .durable_payload()
        .ok_or((subject, CueExportScopeIneligibility::MissingCueSemantics))?;
    if payload.provenance().source_kind() != PlaylistImportSourceKind::Cue {
        return Err((subject, CueExportScopeIneligibility::MissingCueSemantics));
    }
    let semantics = payload
        .cue_export_semantics()
        .ok_or((subject, CueExportScopeIneligibility::MissingCueSemantics))?;
    if semantics.document_eligibility() != PlaylistCueDocumentExportEligibility::Exact {
        return Err((
            subject,
            CueExportScopeIneligibility::SourceDocumentIneligible,
        ));
    }
    let span = payload
        .playback_span()
        .ok_or((subject, CueExportScopeIneligibility::MissingCueSemantics))?;
    if span.start() != semantics.index01().media_time() {
        return Err((
            subject,
            CueExportScopeIneligibility::UnrepresentablePlaybackBoundary,
        ));
    }
    let local_path = payload
        .reopen_locator()
        .expose_local_for_reopen()
        .and_then(|locator| locator.expose_native_path_for_persistence())
        .and_then(|path| path.to_str())
        .ok_or((subject, CueExportScopeIneligibility::NonLocalOrNonUtf8File))?;
    if !cue_file_path_representable(local_path) {
        return Err((subject, CueExportScopeIneligibility::UnrepresentableText));
    }
    Ok(AnalyzedCueTrack {
        item,
        span,
        semantics,
        root: payload.provenance().root_locator(),
        source: payload.reopen_locator(),
    })
}

fn validate_pair(
    current: &AnalyzedCueTrack<'_>,
    next: &AnalyzedCueTrack<'_>,
) -> Result<(), (PlaylistExportSubject, CueExportScopeIneligibility)> {
    let subject = PlaylistExportSubject::Item(next.item.item_id());
    let expected_track_number = current
        .semantics
        .track_number()
        .checked_add(1)
        .ok_or((subject, CueExportScopeIneligibility::NonSequentialTracks))?;
    if next.semantics.track_number() != expected_track_number {
        return Err((subject, CueExportScopeIneligibility::NonSequentialTracks));
    }
    if current.source == next.source {
        if current.semantics.file_type() != next.semantics.file_type()
            || current.span.end_exclusive() != Some(next.semantics.index01().media_time())
        {
            return Err((
                subject,
                CueExportScopeIneligibility::UnrepresentablePlaybackBoundary,
            ));
        }
    } else if current.span.end_exclusive().is_some() {
        return Err((
            subject,
            CueExportScopeIneligibility::UnrepresentablePlaybackBoundary,
        ));
    }
    Ok(())
}

fn validate_metadata(
    tracks: &[AnalyzedCueTrack<'_>],
) -> Result<(), (PlaylistExportSubject, CueExportScopeIneligibility)> {
    let expected_album = tracks[0].item.cached_metadata().album();
    for track in tracks {
        let metadata = track.item.cached_metadata();
        if metadata.album() != expected_album || metadata.artists().len() > 1 {
            return Err((
                PlaylistExportSubject::Item(track.item.item_id()),
                CueExportScopeIneligibility::InconsistentMetadata,
            ));
        }
        for text in metadata
            .title()
            .into_iter()
            .chain(metadata.album())
            .chain(metadata.artists().first().map(String::as_str))
        {
            if !cue_metadata_representable(text) {
                return Err((
                    PlaylistExportSubject::Item(track.item.item_id()),
                    CueExportScopeIneligibility::UnrepresentableText,
                ));
            }
        }
    }
    Ok(())
}

fn common_album(tracks: &[PreparedCueTrack]) -> Option<&str> {
    tracks.first().and_then(|track| track.metadata.album())
}

fn cue_file_path_representable(path: &str) -> bool {
    !path.is_empty() && !path.contains(['"', '\r', '\n'])
}

fn cue_metadata_representable(text: &str) -> bool {
    !text.is_empty() && text.trim() == text && !text.contains(['"', '\r', '\n'])
}

fn push_metadata_command(document: &mut String, indent: &str, command: &str, value: &str) {
    document.push_str(indent);
    document.push_str(command);
    document.push(' ');
    document.push('"');
    document.push_str(value);
    document.push('"');
    document.push('\n');
}

fn push_index(document: &mut String, number: u8, frame_index: PlaylistCueFrameIndex) {
    let total_frames = frame_index.total_frames();
    let total_seconds = total_frames / 75;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    let frames = total_frames % 75;
    document.push_str("    INDEX ");
    document.push_str(&format!(
        "{number:02} {minutes:02}:{seconds:02}:{frames:02}\n"
    ));
}

const fn file_type_token(file_type: PlaylistCueFileType) -> &'static str {
    match file_type {
        PlaylistCueFileType::Wave => "WAVE",
        PlaylistCueFileType::Aiff => "AIFF",
        PlaylistCueFileType::Mp3 => "MP3",
        PlaylistCueFileType::Flac => "FLAC",
    }
}

#[cfg(test)]
mod tests {
    use super::cue_metadata_representable;

    #[test]
    fn metadata_representability_is_fail_closed_for_cue_delimiters() {
        assert!(cue_metadata_representable("Обычное название"));
        assert!(!cue_metadata_representable("Название с \"кавычкой\""));
        assert!(!cue_metadata_representable(" строка с внешним пробелом"));
        assert!(!cue_metadata_representable("первая\nвторая"));
    }
}
