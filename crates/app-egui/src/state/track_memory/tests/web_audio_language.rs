//! Язык дубляжа в памяти приложения: запись, перезапуск, перекрытие оригиналом, неприменимость.

use std::time::Instant;

use playlist_state::RememberedWebAudioLanguage;

use super::*;
use crate::state::track_memory::RecordOutcome;

fn language(tag: &str) -> RememberedWebAudioLanguage {
    RememberedWebAudioLanguage::new(tag.to_owned()).expect("language")
}

#[test]
fn a_chosen_language_reaches_the_disk_and_is_read_back_after_a_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let video = key_of("dubbed-video");
    {
        let (handle, _) = start_in(directory.path(), 10);
        let mut owner = handle.lock();
        let outcome = owner.remember_web_audio_language(video, language("ru"), Instant::now());
        assert_eq!(outcome, RecordOutcome::Stored);
        owner.flush_unsaved_changes();
    }
    wait_for_entry_on_disk(directory.path(), &video);

    let (handle, _) = start_in(directory.path(), 10);

    assert_eq!(
        handle.lock().remembered_for_key(&video).web_audio_language,
        Some(language("ru"))
    );
    assert_eq!(
        handle
            .lock()
            .remembered_for_key(&key_of("another"))
            .web_audio_language,
        None,
        "another video has no remembered language"
    );
}

#[test]
fn choosing_the_original_again_overwrites_the_dub_and_repeating_is_unchanged() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let video = key_of("dubbed-video");
    let mut owner = handle.lock();
    let now = Instant::now();

    owner.remember_web_audio_language(video, language("ru"), now);
    let repeat = owner.remember_web_audio_language(video, language("ru"), now);
    let back = owner.remember_web_audio_language(video, language("en-US"), now);

    assert_eq!(repeat, RecordOutcome::Unchanged);
    assert_eq!(back, RecordOutcome::Stored);
    assert_eq!(
        owner.remembered_for_key(&video).web_audio_language,
        Some(language("en-US"))
    );
}

#[test]
fn a_language_choice_does_not_disturb_audio_or_subtitle_memory() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (handle, _) = start_in(directory.path(), 10);
    let video = key_of("dubbed-video");
    let mut owner = handle.lock();
    owner.begin_video(instance(1), Some(video), Instant::now());
    owner.remember_audio(
        instance(1),
        audio_hint("eng", "Main", "A_OPUS", 0),
        Instant::now(),
    );

    owner.remember_web_audio_language(video, language("ru"), Instant::now());

    let remembered = owner.remembered_for_key(&video);
    assert_eq!(
        remembered.audio,
        Some(audio_hint("eng", "Main", "A_OPUS", 0))
    );
    assert_eq!(remembered.subtitles, None);
}
