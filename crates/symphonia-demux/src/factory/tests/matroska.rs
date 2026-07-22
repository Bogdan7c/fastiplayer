//! Hermetic S28B proof для Matroska/WebM без runtime encoder-а и второго parser-а.

use std::num::NonZeroUsize;
use std::time::Duration;

use bytes::Bytes;
use demux_api::{DemuxHints, DemuxInput, DemuxRegistry, DemuxSniffBudget, OrderedSegmentKind};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, Packet, TrackKind};
use source_core::{CancellationToken, LocalFileSource};

use super::ordered_segments::{open_ordered, segment};
use super::{SymphoniaDemuxFactory, TemporaryMediaFile};
use crate::{DemuxError, DemuxerOptions};

/// EBML element IDs, которые нужны ровно для generated proof corpus-а.
mod id {
    pub const EBML: &[u8] = &[0x1a, 0x45, 0xdf, 0xa3];
    pub const EBML_VERSION: &[u8] = &[0x42, 0x86];
    pub const EBML_READ_VERSION: &[u8] = &[0x42, 0xf7];
    pub const EBML_MAX_ID_LENGTH: &[u8] = &[0x42, 0xf2];
    pub const EBML_MAX_SIZE_LENGTH: &[u8] = &[0x42, 0xf3];
    pub const DOC_TYPE: &[u8] = &[0x42, 0x82];
    pub const DOC_TYPE_VERSION: &[u8] = &[0x42, 0x87];
    pub const DOC_TYPE_READ_VERSION: &[u8] = &[0x42, 0x85];
    pub const SEGMENT: &[u8] = &[0x18, 0x53, 0x80, 0x67];
    pub const INFO: &[u8] = &[0x15, 0x49, 0xa9, 0x66];
    pub const TIMESTAMP_SCALE: &[u8] = &[0x2a, 0xd7, 0xb1];
    pub const DURATION: &[u8] = &[0x44, 0x89];
    pub const MUXING_APP: &[u8] = &[0x4d, 0x80];
    pub const WRITING_APP: &[u8] = &[0x57, 0x41];
    pub const TRACKS: &[u8] = &[0x16, 0x54, 0xae, 0x6b];
    pub const TRACK_ENTRY: &[u8] = &[0xae];
    pub const TRACK_NUMBER: &[u8] = &[0xd7];
    pub const TRACK_UID: &[u8] = &[0x73, 0xc5];
    pub const TRACK_TYPE: &[u8] = &[0x83];
    pub const DEFAULT_DURATION: &[u8] = &[0x23, 0xe3, 0x83];
    pub const CODEC_ID: &[u8] = &[0x86];
    pub const CODEC_PRIVATE: &[u8] = &[0x63, 0xa2];
    pub const VIDEO: &[u8] = &[0xe0];
    pub const PIXEL_WIDTH: &[u8] = &[0xb0];
    pub const PIXEL_HEIGHT: &[u8] = &[0xba];
    pub const AUDIO: &[u8] = &[0xe1];
    pub const SAMPLING_FREQUENCY: &[u8] = &[0xb5];
    pub const CHANNELS: &[u8] = &[0x9f];
    pub const CUES: &[u8] = &[0x1c, 0x53, 0xbb, 0x6b];
    pub const CUE_POINT: &[u8] = &[0xbb];
    pub const CUE_TIME: &[u8] = &[0xb3];
    pub const CUE_TRACK_POSITIONS: &[u8] = &[0xb7];
    pub const CUE_TRACK: &[u8] = &[0xf7];
    pub const CUE_CLUSTER_POSITION: &[u8] = &[0xf1];
    pub const CLUSTER: &[u8] = &[0x1f, 0x43, 0xb6, 0x75];
    pub const TIMESTAMP: &[u8] = &[0xe7];
    pub const SIMPLE_BLOCK: &[u8] = &[0xa3];
    pub const BLOCK_GROUP: &[u8] = &[0xa0];
    pub const BLOCK: &[u8] = &[0xa1];
    pub const CODEC_STATE: &[u8] = &[0xa4];
}

/// Видео codec строки target compatibility profile.
#[derive(Clone, Copy)]
enum VideoCodecFixture {
    Vp8,
    Vp9,
    Av1,
}

impl VideoCodecFixture {
    /// Возвращает Matroska CodecID без локальной нормализации.
    const fn codec_id(self) -> &'static str {
        match self {
            Self::Vp8 => "V_VP8",
            Self::Vp9 => "V_VP9",
            Self::Av1 => "V_AV1",
        }
    }

    /// VP9/AV1 получают initial CodecPrivate, чтобы proof видел его замену.
    fn initial_codec_private(self) -> Option<&'static [u8]> {
        match self {
            Self::Vp8 => None,
            Self::Vp9 | Self::Av1 => Some(&[0x11, 0x12]),
        }
    }
}

/// Независимые init/media bytes позволяют проверить local и ordered paths одним corpus-ом.
struct MatroskaFixture {
    init: Vec<u8>,
    media: Vec<Vec<u8>>,
}

impl MatroskaFixture {
    /// Собирает ровно те bytes, которые local source видит как один файл.
    fn whole(&self) -> Vec<u8> {
        let mut bytes = self.init.clone();
        for media_segment in &self.media {
            bytes.extend_from_slice(media_segment);
        }
        bytes
    }
}

/// Кодирует EBML size VINT до двух байт; generated corpus остаётся заведомо bounded.
fn encode_size(length: usize) -> Vec<u8> {
    if length <= 126 {
        vec![0x80 | length as u8]
    } else {
        assert!(
            length <= 16_382,
            "test element превышает bounded two-byte VINT"
        );
        vec![0x40 | ((length >> 8) as u8), length as u8]
    }
}

/// Кодирует один EBML element с известным размером payload-а.
fn element(element_id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(element_id.len() + 2 + payload.len());
    bytes.extend_from_slice(element_id);
    bytes.extend_from_slice(&encode_size(payload.len()));
    bytes.extend_from_slice(payload);
    bytes
}

/// Кодирует master element из уже готовых children.
fn master(element_id: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
    let payload = children.concat();
    element(element_id, &payload)
}

/// Кодирует unsigned integer минимальной ненулевой ширины.
fn unsigned(element_id: &[u8], value: u64) -> Vec<u8> {
    let encoded = value.to_be_bytes();
    let first = encoded
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(encoded.len() - 1);
    element(element_id, &encoded[first..])
}

/// Кодирует fixed-width u64 для стабильного размера Cues при расчёте offsets.
fn unsigned_u64(element_id: &[u8], value: u64) -> Vec<u8> {
    element(element_id, &value.to_be_bytes())
}

/// Кодирует UTF-8/ASCII значение Matroska string element-а.
fn string(element_id: &[u8], value: &str) -> Vec<u8> {
    element(element_id, value.as_bytes())
}

/// Формирует обязательный EBML header с явным WebM DocType.
fn ebml_header() -> Vec<u8> {
    master(
        id::EBML,
        &[
            unsigned(id::EBML_VERSION, 1),
            unsigned(id::EBML_READ_VERSION, 1),
            unsigned(id::EBML_MAX_ID_LENGTH, 4),
            unsigned(id::EBML_MAX_SIZE_LENGTH, 8),
            string(id::DOC_TYPE, "webm"),
            unsigned(id::DOC_TYPE_VERSION, 4),
            unsigned(id::DOC_TYPE_READ_VERSION, 2),
        ],
    )
}

/// Формирует Segment header с unknown size: media rows законно продолжают этот master.
fn unknown_size_segment_header() -> Vec<u8> {
    let mut bytes = id::SEGMENT.to_vec();
    bytes.push(0xff);
    bytes
}

/// Duration задаётся в Segment ticks; TimestampScale делает один tick равным миллисекунде.
fn info() -> Vec<u8> {
    master(
        id::INFO,
        &[
            unsigned(id::TIMESTAMP_SCALE, 1_000_000),
            element(id::DURATION, &1_200_f64.to_be_bytes()),
            string(id::MUXING_APP, "rustiplayer-s28b"),
            string(id::WRITING_APP, "rustiplayer-s28b"),
        ],
    )
}

/// Формирует video TrackEntry с 100 ms DefaultDuration для точного lacing timing proof-а.
fn video_track(codec: VideoCodecFixture) -> Vec<u8> {
    let mut children = vec![
        unsigned(id::TRACK_NUMBER, 1),
        unsigned(id::TRACK_UID, 1),
        unsigned(id::TRACK_TYPE, 1),
        unsigned(id::DEFAULT_DURATION, 100_000_000),
        string(id::CODEC_ID, codec.codec_id()),
    ];
    if let Some(codec_private) = codec.initial_codec_private() {
        children.push(element(id::CODEC_PRIVATE, codec_private));
    }
    children.push(master(
        id::VIDEO,
        &[
            unsigned(id::PIXEL_WIDTH, 16),
            unsigned(id::PIXEL_HEIGHT, 16),
        ],
    ));
    master(id::TRACK_ENTRY, &children)
}

/// Valid OpusHead доказывает Opus track mapping без обращения к decoder-у.
fn opus_track() -> Vec<u8> {
    let opus_head = [
        b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 2, 0, 0, 0x80, 0xbb, 0, 0, 0, 0, 0,
    ];
    audio_track(2, "A_OPUS", &opus_head)
}

/// Минимальные identification/comment/setup packets в Matroska Xiph CodecPrivate layout.
fn vorbis_track() -> Vec<u8> {
    audio_track(3, "A_VORBIS", &[2, 1, 1, 1, 3, 5])
}

/// Общий audio TrackEntry сохраняет sample-rate/channel evidence рядом с codec identity.
fn audio_track(track_number: u64, codec_id: &str, codec_private: &[u8]) -> Vec<u8> {
    master(
        id::TRACK_ENTRY,
        &[
            unsigned(id::TRACK_NUMBER, track_number),
            unsigned(id::TRACK_UID, track_number),
            unsigned(id::TRACK_TYPE, 2),
            string(id::CODEC_ID, codec_id),
            element(id::CODEC_PRIVATE, codec_private),
            master(
                id::AUDIO,
                &[
                    element(id::SAMPLING_FREQUENCY, &48_000_f64.to_be_bytes()),
                    unsigned(id::CHANNELS, 2),
                ],
            ),
        ],
    )
}

/// Cues с fixed-width positions не меняют собственный размер после расчёта offsets.
fn cues(first_cluster_position: u64, second_cluster_position: u64) -> Vec<u8> {
    master(
        id::CUES,
        &[
            cue_point(0, first_cluster_position),
            cue_point(500, second_cluster_position),
        ],
    )
}

/// Один video cue указывает на начало соответствующего Cluster относительно Segment payload-а.
fn cue_point(timestamp: u64, cluster_position: u64) -> Vec<u8> {
    master(
        id::CUE_POINT,
        &[
            unsigned(id::CUE_TIME, timestamp),
            master(
                id::CUE_TRACK_POSITIONS,
                &[
                    unsigned(id::CUE_TRACK, 1),
                    unsigned_u64(id::CUE_CLUSTER_POSITION, cluster_position),
                ],
            ),
        ],
    )
}

/// Базовый Block header: track 1, signed relative timestamp и explicit lacing flags.
fn block_header(relative_timestamp: i16, flags: u8) -> Vec<u8> {
    let mut bytes = vec![0x81];
    bytes.extend_from_slice(&relative_timestamp.to_be_bytes());
    bytes.push(flags);
    bytes
}

/// Обычный unlaced SimpleBlock.
fn simple_block(relative_timestamp: i16, frame: u8) -> Vec<u8> {
    let mut payload = block_header(relative_timestamp, 0x80);
    payload.push(frame);
    element(id::SIMPLE_BLOCK, &payload)
}

/// Xiph lacing: один stored size и последний frame по остатку block payload-а.
fn xiph_laced_block(relative_timestamp: i16) -> Vec<u8> {
    let mut payload = block_header(relative_timestamp, 0x82);
    payload.extend_from_slice(&[1, 1, 0x20, 0x21]);
    element(id::SIMPLE_BLOCK, &payload)
}

/// Fixed-size lacing: два одно-байтовых frame-а.
fn fixed_laced_block(relative_timestamp: i16) -> Vec<u8> {
    let mut payload = block_header(relative_timestamp, 0x84);
    payload.extend_from_slice(&[1, 0x30, 0x31]);
    element(id::SIMPLE_BLOCK, &payload)
}

/// EBML lacing: три frame-а, первый размер 1, следующий delta 0.
fn ebml_laced_block(relative_timestamp: i16) -> Vec<u8> {
    let mut payload = block_header(relative_timestamp, 0x86);
    payload.extend_from_slice(&[2, 0x81, 0xbf, 0x40, 0x41, 0x42]);
    element(id::SIMPLE_BLOCK, &payload)
}

/// CodecState находится в том же BlockGroup и начинает действовать с его Block-а.
fn codec_state_block(relative_timestamp: i16) -> Vec<u8> {
    let mut block = block_header(relative_timestamp, 0x80);
    block.push(0x50);
    master(
        id::BLOCK_GROUP,
        &[
            element(id::BLOCK, &block),
            element(id::CODEC_STATE, &[0x31, 0x32]),
        ],
    )
}

/// Первый media row упражняет none/Xiph/fixed lacing.
fn first_cluster() -> Vec<u8> {
    master(
        id::CLUSTER,
        &[
            unsigned(id::TIMESTAMP, 0),
            simple_block(0, 0x10),
            xiph_laced_block(100),
            fixed_laced_block(300),
        ],
    )
}

/// Второй media row упражняет EBML lacing и CodecState lifecycle.
fn second_cluster() -> Vec<u8> {
    master(
        id::CLUSTER,
        &[
            unsigned(id::TIMESTAMP, 500),
            ebml_laced_block(0),
            codec_state_block(300),
        ],
    )
}

/// Собирает WebM fixture с optional Cues; никакой production parser здесь не дублируется.
fn fixture(codec: VideoCodecFixture, with_cues: bool) -> MatroskaFixture {
    let info = info();
    let tracks = master(
        id::TRACKS,
        &[video_track(codec), opus_track(), vorbis_track()],
    );
    let first_media = first_cluster();
    let second_media = second_cluster();
    let cues_placeholder = with_cues.then(|| cues(0, 0));
    let first_cluster_position =
        info.len() + tracks.len() + cues_placeholder.as_ref().map_or(0, Vec::len);
    let second_cluster_position = first_cluster_position + first_media.len();
    let cues = with_cues.then(|| {
        cues(
            first_cluster_position as u64,
            second_cluster_position as u64,
        )
    });

    let mut init = ebml_header();
    init.extend_from_slice(&unknown_size_segment_header());
    init.extend_from_slice(&info);
    init.extend_from_slice(&tracks);
    if let Some(cues) = cues {
        init.extend_from_slice(&cues);
    }

    MatroskaFixture {
        init,
        media: vec![first_media, second_media],
    }
}

/// Открывает generated bytes через production registry как seekable local source без hint-а.
fn open_local(bytes: &[u8]) -> (TemporaryMediaFile, Box<dyn Demuxer + Send>) {
    let fixture_file = TemporaryMediaFile::new("bin", bytes);
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            SymphoniaDemuxFactory::new(DemuxerOptions::default()).expect("factory"),
        ))
        .expect("register factory");
    let source = LocalFileSource::open(&fixture_file.path).expect("open generated WebM");
    let demuxer = registry
        .open(
            DemuxInput::byte_source(Box::new(source)),
            DemuxHints::none(),
            DemuxSniffBudget::new(
                NonZeroUsize::new(4_096).expect("sniff bytes"),
                NonZeroUsize::MIN,
                Duration::from_secs(1),
            )
            .expect("sniff budget"),
            CancellationToken::new(),
        )
        .expect("open generated WebM by signature");
    (fixture_file, demuxer)
}

/// Читает finite stream до clean EOF и сохраняет lifecycle ordering.
fn read_all(
    demuxer: &mut dyn Demuxer,
) -> (
    Vec<Packet>,
    Vec<Vec<media_core::TrackInfo>>,
    Vec<usize>,
    bool,
) {
    let mut packets = Vec::new();
    let mut track_changes = Vec::new();
    let mut packet_counts_at_track_change = Vec::new();
    loop {
        match demuxer.next_event().expect("read generated WebM") {
            DemuxReadEvent::Packet(packet) => packets.push(packet),
            DemuxReadEvent::TracksChanged(update) => {
                packet_counts_at_track_change.push(packets.len());
                track_changes.push(update.tracks);
            }
            DemuxReadEvent::MediaMetadataChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                return (packets, track_changes, packet_counts_at_track_change, true);
            }
            DemuxReadEvent::TemporarilyUnavailable(_) => {
                panic!("finite Matroska/WebM не должен публиковать temporary readiness")
            }
        }
    }
}

#[test]
fn local_webm_proves_lacing_audio_timing_duration_and_codec_state_order() {
    let generated = fixture(VideoCodecFixture::Vp9, true);
    let (_file, mut demuxer) = open_local(&generated.whole());
    assert_eq!(demuxer.duration(), Some(Duration::from_millis(1_200)));
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.codec_id == "V_VP9")
    );
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.codec_id == "A_OPUS")
    );
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.codec_id == "A_VORBIS")
    );

    let (packets, track_changes, packet_counts_at_track_change, clean_eof) =
        read_all(demuxer.as_mut());
    assert!(clean_eof);
    assert_eq!(
        packets
            .iter()
            .map(|packet| packet.data[0])
            .collect::<Vec<_>>(),
        vec![0x10, 0x20, 0x21, 0x30, 0x31, 0x40, 0x41, 0x42, 0x50]
    );
    assert_eq!(
        packets.iter().map(|packet| packet.pts).collect::<Vec<_>>(),
        (0..=8)
            .map(|index| Duration::from_millis(index * 100))
            .collect::<Vec<_>>()
    );
    assert!(
        packets
            .iter()
            .all(|packet| packet.duration == Some(Duration::from_millis(100)))
    );
    assert_eq!(
        track_changes.len(),
        1,
        "CodecState должен дать ровно один lifecycle reset"
    );
    assert_eq!(
        packet_counts_at_track_change,
        vec![8],
        "TracksChanged обязан предшествовать первому packet-у нового CodecState"
    );
    let changed_video = track_changes[0]
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("changed video track");
    assert_eq!(
        changed_video.codec_private.as_deref(),
        Some(&[0x31, 0x32][..])
    );
    assert_eq!(
        packets.last().expect("CodecState packet").data.as_ref(),
        [0x50]
    );
}

#[test]
fn ordered_webm_init_and_multiple_media_rows_preserve_packets_and_cancellation() {
    let generated = fixture(VideoCodecFixture::Vp9, true);
    let ordered = vec![
        segment(
            0,
            OrderedSegmentKind::Initialization,
            Bytes::from(generated.init.clone()),
        ),
        segment(
            1,
            OrderedSegmentKind::Media,
            Bytes::from(generated.media[0].clone()),
        ),
        segment(
            2,
            OrderedSegmentKind::Media,
            Bytes::from(generated.media[1].clone()),
        ),
    ];
    let mut demuxer = open_ordered(ordered, CancellationToken::new(), None)
        .expect("finite ordered WebM should open");
    let (packets, track_changes, packet_counts_at_track_change, clean_eof) =
        read_all(demuxer.as_mut());
    assert!(clean_eof);
    assert_eq!(packets.len(), 9);
    assert_eq!(track_changes.len(), 1);
    assert_eq!(packet_counts_at_track_change, vec![8]);
    let current_video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("ordered wrapper должен обновить public video track snapshot");
    assert_eq!(
        current_video_track.codec_private.as_deref(),
        Some(&[0x31, 0x32][..]),
        "tracks() обязан отражать уже опубликованный CodecState"
    );

    let cancelled = open_ordered(
        vec![
            segment(
                0,
                OrderedSegmentKind::Initialization,
                Bytes::from(generated.init),
            ),
            segment(
                1,
                OrderedSegmentKind::Media,
                Bytes::from(generated.media[0].clone()),
            ),
        ],
        CancellationToken::new(),
        Some(2),
    );
    assert!(
        cancelled.is_err(),
        "ordered WebM cancellation не должна теряться"
    );
}

#[test]
fn vp8_vp9_av1_and_cues_no_cues_decode_point_before_are_proven() {
    for codec in [
        VideoCodecFixture::Vp8,
        VideoCodecFixture::Vp9,
        VideoCodecFixture::Av1,
    ] {
        let generated = fixture(codec, true);
        let (_file, demuxer) = open_local(&generated.whole());
        assert!(
            demuxer
                .tracks()
                .iter()
                .any(|track| track.codec_id == codec.codec_id())
        );
    }

    for with_cues in [true, false] {
        let generated = fixture(VideoCodecFixture::Vp9, with_cues);
        let (_file, mut demuxer) = open_local(&generated.whole());
        let seek = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(650),
            ))
            .expect("DecodePointBefore должен найти decode-safe packet");
        assert_eq!(
            seek.requested_position.as_duration(),
            Duration::from_millis(650)
        );
        assert!(seek.actual_position.as_duration() <= Duration::from_millis(650));
    }
}

#[test]
fn malformed_lacing_and_declared_payload_truncation_are_not_clean_eof() {
    let generated = fixture(VideoCodecFixture::Vp9, false);
    let mut malformed = generated.whole();
    let fixed_marker = [0x84, 1, 0x30, 0x31];
    let fixed_position = malformed
        .windows(fixed_marker.len())
        .position(|window| window == fixed_marker)
        .expect("fixed lacing marker");
    malformed[fixed_position + 1] = 2;
    let (_file, mut demuxer) = open_local(&malformed);
    let malformed_error = loop {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::EndOfStream) => panic!("malformed lacing не является clean EOF"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert!(
        malformed_error
            .downcast_ref::<DemuxError>()
            .is_some_and(|error| matches!(error, DemuxError::Parse(_)))
    );

    let mut truncated = generated.whole();
    truncated.pop();
    let (_file, mut demuxer) = open_local(&truncated);
    let truncation_error = loop {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::EndOfStream) => panic!("declared payload truncation не clean EOF"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert!(
        truncation_error
            .downcast_ref::<DemuxError>()
            .is_some_and(|error| matches!(error, DemuxError::Parse(_)))
    );

    let mut truncated_ordered_media = generated.media[1].clone();
    truncated_ordered_media.pop();
    let mut ordered_demuxer = open_ordered(
        vec![
            segment(
                0,
                OrderedSegmentKind::Initialization,
                Bytes::from(generated.init),
            ),
            segment(
                1,
                OrderedSegmentKind::Media,
                Bytes::from(generated.media[0].clone()),
            ),
            segment(
                2,
                OrderedSegmentKind::Media,
                Bytes::from(truncated_ordered_media),
            ),
        ],
        CancellationToken::new(),
        None,
    )
    .expect("ordered truncation проявляется при чтении declared payload");
    let ordered_truncation_error = loop {
        match ordered_demuxer.next_event() {
            Ok(DemuxReadEvent::EndOfStream) => panic!("ordered payload truncation не clean EOF"),
            Ok(_) => {}
            Err(error) => break error,
        }
    };
    assert!(
        ordered_truncation_error
            .downcast_ref::<DemuxError>()
            .is_some_and(|error| matches!(error, DemuxError::Parse(_)))
    );
}
