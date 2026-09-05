//! Focused S10 tests для immutable M3U8/XSPF export core.

use std::path::Path;

use media_core::{MediaDuration, TrackNumber};
use playlist_core::{
    AddPlaylistEntriesOutcome, CachedPlaylistMetadata, DurableReopenLocator, LocalLocator,
    PlaylistCompoundGroupDraft, PlaylistEntryDraft, PlaylistEntryId, PlaylistImportAvailability,
    PlaylistImportProvenance, PlaylistImportSourceKind, PlaylistItemDraft, PlaylistLocator,
    PlaylistMediaKind, PlaylistQueue, PlaylistSingleDurablePayload, SecretUrlLocator,
    ServiceDurableReopenPayload, ServiceReopenMaterialKind,
};
use playlist_io::{
    M3uDeclaredFormat, M3uDocument, M3uParseRequest, M3uParserLimits, PlaylistExportDocumentTarget,
    PlaylistExportFormat, PlaylistExportIneligible, PlaylistExportLocatorPolicy,
    PlaylistExportLocatorRejection, PlaylistExportScope, PlaylistExportSecretClassification,
    PlaylistExportSnapshot, PlaylistExportWarning, PortablePlaylistExportUrl,
    PortableUrlSecretClassification, XspfDocumentSource, XspfParseRequest, XspfParserLimits,
    parse_m3u_document, parse_xspf_document, preflight_playlist_export,
};

/// Fixed local document target даёт deterministic reversible relative paths.
const M3U8_TARGET: &str = "/tmp/fastiplayer-export/list.m3u8";
/// XSPF использует отдельное extension, но тот же base directory.
const XSPF_TARGET: &str = "/tmp/fastiplayer-export/list.xspf";

/// Test-only service owner сохраняет exact direct URL и даёт stable portable service URL.
struct TestLocatorPolicy {
    reject_service_identity: bool,
}

impl PlaylistExportLocatorPolicy for TestLocatorPolicy {
    fn preflight_url(
        &self,
        locator: &SecretUrlLocator,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection> {
        let exact_url = locator.expose_secret_for_persistence();
        let secret_classification =
            if exact_url.contains('@') || exact_url.contains('?') || exact_url.contains('#') {
                PortableUrlSecretClassification::SensitiveDurableIdentity
            } else {
                PortableUrlSecretClassification::Public
            };
        PortablePlaylistExportUrl::new(exact_url, secret_classification)
            .map_err(|_| PlaylistExportLocatorRejection::OwnerPolicyRejected)
    }

    fn preflight_service(
        &self,
        _locator: &ServiceDurableReopenPayload,
    ) -> Result<PortablePlaylistExportUrl, PlaylistExportLocatorRejection> {
        if self.reject_service_identity {
            return Err(PlaylistExportLocatorRejection::NonPortableIdentity);
        }
        PortablePlaylistExportUrl::new(
            "https://example.test/watch?v=stable-child",
            PortableUrlSecretClassification::SensitiveDurableIdentity,
        )
        .map_err(|_| PlaylistExportLocatorRejection::OwnerPolicyRejected)
    }
}

/// Stable IDs, нужные selected-scope assertions.
struct QueueFixture {
    queue: PlaylistQueue,
    first_single: PlaylistEntryId,
    compound: PlaylistEntryId,
    duplicate_single: PlaylistEntryId,
}

/// Строит Single, two-part Compound и duplicate Single в canonical order.
fn queue_fixture() -> QueueFixture {
    let duplicate_url = "https://example.test/media/duplicate.mp4";
    let first_single = PlaylistItemDraft::url(
        secret_url(duplicate_url),
        metadata("Первый", Some(1_250), Some(7)),
    );
    let compound = PlaylistCompoundGroupDraft::new(
        PlaylistLocator::Url(secret_url("https://example.test/collection")),
        metadata("Группа", None, None),
        vec![
            PlaylistItemDraft::local(
                LocalLocator::Native(
                    Path::new("/tmp/fastiplayer-export/media/part one.webm").to_path_buf(),
                ),
                None,
                metadata("Часть & один", Some(2_000), Some(1)),
            ),
            PlaylistItemDraft::url(
                secret_url("https://example.test/media/part-two.webm"),
                metadata("Часть <два>", Some(3_500), Some(2)),
            ),
        ],
    )
    .expect("compound fixture");
    let duplicate_single = PlaylistItemDraft::url(
        secret_url(duplicate_url),
        metadata("Дубликат", Some(4_000), Some(8)),
    );

    let mut queue = PlaylistQueue::new();
    let outcome = queue
        .append_entries(vec![
            PlaylistEntryDraft::Single(first_single),
            PlaylistEntryDraft::Compound(compound),
            PlaylistEntryDraft::Single(duplicate_single),
        ])
        .expect("fixture append");
    let AddPlaylistEntriesOutcome::Added(allocated) = outcome else {
        panic!("non-empty fixture обязана allocate entries");
    };
    let entry_ids: Vec<_> = allocated.iter_entry_ids().collect();
    QueueFixture {
        queue,
        first_single: entry_ids[0],
        compound: entry_ids[1],
        duplicate_single: entry_ids[2],
    }
}

/// Создаёт redacted exact URL locator.
fn secret_url(exact_url: &str) -> SecretUrlLocator {
    SecretUrlLocator::from_reopenable_url(exact_url.to_owned()).expect("fixture URL")
}

/// Создаёт metadata с optional duration/track number.
fn metadata(
    title: &str,
    duration_milliseconds: Option<u64>,
    track_number: Option<u64>,
) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(title, PlaylistMediaKind::Video)
        .with_title(Some(title.to_owned()))
        .with_duration(duration_milliseconds.map(MediaDuration::from_millis))
        .with_sequence(None, track_number.map(TrackNumber::new), None, None)
}

/// Общий permissive policy для обычных focused cases.
fn policy() -> TestLocatorPolicy {
    TestLocatorPolicy {
        reject_service_identity: false,
    }
}

#[test]
fn full_m3u8_preserves_canonical_order_duplicates_and_warns_about_group_flattening() {
    let fixture = queue_fixture();
    let snapshot = PlaylistExportSnapshot::capture(&fixture.queue, PlaylistExportScope::Full)
        .expect("full snapshot");
    let target = PlaylistExportDocumentTarget::local_file(M3U8_TARGET).expect("absolute target");
    let prepared =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::M3u8, &target, &policy())
            .expect("M3U8 preflight");

    assert_eq!(prepared.track_count(), 4);
    assert_eq!(
        prepared.warnings(),
        &[PlaylistExportWarning::CompoundGroupingFlattened {
            compound_group_count: 1,
        }]
    );
    let document = prepared.serialize();
    assert!(document.as_str().starts_with("#EXTM3U\n"));
    let duplicate = "https://example.test/media/duplicate.mp4";
    assert_eq!(document.as_str().matches(duplicate).count(), 2);
    let positions = [
        document.as_str().find("Первый").expect("first"),
        document.as_str().find("Часть & один").expect("part one"),
        document.as_str().find("Часть <два>").expect("part two"),
        document.as_str().find("Дубликат").expect("duplicate"),
    ];
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(document.as_str().contains("media/part one.webm"));
}

#[test]
fn selected_scope_uses_canonical_order_and_includes_every_compound_part() {
    let fixture = queue_fixture();
    let selected = [fixture.duplicate_single, fixture.compound];
    let snapshot =
        PlaylistExportSnapshot::capture(&fixture.queue, PlaylistExportScope::Selected(&selected))
            .expect("selected snapshot");

    assert_eq!(snapshot.top_level_entry_count(), 2);
    assert_eq!(snapshot.retained_item_count(), 3);
    let target = PlaylistExportDocumentTarget::local_file(XSPF_TARGET).expect("absolute target");
    let document =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::Xspf, &target, &policy())
            .expect("XSPF preflight")
            .serialize();

    let part_one = document
        .as_str()
        .find("Часть &amp; один")
        .expect("part one");
    let part_two = document
        .as_str()
        .find("Часть &lt;два&gt;")
        .expect("part two");
    let duplicate = document.as_str().find("Дубликат").expect("duplicate");
    assert!(part_one < part_two && part_two < duplicate);
    assert!(!document.as_str().contains(">Первый<"));
}

#[test]
fn xspf_roundtrip_preserves_flattened_identity_and_compound_extension() {
    let fixture = queue_fixture();
    let snapshot = PlaylistExportSnapshot::capture(&fixture.queue, PlaylistExportScope::Full)
        .expect("full snapshot");
    let target = PlaylistExportDocumentTarget::local_file(XSPF_TARGET).expect("absolute target");
    let serialized =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::Xspf, &target, &policy())
            .expect("XSPF preflight")
            .serialize();
    let parsed = parse_xspf_document(XspfParseRequest::new(
        serialized.as_bytes(),
        XspfDocumentSource::local(XSPF_TARGET),
        XspfParserLimits::default(),
    ))
    .expect("exported XSPF должен пройти hardened parser");

    assert_eq!(parsed.tracks().len(), 4);
    assert_eq!(
        parsed.tracks()[0].location_candidates()[0].expose_uri_for_admission(),
        "https://example.test/media/duplicate.mp4"
    );
    assert_eq!(
        parsed.tracks()[1].location_candidates()[0].expose_uri_for_admission(),
        "file:///tmp/fastiplayer-export/media/part%20one.webm"
    );
    assert_eq!(parsed.groups().len(), 1);
    assert_eq!(parsed.groups()[0].first_track().get(), 2);
    assert_eq!(parsed.groups()[0].track_count().get(), 2);
    assert_eq!(
        parsed.groups()[0]
            .root_location()
            .expose_uri_for_admission(),
        "https://example.test/collection"
    );
}

#[test]
fn m3u8_roundtrip_restores_relative_local_path_and_url_duplicates() {
    let fixture = queue_fixture();
    let snapshot = PlaylistExportSnapshot::capture(&fixture.queue, PlaylistExportScope::Full)
        .expect("full snapshot");
    let target = PlaylistExportDocumentTarget::local_file(M3U8_TARGET).expect("absolute target");
    let serialized =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::M3u8, &target, &policy())
            .expect("M3U8 preflight")
            .serialize();
    let parsed = parse_m3u_document(M3uParseRequest::new(
        serialized.as_bytes(),
        playlist_io::M3uDocumentSource::local(M3U8_TARGET),
        M3uDeclaredFormat::M3u8,
        M3uParserLimits::default(),
    ))
    .expect("exported M3U8 должен пройти parser");
    let M3uDocument::Generic(preview) = parsed else {
        panic!("exported M3U8 не должен классифицироваться как HLS");
    };
    let drafts: Vec<_> = preview.entries().collect();

    assert_eq!(drafts.len(), 4);
    assert_eq!(
        drafts[1]
            .import_draft()
            .reopen_locator()
            .expose_local_for_reopen()
            .and_then(LocalLocator::expose_native_path_for_persistence),
        Some(Path::new("/tmp/fastiplayer-export/media/part one.webm"))
    );
    let duplicate_count = drafts
        .iter()
        .filter(|entry| {
            entry
                .import_draft()
                .reopen_locator()
                .expose_url_for_reopen()
                .is_some_and(|url| {
                    url.expose_secret_for_persistence()
                        == "https://example.test/media/duplicate.mp4"
                })
        })
        .count();
    assert_eq!(duplicate_count, 2);
}

#[test]
fn m3u8_uses_file_uri_when_relative_filename_would_be_misread_as_uri_scheme() {
    let native_path = Path::new("/tmp/fastiplayer-export/media:clip.webm");
    let draft = PlaylistItemDraft::local(
        LocalLocator::Native(native_path.to_path_buf()),
        None,
        metadata("Colon filename", None, None),
    );
    let mut queue = PlaylistQueue::new();
    queue.append_one(draft).expect("append colon filename");
    let snapshot =
        PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full).expect("snapshot");
    let target = PlaylistExportDocumentTarget::local_file(M3U8_TARGET).expect("absolute target");
    let serialized =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::M3u8, &target, &policy())
            .expect("preflight")
            .serialize();

    assert!(
        serialized
            .as_str()
            .contains("file:///tmp/fastiplayer-export/media:clip.webm")
    );
    let parsed = parse_m3u_document(M3uParseRequest::new(
        serialized.as_bytes(),
        playlist_io::M3uDocumentSource::local(M3U8_TARGET),
        M3uDeclaredFormat::M3u8,
        M3uParserLimits::default(),
    ))
    .expect("file URI fallback должен roundtrip");
    let M3uDocument::Generic(preview) = parsed else {
        panic!("generic export не является HLS");
    };
    assert_eq!(
        preview
            .entries()
            .next()
            .expect("one entry")
            .import_draft()
            .reopen_locator()
            .expose_local_for_reopen()
            .and_then(LocalLocator::expose_native_path_for_persistence),
        Some(native_path)
    );
}

#[test]
fn service_payload_requires_owner_portable_url_and_contributes_secret_classification() {
    let service_locator = DurableReopenLocator::from_service_payload(
        "yt-dlp",
        1,
        ServiceReopenMaterialKind::StableExtractorIdentity,
        b"opaque-stable-child".to_vec(),
    )
    .expect("stable service payload");
    let provenance = PlaylistImportProvenance::new(
        DurableReopenLocator::url(secret_url("https://example.test/watch")),
        PlaylistImportSourceKind::Xspf,
        None,
    );
    let durable_payload = PlaylistSingleDurablePayload::new(
        service_locator,
        None,
        Vec::new(),
        provenance,
        PlaylistImportAvailability::Available,
    )
    .expect("durable payload");
    let draft = PlaylistItemDraft::url(
        secret_url("https://transient.invalid/signed?token=must-not-export"),
        metadata("Service child", None, None),
    )
    .with_durable_payload(durable_payload);
    let mut queue = PlaylistQueue::new();
    queue.append_one(draft).expect("append service fixture");
    let snapshot = PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full)
        .expect("service snapshot");
    let target = PlaylistExportDocumentTarget::local_file(M3U8_TARGET).expect("absolute target");

    let rejection = preflight_playlist_export(
        &snapshot,
        PlaylistExportFormat::M3u8,
        &target,
        &TestLocatorPolicy {
            reject_service_identity: true,
        },
    )
    .expect_err("nonportable service identity");
    assert_eq!(
        rejection.reason(),
        PlaylistExportIneligible::LocatorPolicy(
            PlaylistExportLocatorRejection::NonPortableIdentity
        )
    );
    let safe_error = format!("{rejection:?} {rejection}");
    assert!(!safe_error.contains("opaque-stable-child"));
    assert!(!safe_error.contains("must-not-export"));

    let prepared =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::M3u8, &target, &policy())
            .expect("portable service URL");
    assert_eq!(
        prepared.secret_classification(),
        PlaylistExportSecretClassification::SensitiveDurableLocators { locator_count: 1 }
    );
    let serialized = prepared.serialize();
    assert!(serialized.as_str().contains("stable-child"));
    assert!(!serialized.as_str().contains("transient.invalid"));
    assert!(!serialized.as_str().contains("must-not-export"));
}

#[cfg(unix)]
#[test]
fn non_utf_local_path_is_typed_ineligible_and_error_is_secret_safe() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let secret_filename = OsString::from_vec(b"private-\xFF-token.webm".to_vec());
    let native_path = Path::new("/tmp/fastiplayer-export").join(secret_filename);
    let draft = PlaylistItemDraft::local(
        LocalLocator::Native(native_path),
        None,
        metadata("Non UTF", None, None),
    );
    let mut queue = PlaylistQueue::new();
    queue.append_one(draft).expect("append non-UTF fixture");
    let snapshot =
        PlaylistExportSnapshot::capture(&queue, PlaylistExportScope::Full).expect("snapshot");
    let target = PlaylistExportDocumentTarget::local_file(M3U8_TARGET).expect("absolute target");
    let error =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::M3u8, &target, &policy())
            .expect_err("non-UTF must be ineligible");

    assert_eq!(error.reason(), PlaylistExportIneligible::NonUtf8LocalPath);
    assert!(!format!("{error:?} {error}").contains("private"));
}

#[test]
fn snapshot_and_serialization_do_not_mutate_queue_and_snapshot_stays_immutable() {
    let mut fixture = queue_fixture();
    let revisions_before_capture = fixture.queue.revision_snapshot();
    let snapshot = PlaylistExportSnapshot::capture(&fixture.queue, PlaylistExportScope::Full)
        .expect("full snapshot");
    assert_eq!(fixture.queue.revision_snapshot(), revisions_before_capture);

    fixture
        .queue
        .append_one(PlaylistItemDraft::url(
            secret_url("https://example.test/media/later.mp4"),
            metadata("Позже", None, None),
        ))
        .expect("later mutation");
    let revisions_after_mutation = fixture.queue.revision_snapshot();
    let target = PlaylistExportDocumentTarget::local_file(M3U8_TARGET).expect("absolute target");
    let prepared =
        preflight_playlist_export(&snapshot, PlaylistExportFormat::M3u8, &target, &policy())
            .expect("snapshot preflight");
    let serialized = prepared.serialize();

    assert_eq!(prepared.track_count(), 4);
    assert!(!serialized.as_str().contains("Позже"));
    assert_eq!(fixture.queue.revision_snapshot(), revisions_after_mutation);
}

#[test]
fn selected_scope_rejects_subordinate_part_without_mutating_queue() {
    let fixture = queue_fixture();
    let compound = fixture
        .queue
        .top_level_entry(fixture.compound)
        .and_then(playlist_core::PlaylistEntry::as_compound)
        .expect("compound");
    let part_id = compound
        .parts()
        .next()
        .expect("first part")
        .item()
        .item_id();
    let revisions = fixture.queue.revision_snapshot();
    let error = PlaylistExportSnapshot::capture(
        &fixture.queue,
        PlaylistExportScope::Selected(&[PlaylistEntryId::Single(part_id)]),
    )
    .expect_err("part is not top-level");

    assert!(matches!(
        error,
        playlist_io::PlaylistExportSnapshotError::CompoundPartIsNotTopLevel(_)
    ));
    assert_eq!(fixture.queue.revision_snapshot(), revisions);
    assert_ne!(fixture.first_single, fixture.duplicate_single);
}
