//! Поведение владельца: отложенная запись, смена media, вытеснение, переходы по очереди.

use std::time::{Duration, Instant};

use playlist_state::RememberedSubtitleChoice;

use super::*;
use crate::state::track_memory::{RecordOutcome, TRACK_MEMORY_SAVE_DEBOUNCE};

#[test]
fn changes_reach_the_disk_only_after_the_idle_pause() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let key = key_of("movie.mkv");
    let t0 = Instant::now();
    let mut owner = handle.lock();
    owner.begin_video(instance(1), Some(key), t0);

    let outcome = owner.remember_subtitles(instance(1), RememberedSubtitleChoice::Off, t0);
    assert_eq!(outcome, RecordOutcome::Stored);
    assert_eq!(
        owner.next_wake_deadline(),
        Some(t0 + TRACK_MEMORY_SAVE_DEBOUNCE),
        "the event loop is told when to wake for the write"
    );
    owner.poll(t0 + Duration::from_secs(1));
    assert!(
        !memory_file(directory.path()).exists(),
        "nothing is written before the pause is over"
    );

    owner.poll(t0 + TRACK_MEMORY_SAVE_DEBOUNCE);
    assert_eq!(
        owner.next_wake_deadline(),
        None,
        "the snapshot was handed off"
    );
    drop(owner);

    let on_disk = wait_for_entry_on_disk(directory.path(), &key);
    assert_eq!(
        on_disk.entry_for(&key).and_then(|entry| entry.subtitles()),
        Some(&RememberedSubtitleChoice::Off)
    );
}

#[test]
fn each_new_change_pushes_the_write_further_away() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let t0 = Instant::now();
    let mut owner = handle.lock();
    owner.begin_video(instance(1), Some(key_of("movie.mkv")), t0);

    owner.remember_subtitles(instance(1), RememberedSubtitleChoice::Off, t0);
    let later = t0 + Duration::from_secs(2);
    owner.remember_audio(
        instance(1),
        audio_hint("eng", "Original", "A_OPUS", 1),
        later,
    );

    assert_eq!(
        owner.next_wake_deadline(),
        Some(later + TRACK_MEMORY_SAVE_DEBOUNCE),
        "the pause counts from the LAST change"
    );
}

#[test]
fn switching_media_flushes_the_previous_videos_choices_immediately() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let first = key_of("a.mkv");
    let t0 = Instant::now();
    {
        let mut owner = handle.lock();
        owner.begin_video(instance(1), Some(first), t0);
        owner.remember_audio(instance(1), audio_hint("rus", "Dub", "A_FLAC", 0), t0);
        // Следующее видео открыто раньше, чем прошла пауза покоя.
        owner.begin_video(instance(2), Some(key_of("b.mkv")), t0);
        assert_eq!(owner.next_wake_deadline(), None, "flushed, not waiting");
    }

    let on_disk = wait_for_entry_on_disk(directory.path(), &first);
    assert_eq!(
        on_disk.entry_for(&first).and_then(|entry| entry.audio()),
        Some(&audio_hint("rus", "Dub", "A_FLAC", 0))
    );
}

#[test]
fn a_change_for_a_media_that_is_no_longer_current_is_dropped() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let now = Instant::now();
    let mut owner = handle.lock();
    owner.begin_video(instance(2), Some(key_of("b.mkv")), now);

    let stale = owner.remember_audio(instance(1), audio_hint("eng", "X", "A_AAC", 0), now);

    assert_eq!(stale, RecordOutcome::NotApplicable);
    assert_eq!(owner.remembered_video_count(), 0, "nothing leaked into B");
    assert_eq!(owner.next_wake_deadline(), None);
}

#[test]
fn a_source_without_a_key_remembers_nothing() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let now = Instant::now();
    let mut owner = handle.lock();
    owner.begin_video(instance(1), None, now);

    let outcome = owner.remember_subtitles(instance(1), RememberedSubtitleChoice::Off, now);

    assert_eq!(outcome, RecordOutcome::NotApplicable);
    assert_eq!(owner.remembered_for(instance(1)).subtitles, None);
}

#[test]
fn repeating_the_same_choice_does_not_schedule_another_write() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let t0 = Instant::now();
    let mut owner = handle.lock();
    owner.begin_video(instance(1), Some(key_of("movie.mkv")), t0);
    owner.remember_subtitles(instance(1), RememberedSubtitleChoice::Off, t0);
    owner.poll(t0 + TRACK_MEMORY_SAVE_DEBOUNCE);
    assert_eq!(owner.next_wake_deadline(), None);

    let repeated = owner.remember_subtitles(
        instance(1),
        RememberedSubtitleChoice::Off,
        t0 + Duration::from_secs(10),
    );

    assert_eq!(repeated, RecordOutcome::Unchanged);
    assert_eq!(owner.next_wake_deadline(), None, "no pointless disk write");
}

#[test]
fn the_oldest_video_is_forgotten_when_the_capacity_is_exceeded() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 3);
    let now = Instant::now();
    let mut owner = handle.lock();
    for (raw, name) in [(1, "a"), (2, "b"), (3, "c"), (4, "d")] {
        owner.begin_video(instance(raw), Some(key_of(name)), now);
        owner.remember_audio(instance(raw), audio_hint("eng", name, "A_AAC", 0), now);
    }

    assert_eq!(owner.remembered_video_count(), 3);
    assert_eq!(
        owner.remembered_for_key(&key_of("a")).audio,
        None,
        "the oldest entry was evicted"
    );
    for survivor in ["b", "c", "d"] {
        assert!(owner.remembered_for_key(&key_of(survivor)).audio.is_some());
    }
}

#[test]
fn reopening_a_video_protects_it_from_eviction() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 3);
    let now = Instant::now();
    let mut owner = handle.lock();
    for (raw, name) in [(1, "a"), (2, "b"), (3, "c")] {
        owner.begin_video(instance(raw), Some(key_of(name)), now);
        owner.remember_audio(instance(raw), audio_hint("eng", name, "A_AAC", 0), now);
    }
    // Открываем самое старое видео снова: оно становится самым свежим.
    owner.begin_video(instance(4), Some(key_of("a")), now);
    owner.begin_video(instance(5), Some(key_of("d")), now);
    owner.remember_audio(instance(5), audio_hint("eng", "d", "A_AAC", 0), now);

    assert!(owner.remembered_for_key(&key_of("a")).audio.is_some());
    assert_eq!(
        owner.remembered_for_key(&key_of("b")).audio,
        None,
        "the next oldest went instead"
    );
}

#[test]
fn shrinking_the_capacity_from_settings_evicts_the_oldest_and_reports_the_count() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 5);
    let now = Instant::now();
    let mut owner = handle.lock();
    for (raw, name) in [(1, "a"), (2, "b"), (3, "c"), (4, "d")] {
        owner.begin_video(instance(raw), Some(key_of(name)), now);
        owner.remember_audio(instance(raw), audio_hint("eng", name, "A_AAC", 0), now);
    }

    assert_eq!(
        owner.apply_capacity(capacity(5), now),
        0,
        "same capacity: no-op"
    );
    assert_eq!(owner.apply_capacity(capacity(2), now), 2);

    assert_eq!(owner.capacity(), capacity(2));
    assert_eq!(owner.remembered_for_key(&key_of("a")).audio, None);
    assert_eq!(owner.remembered_for_key(&key_of("b")).audio, None);
    assert!(owner.remembered_for_key(&key_of("c")).audio.is_some());
    assert!(owner.remembered_for_key(&key_of("d")).audio.is_some());
    assert!(
        owner.next_wake_deadline().is_some(),
        "the shrunk memory is scheduled for the disk"
    );
}

#[test]
fn queue_transition_gives_each_video_its_own_memory_and_returns_the_first_back() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let now = Instant::now();
    let mut owner = handle.lock();
    let dub = audio_hint("rus", "Dub", "A_FLAC", 0);
    let original = audio_hint("eng", "Original", "A_OPUS", 1);

    owner.begin_video(instance(1), Some(key_of("episode-1.mkv")), now);
    owner.remember_audio(instance(1), original.clone(), now);
    owner.remember_subtitles(instance(1), RememberedSubtitleChoice::Off, now);

    // Следующий эпизод: у него своя (пустая) память, а не соседняя.
    owner.begin_video(instance(2), Some(key_of("episode-2.mkv")), now);
    let second = owner.remembered_for(instance(2));
    assert_eq!(second.audio, None);
    assert_eq!(second.subtitles, None);
    owner.remember_audio(instance(2), dub.clone(), now);

    // Назад к первому: его выбор на месте и не перезаписан вторым.
    owner.begin_video(instance(3), Some(key_of("episode-1.mkv")), now);
    let first_again = owner.remembered_for(instance(3));
    assert_eq!(first_again.audio, Some(original));
    assert_eq!(first_again.subtitles, Some(RememberedSubtitleChoice::Off));
    assert_eq!(
        owner.remembered_for_key(&key_of("episode-2.mkv")).audio,
        Some(dub)
    );
}
