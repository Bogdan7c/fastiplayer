use playlist_core::{
    PlaylistEntryId, PlaylistImportEntryDraft, PlaylistQueue, ServiceDurableReopenPayload,
};
use playlist_io::{
    CueDocumentSource, CueExportScopeIneligibility, CueParseRequest, CueParserLimits,
    PlaylistExportAvailability, PlaylistExportDocumentTarget, PlaylistExportFormat,
    PlaylistExportLocatorPolicy, PlaylistExportLocatorRejection, PlaylistExportScope,
    PlaylistExportSnapshot, PortablePlaylistExportUrl, cue_export_scope_availability,
    parse_cue_document, preflight_playlist_export,
};

struct LocalOnlyPolicy;

impl PlaylistExportLocatorPolicy for LocalOnlyPolicy {
    fn preflight_url(
        &self,
        _locator: &playlist_core::SecretUrlLocator,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection> {
        Err(PlaylistExportLocatorRejection::OwnerPolicyRejected)
    }

    fn preflight_service(
        &self,
        _payload: &ServiceDurableReopenPayload,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection> {
        Err(PlaylistExportLocatorRejection::OwnerPolicyRejected)
    }
}

fn cue_queue(document_text: &str) -> PlaylistQueue {
    let document = parse_cue_document(CueParseRequest::new(
        document_text.as_bytes(),
        CueDocumentSource::local("/music/disc/source.cue"),
        CueParserLimits::default(),
    ))
    .expect("valid focused CUE document");
    let entries = document
        .tracks()
        .iter()
        .map(|track| PlaylistImportEntryDraft::Single(track.import_draft().clone()))
        .map(|draft| draft.into_queue_draft().expect("materialize CUE single"))
        .collect();
    let mut queue = PlaylistQueue::new();
    queue.append_entries(entries).expect("commit CUE singles");
    queue
}

fn two_track_cue(extra_command: &str) -> String {
    format!(
        "TITLE \"Album\"\nPERFORMER \"Artist\"\n{extra_command}FILE \"album.flac\" FLAC\n\
         TRACK 01 AUDIO\nTITLE \"First\"\nINDEX 01 00:00:00\n\
         TRACK 02 AUDIO\nTITLE \"Second\"\nINDEX 00 00:59:70\nINDEX 01 01:00:00\n"
    )
}

#[test]
fn full_and_selected_cue_scope_require_exact_representable_boundaries() {
    let queue = cue_queue(&two_track_cue(""));
    let full = PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full)
        .expect("capture full CUE scope");
    assert_eq!(
        cue_export_scope_availability(&full),
        PlaylistExportAvailability::Available
    );

    let item_ids = queue.iter_playable_ids().collect::<Vec<_>>();
    let first_only_ids = [PlaylistEntryId::Single(item_ids[0])];
    let first_only =
        PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Selected(&first_only_ids))
            .expect("capture first track");
    assert_eq!(
        cue_export_scope_availability(&first_only),
        PlaylistExportAvailability::Disabled(
            CueExportScopeIneligibility::UnrepresentablePlaybackBoundary
        )
    );

    let second_only_ids = [PlaylistEntryId::Single(item_ids[1])];
    let second_only =
        PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Selected(&second_only_ids))
            .expect("capture EOF-bound suffix");
    assert_eq!(
        cue_export_scope_availability(&second_only),
        PlaylistExportAvailability::Available
    );
}

#[test]
fn cue_serializer_roundtrips_exact_frames_metadata_and_file_type() {
    let queue = cue_queue(&two_track_cue(""));
    let snapshot = PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full)
        .expect("capture CUE scope");
    let target = PlaylistExportDocumentTarget::local_file("/music/disc/export.cue")
        .expect("absolute export target");
    let serialized = preflight_playlist_export(
        &snapshot,
        PlaylistExportFormat::Cue,
        &target,
        &LocalOnlyPolicy,
    )
    .expect("exact CUE preflight")
    .serialize();
    let reparsed = parse_cue_document(CueParseRequest::new(
        serialized.as_bytes(),
        CueDocumentSource::local("/music/disc/export.cue"),
        CueParserLimits::default(),
    ))
    .expect("serialized CUE must satisfy authoritative parser");

    assert_eq!(reparsed.title(), Some("Album"));
    assert_eq!(reparsed.tracks().len(), 2);
    assert_eq!(reparsed.tracks()[0].number(), 1);
    assert_eq!(reparsed.tracks()[1].number(), 2);
    assert_eq!(
        reparsed.tracks()[1].indexes()[0].timestamp().total_frames(),
        4_495
    );
    assert_eq!(
        reparsed.tracks()[1].indexes()[1].timestamp().total_frames(),
        4_500
    );
    assert_eq!(
        reparsed.tracks()[1].import_draft().cue_export_semantics(),
        queue
            .item(queue.iter_playable_ids().nth(1).expect("second item"))
            .and_then(playlist_core::PlaylistItem::durable_payload)
            .and_then(playlist_core::PlaylistSingleDurablePayload::cue_export_semantics)
    );
}

#[test]
fn retained_unknown_command_rejects_cue_export_without_blocking_import() {
    let queue = cue_queue(&two_track_cue("REM retained but unsupported\n"));
    assert_eq!(queue.top_level_entry_count(), 2);
    let snapshot = PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full)
        .expect("capture imported CUE items");
    assert_eq!(
        cue_export_scope_availability(&snapshot),
        PlaylistExportAvailability::Disabled(CueExportScopeIneligibility::SourceDocumentIneligible)
    );
    let target = PlaylistExportDocumentTarget::local_file("/music/disc/rejected.cue")
        .expect("absolute export target");
    let error = preflight_playlist_export(
        &snapshot,
        PlaylistExportFormat::Cue,
        &target,
        &LocalOnlyPolicy,
    )
    .expect_err("unknown command must fail closed");
    assert_eq!(
        error.reason(),
        playlist_io::PlaylistExportIneligible::Cue(
            CueExportScopeIneligibility::SourceDocumentIneligible
        )
    );
}
