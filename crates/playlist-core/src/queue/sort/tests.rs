use std::cmp::Ordering;
use std::path::PathBuf;

use media_core::{DiscNumber, MediaDuration, TrackNumber, TvEpisodeNumber, TvSeasonNumber};
use rand::SeedableRng;
use rand::rngs::StdRng;

use super::{
    PlaylistSortKey, SortCanonicalQueue, SortCanonicalQueueOutcome, SortDirection, compare_entries,
    prepare_sort_entries,
};
use crate::{
    CachedPlaylistMetadata, ForeignPathEncoding, ForeignPathPlatform, ForeignPlatformPath,
    LocalLocator, PlaylistItemDraft, PlaylistItemId, PlaylistMediaKind, PlaylistQueue,
    ReservedQueueMutation, SecretUrlLocator,
};

fn audio_metadata(
    fallback_name: &str,
    title: Option<&str>,
    artist: Option<&str>,
    album: Option<&str>,
    duration_seconds: Option<u64>,
    disc_number: Option<u64>,
    track_number: Option<u64>,
) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(fallback_name, PlaylistMediaKind::Audio)
        .with_title(title.map(str::to_owned))
        .with_artists(artist.into_iter().map(str::to_owned).collect())
        .expect("test metadata stays below artists limit")
        .with_album(album.map(str::to_owned))
        .with_duration(duration_seconds.map(MediaDuration::from_secs))
        .with_sequence(
            disc_number.map(DiscNumber::new),
            track_number.map(TrackNumber::new),
            None,
            None,
        )
}

fn video_metadata(
    fallback_name: &str,
    title: Option<&str>,
    season_number: Option<u64>,
    episode_number: Option<u64>,
) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(fallback_name, PlaylistMediaKind::Video)
        .with_title(title.map(str::to_owned))
        .with_sequence(
            None,
            None,
            season_number.map(TvSeasonNumber::new),
            episode_number.map(TvEpisodeNumber::new),
        )
}

fn unknown_metadata(fallback_name: &str) -> CachedPlaylistMetadata {
    CachedPlaylistMetadata::new(fallback_name, PlaylistMediaKind::Unknown)
}

fn local_draft(filename: &str, metadata: CachedPlaylistMetadata) -> PlaylistItemDraft {
    PlaylistItemDraft::local(
        LocalLocator::Native(PathBuf::from("/music").join(filename)),
        None,
        metadata,
    )
}

fn queue_from_drafts(drafts: Vec<PlaylistItemDraft>) -> PlaylistQueue {
    let mut queue = PlaylistQueue::new();
    queue.append_batch(drafts).expect("append test queue");
    queue
}

fn canonical_ids(queue: &PlaylistQueue) -> Vec<PlaylistItemId> {
    queue.items().iter().map(|item| item.item_id()).collect()
}

fn fallback_names(queue: &PlaylistQueue) -> Vec<&str> {
    queue
        .items()
        .iter()
        .map(|item| item.cached_metadata().fallback_display_name())
        .collect()
}

fn sort(queue: &mut PlaylistQueue, key: PlaylistSortKey, direction: SortDirection) {
    assert!(matches!(
        queue.sort_canonical(SortCanonicalQueue::new(key, direction)),
        SortCanonicalQueueOutcome::Reordered { .. }
            | SortCanonicalQueueOutcome::AlreadyInCanonicalOrder
    ));
}

#[test]
fn every_public_key_supports_both_directions() {
    let drafts = vec![
        local_draft(
            "track 10.flac",
            audio_metadata(
                "track 10.flac",
                Some("Charlie"),
                Some("Artist C"),
                Some("Album C"),
                Some(30),
                Some(1),
                Some(10),
            ),
        ),
        local_draft(
            "track 2.flac",
            audio_metadata(
                "track 2.flac",
                Some("Bravo"),
                Some("Artist B"),
                Some("Album B"),
                Some(20),
                Some(1),
                Some(2),
            ),
        ),
        local_draft(
            "track 1.flac",
            audio_metadata(
                "track 1.flac",
                Some("Alpha"),
                Some("Artist A"),
                Some("Album A"),
                Some(10),
                Some(1),
                Some(1),
            ),
        ),
    ];

    for key in [
        PlaylistSortKey::NaturalFilename,
        PlaylistSortKey::Title,
        PlaylistSortKey::Artist,
        PlaylistSortKey::Album,
        PlaylistSortKey::Duration,
        PlaylistSortKey::SmartSequence,
    ] {
        let mut ascending_queue = queue_from_drafts(drafts.clone());
        sort(&mut ascending_queue, key, SortDirection::Ascending);
        assert_eq!(
            fallback_names(&ascending_queue),
            ["track 1.flac", "track 2.flac", "track 10.flac"],
            "ascending mismatch for {key:?}"
        );

        let mut descending_queue = queue_from_drafts(drafts.clone());
        sort(&mut descending_queue, key, SortDirection::Descending);
        assert_eq!(
            fallback_names(&descending_queue),
            ["track 10.flac", "track 2.flac", "track 1.flac"],
            "descending mismatch for {key:?}"
        );
    }
}

#[test]
fn natural_filename_handles_numbers_leading_zeroes_case_and_unicode_ties() {
    let mut queue = queue_from_drafts(vec![
        local_draft("Épisode 10.mkv", unknown_metadata("Épisode 10.mkv")),
        local_draft("épisode 2.mkv", unknown_metadata("épisode 2.mkv")),
        local_draft("ÉPISODE 02.mkv", unknown_metadata("ÉPISODE 02.mkv")),
        local_draft("Case.mkv", unknown_metadata("Case.mkv")),
        local_draft("case.mkv", unknown_metadata("case.mkv")),
    ]);

    sort(
        &mut queue,
        PlaylistSortKey::NaturalFilename,
        SortDirection::Ascending,
    );

    assert_eq!(
        fallback_names(&queue),
        [
            "Case.mkv",
            "case.mkv",
            "ÉPISODE 02.mkv",
            "épisode 2.mkv",
            "Épisode 10.mkv",
        ]
    );
}

#[cfg(unix)]
#[test]
fn native_non_utf8_filename_uses_exact_bytes_without_panic() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let invalid_ten =
        PathBuf::from("/music").join(OsString::from_vec(b"episode 10-\xff.mkv".to_vec()));
    let invalid_two =
        PathBuf::from("/music").join(OsString::from_vec(b"episode 2-\xfe.mkv".to_vec()));
    let mut queue = queue_from_drafts(vec![
        PlaylistItemDraft::local(
            LocalLocator::Native(invalid_ten),
            None,
            unknown_metadata("invalid-ten"),
        ),
        PlaylistItemDraft::local(
            LocalLocator::Native(invalid_two.clone()),
            None,
            unknown_metadata("invalid-two"),
        ),
    ]);

    sort(
        &mut queue,
        PlaylistSortKey::NaturalFilename,
        SortDirection::Ascending,
    );

    assert_eq!(
        queue.items()[0]
            .locator()
            .as_local()
            .and_then(LocalLocator::expose_native_path_for_persistence),
        Some(invalid_two.as_path())
    );
}

#[test]
fn foreign_non_utf_units_use_reversible_natural_policy() {
    let foreign_ten = ForeignPlatformPath::new(
        ForeignPathPlatform::Linux,
        ForeignPathEncoding::Bytes(b"/music/episode 10-\xff.mkv".to_vec()),
    );
    let foreign_two = ForeignPlatformPath::new(
        ForeignPathPlatform::Linux,
        ForeignPathEncoding::Bytes(b"/music/episode 2-\xfe.mkv".to_vec()),
    );
    let mut queue = queue_from_drafts(vec![
        PlaylistItemDraft::local(
            LocalLocator::Foreign(foreign_ten),
            None,
            unknown_metadata("foreign-ten"),
        ),
        PlaylistItemDraft::local(
            LocalLocator::Foreign(foreign_two),
            None,
            unknown_metadata("foreign-two"),
        ),
    ]);

    sort(
        &mut queue,
        PlaylistSortKey::NaturalFilename,
        SortDirection::Ascending,
    );
    assert_eq!(fallback_names(&queue), ["foreign-two", "foreign-ten"]);
}

#[test]
fn normalized_metadata_text_uses_artist_order_and_exact_case_fallback() {
    let first_artist = audio_metadata(
        "z.flac",
        Some("alpha"),
        Some("Artist"),
        Some("Älbum"),
        None,
        None,
        None,
    )
    .with_artists(vec!["Artist".into(), "Zulu".into()])
    .expect("bounded artists");
    let second_artist = audio_metadata(
        "a.flac",
        Some("Alpha"),
        Some("artist"),
        Some("älbum"),
        None,
        None,
        None,
    )
    .with_artists(vec!["artist".into(), "Alpha".into()])
    .expect("bounded artists");

    let drafts = vec![
        local_draft("z.flac", first_artist),
        local_draft("a.flac", second_artist),
    ];

    let mut title_queue = queue_from_drafts(drafts.clone());
    sort(
        &mut title_queue,
        PlaylistSortKey::Title,
        SortDirection::Ascending,
    );
    assert_eq!(fallback_names(&title_queue), ["a.flac", "z.flac"]);

    let mut artist_queue = queue_from_drafts(drafts.clone());
    sort(
        &mut artist_queue,
        PlaylistSortKey::Artist,
        SortDirection::Ascending,
    );
    assert_eq!(fallback_names(&artist_queue), ["z.flac", "a.flac"]);

    let mut album_queue = queue_from_drafts(drafts);
    sort(
        &mut album_queue,
        PlaylistSortKey::Album,
        SortDirection::Ascending,
    );
    assert_eq!(fallback_names(&album_queue), ["z.flac", "a.flac"]);
}

#[test]
fn metadata_missing_group_stays_last_and_uses_ascending_natural_fallback() {
    let known = audio_metadata(
        "known.flac",
        Some("Known"),
        Some("Known"),
        Some("Known"),
        Some(1),
        Some(1),
        Some(1),
    );
    let missing_a = audio_metadata("missing 2.flac", None, None, None, None, None, None);
    let missing_b = audio_metadata("missing 10.flac", None, None, None, None, None, None);

    for key in [
        PlaylistSortKey::Title,
        PlaylistSortKey::Artist,
        PlaylistSortKey::Album,
        PlaylistSortKey::Duration,
    ] {
        let mut queue = queue_from_drafts(vec![
            local_draft("missing 10.flac", missing_b.clone()),
            local_draft("known.flac", known.clone()),
            local_draft("missing 2.flac", missing_a.clone()),
        ]);
        sort(&mut queue, key, SortDirection::Descending);
        assert_eq!(
            fallback_names(&queue),
            ["known.flac", "missing 2.flac", "missing 10.flac"],
            "missing policy mismatch for {key:?}"
        );
    }
}

#[test]
fn smart_sequence_orders_audio_video_partial_tuples_and_unknown_last() {
    let audio_known = audio_metadata(
        "audio-known.flac",
        Some("Song"),
        None,
        Some("Album"),
        None,
        Some(1),
        Some(2),
    );
    let audio_partial = audio_metadata(
        "audio-partial.flac",
        Some("Song"),
        None,
        Some("Album"),
        None,
        None,
        None,
    );
    let video_known = video_metadata("video-known.mkv", Some("Episode"), Some(1), Some(2));
    let video_partial = video_metadata("video-partial.mkv", Some("Episode"), Some(1), None);
    let unknown = unknown_metadata("00-unknown.bin");
    let drafts = vec![
        local_draft("00-unknown.bin", unknown),
        local_draft("video-partial.mkv", video_partial),
        local_draft("audio-partial.flac", audio_partial),
        local_draft("video-known.mkv", video_known),
        local_draft("audio-known.flac", audio_known),
    ];

    let mut ascending_queue = queue_from_drafts(drafts.clone());
    sort(
        &mut ascending_queue,
        PlaylistSortKey::SmartSequence,
        SortDirection::Ascending,
    );
    assert_eq!(
        fallback_names(&ascending_queue),
        [
            "audio-known.flac",
            "audio-partial.flac",
            "video-known.mkv",
            "video-partial.mkv",
            "00-unknown.bin",
        ]
    );

    let mut descending_queue = queue_from_drafts(drafts);
    sort(
        &mut descending_queue,
        PlaylistSortKey::SmartSequence,
        SortDirection::Descending,
    );
    assert_eq!(
        fallback_names(&descending_queue),
        [
            "video-known.mkv",
            "video-partial.mkv",
            "audio-known.flac",
            "audio-partial.flac",
            "00-unknown.bin",
        ]
    );
}

#[test]
fn identical_locator_and_metadata_fall_back_to_stable_item_id() {
    let locator = SecretUrlLocator::from_reopenable_url("https://example.test/media")
        .expect("valid test URL identity");
    let metadata = unknown_metadata("same-name");
    let mut queue = queue_from_drafts(vec![
        PlaylistItemDraft::url(locator.clone(), metadata.clone()),
        PlaylistItemDraft::url(locator, metadata),
    ]);
    let allocated_ids = canonical_ids(&queue);

    sort(
        &mut queue,
        PlaylistSortKey::NaturalFilename,
        SortDirection::Ascending,
    );
    assert_eq!(canonical_ids(&queue), allocated_ids);
}

#[test]
fn canonical_sort_preserves_item_ids_current_and_shuffle_snapshot() {
    let mut queue = queue_from_drafts(vec![
        local_draft("item 10.flac", unknown_metadata("item 10.flac")),
        local_draft("item 2.flac", unknown_metadata("item 2.flac")),
        local_draft("item 1.flac", unknown_metadata("item 1.flac")),
    ]);
    let ids_before = canonical_ids(&queue);
    queue
        .set_traversal_current(ids_before[1])
        .expect("set traversal current");
    let mut random = StdRng::seed_from_u64(5);
    queue
        .enable_shuffle_with_rng(&mut random)
        .expect("enable deterministic shuffle");
    queue
        .commit_manual_play(ids_before[2])
        .expect("create factual shuffle history");
    let current_before = queue.traversal_current();
    let shuffle_before = queue.shuffle_traversal_snapshot();

    sort(
        &mut queue,
        PlaylistSortKey::NaturalFilename,
        SortDirection::Ascending,
    );

    let mut ids_after = canonical_ids(&queue);
    ids_after.sort_unstable();
    let mut expected_ids = ids_before;
    expected_ids.sort_unstable();
    assert_eq!(ids_after, expected_ids);
    assert_eq!(queue.traversal_current(), current_before);
    assert_eq!(queue.shuffle_traversal_snapshot(), shuffle_before);
}

#[test]
fn unchanged_order_is_typed_noop_without_dirty_revision() {
    let mut queue = queue_from_drafts(vec![
        local_draft("item 1.flac", unknown_metadata("item 1.flac")),
        local_draft("item 2.flac", unknown_metadata("item 2.flac")),
    ]);
    let revision_before = queue.revision_snapshot();

    assert_eq!(
        queue.sort_canonical(SortCanonicalQueue::new(
            PlaylistSortKey::NaturalFilename,
            SortDirection::Ascending,
        )),
        SortCanonicalQueueOutcome::AlreadyInCanonicalOrder
    );
    assert_eq!(queue.revision_snapshot(), revision_before);
}

#[test]
fn empty_single_and_reserved_queue_keep_noop_or_linearization_accounting() {
    for mut queue in [
        PlaylistQueue::new(),
        queue_from_drafts(vec![local_draft(
            "only.flac",
            unknown_metadata("only.flac"),
        )]),
    ] {
        let revision_before = queue.revision_snapshot();
        assert_eq!(
            queue.sort_canonical(SortCanonicalQueue::new(
                PlaylistSortKey::NaturalFilename,
                SortDirection::Ascending,
            )),
            SortCanonicalQueueOutcome::AlreadyInCanonicalOrder
        );
        assert_eq!(queue.revision_snapshot(), revision_before);
    }

    let mut reserved_queue = queue_from_drafts(vec![
        local_draft("item 10.flac", unknown_metadata("item 10.flac")),
        local_draft("item 2.flac", unknown_metadata("item 2.flac")),
    ]);
    let ids_before = canonical_ids(&reserved_queue);
    let revision_before = reserved_queue.revision_snapshot();
    let reservation = reserved_queue
        .prepare_reserved_mutation(
            revision_before,
            ReservedQueueMutation::select_committed(ids_before[0]),
        )
        .expect("install reservation lock");

    assert_eq!(
        reserved_queue.sort_canonical(SortCanonicalQueue::new(
            PlaylistSortKey::NaturalFilename,
            SortDirection::Ascending,
        )),
        SortCanonicalQueueOutcome::InstallCommitLinearizing
    );
    assert_eq!(canonical_ids(&reserved_queue), ids_before);
    assert_eq!(reserved_queue.revision_snapshot(), revision_before);
    reserved_queue.abort_reserved(reservation);
}

#[test]
fn preparation_observer_proves_exactly_one_pass_per_item() {
    let queue = queue_from_drafts(
        (0..257)
            .rev()
            .map(|index| {
                let filename = format!("item {index}.flac");
                local_draft(&filename, unknown_metadata(&filename))
            })
            .collect(),
    );
    let mut prepared_ids = Vec::new();

    let mut entries =
        prepare_sort_entries(queue.items(), PlaylistSortKey::NaturalFilename, |item_id| {
            prepared_ids.push(item_id)
        });
    entries.sort_by(|left, right| compare_entries(left, right, SortDirection::Ascending));

    assert_eq!(entries.len(), queue.len());
    assert_eq!(prepared_ids, canonical_ids(&queue));
}

#[test]
fn ten_thousand_items_have_deterministic_non_timing_characterization() {
    let drafts = (0..10_000)
        .rev()
        .map(|index| {
            let filename = format!("episode {index}.mkv");
            local_draft(&filename, unknown_metadata(&filename))
        })
        .collect::<Vec<_>>();
    let mut first_queue = queue_from_drafts(drafts.clone());
    let mut second_queue = queue_from_drafts(drafts);

    sort(
        &mut first_queue,
        PlaylistSortKey::NaturalFilename,
        SortDirection::Ascending,
    );
    sort(
        &mut second_queue,
        PlaylistSortKey::NaturalFilename,
        SortDirection::Ascending,
    );

    assert_eq!(canonical_ids(&first_queue), canonical_ids(&second_queue));
    assert_eq!(
        first_queue
            .items()
            .first()
            .expect("non-empty characterization")
            .cached_metadata()
            .fallback_display_name(),
        "episode 0.mkv"
    );
    assert_eq!(
        first_queue
            .items()
            .last()
            .expect("non-empty characterization")
            .cached_metadata()
            .fallback_display_name(),
        "episode 9999.mkv"
    );
}

#[test]
fn prepared_comparator_is_total_antisymmetric_and_transitive() {
    let queue = queue_from_drafts(
        [
            "item 0",
            "Item 00",
            "item 2",
            "ITEM 02",
            "item 10",
            "épisode 1",
            "Épisode 01",
            "zeta",
        ]
        .into_iter()
        .map(|filename| local_draft(filename, unknown_metadata(filename)))
        .collect(),
    );
    let entries = prepare_sort_entries(queue.items(), PlaylistSortKey::NaturalFilename, |_| {});

    for direction in [SortDirection::Ascending, SortDirection::Descending] {
        for left in &entries {
            assert_eq!(compare_entries(left, left, direction), Ordering::Equal);
            for right in &entries {
                assert_eq!(
                    compare_entries(left, right, direction),
                    compare_entries(right, left, direction).reverse()
                );
                for third in &entries {
                    let left_before_right =
                        compare_entries(left, right, direction) != Ordering::Greater;
                    let right_before_third =
                        compare_entries(right, third, direction) != Ordering::Greater;
                    if left_before_right && right_before_third {
                        assert_ne!(compare_entries(left, third, direction), Ordering::Greater);
                    }
                }
            }
        }
    }
}
