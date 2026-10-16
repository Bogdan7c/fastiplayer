//! Тесты памяти дорожек: общие помощники и проверки вытеснения/ключей.

mod matcher;
mod persistence;
mod segment_key;
mod writer;

use playlist_core::{LocalLocator, PlaylistLocator, SecretUrlLocator};

use super::*;

pub(super) fn local_locator(name: &str) -> PlaylistLocator {
    PlaylistLocator::Local(LocalLocator::Native(name.into()))
}

pub(super) fn url_locator(url: &str) -> PlaylistLocator {
    PlaylistLocator::Url(
        SecretUrlLocator::from_reopenable_url(url.to_owned()).expect("valid absolute URL"),
    )
}

pub(super) fn key_of(name: &str) -> VideoKey {
    VideoKey::for_locator(&local_locator(name)).expect("local key")
}

pub(super) fn capacity(entries: usize) -> TrackMemoryCapacity {
    TrackMemoryCapacity::new(entries).expect("valid capacity")
}

pub(super) fn hint(language: &str, codec: &str, ordinal: u32) -> RememberedTrackHint {
    RememberedTrackHint::new(
        Some(language.to_owned()),
        Some("Main".to_owned()),
        codec.to_owned(),
        ordinal,
    )
    .expect("valid hint")
}

pub(super) fn offset(millis: i64) -> SubtitleSyncOffset {
    SubtitleSyncOffset::from_millis(millis).expect("offset in range")
}

#[test]
fn lru_evicts_the_oldest_entry_when_capacity_is_exceeded() {
    let mut memory = TrackMemory::new(capacity(3));
    for name in ["a", "b", "c"] {
        memory.remember_audio(key_of(name), hint("eng", "A_AC3", 0));
    }

    let outcome = memory.remember_audio(key_of("d"), hint("eng", "A_AC3", 0));

    assert_eq!(outcome, RememberOutcome::Stored { evicted_entries: 1 });
    assert_eq!(memory.len(), 3);
    assert!(memory.entry_for(&key_of("a")).is_none(), "oldest evicted");
    for survivor in ["b", "c", "d"] {
        assert!(memory.entry_for(&key_of(survivor)).is_some());
    }
}

#[test]
fn touching_an_old_entry_protects_it_from_eviction() {
    let mut memory = TrackMemory::new(capacity(3));
    for name in ["a", "b", "c"] {
        memory.remember_audio(key_of(name), hint("eng", "A_AC3", 0));
    }

    assert_eq!(memory.touch(&key_of("a")), TouchOutcome::Touched);
    memory.remember_audio(key_of("d"), hint("eng", "A_AC3", 0));

    assert!(memory.entry_for(&key_of("a")).is_some(), "touched survives");
    assert!(
        memory.entry_for(&key_of("b")).is_none(),
        "next oldest evicted"
    );
}

#[test]
fn touch_of_unknown_video_neither_creates_an_entry_nor_changes_memory() {
    let mut memory = TrackMemory::new(capacity(3));
    memory.remember_audio(key_of("a"), hint("eng", "A_AC3", 0));
    let before = memory.clone();

    assert_eq!(
        memory.touch(&key_of("missing")),
        TouchOutcome::NotRemembered
    );

    assert_eq!(memory, before);
}

#[test]
fn shrinking_capacity_evicts_the_oldest_entries_and_reports_the_count() {
    let mut memory = TrackMemory::new(capacity(5));
    for name in ["a", "b", "c", "d", "e"] {
        memory.remember_audio(key_of(name), hint("eng", "A_AC3", 0));
    }

    let evicted = memory.set_capacity(capacity(2));

    assert_eq!(evicted, 3);
    assert_eq!(memory.len(), 2);
    assert!(memory.entry_for(&key_of("d")).is_some());
    assert!(memory.entry_for(&key_of("e")).is_some());
    assert_eq!(memory.capacity(), capacity(2));
}

#[test]
fn identical_remember_is_unchanged_and_does_not_refresh_recency() {
    let mut memory = TrackMemory::new(capacity(2));
    memory.remember_audio(key_of("a"), hint("eng", "A_AC3", 0));
    memory.remember_audio(key_of("b"), hint("eng", "A_AC3", 0));

    let repeated = memory.remember_audio(key_of("a"), hint("eng", "A_AC3", 0));
    memory.remember_audio(key_of("c"), hint("eng", "A_AC3", 0));

    assert_eq!(repeated, RememberOutcome::Unchanged);
    assert!(memory.entry_for(&key_of("a")).is_none(), "a stayed oldest");
}

#[test]
fn remember_methods_fill_independent_fields_of_one_entry() {
    let mut memory = TrackMemory::new(capacity(4));
    let key = key_of("video");
    memory.remember_audio(key, hint("jpn", "A_AC3", 1));
    memory.remember_subtitles(key, RememberedSubtitleChoice::Off);
    memory.remember_sync_offset(key, offset(-250));

    let entry = memory.entry_for(&key).expect("entry exists");
    assert_eq!(entry.audio(), Some(&hint("jpn", "A_AC3", 1)));
    assert_eq!(entry.subtitles(), Some(&RememberedSubtitleChoice::Off));
    assert_eq!(entry.subtitle_sync_offset().millis(), -250);
    assert_eq!(memory.len(), 1);
}

#[test]
fn zero_sync_offset_for_unknown_video_does_not_create_an_entry() {
    let mut memory = TrackMemory::new(capacity(4));

    let outcome = memory.remember_sync_offset(key_of("video"), SubtitleSyncOffset::ZERO);

    assert_eq!(outcome, RememberOutcome::Unchanged);
    assert!(memory.is_empty());
}

#[test]
fn never_chosen_fields_stay_none_and_offset_defaults_to_zero() {
    let mut memory = TrackMemory::new(capacity(4));
    memory.remember_audio(key_of("video"), hint("eng", "A_AC3", 0));

    let entry = memory.entry_for(&key_of("video")).expect("entry");

    assert!(entry.subtitles().is_none(), "never chosen != Off");
    assert_eq!(entry.subtitle_sync_offset(), SubtitleSyncOffset::ZERO);
}

#[test]
fn capacity_and_offset_and_hint_constructors_reject_out_of_range_values() {
    assert_eq!(
        TrackMemoryCapacity::new(0),
        Err(TrackMemoryCapacityError::Zero)
    );
    assert_eq!(
        TrackMemoryCapacity::new(TRACK_MEMORY_HARD_MAX_ENTRIES + 1),
        Err(TrackMemoryCapacityError::AboveHardMax)
    );
    assert!(SubtitleSyncOffset::from_millis(600_000).is_ok());
    assert!(SubtitleSyncOffset::from_millis(-600_000).is_ok());
    assert!(SubtitleSyncOffset::from_millis(600_001).is_err());
    assert!(SubtitleSyncOffset::from_millis(i64::MIN).is_err());
    assert_eq!(
        RememberedTrackHint::new(None, None, String::new(), 0),
        Err(TrackHintError::CodecIdEmpty)
    );
    assert_eq!(
        RememberedTrackHint::new(
            Some("x".repeat(MAX_HINT_LANGUAGE_BYTES + 1)),
            None,
            "A".into(),
            0
        ),
        Err(TrackHintError::LanguageTooLong)
    );
    assert_eq!(
        RememberedTrackHint::new(
            None,
            Some("x".repeat(MAX_HINT_TITLE_BYTES + 1)),
            "A".into(),
            0
        ),
        Err(TrackHintError::TitleTooLong)
    );
    assert_eq!(
        RememberedTrackHint::new(None, None, "x".repeat(MAX_HINT_CODEC_ID_BYTES + 1), 0),
        Err(TrackHintError::CodecIdTooLong)
    );
    assert!(RememberedFilePath::new(std::path::PathBuf::new()).is_err());
    assert_eq!(
        RememberedFilePath::new("x".repeat(MAX_REMEMBERED_FILE_PATH_UNITS + 1).into()),
        Err(RememberedFilePathError::TooLong)
    );
}

#[test]
fn empty_language_and_title_are_normalized_to_none() {
    let hint = RememberedTrackHint::new(Some(String::new()), Some(String::new()), "A".into(), 0)
        .expect("valid hint");

    assert_eq!(hint.language(), None);
    assert_eq!(hint.title(), None);
}

#[test]
fn video_key_is_stable_distinct_and_never_exposes_the_locator() {
    let secret = "https://user:password@example.test/private/video.mkv?token=secret";
    let first = VideoKey::for_locator(&url_locator(secret)).expect("key");
    let again = VideoKey::for_locator(&url_locator(secret)).expect("key");
    let other = VideoKey::for_locator(&url_locator(&format!("{secret}x"))).expect("key");
    let local = key_of("episode.mkv");

    assert_eq!(first, again);
    assert_ne!(first, other);
    assert_ne!(first, local);
    let debug = format!("{first:?}");
    assert!(!debug.contains("password") && !debug.contains("example"));
    assert_eq!(first.to_hex().len(), 64);
}

#[test]
fn video_key_uses_its_own_domain_and_differs_from_the_resume_fingerprint() {
    use crate::locator_fingerprint::{encode_lower_hex, locator_fingerprint_with_domain};

    let locator = local_locator("episode.mkv");
    let track_memory_hex = VideoKey::for_locator(&locator).expect("key").to_hex();
    let resume_hex = encode_lower_hex(
        &locator_fingerprint_with_domain(b"fastiplayer/playlist-resume/locator/v1", &locator)
            .expect("resume fingerprint"),
    );

    assert_eq!(
        resume_hex,
        "7b2e066cb7cdda835a35f379164ee3c984bfb6e0db31c0cb9a54b02cf6fd0017"
    );
    assert_ne!(track_memory_hex, resume_hex);
}

#[test]
fn restore_rejects_duplicate_keys_stamps_and_oversized_files() {
    let entry = |stamp: u64| {
        TrackMemoryEntry::restored(
            RecencyStamp::from_persisted(stamp),
            None,
            None,
            SubtitleSyncOffset::ZERO,
        )
    };
    assert!(
        TrackMemory::restore(
            capacity(5),
            vec![(key_of("a"), entry(1)), (key_of("a"), entry(2))]
        )
        .is_err()
    );
    assert!(
        TrackMemory::restore(
            capacity(5),
            vec![(key_of("a"), entry(1)), (key_of("b"), entry(1))]
        )
        .is_err()
    );
    let too_many: Vec<_> = (0..=TRACK_MEMORY_HARD_MAX_ENTRIES)
        .map(|index| {
            let mut bytes = [0_u8; 32];
            bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
            (
                VideoKey::from_bytes_for_tests(bytes),
                entry(index as u64 + 1),
            )
        })
        .collect();
    assert!(TrackMemory::restore(TrackMemoryCapacity::HARD_MAX, too_many).is_err());
}

#[test]
fn stamps_stay_ordered_after_exhaustion_renumbering() {
    let mut memory = TrackMemory::new(capacity(3));
    memory.remember_audio(key_of("a"), hint("eng", "A", 0));
    memory.remember_audio(key_of("b"), hint("eng", "A", 0));
    memory.exhaust_stamps_for_tests();

    memory.remember_audio(key_of("c"), hint("eng", "A", 0));
    memory.remember_audio(key_of("d"), hint("eng", "A", 0));

    assert!(memory.entry_for(&key_of("a")).is_none(), "a was oldest");
    let stamps: Vec<u64> = memory
        .entries_oldest_first()
        .iter()
        .map(|(_, entry)| entry.last_used().get())
        .collect();
    assert!(stamps.windows(2).all(|pair| pair[0] < pair[1]));
}
