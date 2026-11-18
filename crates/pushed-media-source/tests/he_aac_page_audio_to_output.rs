//! Звук HE-AAC v2 со страницы (как у TikTok) через мост — до выхода звука.
//!
//! Страница кормит MSE фрагментированным MP4 с `mp4a.40.29`: ядро моно 22,05 кГц,
//! стерео и верх восстанавливают SBR и Parametric Stereo. Путь — тот же, что у
//! браузерного режима: куски SourceBuffer → `pushed-media-source` → демуксер →
//! production-декодер звука (маршрут AAC → FFmpeg) → выход. Проверяется сам PCM
//! в выходе: выходная частота 44,1 кГц, стерео и разные тоны в левом и правом
//! канале (из моно-ядра без PS получились бы одинаковые каналы).
//!
//! Фикстура: `crates/audio-ffmpeg/tests/fixtures/he-aac/v2-ps-hierarchical-fragmented.mp4`
//! (2 с, фрагменты по 0,5 с; слева 440 Гц, справа 1 кГц).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::path::Path;
use std::time::Duration;

use player_core::PlaybackState;
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamDemuxer, pushed_stream,
};
use support::media::FragmentedMp4;
use support::playback::{PlaybackReport, open, play};

const AUDIO: LaneId = LaneId::new(0);
/// Длительность фикстуры.
const CLIP: Duration = Duration::from_secs(2);
/// Выходная частота HE-AAC (ядро 22,05 кГц).
const OUTPUT_RATE: u32 = 44_100;
/// Кадров HE-AAC в фикстуре: 2 с сигнала плюс задержка и добивка кодера fdkaac.
/// Priming AAC плеер не обрезает (общее ограничение: обрезка не переносится
/// через демуксер), поэтому до выхода доходят все кодированные кадры.
const CODED_HE_AAC_FRAMES: u32 = 47;
/// Отсчётов на выходе одного кадра HE-AAC (1024 ядра × 2 после SBR).
const HE_AAC_FRAME_SAMPLES: u32 = 2_048;
/// Точность длины записанного звука: один кадр HE-AAC.
const AUDIO_LENGTH_TOLERANCE: Duration = Duration::from_millis(47);
/// Насколько тон «своего» канала громче чужого: FAAD2 даёт ~50 дБ, моно — 0 дБ.
const MIN_CHANNEL_SEPARATION_DB: f64 = 30.0;
/// Размер куска, которым страница кормит SourceBuffer (меньше фрагмента).
const PAGE_APPEND_CHUNK_BYTES: usize = 1_000;

/// Поток страницы: инициализация и фрагменты, порезанные на куски, как кормит MSE.
fn page_stream() -> PushedStreamDemuxer {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../audio-ffmpeg/tests/fixtures/he-aac/v2-ps-hierarchical-fragmented.mp4");
    let fixture = FragmentedMp4::split(&std::fs::read(fixture_path).expect("fixture reads"));
    let page_bytes = fixture.stream_of(0..fixture.fragments.len());

    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(AUDIO, PushedContainer::FragmentedMp4)
        .expect("audio lane");
    writer.set_duration(PushedDuration::Finite(CLIP));
    for chunk in page_bytes.chunks(PAGE_APPEND_CHUNK_BYTES) {
        writer.append(AUDIO, chunk).expect("page bytes");
    }
    writer.end_of_stream().expect("end of stream");
    open(&mut opener)
}

#[test]
fn tiktok_like_he_aac_from_the_page_reaches_the_output_in_stereo() {
    let report = play(page_stream(), |_| {});

    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    assert_eq!(
        report.audio_format,
        Some((OUTPUT_RATE, 2)),
        "HE-AAC plays at the doubled rate in stereo"
    );
    let written = report.audio_duration();
    let coded = Duration::from_secs_f64(
        f64::from(CODED_HE_AAC_FRAMES * HE_AAC_FRAME_SAMPLES) / f64::from(OUTPUT_RATE),
    );
    assert!(
        written.abs_diff(coded) <= AUDIO_LENGTH_TOLERANCE,
        "every coded frame reaches the output: wrote {written:?}, coded {coded:?}"
    );
    assert!(report.audio_rms() > 0.01, "audio is not silent");

    // Parametric Stereo: 440 Гц слева, 1 кГц справа — из одного моно-ядра.
    let separation_440 = tone_level_db(&report, 0, 440.0) - tone_level_db(&report, 1, 440.0);
    let separation_1k = tone_level_db(&report, 1, 1_000.0) - tone_level_db(&report, 0, 1_000.0);
    assert!(
        separation_440 > MIN_CHANNEL_SEPARATION_DB,
        "440 Hz L-R: {separation_440:.1} dB"
    );
    assert!(
        separation_1k > MIN_CHANNEL_SEPARATION_DB,
        "1 kHz R-L: {separation_1k:.1} dB"
    );
}

/// Уровень тона в канале выхода (фильтр Гёрцеля, окно Ханна, секунда с 0,5 с).
fn tone_level_db(report: &PlaybackReport, lane: usize, frequency_hz: f64) -> f64 {
    let (sample_rate, channels) = report.audio_format.expect("audio output was created");
    let sample_rate = f64::from(sample_rate);
    let start = (0.5 * sample_rate) as usize;
    let length = sample_rate as usize;
    let lane_samples: Vec<f64> = report
        .audio_samples
        .iter()
        .skip(lane)
        .step_by(channels)
        .skip(start)
        .take(length)
        .map(|sample| f64::from(*sample))
        .collect();
    assert_eq!(
        lane_samples.len(),
        length,
        "output shorter than the analysis window"
    );

    let coefficient = 2.0 * (2.0 * std::f64::consts::PI * frequency_hz / sample_rate).cos();
    let (mut previous, mut before_previous) = (0.0_f64, 0.0_f64);
    for (index, sample) in lane_samples.iter().enumerate() {
        let hann = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * index as f64 / length as f64).cos();
        let current = sample * hann + coefficient * previous - before_previous;
        before_previous = previous;
        previous = current;
    }
    let power = previous * previous + before_previous * before_previous
        - coefficient * previous * before_previous;
    let amplitude = 4.0 * power.max(0.0).sqrt() / length as f64;
    20.0 * amplitude.max(1e-12).log10()
}
