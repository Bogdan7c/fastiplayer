//! Вещательная смена 2.0 → 5.1 → 2.0 в локальном MPEG-TS через production путь:
//! local registry → MPEG-TS demuxer → production decoder.
//!
//! Доказывает, что смену формата видит decoder (decoded spec каждого packet-а — его
//! настоящий формат, по нему player-core пересобирает output без потери A/V sync,
//! `player-core/src/session/audio_spec_change.rs`), а не lifecycle demuxer-а:
//! `TracksChanged` в player-core — seek-подобный полный сброс с заморозкой видео.
//!
//! Фикстура: `crates/mpeg-ts-demux/tests/fixtures/ac3/ac3-broadcast-2p0-5p1-2p0-48k.ts`
//! (склейка `audio/tests/fixtures/ac3/tones-{stereo,5.1,stereo}.ac3`, README там же).

use audio::{AudioDecoderConfig, EncodedAudioPacket, create_audio_decoder};
use media_core::{DemuxReadEvent, Packet};

use super::ac3_elementary_tests::open_local;
use crate::ac3_test_support::{
    DecodedPcm, SAMPLES_PER_ACCESS_UNIT, STEREO_TONES, SURROUND_TONES, assert_channel_tones,
    first_audio_track,
};

const BROADCAST_SWITCH_TS: &[u8] =
    include_bytes!("../../../mpeg-ts-demux/tests/fixtures/ac3/ac3-broadcast-2p0-5p1-2p0-48k.ts");
/// Кадров в каждом участке склейки.
const FRAMES_PER_SEGMENT: usize = 16;
const SAMPLE_RATE: u32 = 48_000;

/// Packet, декодированный в формате, который decoder сообщил сразу после него.
struct DecodedPacket {
    interleaved: Vec<f32>,
    sample_rate: u32,
    positions: Vec<audio::AudioChannelPosition>,
}

/// Декодирует пакеты по одному и снимает decoded spec после каждого.
fn decode_packet_by_packet(
    track: &media_core::TrackInfo,
    packets: &[Packet],
) -> Vec<DecodedPacket> {
    let config = AudioDecoderConfig::from_track_metadata(
        track.id.get(),
        track.codec_id.clone(),
        track.sample_rate,
        track.channels,
    );
    let mut decoder = create_audio_decoder(config).expect("production AC-3 decoder");
    packets
        .iter()
        .map(|packet| {
            let interleaved = decoder
                .decode(&EncodedAudioPacket::without_timing(
                    packet.track_id.get(),
                    &packet.data,
                ))
                .expect("decode packet");
            let spec = decoder.decoded_output_spec().expect("decoded spec");
            let layout = spec.channel_layout;
            DecodedPacket {
                interleaved,
                sample_rate: spec.sample_rate,
                positions: (0..layout.channel_count() as usize)
                    .map(|lane| layout.position_at(lane).expect("positioned layout"))
                    .collect(),
            }
        })
        .collect()
}

/// Склеивает PCM одного участка (все packet-ы в одном формате).
fn segment_pcm(decoded: &[DecodedPacket]) -> DecodedPcm {
    DecodedPcm {
        interleaved: decoded
            .iter()
            .flat_map(|packet| packet.interleaved.iter().copied())
            .collect(),
        sample_rate: decoded[0].sample_rate,
        positions: decoded[0].positions.clone(),
    }
}

#[test]
fn local_ts_broadcast_channel_switch_reaches_decoder_without_track_reset() {
    let mut demuxer = open_local("broadcast-switch.ts", BROADCAST_SWITCH_TS);
    let track = first_audio_track(demuxer.as_ref());
    assert_eq!(track.codec_id, "A_AC3");
    assert_eq!(
        track.channels,
        Some(2),
        "track keeps the first frame format"
    );

    let mut packets = Vec::new();
    let mut track_list_updates = 0;
    loop {
        match demuxer.next_event().expect("demux event") {
            DemuxReadEvent::Packet(packet) if packet.track_id == track.id => packets.push(packet),
            DemuxReadEvent::TracksChanged(_) => track_list_updates += 1,
            DemuxReadEvent::EndOfStream => break,
            _ => {}
        }
    }
    assert_eq!(
        track_list_updates, 0,
        "the format switch must not reach the player as a track reset"
    );
    assert_eq!(packets.len(), 3 * FRAMES_PER_SEGMENT);

    let decoded = decode_packet_by_packet(&track, &packets);
    let channel_counts: Vec<usize> = decoded
        .iter()
        .map(|packet| packet.positions.len())
        .collect();
    let expected_counts: Vec<usize> = [2, 6, 2]
        .into_iter()
        .flat_map(|count| std::iter::repeat_n(count, FRAMES_PER_SEGMENT))
        .collect();
    assert_eq!(
        channel_counts, expected_counts,
        "decoded spec follows each frame: this is what drives the output rebuild"
    );
    for packet in &decoded {
        assert_eq!(packet.sample_rate, SAMPLE_RATE);
        assert_eq!(
            packet.interleaved.len(),
            SAMPLES_PER_ACCESS_UNIT * packet.positions.len(),
            "every packet yields one full frame in its own layout"
        );
    }

    let (stereo_before, rest) = decoded.split_at(FRAMES_PER_SEGMENT);
    let (surround, stereo_after) = rest.split_at(FRAMES_PER_SEGMENT);
    assert_channel_tones(&segment_pcm(stereo_before), &STEREO_TONES);
    assert_channel_tones(&segment_pcm(surround), &SURROUND_TONES);
    assert_channel_tones(&segment_pcm(stereo_after), &STEREO_TONES);
}
