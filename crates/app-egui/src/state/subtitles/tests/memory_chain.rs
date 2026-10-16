//! Сквозная цепочка памяти выбора: пользователь выбрал вторую озвучку, субтитры `en` и сдвиг
//! +0,5 с -> память -> файл на диске -> «перезапуск» (новое чтение файла) -> то же видео снова:
//! план звука выбирает вторую озвучку, а в кадре (после настоящего GPU-прохода) `en` со сдвигом.
//!
//! Всё, кроме окна, настоящее: фикстура-фильм, демуксер, владелец субтитров, владелец памяти,
//! файл в временном каталоге, GPU-проход с readback. Выбор звука плеером проверяет отдельно
//! `player-core` (`session/tests/audio_track_switch.rs`: `InstalledTrackRestore::Select`
//! приводит к звуку второй дорожки на выходе) - здесь проверяется, что память выдаёт именно
//! этот выбор для настоящего списка дорожек фильма.

use std::time::{Duration, Instant, SystemTime};

use media_core::TrackKind;
use playlist_state::TrackMemoryStore;

use super::embedded_support::*;
use super::gpu_chain::Harness;
use super::*;
use crate::state::ActiveMediaSource;
use crate::state::subtitles::SyncNudge;
use crate::state::track_memory::{
    RememberedAudioPlan, TrackMemoryShutdownOutcome, TrackMemoryStartOutcome, plan_audio_restore,
    record_subtitle_changes, record_user_audio_choice, start_track_memory,
    subtitle_restore_request, video_key_for_source,
};

fn picture() -> render_core::PictureRect {
    letterboxed_rect(256, 144)
}

fn differing_pixels(a: &[u8], b: &[u8]) -> usize {
    let (left, _) = a.as_chunks::<4>();
    let (right, _) = b.as_chunks::<4>();
    left.iter().zip(right).filter(|(l, r)| l != r).count()
}

#[test]
fn user_choices_survive_a_restart_for_the_same_video_but_not_for_another_one() {
    let fixture = demux_fixture("two-audio-two-srt.mkv");
    let directory = tempfile::tempdir().expect("tempdir");
    let memory_path = directory.path().join(playlist_state::TRACK_MEMORY_FILENAME);
    let movie = ActiveMediaSource::LocalFile(directory.path().join("Movie.mkv"));
    let other_movie = ActiveMediaSource::LocalFile(directory.path().join("Other.mkv"));
    let key = video_key_for_source(&movie).expect("movie key");
    let audio_rows: Vec<_> = fixture
        .tracks
        .iter()
        .filter(|row| row.kind == TrackKind::Audio)
        .collect();
    let (default_audio, second_audio) = (audio_rows[0], audio_rows[1]);
    assert_eq!(
        second_audio.sample_rate,
        Some(48_000),
        "second audio is the 880 Hz Opus dub"
    );

    // ---- первый запуск: пользователь делает выбор ----
    let first_media = instance(2);
    let (first_memory, outcome) = start_track_memory(
        std::sync::Arc::new(TrackMemoryStore::new(&memory_path)),
        capacity(10),
        SystemTime::now(),
    );
    assert!(matches!(outcome, TrackMemoryStartOutcome::StartedEmpty));
    let mut first_owner = embedded_owner(&disabled_config(), first_media, &fixture);
    first_memory
        .lock()
        .begin_video(first_media, Some(key), Instant::now());

    record_user_audio_choice(
        &mut first_memory.lock(),
        first_media,
        &fixture.tracks,
        second_audio.id,
        Instant::now(),
    );
    let english = first_owner.menu_view().tracks[0].id;
    first_owner.select_track(english);
    for _ in 0..5 {
        first_owner.nudge_sync(SyncNudge::Later);
    }
    record_subtitle_changes(
        &mut first_memory.lock(),
        first_owner.take_memory_changes().expect("user changes"),
        Instant::now(),
    );
    assert!(matches!(
        first_memory.lock().shutdown(Duration::from_secs(5)),
        TrackMemoryShutdownOutcome::Completed(_)
    ));
    drop(first_owner);

    // ---- перезапуск: память читается заново с диска ----
    let (second_memory, outcome) = start_track_memory(
        std::sync::Arc::new(TrackMemoryStore::new(&memory_path)),
        capacity(10),
        SystemTime::now(),
    );
    assert!(
        matches!(
            outcome,
            TrackMemoryStartOutcome::Loaded {
                remembered_videos: 1
            }
        ),
        "got {outcome:?}"
    );

    // Звук: до начала воспроизведения выбирается вторая озвучка вместо первой по умолчанию.
    let remembered = second_memory.lock().remembered_for_key(&key);
    assert_eq!(
        plan_audio_restore(
            remembered.audio.as_ref(),
            &fixture.tracks,
            Some(default_audio.id)
        ),
        RememberedAudioPlan::Select(second_audio.id)
    );

    // Другое видео: своя (пустая) память, ничего чужого.
    let other_key = video_key_for_source(&other_movie).expect("other key");
    let other = second_memory.lock().remembered_for_key(&other_key);
    assert_eq!(
        plan_audio_restore(
            other.audio.as_ref(),
            &fixture.tracks,
            Some(default_audio.id)
        ),
        RememberedAudioPlan::NothingRemembered
    );
    assert_eq!(other.subtitles, None);

    // Субтитры и сдвиг: открываем то же видео заново (новый MediaInstanceId).
    let second_media = instance(9);
    second_memory
        .lock()
        .begin_video(second_media, Some(key), Instant::now());
    let request = subtitle_restore_request(&second_memory.lock().remembered_for(second_media));
    let mut owner = owner_with_config(&disabled_config());
    owner.begin_media_restoring(second_media, SubtitleMediaSource::NotLocal, request);
    owner.sync_embedded_tracks(Some(second_media), &fixture.tracks);
    wait_until(&mut owner, "rasterizer", |owner| {
        owner.presenter.has_rasterizer()
    });
    owner.deliver_feed_messages(fixture.feed_messages(second_media));

    let menu = owner.menu_view();
    assert!(menu.tracks[0].is_selected, "the English track is on again");
    assert_eq!(
        menu.sync_offset,
        subtitle_core::SubtitleSyncOffset::from_millis(500)
    );
    assert_eq!(
        owner.take_enabled_preference_change(),
        None,
        "the global `subtitles.enabled` stays untouched"
    );
    // «Hello» идёт 0,5-1,5 с; со сдвигом +0,5 с - 1,0-2,0 с.
    assert_eq!(visible_texts(&mut owner, second_media, 700), 0);
    assert!(visible_texts(&mut owner, second_media, 1_800) > 0);

    // На экране: настоящий GPU-проход и readback.
    match Harness::new() {
        Some(mut harness) => {
            let rect = picture();
            let (_, video_only) = harness.frame(rect, None);
            let render = |owner: &mut SubtitleOwner, harness: &mut Harness, pts_ms: u64| {
                let overlay = owner
                    .overlay_for_frame(
                        current(second_media, GENERATION),
                        &frame(pts_ms, GENERATION, rect),
                    )
                    .cloned();
                harness.frame(rect, overlay.as_ref()).1
            };
            let before_shifted_start = render(&mut owner, &mut harness, 700);
            let inside_shifted_cue = render(&mut owner, &mut harness, 1_800);
            assert_eq!(
                before_shifted_start, video_only,
                "no subtitle at 0.7 s: the cue is shifted to start at 1.0 s"
            );
            assert!(
                differing_pixels(&inside_shifted_cue, &video_only) > 50,
                "the English cue is painted at 1.8 s only because of the remembered +0.5 s"
            );
        }
        None => eprintln!("skip: no GPU adapter for the pixel check of the memory chain"),
    }
}

fn capacity(entries: usize) -> playlist_state::TrackMemoryCapacity {
    playlist_state::TrackMemoryCapacity::new(entries).expect("valid capacity")
}

fn disabled_config() -> SubtitlesConfig {
    SubtitlesConfig {
        enabled: false,
        ..SubtitlesConfig::default()
    }
}
