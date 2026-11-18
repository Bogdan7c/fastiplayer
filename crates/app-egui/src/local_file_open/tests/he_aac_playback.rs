//! Сквозной путь обычного плеера для HE-AAC (решения 347–349 плана моста).
//!
//! Настоящий job открытия локального файла → настоящий `PlayerWorker` с
//! production-декодером звука → фейковый вывод, который считает дошедший PCM и
//! запоминает формат каждого созданного вывода. Доказывается не «декодер открылся»,
//! а «звук HE-AAC дошёл до вывода на выходной частоте, без пересборки вывода».

use std::path::Path;

use player_core::PlaybackState;

use super::capturing_audio_output::CapturedAudio;
use super::prepared_handoff::{
    AfterHandoff, LocalFixture, MINIMUM_DELIVERED_SHARE_PERCENT, play_to_terminal_state,
    prepare_and_hand_off,
};

/// Длительность сигналов фикстур HE-AAC (`generate.sh`).
const FIXTURE_SECONDS: usize = 2;
/// Выходная частота всех стерео-фикстур (ядро 22,05 кГц).
const OUTPUT_RATE_44K: u32 = 44_100;

fn he_aac_fixture(file_name: &str, output_rate: u32) -> LocalFixture {
    LocalFixture {
        path: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../audio-ffmpeg/tests/fixtures/he-aac")
            .join(file_name),
        expected_pcm_frames: FIXTURE_SECONDS * output_rate as usize,
        _scratch_directory: None,
    }
}

/// Файл доиграл, почти весь звук дошёл, и вывод создан ровно один раз — сразу
/// на выходной частоте HE-AAC и в стерео (глухое ядро дало бы 22,05 кГц).
fn assert_he_aac_played_through(fixture: &LocalFixture, output_rate: u32) -> CapturedAudio {
    let prepared_media = prepare_and_hand_off(fixture, AfterHandoff::DropJob);
    let (snapshot, captured) = play_to_terminal_state(prepared_media);

    assert_eq!(
        snapshot.playback_state,
        PlaybackState::Ended,
        "playback must reach the end, last error: {:?}",
        snapshot.last_error
    );
    assert!(snapshot.last_error.is_none(), "{:?}", snapshot.last_error);
    let minimum_frames = fixture.expected_pcm_frames * MINIMUM_DELIVERED_SHARE_PERCENT / 100;
    assert!(
        captured.pcm_frames >= minimum_frames,
        "only {} of {} PCM frames reached the output",
        captured.pcm_frames,
        fixture.expected_pcm_frames
    );
    assert!(captured.peak_amplitude > 0.01, "decoded audio is silent");
    let output_formats: Vec<(u32, u32)> = captured
        .output_specs
        .iter()
        .map(|spec| (spec.sample_rate, spec.channels()))
        .collect();
    assert_eq!(
        output_formats,
        vec![(output_rate, 2)],
        "one output at the HE-AAC rate"
    );
    captured
}

/// MP4 с паспортом как у Instagram Reels (`mp4a.40.5`).
#[test]
fn instagram_like_he_aac_v1_file_plays_through_at_the_output_rate() {
    assert_he_aac_played_through(
        &he_aac_fixture("v1-stereo-hierarchical.m4a", OUTPUT_RATE_44K),
        OUTPUT_RATE_44K,
    );
}

/// MP4 с паспортом как у TikTok (`mp4a.40.29`): моно-ядро, стерео на выходе.
#[test]
fn tiktok_like_he_aac_v2_file_plays_through_in_stereo() {
    assert_he_aac_played_through(
        &he_aac_fixture("v2-ps-hierarchical.m4a", OUTPUT_RATE_44K),
        OUTPUT_RATE_44K,
    );
}

/// MPEG-TS с ADTS (радио, IPTV): о SBR сказано только внутри пакетов.
#[test]
fn implicit_he_aac_in_mpeg_ts_plays_through_at_the_output_rate() {
    assert_he_aac_played_through(
        &he_aac_fixture("v1-stereo-implicit.ts", OUTPUT_RATE_44K),
        OUTPUT_RATE_44K,
    );
}
