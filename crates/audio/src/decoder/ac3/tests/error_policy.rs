//! Политика ошибок adapter-а: битый packet, постоянная причина, reset, смена формата.

use ac3_decoder::{ChannelPosition, UnsupportedFeature};
use audio_core::{AudioChannelLayout, AudioDecoder, AudioOutputSpec, EncodedAudioPacket};

use super::super::persistent_failure::PersistentFailure;
use super::super::{Ac3CodecIdentity, Ac3DecodeError, PacketOutcome};
use super::{
    FRAMES_PER_PACKET, SAMPLE_RATE, STEREO_AC3, STEREO_TONES, SURROUND_AC3, SURROUND_TONES,
    TRACK_ID, assert_lane_tones, concrete_decoder, corrupted, decode_all, decode_packet,
    factory_decoder, lane_rms, packets, surround_side_layout,
};

/// Stereo AC-3 44,1 кГц из фикстур `ac3-decoder` (128k, без coupling).
const STEREO_44K1_AC3: &[u8] =
    include_bytes!("../../../../../ac3-decoder/tests/fixtures/stereo-44k1-128k-plain.ac3");

/// Раскладка, которую тесты используют как «неконвертируемую» постоянную причину.
fn unconvertible_failure() -> PersistentFailure {
    PersistentFailure::UnconvertibleLayout(vec![
        ChannelPosition::Left,
        ChannelPosition::Right,
        ChannelPosition::LeftSurroundDirect,
    ])
}

#[test]
fn corrupted_packet_is_typed_error_and_becomes_silence_of_same_duration() {
    let packet_list = packets(STEREO_AC3);
    let mut decoder = concrete_decoder(Ac3CodecIdentity::Ac3, 2);
    assert!(matches!(
        decoder.decode_access_unit(packet_list[0]),
        PacketOutcome::Decoded(_)
    ));

    let concealed = decode_packet(&mut decoder, &corrupted(packet_list[1]));
    assert_eq!(concealed, vec![0.0; FRAMES_PER_PACKET * 2]);
    assert_eq!(
        decoder.decoded_output_spec().unwrap().channel_layout,
        AudioChannelLayout::stereo()
    );

    // Обрезка packet-а — тоже битый packet, а не ошибка `decode`.
    let truncated = &packet_list[2][..packet_list[2].len() / 3];
    assert_eq!(
        decoder.decode_access_unit(truncated),
        PacketOutcome::Concealed(vec![0.0; FRAMES_PER_PACKET * 2])
    );
    let recovered: Vec<f32> = packet_list[3..]
        .iter()
        .flat_map(|packet| decode_packet(&mut decoder, packet))
        .collect();
    assert_lane_tones(&recovered, &STEREO_TONES);
}

#[test]
fn single_flipped_bit_fails_crc_and_is_concealed_not_played_as_noise() {
    let packet_list = packets(STEREO_AC3);
    let mut decoder = concrete_decoder(Ac3CodecIdentity::Ac3, 2);
    decode_packet(&mut decoder, packet_list[0]);
    let mut flipped = packet_list[1].to_vec();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 0x10;

    assert_eq!(
        decoder.decode_access_unit(&flipped),
        PacketOutcome::Concealed(vec![0.0; FRAMES_PER_PACKET * 2])
    );
}

#[test]
fn corrupted_packet_before_first_frame_publishes_nothing() {
    let mut decoder = concrete_decoder(Ac3CodecIdentity::Ac3, 2);

    let outcome = decoder.decode_access_unit(b"\x0b\x77garbage");

    assert_eq!(outcome, PacketOutcome::DroppedBeforeFirstFrame);
    assert_eq!(decoder.frame_format.published(), None);
    assert!(decode_packet(&mut decoder, b"not ac3").is_empty());
    // До первого кадра формат — только container metadata, а не догадка по мусору.
    assert_eq!(decoder.frame_format.published(), None);
    assert_eq!(decoder.channel_layout(), Some(AudioChannelLayout::stereo()));
}

#[test]
fn packet_of_another_track_is_rejected_without_decoding() {
    let mut decoder = concrete_decoder(Ac3CodecIdentity::Ac3, 2);

    let result = decoder.decode(&EncodedAudioPacket::without_timing(
        TRACK_ID + 1,
        packets(STEREO_AC3)[0],
    ));

    assert!(result.is_err());
    assert_eq!(decoder.frame_format.published(), None);
}

#[test]
fn persistent_failure_is_reported_once_then_becomes_silence_until_a_frame_decodes() {
    let packet_list = packets(STEREO_AC3);
    let mut decoder = concrete_decoder(Ac3CodecIdentity::Ac3, 2);
    decode_packet(&mut decoder, packet_list[0]);
    let published_before = decoder.frame_format.published();

    let first = decoder.handle_persistent_failure(unconvertible_failure());
    let repeated = decoder.handle_persistent_failure(unconvertible_failure());

    assert!(matches!(
        first,
        PacketOutcome::Rejected(Ac3DecodeError::UnconvertibleChannelLayout { .. })
    ));
    assert_eq!(
        repeated,
        PacketOutcome::Concealed(vec![0.0; FRAMES_PER_PACKET * 2])
    );
    // Постоянная причина не трогает опубликованный формат последнего кадра.
    assert_eq!(decoder.frame_format.published(), published_before);

    // Другая причина — новое сообщение.
    let other = decoder.handle_persistent_failure(PersistentFailure::UnsupportedFeature(
        UnsupportedFeature::EnhancedAc3,
    ));
    assert!(matches!(
        other,
        PacketOutcome::Rejected(Ac3DecodeError::UnsupportedStream {
            codec_id: "A_AC3",
            feature: UnsupportedFeature::EnhancedAc3,
        })
    ));

    // Удачный кадр снимает состояние: вернувшаяся причина снова сообщается.
    decode_packet(&mut decoder, packet_list[1]);
    assert!(matches!(
        decoder.handle_persistent_failure(unconvertible_failure()),
        PacketOutcome::Rejected(_)
    ));
}

#[test]
fn persistent_failure_before_first_frame_reports_once_and_then_emits_nothing() {
    let mut concrete = concrete_decoder(Ac3CodecIdentity::Ac3, 2);

    let first = concrete.handle_persistent_failure(unconvertible_failure());
    let repeated = concrete.handle_persistent_failure(unconvertible_failure());

    assert!(matches!(first, PacketOutcome::Rejected(_)));
    assert_eq!(repeated, PacketOutcome::DroppedBeforeFirstFrame);
    assert_eq!(concrete.frame_format.published(), None);
}

#[test]
fn reset_restarts_overlap_but_keeps_published_format() {
    let packet_list = packets(SURROUND_AC3);
    let mut fresh = factory_decoder("A_AC3", 6);
    let expected = decode_packet(fresh.as_mut(), packet_list[5]);

    let mut reused = factory_decoder("A_AC3", 6);
    decode_all(reused.as_mut(), &packet_list[..5], surround_side_layout());
    let continued = decode_packet(reused.as_mut(), packet_list[5]);
    reused.reset().unwrap();
    let spec_after_reset = reused.decoded_output_spec();
    let after_reset = decode_packet(reused.as_mut(), packet_list[5]);

    assert_ne!(
        continued, expected,
        "without reset overlap-add carries the previous frame"
    );
    // После reset декодер детерминированно совпадает со свежим (dither — с зерна).
    assert_eq!(after_reset, expected);
    assert_eq!(
        spec_after_reset.unwrap().channel_layout,
        surround_side_layout()
    );
}

/// Декодирует поток пакет за пакетом и возвращает spec каждого непустого buffer-а.
fn decode_with_specs(
    decoder: &mut dyn AudioDecoder,
    packet_list: &[&[u8]],
) -> Vec<(AudioOutputSpec, Vec<f32>)> {
    packet_list
        .iter()
        .map(|packet_bytes| {
            let samples = decode_packet(decoder, packet_bytes);
            (decoder.decoded_output_spec().unwrap(), samples)
        })
        .collect()
}

#[test]
fn broadcast_switch_2_0_to_5_1_publishes_real_5_1_spec_after_switch() {
    let stream = [STEREO_AC3, SURROUND_AC3].concat();
    let packet_list = packets(&stream);
    let stereo_packet_count = packets(STEREO_AC3).len();
    let mut decoder = factory_decoder("A_AC3", 2);

    let decoded = decode_with_specs(decoder.as_mut(), &packet_list);

    let (before_switch, after_switch) = decoded.split_at(stereo_packet_count);
    for (spec, samples) in before_switch {
        assert_eq!(spec.channel_layout, AudioChannelLayout::stereo());
        assert_eq!(samples.len(), FRAMES_PER_PACKET * 2);
    }
    for (spec, samples) in after_switch {
        assert_eq!(spec.sample_rate, SAMPLE_RATE);
        assert_eq!(spec.channel_layout, surround_side_layout());
        assert_eq!(samples.len(), FRAMES_PER_PACKET * 6);
    }
    let surround: Vec<f32> = after_switch[1..]
        .iter()
        .flat_map(|(_, samples)| samples.iter().copied())
        .collect();
    assert_lane_tones(&surround, &SURROUND_TONES);
}

#[test]
fn broadcast_switch_5_1_to_2_0_publishes_stereo_again() {
    let stream = [SURROUND_AC3, STEREO_AC3].concat();
    let packet_list = packets(&stream);
    let surround_packet_count = packets(SURROUND_AC3).len();
    let mut decoder = factory_decoder("A_AC3", 6);

    let decoded = decode_with_specs(decoder.as_mut(), &packet_list);

    let (before_switch, after_switch) = decoded.split_at(surround_packet_count);
    assert!(
        before_switch
            .iter()
            .all(|(spec, _)| spec.channel_layout == surround_side_layout())
    );
    assert!(after_switch.iter().all(|(spec, samples)| {
        spec.channel_layout == AudioChannelLayout::stereo()
            && samples.len() == FRAMES_PER_PACKET * 2
    }));
    let stereo: Vec<f32> = after_switch[1..]
        .iter()
        .flat_map(|(_, samples)| samples.iter().copied())
        .collect();
    assert_lane_tones(&stereo, &STEREO_TONES);
}

#[test]
fn corrupted_packet_after_switch_is_silence_in_the_new_format() {
    let stream = [STEREO_AC3, SURROUND_AC3].concat();
    let packet_list = packets(&stream);
    let first_surround = packets(STEREO_AC3).len();
    let mut decoder = factory_decoder("A_AC3", 2);
    decode_with_specs(decoder.as_mut(), &packet_list[..=first_surround]);

    let concealed = decode_packet(
        decoder.as_mut(),
        &corrupted(packet_list[first_surround + 1]),
    );

    assert_eq!(concealed, vec![0.0; FRAMES_PER_PACKET * 6]);
    assert_eq!(
        decoder.decoded_output_spec().unwrap().channel_layout,
        surround_side_layout()
    );
}

#[test]
fn sample_rate_change_mid_stream_publishes_each_frame_rate() {
    // Настоящие кадры 44,1 кГц после 48 кГц: adapter не приводит их к первой частоте.
    let stream = [STEREO_AC3, STEREO_44K1_AC3].concat();
    let packet_list = packets(&stream);
    let first_44k1 = packets(STEREO_AC3).len();
    let mut decoder = factory_decoder("A_AC3", 2);

    let decoded = decode_with_specs(decoder.as_mut(), &packet_list);

    assert!(
        decoded[..first_44k1]
            .iter()
            .all(|(spec, _)| spec.sample_rate == SAMPLE_RATE)
    );
    for (spec, samples) in &decoded[first_44k1..] {
        assert_eq!(
            *spec,
            AudioOutputSpec::new(44_100, AudioChannelLayout::stereo())
        );
        assert_eq!(samples.len(), FRAMES_PER_PACKET * 2);
    }
    // После смены частоты звучит уже новая программа, а не тишина.
    let after_change: Vec<f32> = decoded[first_44k1 + 1..]
        .iter()
        .flat_map(|(_, samples)| samples.iter().copied())
        .collect();
    assert!(lane_rms(&after_change, 2, 0) > 0.01 && lane_rms(&after_change, 2, 1) > 0.01);
}
