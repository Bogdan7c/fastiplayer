//! Звук доходит до production `ChannelMixer`: factory → decoder → canonical PCM.

use audio_core::{AudioChannelLayout, AudioDecoderConfig, AudioDecoderError};

use super::{
    FRAMES_PER_PACKET, SAMPLE_RATE, STEREO_AC3, STEREO_TONES, SURROUND_AC3, SURROUND_EAC3,
    SURROUND_TONES, assert_lane_tones, decode_all, factory_decoder, lane_rms, packets,
    surround_side_layout, tone_energy,
};
use crate::channel_mixer::ChannelMixer;
use crate::decoder::create_audio_decoder;

/// RMS каждого канала по декоду `ffmpeg -c:a pcm_f32le` (одинаков для всех фикстур `tones-*`).
const FFMPEG_REFERENCE_RMS: f32 = 0.174_655;
/// Допуск RMS против ffmpeg (0,5 %): тон −12 dBFS, dither почти не влияет.
const FFMPEG_RMS_TOLERANCE: f32 = 0.005;

/// Каждая lane совпадает по RMS с декодом ffmpeg.
fn assert_matches_ffmpeg_rms(interleaved: &[f32], channels: usize) {
    for lane in 0..channels {
        let rms = lane_rms(interleaved, channels, lane);
        assert!(
            (rms - FFMPEG_REFERENCE_RMS).abs() < FFMPEG_REFERENCE_RMS * FFMPEG_RMS_TOLERANCE,
            "lane {lane} rms {rms} vs ffmpeg {FFMPEG_REFERENCE_RMS}"
        );
    }
}

#[test]
fn factory_routes_only_exact_ac3_identities() {
    let ac3 = factory_decoder("A_AC3", 2);
    let eac3 = factory_decoder("A_EAC3", 6);

    // До первого кадра decoder сообщает container metadata; 6 каналов без
    // позиций — это `Discrete`, позиции появятся только из битового потока.
    assert_eq!((ac3.sample_rate(), ac3.channels()), (SAMPLE_RATE, 2));
    assert_eq!(ac3.channel_layout(), Some(AudioChannelLayout::stereo()));
    assert_eq!(
        eac3.channel_layout(),
        Some(AudioChannelLayout::discrete(6).unwrap())
    );
    for lookalike in ["a_ac3", "A_AC3 ", "A_EAC3/JOC"] {
        let error = match create_audio_decoder(AudioDecoderConfig::new(1, lookalike, 48_000, 2)) {
            Ok(_) => panic!("{lookalike:?} must not reach the AC-3 adapter"),
            Err(error) => error,
        };
        assert!(matches!(
            error.downcast_ref::<AudioDecoderError>(),
            Some(AudioDecoderError::UnsupportedCodec { .. })
        ));
    }
}

#[test]
fn stereo_ac3_packets_decode_to_left_and_right_tones() {
    let mut decoder = factory_decoder("A_AC3", 2);

    let interleaved = decode_all(
        decoder.as_mut(),
        &packets(STEREO_AC3),
        AudioChannelLayout::stereo(),
    );

    assert_eq!(interleaved.len(), 16 * FRAMES_PER_PACKET * 2);
    assert_lane_tones(&interleaved, &STEREO_TONES);
    assert_matches_ffmpeg_rms(&interleaved, 2);
}

/// Проверяет 5.1 decode и production downmix в stereo.
fn assert_surround_reaches_stereo_mixer(codec_id: &str, stream: &[u8]) {
    let mut decoder = factory_decoder(codec_id, 6);
    let interleaved = decode_all(decoder.as_mut(), &packets(stream), surround_side_layout());
    assert_lane_tones(&interleaved, &SURROUND_TONES);
    assert_matches_ffmpeg_rms(&interleaved, 6);

    let spec = decoder.decoded_output_spec().unwrap();
    let mixer = ChannelMixer::new(spec.channel_layout, 2);
    let mut stereo = Vec::new();
    mixer
        .mix_interleaved_into(&interleaved, &mut stereo)
        .unwrap();

    // Левый выход: FL + FC + SL; правый: FR + FC + SR; LFE не подмешивается.
    let [fl, fr, fc, lfe, sl, sr] = SURROUND_TONES;
    for (lane, present, absent) in [(0, [fl, fc, sl], [fr, sr]), (1, [fr, fc, sr], [fl, sl])] {
        for tone in present {
            assert!(
                tone_energy(&stereo, 2, lane, tone) > 1e-3,
                "{tone} Hz missing in lane {lane}"
            );
        }
        for tone in absent.into_iter().chain([lfe]) {
            assert!(
                tone_energy(&stereo, 2, lane, tone) < 1e-6,
                "{tone} Hz leaked into lane {lane}"
            );
        }
    }
}

#[test]
fn ac3_5_1_packets_reach_canonical_layout_and_production_stereo_mixer() {
    assert_surround_reaches_stereo_mixer("A_AC3", SURROUND_AC3);
}

#[test]
fn eac3_5_1_packets_reach_canonical_layout_and_production_stereo_mixer() {
    assert_surround_reaches_stereo_mixer("A_EAC3", SURROUND_EAC3);
}

#[test]
fn one_decoder_follows_ac3_to_eac3_switch_on_the_same_track() {
    // TS-поток может сменить AC-3 на E-AC-3 на том же PID без новой дорожки:
    // один decoder выбирает синтаксис по bsid каждого access unit-а.
    let stream = [SURROUND_AC3, SURROUND_EAC3].concat();
    let packet_list = packets(&stream);
    let ac3_packet_count = packets(SURROUND_AC3).len();
    let mut decoder = factory_decoder("A_AC3", 6);

    let interleaved = decode_all(decoder.as_mut(), &packet_list, surround_side_layout());

    // Первый E-AC-3 кадр несёт overlap-хвост AC-3 — тоны меряем со второго.
    let after_switch = &interleaved[(ac3_packet_count + 1) * FRAMES_PER_PACKET * 6..];
    assert_lane_tones(after_switch, &SURROUND_TONES);
}
