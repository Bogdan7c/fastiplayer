//! Сквозной путь локального AC-3 / E-AC-3 файла: файл → production local registry
//! (выбор factory по содержимому) → demux → production `audio::create_audio_decoder`
//! → PCM с нужным тоном в каждом канале.
//!
//! Сырые фикстуры — `crates/audio/tests/fixtures/ac3/` (в каждом канале свой синус,
//! поэтому энергия тонов по lanes доказывает и декодирование, и раскладку каналов);
//! MPEG-TS — `crates/mpeg-ts-demux/tests/fixtures/ac3/` (H.264 + синус 440 Гц, README там же).

use std::path::Path;
use std::time::Duration;

use audio::AudioChannelPosition;
use fastiplayer_config::PlayerDemuxConfig;
use media_core::{Demuxer, MediaTime, TrackKind, TrackLanguage};
use source_core::{CancellationToken, LocalFileSource};

use super::{AttachmentReadLimits, open_local_demuxer_from_source};
use crate::ac3_test_support::{
    DecodedPcm, SAMPLES_PER_ACCESS_UNIT, STEREO_AC3, STEREO_TONES, SURROUND_EAC3, SURROUND_TONES,
    assert_channel_tones, assert_tones_in_lanes, collect_track_packets, decode_track_packets,
    first_audio_track,
};

/// ATSC AC-3 stereo 48 кГц + H.264 (16 audio packets, 440 Гц в обоих каналах).
const H264_AC3_STEREO_TS: &[u8] =
    include_bytes!("../../../mpeg-ts-demux/tests/fixtures/ac3/h264-ac3-stereo-48k.ts");
/// ATSC E-AC-3 5.1 44.1 кГц + H.264 (15 audio packets, 440 Гц во всех каналах).
const H264_EAC3_SURROUND_TS: &[u8] =
    include_bytes!("../../../mpeg-ts-demux/tests/fixtures/ac3/h264-eac3-5p1-44k1.ts");
/// DVB (System B) E-AC-3 stereo 32 кГц, `enhanced_AC-3_descriptor` + язык eng (11 packets).
const DVB_EAC3_STEREO_TS: &[u8] =
    include_bytes!("../../../mpeg-ts-demux/tests/fixtures/ac3/audio-only-eac3-dvb-stereo-32k.ts");

const SAMPLE_RATE: u32 = 48_000;
const FIXTURE_FRAMES: usize = 16;
/// Тон всех каналов TS-фикстур.
const TS_TONE_HZ: f32 = 440.0;

/// Пишет байты во временный файл и открывает его тем же путём, что приложение.
pub(super) fn open_local(file_name: &str, bytes: &[u8]) -> Box<dyn Demuxer + Send> {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join(file_name);
    std::fs::write(&path, bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open local file");
    let extension = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str());
    open_local_demuxer_from_source(
        source,
        extension,
        &PlayerDemuxConfig::default(),
        AttachmentReadLimits::default(),
        CancellationToken::never_cancelled(),
    )
    .expect("production local registry opens Dolby media")
}

/// Файл без известного расширения: E-AC-3 узнаётся по содержимому и играет 5.1.
#[test]
fn extensionless_eac3_file_is_selected_by_content_and_decodes_every_channel() {
    let mut demuxer = open_local("surround-capture.bin", SURROUND_EAC3);
    let track = first_audio_track(demuxer.as_ref());
    assert_eq!(track.codec_id, "A_EAC3");
    assert_eq!(
        (track.sample_rate, track.channels),
        (Some(SAMPLE_RATE), Some(6))
    );
    assert_eq!(
        demuxer.duration(),
        Some(Duration::from_millis(512)),
        "16 access units × 32 ms"
    );

    let packets = collect_track_packets(demuxer.as_mut(), &track);
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    let decoded = decode_track_packets(&track, &packets);
    assert_eq!(decoded.sample_rate, SAMPLE_RATE);
    assert_eq!(
        decoded.interleaved.len(),
        FIXTURE_FRAMES * SAMPLES_PER_ACCESS_UNIT * 6
    );
    assert_channel_tones(&decoded, &SURROUND_TONES);
}

/// `.ac3` с ID3v2 впереди: Symphonia узнал бы в ID3 «MP3», но точное
/// доказательство AC-3 выигрывает; звук доходит до PCM по обоим каналам.
#[test]
fn ac3_file_with_id3_tag_wins_over_mp3_guess_and_decodes_stereo() {
    let mut bytes = b"ID3\x04\x00\x00\x00\x00\x00\x40".to_vec();
    bytes.extend(std::iter::repeat_n(0_u8, 0x40));
    bytes.extend_from_slice(STEREO_AC3);
    let mut demuxer = open_local("tagged.ac3", &bytes);
    let track = first_audio_track(demuxer.as_ref());
    assert_eq!(track.codec_id, "A_AC3");

    let packets = collect_track_packets(demuxer.as_mut(), &track);
    assert_eq!(packets.len(), FIXTURE_FRAMES);
    assert_channel_tones(&decode_track_packets(&track, &packets), &STEREO_TONES);
}

/// Seek в середину через production путь: посадка на кадр 7, звук после seek-а цел.
#[test]
fn seek_to_middle_then_decode_keeps_tones() {
    let mut demuxer = open_local("tones.eac3", SURROUND_EAC3);
    let track = first_audio_track(demuxer.as_ref());
    let result = demuxer.seek(Duration::from_millis(250)).expect("seek");
    assert_eq!(
        result.actual_position,
        MediaTime::from_duration(Duration::from_millis(224))
    );
    let packets = collect_track_packets(demuxer.as_mut(), &track);
    assert_eq!(packets.len(), FIXTURE_FRAMES - 7);
    assert_eq!(packets[0].pts, Duration::from_millis(224));
    let decoded = decode_track_packets(&track, &packets);
    assert_eq!(
        decoded.interleaved.len(),
        (FIXTURE_FRAMES - 7) * SAMPLES_PER_ACCESS_UNIT * 6
    );
    assert_channel_tones(&decoded, &SURROUND_TONES);
}

/// Порог «тишины» lane-а TS-фикстуры (RMS): ffmpeg кладёт моно-синус только в
/// центр при `-ac 6`, остальные каналы — цифровой ноль плюс шум квантования.
const SILENT_LANE_MAX_RMS: f32 = 1e-3;

/// Какие позиции TS-фикстуры несут 440 Гц (README `mpeg-ts-demux/tests/fixtures/ac3`).
#[derive(Clone, Copy)]
enum TsToneLanes {
    /// Моно-синус, размноженный в оба канала stereo.
    EveryLane,
    /// Моно-синус при `-ac 6` попадает только в FC.
    FrontCenterOnly,
}

/// Ожидаемое содержимое TS-фикстуры.
struct TsExpectation {
    codec_id: &'static str,
    sample_rate: u32,
    channels: u32,
    packet_count: usize,
    tone_lanes: TsToneLanes,
}

fn lane_rms(decoded: &DecodedPcm, lane: usize) -> f32 {
    let channels = decoded.positions.len();
    let (sum, count) = decoded
        .interleaved
        .iter()
        .skip(lane)
        .step_by(channels)
        .fold((0.0_f32, 0_usize), |(sum, count), sample| {
            (sum + sample * sample, count + 1)
        });
    (sum / count.max(1) as f32).sqrt()
}

/// MPEG-TS с видео и Dolby звуком: production local registry выбирает MPEG-TS owner-а,
/// каждый syncframe доходит до PCM, тон — ровно в тех lanes, где его записал ffmpeg.
fn assert_local_ts_playback(
    file_name: &str,
    bytes: &[u8],
    expected: &TsExpectation,
) -> media_core::TrackInfo {
    let mut demuxer = open_local(file_name, bytes);
    let track = first_audio_track(demuxer.as_ref());
    assert_eq!(track.codec_id, expected.codec_id, "{file_name}");
    assert_eq!(
        (track.sample_rate, track.channels),
        (Some(expected.sample_rate), Some(expected.channels)),
        "{file_name}"
    );
    let packets = collect_track_packets(demuxer.as_mut(), &track);
    assert_eq!(
        packets.len(),
        expected.packet_count,
        "{file_name}: every syncframe"
    );
    let decoded = decode_track_packets(&track, &packets);
    assert_eq!(decoded.sample_rate, expected.sample_rate);
    assert_eq!(
        decoded.interleaved.len(),
        expected.packet_count * SAMPLES_PER_ACCESS_UNIT * expected.channels as usize
    );
    let toned: Vec<(AudioChannelPosition, f32)> = decoded
        .positions
        .iter()
        .filter(|position| match expected.tone_lanes {
            TsToneLanes::EveryLane => true,
            TsToneLanes::FrontCenterOnly => **position == AudioChannelPosition::FrontCenter,
        })
        .map(|position| (*position, TS_TONE_HZ))
        .collect();
    assert!(
        !toned.is_empty(),
        "{file_name}: layout {:?}",
        decoded.positions
    );
    for (lane, position) in decoded.positions.iter().enumerate() {
        if toned
            .iter()
            .all(|(toned_position, _)| toned_position != position)
        {
            let rms = lane_rms(&decoded, lane);
            assert!(
                rms < SILENT_LANE_MAX_RMS,
                "{file_name}: {position:?} must be silent, rms={rms}"
            );
        }
    }
    assert_tones_in_lanes(&decoded, &toned);
    track
}

#[test]
fn local_ts_with_h264_and_ac3_stereo_reaches_pcm() {
    let demuxer = open_local("broadcast.ts", H264_AC3_STEREO_TS);
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video),
        "H.264 video stays next to AC-3"
    );
    assert_local_ts_playback(
        "broadcast.ts",
        H264_AC3_STEREO_TS,
        &TsExpectation {
            codec_id: "A_AC3",
            sample_rate: SAMPLE_RATE,
            channels: 2,
            packet_count: 16,
            tone_lanes: TsToneLanes::EveryLane,
        },
    );
}

#[test]
fn local_ts_with_h264_and_eac3_surround_reaches_pcm_in_every_lane() {
    assert_local_ts_playback(
        "broadcast-5.1.m2ts",
        H264_EAC3_SURROUND_TS,
        &TsExpectation {
            codec_id: "A_EAC3",
            sample_rate: 44_100,
            channels: 6,
            packet_count: 15,
            tone_lanes: TsToneLanes::FrontCenterOnly,
        },
    );
}

/// DVB-сигнализация (`enhanced_AC-3_descriptor`): язык из `ISO_639_language_descriptor`.
#[test]
fn local_dvb_ts_eac3_keeps_language_and_reaches_pcm() {
    let track = assert_local_ts_playback(
        "dvb.ts",
        DVB_EAC3_STEREO_TS,
        &TsExpectation {
            codec_id: "A_EAC3",
            sample_rate: 32_000,
            channels: 2,
            packet_count: 11,
            tone_lanes: TsToneLanes::EveryLane,
        },
    );
    assert_eq!(track.labels.language, Some(TrackLanguage::new("eng")));
}
