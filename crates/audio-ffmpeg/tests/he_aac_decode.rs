// Интеграционный тест целиком является тестовым кодом: unwrap/expect/panic
// здесь работают как assertions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration test crate: panics are test assertions"
)]
#![cfg(feature = "ffmpeg")]

//! Настоящие файлы HE-AAC (как у Instagram и TikTok) → демуксер плеера →
//! FFmpeg → PCM. Проверяется то, что отличает HE-AAC от «глухого» ядра:
//! удвоенная частота с первого пакета, тон выше половины частоты ядра (SBR)
//! и разные тоны в левом и правом канале из моно-ядра (PS).

mod support;

use audio_ffmpeg::{AacStreamConfig, AudioFfmpegError, FfmpegAacDecoder};
use support::{
    DecodedStream, band_level_above_db, decode_all, demux_audio_packets, fixture, tone_level_db,
};

/// Насколько тон в «своём» канале должен быть громче, чем в чужом.
/// FAAD2 на тех же файлах даёт ~50 дБ; дублированное моно дало бы 0 дБ.
const MIN_CHANNEL_SEPARATION_DB: f64 = 30.0;
/// Тон, которого в сигнале нет (или который не восстановлен), — ниже этого уровня.
const ABSENT_TONE_MAX_DB: f64 = -40.0;
/// Тон, который должен быть слышен, — выше этого уровня (источник: −24 дБ на канал).
const PRESENT_TONE_MIN_DB: f64 = -35.0;
/// Половина частоты ядра v1 стерео (22,05 кГц): выше неё звук даёт только SBR.
const CORE_NYQUIST_22K_HZ: f64 = 11_025.0;
/// Половина частоты ядра v1 моно (24 кГц).
const CORE_NYQUIST_24K_HZ: f64 = 12_000.0;

#[test]
fn instagram_like_v1_stereo_restores_highs_from_the_first_packet() {
    let stream = decode_fixture("v1-stereo-hierarchical.m4a");

    stream.assert_first_output(44_100, 2);
    // Слева 440 Гц, справа 14 кГц — выше 11,025 кГц ядра: его даёт только SBR.
    // SBR восстанавливает энергию полосами (~344 Гц), а не точную частоту,
    // поэтому верх меряется энергией выше половины частоты ядра.
    let left_440 = tone_level_db(&stream, 0, 440.0);
    let right_highs = band_level_above_db(&stream, 1, CORE_NYQUIST_22K_HZ);
    assert!(
        left_440 > PRESENT_TONE_MIN_DB,
        "left 440 Hz: {left_440:.1} dB"
    );
    assert!(
        right_highs > PRESENT_TONE_MIN_DB,
        "SBR highs on the right: {right_highs:.1} dB"
    );
    let left_highs = band_level_above_db(&stream, 0, CORE_NYQUIST_22K_HZ);
    assert!(
        left_highs < ABSENT_TONE_MAX_DB,
        "highs must stay on the right: {left_highs:.1} dB"
    );
}

#[test]
fn backward_compatible_signalling_decodes_like_hierarchical() {
    let hierarchical = decode_fixture("v1-stereo-hierarchical.m4a");
    let backward_compatible = decode_fixture("v1-stereo-backward-compatible.m4a");

    backward_compatible.assert_first_output(44_100, 2);
    // Кадры одинаковы, отличается только паспорт: PCM обязан совпасть.
    assert_eq!(hierarchical.samples, backward_compatible.samples);
}

#[test]
fn instagram_like_v1_mono_24k_core_plays_at_48k_with_highs() {
    let stream = decode_fixture("v1-mono-48k-hierarchical.m4a");

    // FFmpeg выдаёт моно HE-AAC как два одинаковых канала (на случай PS).
    stream.assert_first_output(48_000, 2);
    let highs = band_level_above_db(&stream, 0, CORE_NYQUIST_24K_HZ);
    assert!(
        highs > PRESENT_TONE_MIN_DB,
        "SBR 15 kHz above the 12 kHz core: {highs:.1} dB"
    );
    assert!(tone_level_db(&stream, 0, 440.0) > PRESENT_TONE_MIN_DB);
}

#[test]
fn tiktok_like_v2_builds_stereo_from_a_mono_core() {
    assert_parametric_stereo(&decode_fixture("v2-ps-hierarchical.m4a"));
}

#[test]
fn fragmented_mp4_v2_from_a_page_decodes_like_the_file() {
    let fragmented = decode_fixture("v2-ps-hierarchical-fragmented.mp4");
    assert_parametric_stereo(&fragmented);
    assert_eq!(
        fragmented.samples,
        decode_fixture("v2-ps-hierarchical.m4a").samples
    );
}

#[test]
fn bare_adts_frames_find_sbr_inside_the_packet() {
    // Как ADTS в MPEG-TS: паспорта нет, известны только ядро 22,05 кГц и 2 канала.
    let stream = decode_bare_frames("v1-stereo-implicit.adts", 22_050, 2);

    stream.assert_first_output(44_100, 2);
    let right_highs = band_level_above_db(&stream, 1, CORE_NYQUIST_22K_HZ);
    assert!(
        right_highs > PRESENT_TONE_MIN_DB,
        "implicit SBR highs: {right_highs:.1} dB"
    );
}

#[test]
fn bare_adts_frames_find_parametric_stereo_inside_the_packet() {
    // Ядро v2 — моно: заголовок ADTS говорит «1 канал», стерео даёт только PS.
    assert_parametric_stereo(&decode_bare_frames("v2-ps-implicit.adts", 22_050, 1));
}

#[test]
fn flush_after_seek_keeps_decoding_in_the_same_format() {
    let (config, packets) = demux_audio_packets("v1-stereo-hierarchical.m4a");
    let mut decoder = FfmpegAacDecoder::open(&config).expect("decoder opens");
    let first = decode_all(&mut decoder, &packets[..5]);
    decoder.flush();
    // «Перемотка» вперёд на 10 пакетов: после flush декодер не ждёт продолжения.
    let after_seek = decode_all(&mut decoder, &packets[15..]);

    assert_eq!(
        first.output_specs,
        after_seek.output_specs[..1].repeat(first.output_specs.len())
    );
    let right_highs = band_level_above_db(&after_seek, 1, CORE_NYQUIST_22K_HZ);
    assert!(
        right_highs > PRESENT_TONE_MIN_DB,
        "after flush: {right_highs:.1} dB"
    );
}

#[test]
fn corrupt_packet_is_a_packet_error_and_the_decoder_survives() {
    let (config, packets) = demux_audio_packets("v1-stereo-hierarchical.m4a");
    let mut decoder = FfmpegAacDecoder::open(&config).expect("decoder opens");
    decode_all(&mut decoder, &packets[..5]);

    let garbage = vec![0xFF_u8; 300];
    let result = decoder.decode_packet(&garbage);
    assert!(
        matches!(result, Err(AudioFfmpegError::CorruptPacket { .. })),
        "garbage packet: {result:?}"
    );
    // Следующие настоящие пакеты снова дают звук на выходной частоте.
    let recovered = decode_all(&mut decoder, &packets[5..]);
    recovered.assert_every_output(44_100, 2);
    assert!(!recovered.samples.is_empty());
}

#[test]
fn empty_packet_is_not_sent_to_ffmpeg_as_end_of_stream() {
    let (config, packets) = demux_audio_packets("v1-stereo-hierarchical.m4a");
    let mut decoder = FfmpegAacDecoder::open(&config).expect("decoder opens");
    assert_eq!(decoder.decode_packet(&[]), Ok(None));
    // Если бы пустой пакет ушёл как «конец потока», следующие дали бы EOF.
    assert!(!decode_all(&mut decoder, &packets).samples.is_empty());
}

#[test]
fn decoder_can_move_to_the_player_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<FfmpegAacDecoder>();
    assert!(FfmpegAacDecoder::is_available());
}

fn decode_fixture(file_name: &str) -> DecodedStream {
    let (config, packets) = demux_audio_packets(file_name);
    let mut decoder = FfmpegAacDecoder::open(&config).expect("decoder opens");
    decode_all(&mut decoder, &packets)
}

fn decode_bare_frames(file_name: &str, core_sample_rate: u32, core_channels: u32) -> DecodedStream {
    let packets = support::adts_raw_frames(&fixture(file_name));
    let config = AacStreamConfig::BareFrames {
        core_sample_rate,
        core_channels,
    };
    let mut decoder = FfmpegAacDecoder::open(&config).expect("decoder opens");
    decode_all(&mut decoder, &packets)
}

/// Слева 440 Гц, справа 1 кГц: моно-ядро развести по каналам может только PS.
fn assert_parametric_stereo(stream: &DecodedStream) {
    stream.assert_first_output(44_100, 2);
    let separation_440 = tone_level_db(stream, 0, 440.0) - tone_level_db(stream, 1, 440.0);
    let separation_1k = tone_level_db(stream, 1, 1_000.0) - tone_level_db(stream, 0, 1_000.0);
    assert!(
        separation_440 > MIN_CHANNEL_SEPARATION_DB,
        "440 Hz L-R: {separation_440:.1} dB"
    );
    assert!(
        separation_1k > MIN_CHANNEL_SEPARATION_DB,
        "1 kHz R-L: {separation_1k:.1} dB"
    );
}
