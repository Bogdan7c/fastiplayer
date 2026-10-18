//! Выбор субтитров потока в `track-memory.json`: круговой проход и совместимость схем 1-3.

use super::super::*;
use super::{capacity, key_of};
use crate::AtomicWriteOutcome;

fn store_in(directory: &tempfile::TempDir) -> TrackMemoryStore {
    TrackMemoryStore::new(directory.path().join(TRACK_MEMORY_FILENAME))
}

fn stream_choice(language: Option<&str>, name: &str, forced: bool) -> RememberedSubtitleChoice {
    RememberedSubtitleChoice::Stream(
        RememberedStreamSubtitle::new(language.map(str::to_owned), name.to_owned(), forced)
            .expect("valid stream choice"),
    )
}

#[test]
fn stream_choice_survives_write_and_inspect_and_file_uses_schema_three() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let mut memory = TrackMemory::new(capacity(10));
    memory.remember_subtitles(key_of("full"), stream_choice(Some("en"), "English", false));
    memory.remember_subtitles(
        key_of("forced"),
        stream_choice(Some("ru"), "Russian (forced)", true),
    );
    memory.remember_subtitles(
        key_of("no-language"),
        stream_choice(None, "Commentary", false),
    );

    assert!(matches!(store.write(&memory), AtomicWriteOutcome::Durable));

    let text = std::fs::read_to_string(store.track_memory_path()).expect("file");
    assert!(text.contains(r#""schema_version": 3"#), "{text}");
    let TrackMemoryInspection::Loaded(loaded) = store.inspect(capacity(10)) else {
        panic!("memory must load");
    };
    for (name, expected) in [
        ("full", stream_choice(Some("en"), "English", false)),
        (
            "forced",
            stream_choice(Some("ru"), "Russian (forced)", true),
        ),
        ("no-language", stream_choice(None, "Commentary", false)),
    ] {
        assert_eq!(
            loaded
                .entry_for(&key_of(name))
                .and_then(|entry| entry.subtitles()),
            Some(&expected),
            "{name}"
        );
    }
}

#[test]
fn files_written_by_schemas_one_and_two_still_load() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let fingerprint = key_of("old").to_hex();
    for (version, subtitles) in [
        (1, r#"{"kind":"off"}"#),
        (2, r#"{"kind":"web","language":"en","track_kind":"manual"}"#),
    ] {
        let file = format!(
            r#"{{"schema_version":{version},"entries":[{{"video_fingerprint_sha256":"{fingerprint}","last_used":1,"subtitles":{subtitles},"subtitle_sync_offset_ms":250}}]}}"#
        );
        std::fs::write(store.track_memory_path(), file).expect("fixture");
        let TrackMemoryInspection::Loaded(loaded) = store.inspect(capacity(10)) else {
            panic!("a schema {version} file must load");
        };
        let entry = loaded.entry_for(&key_of("old")).expect("entry");
        assert!(entry.subtitles().is_some(), "schema {version}");
        assert_eq!(entry.subtitle_sync_offset().millis(), 250);
    }
}

#[test]
fn a_stream_choice_inside_an_older_schema_file_is_rejected_as_corrupt() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let fingerprint = key_of("forged").to_hex();
    for version in [1, 2] {
        let forged = format!(
            r#"{{"schema_version":{version},"entries":[{{"video_fingerprint_sha256":"{fingerprint}","last_used":1,"subtitles":{{"kind":"stream","language":"en","name":"English","forced":false}},"subtitle_sync_offset_ms":0}}]}}"#
        );
        std::fs::write(store.track_memory_path(), forged).expect("fixture");
        assert!(
            matches!(
                store.inspect(capacity(10)),
                TrackMemoryInspection::CorruptNeedsQuarantine { .. }
            ),
            "schema {version} must reject a stream choice"
        );
    }
}

#[test]
fn a_newer_schema_four_file_is_not_touched_by_this_binary() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    std::fs::write(
        store.track_memory_path(),
        r#"{"schema_version":4,"entries":[]}"#,
    )
    .expect("fixture");
    assert!(!matches!(
        store.inspect(capacity(10)),
        TrackMemoryInspection::Loaded(_)
    ));
}

#[test]
fn stream_choice_rejects_bad_values() {
    assert_eq!(
        RememberedStreamSubtitle::new(Some(String::new()), "English".to_owned(), false),
        Err(RememberedStreamSubtitleError::EmptyLanguage)
    );
    assert_eq!(
        RememberedStreamSubtitle::new(
            Some("x".repeat(MAX_HINT_LANGUAGE_BYTES + 1)),
            "English".to_owned(),
            false
        ),
        Err(RememberedStreamSubtitleError::LanguageTooLong)
    );
    assert_eq!(
        RememberedStreamSubtitle::new(None, String::new(), false),
        Err(RememberedStreamSubtitleError::EmptyName)
    );
    assert_eq!(
        RememberedStreamSubtitle::new(None, "x".repeat(MAX_HINT_TITLE_BYTES + 1), false),
        Err(RememberedStreamSubtitleError::NameTooLong)
    );
}

#[test]
fn stream_choice_with_an_empty_name_in_a_schema_three_file_is_corrupt() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let fingerprint = key_of("empty-name").to_hex();
    let bad = format!(
        r#"{{"schema_version":3,"entries":[{{"video_fingerprint_sha256":"{fingerprint}","last_used":1,"subtitles":{{"kind":"stream","language":null,"name":"","forced":false}},"subtitle_sync_offset_ms":0}}]}}"#
    );
    std::fs::write(store.track_memory_path(), bad).expect("fixture");
    assert!(matches!(
        store.inspect(capacity(10)),
        TrackMemoryInspection::CorruptNeedsQuarantine { .. }
    ));
}
