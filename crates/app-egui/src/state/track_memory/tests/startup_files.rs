//! Файл памяти при запуске: повреждённый, от новой версии, обычный круг «записали - перечитали».

use std::time::{Duration, Instant};

use playlist_state::{RememberedSubtitleChoice, SubtitleSyncOffset};

use super::*;
use crate::state::track_memory::TrackMemoryShutdownOutcome;

#[test]
fn corrupt_file_is_set_aside_with_a_warning_and_recreated_on_the_next_change() {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        memory_file(directory.path()),
        b"{ definitely not the schema",
    )
    .expect("seed");

    let ((handle, outcome), log) = with_captured_log(|| start_in(directory.path(), 10));

    assert!(
        matches!(
            outcome,
            TrackMemoryStartOutcome::RecoveredFromCorruptFile { .. }
        ),
        "got {outcome:?}"
    );
    assert!(log.contains("WARN"), "{log}");
    assert!(
        log.contains("Corrupted track memory was set aside"),
        "{log}"
    );
    assert!(
        !log.contains(directory.path().to_str().expect("utf-8 path")),
        "the warning must not contain paths: {log}"
    );
    let quarantined: Vec<_> = std::fs::read_dir(directory.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("track-memory.corrupt-")
        })
        .collect();
    assert_eq!(quarantined.len(), 1, "exactly one quarantine file");
    assert_eq!(
        std::fs::read(quarantined[0].path()).expect("read quarantine"),
        b"{ definitely not the schema",
        "the damaged bytes are preserved, not deleted"
    );
    assert!(
        !memory_file(directory.path()).exists(),
        "the damaged file moved away"
    );
    assert_eq!(handle.lock().remembered_video_count(), 0, "clean start");
    assert!(handle.lock().is_writing_to_disk(), "writing stays enabled");

    // Первое изменение пересоздаёт файл.
    let key = key_of("movie.mkv");
    {
        let mut owner = handle.lock();
        let now = Instant::now();
        owner.begin_video(instance(1), Some(key), now);
        owner.remember_audio(instance(1), audio_hint("eng", "Original", "A_OPUS", 1), now);
        assert!(matches!(
            owner.shutdown(Duration::from_secs(5)),
            TrackMemoryShutdownOutcome::Completed(_)
        ));
    }
    let on_disk = wait_for_entry_on_disk(directory.path(), &key);
    assert_eq!(on_disk.len(), 1);
}

#[test]
fn newer_schema_file_is_never_overwritten_and_choices_live_in_memory_only() {
    let directory = tempfile::tempdir().expect("tempdir");
    let original = br#"{"schema_version": 999, "future": true}"#;
    std::fs::write(memory_file(directory.path()), original).expect("seed");

    let ((handle, outcome), log) = with_captured_log(|| start_in(directory.path(), 10));

    assert!(
        matches!(
            outcome,
            TrackMemoryStartOutcome::NewerSchema {
                schema_version: 999
            }
        ),
        "got {outcome:?}"
    );
    assert!(log.contains("newer version"), "{log}");
    let key = key_of("movie.mkv");
    let mut owner = handle.lock();
    let now = Instant::now();
    owner.begin_video(instance(1), Some(key), now);
    owner.remember_subtitles(instance(1), RememberedSubtitleChoice::Off, now);
    assert_eq!(
        owner.remembered_for(instance(1)).subtitles,
        Some(RememberedSubtitleChoice::Off),
        "in-session memory still works"
    );
    assert_eq!(
        owner.next_wake_deadline(),
        None,
        "nothing is scheduled for the disk"
    );
    assert_eq!(
        owner.shutdown(Duration::from_secs(1)),
        TrackMemoryShutdownOutcome::NothingToFlush
    );
    assert_eq!(
        std::fs::read(memory_file(directory.path())).expect("file still there"),
        original,
        "the newer file is byte-identical"
    );
}

#[test]
fn saved_choices_survive_a_restart_and_a_missing_file_starts_empty() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (first_run, outcome) = start_in(directory.path(), 10);
    assert!(matches!(outcome, TrackMemoryStartOutcome::StartedEmpty));
    let key = key_of("movie.mkv");
    {
        let mut owner = first_run.lock();
        let now = Instant::now();
        owner.begin_video(instance(1), Some(key), now);
        owner.remember_audio(instance(1), audio_hint("eng", "Original", "A_OPUS", 1), now);
        owner.remember_subtitles(instance(1), RememberedSubtitleChoice::Off, now);
        owner.remember_sync_offset(
            instance(1),
            SubtitleSyncOffset::from_millis(500).expect("in range"),
            now,
        );
        owner.shutdown(Duration::from_secs(5));
    }

    let (second_run, outcome) = start_in(directory.path(), 10);

    assert!(
        matches!(
            outcome,
            TrackMemoryStartOutcome::Loaded {
                remembered_videos: 1
            }
        ),
        "got {outcome:?}"
    );
    let remembered = second_run.lock().remembered_for_key(&key);
    assert_eq!(
        remembered.audio,
        Some(audio_hint("eng", "Original", "A_OPUS", 1))
    );
    assert_eq!(remembered.subtitles, Some(RememberedSubtitleChoice::Off));
    assert_eq!(remembered.sync_offset.millis(), 500);
}
