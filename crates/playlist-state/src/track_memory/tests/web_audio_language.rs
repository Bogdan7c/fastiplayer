//! Язык дубляжа в `track-memory.json`: круговой проход, старые файлы, границы значения.

use super::super::*;
use super::{capacity, key_of};
use crate::AtomicWriteOutcome;

fn store_in(directory: &tempfile::TempDir) -> TrackMemoryStore {
    TrackMemoryStore::new(directory.path().join(TRACK_MEMORY_FILENAME))
}

fn language(tag: &str) -> RememberedWebAudioLanguage {
    RememberedWebAudioLanguage::new(tag.to_owned()).expect("valid language")
}

#[test]
fn remembered_language_survives_write_and_inspect_next_to_other_choices() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let mut memory = TrackMemory::new(capacity(10));
    memory.remember_web_audio_language(key_of("dubbed"), language("ru"));
    memory.remember_sync_offset(
        key_of("dubbed"),
        SubtitleSyncOffset::from_millis(250).expect("offset"),
    );
    memory.remember_sync_offset(
        key_of("plain"),
        SubtitleSyncOffset::from_millis(100).expect("offset"),
    );

    assert!(matches!(store.write(&memory), AtomicWriteOutcome::Durable));

    let TrackMemoryInspection::Loaded(loaded) = store.inspect(capacity(10)) else {
        panic!("memory must load");
    };
    let dubbed = loaded.entry_for(&key_of("dubbed")).expect("entry");
    assert_eq!(dubbed.web_audio_language(), Some(&language("ru")));
    assert_eq!(dubbed.subtitle_sync_offset().millis(), 250);
    assert_eq!(
        loaded
            .entry_for(&key_of("plain"))
            .and_then(TrackMemoryEntry::web_audio_language),
        None,
        "a video without a dub choice stays without one"
    );
    let text = std::fs::read_to_string(store.track_memory_path()).expect("file");
    assert_eq!(text.matches("web_audio_language").count(), 1, "{text}");
}

#[test]
fn schema_one_and_schema_two_files_without_the_field_still_load() {
    for schema in [1, 2] {
        let directory = tempfile::tempdir().expect("dir");
        let store = store_in(&directory);
        let fingerprint = key_of("old").to_hex();
        let old_file = format!(
            r#"{{"schema_version":{schema},"entries":[{{"video_fingerprint_sha256":"{fingerprint}","last_used":1,"subtitles":{{"kind":"off"}},"subtitle_sync_offset_ms":250}}]}}"#
        );
        std::fs::write(store.track_memory_path(), old_file).expect("fixture");

        let TrackMemoryInspection::Loaded(loaded) = store.inspect(capacity(10)) else {
            panic!("a schema {schema} file must load");
        };
        let entry = loaded.entry_for(&key_of("old")).expect("entry");
        assert_eq!(entry.web_audio_language(), None);
        assert_eq!(entry.subtitles(), Some(&RememberedSubtitleChoice::Off));
    }
}

#[test]
fn an_invalid_language_in_the_file_marks_the_file_corrupt() {
    let directory = tempfile::tempdir().expect("dir");
    let store = store_in(&directory);
    let fingerprint = key_of("bad").to_hex();
    let file = format!(
        r#"{{"schema_version":2,"entries":[{{"video_fingerprint_sha256":"{fingerprint}","last_used":1,"subtitle_sync_offset_ms":0,"web_audio_language":""}}]}}"#
    );
    std::fs::write(store.track_memory_path(), file).expect("fixture");

    assert!(!matches!(
        store.inspect(capacity(10)),
        TrackMemoryInspection::Loaded(_)
    ));
}

#[test]
fn remember_reports_unchanged_for_the_same_language_and_stored_for_a_change() {
    let mut memory = TrackMemory::new(capacity(10));
    let first = memory.remember_web_audio_language(key_of("v"), language("ru"));
    let same = memory.remember_web_audio_language(key_of("v"), language("ru"));
    let back_to_original = memory.remember_web_audio_language(key_of("v"), language("en-US"));

    assert_eq!(first, RememberOutcome::Stored { evicted_entries: 0 });
    assert_eq!(same, RememberOutcome::Unchanged);
    assert_eq!(
        back_to_original,
        RememberOutcome::Stored { evicted_entries: 0 }
    );
    assert_eq!(
        memory
            .entry_for(&key_of("v"))
            .and_then(TrackMemoryEntry::web_audio_language),
        Some(&language("en-US")),
        "choosing the original again overwrites the remembered dub"
    );
}

#[test]
fn language_value_rejects_empty_and_oversized_tags_and_compares_case_insensitively() {
    assert_eq!(
        RememberedWebAudioLanguage::new(String::new()),
        Err(RememberedWebAudioLanguageError::Empty)
    );
    assert_eq!(
        RememberedWebAudioLanguage::new("x".repeat(MAX_HINT_LANGUAGE_BYTES + 1)),
        Err(RememberedWebAudioLanguageError::TooLong)
    );
    assert!(language("en-US").matches_tag("en-us"));
    assert!(!language("en-US").matches_tag("ru"));
}
