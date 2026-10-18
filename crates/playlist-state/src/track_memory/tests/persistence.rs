//! Файл `track-memory.json`: круговой проход, повреждения, новая схема, права.

use std::path::PathBuf;
use std::time::SystemTime;

use super::super::*;
use super::{capacity, hint, key_of, local_locator, offset, url_locator};
use crate::{AtomicWriteOutcome, QuarantineFileName, QuarantineOutcome};

fn store_in(directory: &tempfile::TempDir) -> TrackMemoryStore {
    TrackMemoryStore::new(directory.path().join(TRACK_MEMORY_FILENAME))
}

fn filled_memory() -> TrackMemory {
    let mut memory = TrackMemory::new(capacity(10));
    memory.remember_audio(key_of("a"), hint("jpn", "A_AC3", 1));
    memory.remember_subtitles(
        key_of("a"),
        RememberedSubtitleChoice::Embedded(hint("eng", "S_TEXT/UTF8", 0)),
    );
    memory.remember_sync_offset(key_of("a"), offset(-250));
    memory.remember_subtitles(key_of("b"), RememberedSubtitleChoice::Off);
    memory.remember_subtitles(
        key_of("c"),
        RememberedSubtitleChoice::File(
            RememberedFilePath::new(PathBuf::from("/videos/c.ru.srt")).expect("path"),
        ),
    );
    memory
}

fn assert_corrupt(store: &TrackMemoryStore, expected: TrackMemoryCorruptCause) {
    match store.inspect(capacity(10)) {
        TrackMemoryInspection::CorruptNeedsQuarantine { cause, .. } => assert_eq!(cause, expected),
        other => panic!("corrupt expected, got {other:?}"),
    }
}

fn write_raw(store: &TrackMemoryStore, text: &str) {
    std::fs::write(store.track_memory_path(), text).expect("write fixture");
}

#[test]
fn write_then_inspect_roundtrips_every_field_and_recency_order() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let memory = filled_memory();

    assert!(matches!(store.write(&memory), AtomicWriteOutcome::Durable));

    match store.inspect(capacity(10)) {
        TrackMemoryInspection::Loaded(loaded) => assert_eq!(loaded, memory),
        other => panic!("loaded expected, got {other:?}"),
    }
}

#[test]
fn loaded_memory_continues_lru_order_after_restart() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let mut memory = TrackMemory::new(capacity(3));
    for name in ["a", "b", "c"] {
        memory.remember_audio(key_of(name), hint("eng", "A", 0));
    }
    memory.touch(&key_of("a"));
    store.write(&memory);

    let TrackMemoryInspection::Loaded(mut loaded) = store.inspect(capacity(3)) else {
        panic!("loaded expected");
    };
    loaded.remember_audio(key_of("d"), hint("eng", "A", 0));

    assert!(loaded.entry_for(&key_of("b")).is_none(), "b was oldest");
    assert!(loaded.entry_for(&key_of("a")).is_some());
}

#[test]
fn inspect_with_smaller_capacity_keeps_only_the_newest_entries() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    store.write(&filled_memory());

    let TrackMemoryInspection::Loaded(loaded) = store.inspect(capacity(1)) else {
        panic!("loaded expected");
    };

    assert_eq!(loaded.len(), 1);
    assert!(loaded.entry_for(&key_of("c")).is_some(), "newest survives");
}

#[test]
fn missing_file_is_missing_and_allows_saving() {
    let directory = tempfile::tempdir().expect("dir");
    let inspection = store_in(&directory).inspect(capacity(10));

    assert!(matches!(inspection, TrackMemoryInspection::Missing));
    assert!(inspection.allows_saving_immediately());
}

#[test]
fn garbage_json_is_corrupt_quarantined_and_next_write_recreates_the_file() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    write_raw(&store, r#"{"schema_version":1,"entries":"nope"}"#);

    let TrackMemoryInspection::CorruptNeedsQuarantine {
        inspected_identity,
        cause,
    } = store.inspect(capacity(10))
    else {
        panic!("corrupt expected");
    };
    assert_eq!(cause, TrackMemoryCorruptCause::InvalidV1Payload);
    let outcome = store.apply_quarantine(
        &inspected_identity,
        &QuarantineFileName::track_memory_from_timestamp(SystemTime::UNIX_EPOCH),
    );

    assert!(matches!(outcome, QuarantineOutcome::Applied { .. }));
    assert!(!store.track_memory_path().exists(), "original moved away");
    assert!(
        directory
            .path()
            .join("track-memory.corrupt-0-000000000.json")
            .exists()
    );
    assert!(matches!(
        store.inspect(capacity(10)),
        TrackMemoryInspection::Missing
    ));
    assert!(matches!(
        store.write(&filled_memory()),
        AtomicWriteOutcome::Durable
    ));
    assert!(matches!(
        store.inspect(capacity(10)),
        TrackMemoryInspection::Loaded(_)
    ));
}

#[test]
fn invalid_domain_values_are_corrupt() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let fingerprint = key_of("a").to_hex();
    let entry = |extra: &str, offset_ms: i64, fp: &str| {
        format!(
            r#"{{"schema_version":1,"entries":[{{"video_fingerprint_sha256":"{fp}","last_used":1,{extra}"subtitle_sync_offset_ms":{offset_ms}}}]}}"#
        )
    };

    write_raw(&store, &entry("", 600_001, &fingerprint));
    assert_corrupt(&store, TrackMemoryCorruptCause::InvalidDomainValue);

    write_raw(&store, &entry("", 0, "not-hex"));
    assert_corrupt(&store, TrackMemoryCorruptCause::InvalidDomainValue);

    let long_codec = "x".repeat(MAX_HINT_CODEC_ID_BYTES + 1);
    let overlong = format!(
        r#""audio":{{"language":null,"title":null,"codec_id":"{long_codec}","ordinal_among_kind":0}},"#
    );
    write_raw(&store, &entry(&overlong, 0, &fingerprint));
    assert_corrupt(&store, TrackMemoryCorruptCause::InvalidDomainValue);

    let empty_codec =
        r#""audio":{"language":null,"title":null,"codec_id":"","ordinal_among_kind":0},"#;
    write_raw(&store, &entry(empty_codec, 0, &fingerprint));
    assert_corrupt(&store, TrackMemoryCorruptCause::InvalidDomainValue);

    // Один и тот же видео-ключ дважды.
    let valid_entry = format!(
        r#"{{"video_fingerprint_sha256":"{fingerprint}","last_used":1,"subtitle_sync_offset_ms":0}}"#
    );
    let duplicate = valid_entry.replace(r#""last_used":1"#, r#""last_used":2"#);
    write_raw(
        &store,
        &format!(r#"{{"schema_version":1,"entries":[{valid_entry},{duplicate}]}}"#),
    );
    assert_corrupt(&store, TrackMemoryCorruptCause::InvalidDomainValue);
}

#[test]
fn unknown_fields_and_wrong_types_are_corrupt() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);

    write_raw(&store, r#"{"schema_version":1,"entries":[],"extra":1}"#);
    assert_corrupt(&store, TrackMemoryCorruptCause::InvalidV1Payload);

    write_raw(&store, r#"{"schema_version":1}"#);
    assert_corrupt(&store, TrackMemoryCorruptCause::InvalidV1Payload);
}

#[test]
fn non_json_garbage_is_quarantinable_and_its_bytes_are_preserved() {
    for garbage in [
        "{ definitely not the schema",
        "just text",
        r#"{"entries":[]}"#,
        r#"{"schema_version":"one"}"#,
        r#"{"schema_version":1,"schema_version":1}"#,
    ] {
        let directory = tempfile::tempdir().expect("dir");
        let store = store_in(&directory);
        write_raw(&store, garbage);

        let TrackMemoryInspection::CorruptNeedsQuarantine {
            inspected_identity,
            cause,
        } = store.inspect(capacity(10))
        else {
            panic!("garbage {garbage:?} must be quarantinable");
        };
        assert_eq!(cause, TrackMemoryCorruptCause::UnreadableEnvelope);
        let outcome = store.apply_quarantine(
            &inspected_identity,
            &QuarantineFileName::track_memory_from_timestamp(SystemTime::UNIX_EPOCH),
        );

        assert!(matches!(outcome, QuarantineOutcome::Applied { .. }));
        assert_eq!(
            std::fs::read_to_string(
                directory
                    .path()
                    .join("track-memory.corrupt-0-000000000.json")
            )
            .expect("quarantine file"),
            garbage,
            "nothing is deleted: the bytes move to the quarantine file"
        );
        assert!(matches!(
            store.inspect(capacity(10)),
            TrackMemoryInspection::Missing
        ));
    }
}

#[test]
fn oversized_v1_file_is_corrupt_not_protected() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let padding = " ".repeat(MAX_TRACK_MEMORY_BYTES as usize);
    write_raw(
        &store,
        &format!(r#"{{"schema_version":1,"entries":[]{padding}}}"#),
    );

    assert_corrupt(&store, TrackMemoryCorruptCause::SupportedFileTooLarge);
}

#[test]
fn newer_schema_blocks_saving_and_the_file_stays_untouched() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let future = r#"{"schema_version":4,"entries":[],"future_secret":"untouched"}"#;
    write_raw(&store, future);

    let inspection = store.inspect(capacity(10));

    assert!(matches!(
        inspection,
        TrackMemoryInspection::NewerSchemaSaveBlocked { schema_version: 4 }
    ));
    assert!(!inspection.allows_saving_immediately());
    assert_eq!(
        std::fs::read_to_string(store.track_memory_path()).expect("read"),
        future
    );
}

#[test]
fn directory_in_place_of_file_is_protected_not_corrupt() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    std::fs::create_dir(store.track_memory_path()).expect("dir instead of file");

    assert!(matches!(
        store.inspect(capacity(10)),
        TrackMemoryInspection::ProtectedSaveBlocked { .. }
    ));
}

#[test]
fn file_never_contains_raw_path_or_url_and_has_private_mode() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let secret_url = "https://user:password@example.test/private/video.mkv?token=secret";
    let mut memory = TrackMemory::new(capacity(10));
    memory.remember_audio(
        VideoKey::for_locator(&url_locator(secret_url)).expect("key"),
        hint("eng", "A_AC3", 0),
    );
    memory.remember_audio(
        VideoKey::for_locator(&local_locator("/home/user/secret-movie.mkv")).expect("key"),
        hint("eng", "A_AC3", 0),
    );
    store.write(&memory);

    let text = std::fs::read_to_string(store.track_memory_path()).expect("read");

    for forbidden in [
        "password",
        "token",
        "example.test",
        "secret-movie",
        "/home/user",
    ] {
        assert!(!text.contains(forbidden), "leaked {forbidden}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(store.track_memory_path())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn subtitle_file_path_roundtrips_utf8_and_non_utf8_unix_bytes() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let mut memory = TrackMemory::new(capacity(10));
    let utf8_path = PathBuf::from("/videos/Фильм (2020)/sub.ass");
    memory.remember_subtitles(
        key_of("utf8"),
        RememberedSubtitleChoice::File(RememberedFilePath::new(utf8_path).expect("path")),
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        let raw =
            std::ffi::OsString::from_vec(vec![b'/', b'v', 0xff, 0xfe, b'.', b's', b'r', b't']);
        memory.remember_subtitles(
            key_of("raw"),
            RememberedSubtitleChoice::File(
                RememberedFilePath::new(PathBuf::from(raw)).expect("path"),
            ),
        );
    }
    store.write(&memory);

    let TrackMemoryInspection::Loaded(loaded) = store.inspect(capacity(10)) else {
        panic!("loaded expected");
    };

    assert_eq!(loaded, memory);
}

#[test]
fn oversized_memory_is_refused_by_write_without_touching_the_existing_file() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    store.write(&filled_memory());
    let before = std::fs::read(store.track_memory_path()).expect("read");
    let serialized = super::super::dto::serialize_track_memory(&filled_memory(), 16);

    assert_eq!(
        serialized,
        Err(crate::StateSerializationError::SerializedStateTooLarge)
    );
    assert_eq!(
        std::fs::read(store.track_memory_path()).expect("read"),
        before
    );
}
