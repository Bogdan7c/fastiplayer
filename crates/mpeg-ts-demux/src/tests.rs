use std::collections::VecDeque;
use std::io::{Cursor, Read};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use demux_api::{
    DemuxHints, DemuxInput, DemuxOpenError, DemuxRegistry, DemuxSniffBudget, DemuxSourceExtension,
    OrderedSegment, OrderedSegmentDiscontinuity, OrderedSegmentKind, OrderedSegmentReadError,
    OrderedSegmentSequence, OrderedSegmentSource,
};
use media_core::{DemuxReadEvent, DemuxSeekRequest, Demuxer, TrackKind, VideoPacketFraming};
use source_core::{
    ByteSource, CancellationToken, LocalFileSource, Seekability, SourceFingerprint, SourceResult,
    SourceValidators,
};

use crate::psi::{ProgramMap, mpeg_crc32, parse_pat, parse_pmt, select_program};
use crate::timestamps::TimestampUnwrapper;
use crate::{MpegTsDemuxError, MpegTsDemuxFactory, MpegTsDemuxOptions, MpegTsDemuxer, MpegTsLimit};

const PMT_PID: u16 = 0x0100;
const VIDEO_PID: u16 = 0x0101;
const AUDIO_PID: u16 = 0x0102;

/// Hermetic builder производит только минимальные deterministic 188-byte fixtures.
struct TsFixtureBuilder {
    bytes: Vec<u8>,
    continuity: [u8; 8_192],
}

impl TsFixtureBuilder {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            continuity: [0; 8_192],
        }
    }

    fn pat(mut self, programs: &[(u16, u16)], version: u8) -> Self {
        let mut body = vec![
            0x00,
            0xb0,
            0x00,
            0x00,
            0x01,
            0xc1 | (version << 1),
            0x00,
            0x00,
        ];
        for &(program_number, pmt_pid) in programs {
            body.extend_from_slice(&program_number.to_be_bytes());
            body.push(0xe0 | ((pmt_pid >> 8) as u8 & 0x1f));
            body.push(pmt_pid as u8);
        }
        finalize_section(&mut body);
        let mut payload = vec![0];
        payload.extend(body);
        self.push_payload(0, true, false, &payload);
        self
    }

    fn pmt(mut self, pmt_pid: u16, program: u16, streams: &[(u8, u16)], version: u8) -> Self {
        let pcr_pid = streams.first().map_or(0x1fff, |stream| stream.1);
        self.push_pmt(pmt_pid, program, pcr_pid, streams, version);
        self
    }

    fn pmt_with_pcr(
        mut self,
        pmt_pid: u16,
        program: u16,
        pcr_pid: u16,
        streams: &[(u8, u16)],
        version: u8,
    ) -> Self {
        self.push_pmt(pmt_pid, program, pcr_pid, streams, version);
        self
    }

    fn push_pmt(
        &mut self,
        pmt_pid: u16,
        program: u16,
        pcr_pid: u16,
        streams: &[(u8, u16)],
        version: u8,
    ) {
        let mut body = vec![
            0x02,
            0xb0,
            0x00,
            (program >> 8) as u8,
            program as u8,
            0xc1 | (version << 1),
            0x00,
            0x00,
            0xe0 | ((pcr_pid >> 8) as u8 & 0x1f),
            pcr_pid as u8,
            0xf0,
            0x00,
        ];
        for &(stream_type, pid) in streams {
            body.extend_from_slice(&[
                stream_type,
                0xe0 | ((pid >> 8) as u8 & 0x1f),
                pid as u8,
                0xf0,
                0x00,
            ]);
        }
        finalize_section(&mut body);
        let mut payload = vec![0];
        payload.extend(body);
        self.push_payload(pmt_pid, true, false, &payload);
    }

    fn pes(mut self, pid: u16, pts: u64, dts: Option<u64>, elementary: &[u8]) -> Self {
        let mut pes = vec![0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x80];
        if let Some(dts) = dts {
            pes.extend_from_slice(&[0xc0, 10]);
            pes.extend_from_slice(&encode_timestamp(0b0011, pts));
            pes.extend_from_slice(&encode_timestamp(0b0001, dts));
        } else {
            pes.extend_from_slice(&[0x80, 5]);
            pes.extend_from_slice(&encode_timestamp(0b0010, pts));
        }
        pes.extend_from_slice(elementary);
        let packet_length = pes.len() - 6;
        pes[4..6].copy_from_slice(&(packet_length as u16).to_be_bytes());
        for (index, chunk) in pes.chunks(184).enumerate() {
            self.push_payload(pid, index == 0, false, chunk);
        }
        self
    }

    fn discontinuity(mut self, pid: u16) -> Self {
        self.push_payload(pid, false, true, &[]);
        self
    }

    fn pcr(mut self, pid: u16, pcr_base: u64) -> Self {
        let mut packet = [0xff_u8; 188];
        packet[0] = 0x47;
        packet[1] = (pid >> 8) as u8 & 0x1f;
        packet[2] = pid as u8;
        let continuity = self.continuity[usize::from(pid)];
        packet[3] = 0x20 | continuity;
        packet[4] = 183;
        packet[5] = 0x10;
        packet[6] = (pcr_base >> 25) as u8;
        packet[7] = (pcr_base >> 17) as u8;
        packet[8] = (pcr_base >> 9) as u8;
        packet[9] = (pcr_base >> 1) as u8;
        packet[10] = ((pcr_base & 1) as u8) << 7 | 0x7e;
        packet[11] = 0;
        self.bytes.extend_from_slice(&packet);
        self
    }

    fn push_payload(&mut self, pid: u16, payload_start: bool, discontinuity: bool, payload: &[u8]) {
        assert!(payload.len() <= 184);
        let mut packet = [0xff_u8; 188];
        packet[0] = 0x47;
        packet[1] = ((payload_start as u8) << 6) | ((pid >> 8) as u8 & 0x1f);
        packet[2] = pid as u8;
        let continuity = self.continuity[usize::from(pid)];
        self.continuity[usize::from(pid)] = (continuity + 1) & 0x0f;
        if discontinuity || payload.len() < 184 {
            let adaptation_length = 183 - payload.len();
            packet[3] = 0x30 | continuity;
            packet[4] = adaptation_length as u8;
            if adaptation_length > 0 {
                packet[5] = if discontinuity { 0x80 } else { 0x00 };
            }
            let payload_offset = 5 + adaptation_length;
            packet[payload_offset..payload_offset + payload.len()].copy_from_slice(payload);
        } else {
            packet[3] = 0x10 | continuity;
            packet[4..].copy_from_slice(payload);
        }
        self.bytes.extend_from_slice(&packet);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Reader намеренно дробит source иначе, чем TS packet boundaries.
struct ChunkedReader {
    bytes: Cursor<Vec<u8>>,
    maximum_chunk: usize,
}

struct CancellingProbeReader {
    bytes: Cursor<Vec<u8>>,
    cancellation: CancellationToken,
    first_read: bool,
}

impl Read for CancellingProbeReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let bounded = output.len().min(188);
        let count = self.bytes.read(&mut output[..bounded])?;
        if self.first_read {
            self.first_read = false;
            self.cancellation.cancel();
        }
        Ok(count)
    }
}

impl Read for ChunkedReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let bounded = output.len().min(self.maximum_chunk);
        self.bytes.read(&mut output[..bounded])
    }
}

/// Seekable source, который по test-owned arm отменяет token после одного scan read-а.
struct CancellingSeekSource {
    inner: LocalFileSource,
    cancellation: CancellationToken,
    armed: Arc<AtomicBool>,
    cancel_after_position: Option<u64>,
}

impl ByteSource for CancellingSeekSource {
    fn read(&mut self, output: &mut [u8], cancellation: &CancellationToken) -> SourceResult<usize> {
        let bounded = output.len().min(188);
        let count = self.inner.read(&mut output[..bounded], cancellation)?;
        if self.armed.load(Ordering::SeqCst)
            && self
                .cancel_after_position
                .is_none_or(|offset| self.inner.position() >= offset)
        {
            self.cancellation.cancel();
        }
        Ok(count)
    }

    fn seek(&mut self, offset: u64) -> SourceResult<()> {
        self.inner.seek(offset)
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn seekability(&self) -> Seekability {
        self.inner.seekability()
    }

    fn validators(&self) -> SourceValidators {
        self.inner.validators()
    }

    fn content_length(&self) -> Option<u64> {
        self.inner.content_length()
    }

    fn fingerprint(&self) -> SourceFingerprint {
        self.inner.fingerprint()
    }
}

struct SegmentSource {
    segments: VecDeque<OrderedSegment>,
}

impl OrderedSegmentSource for SegmentSource {
    fn next_segment(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<OrderedSegment>, OrderedSegmentReadError> {
        if cancellation.is_cancelled() {
            return Err(OrderedSegmentReadError::Cancelled);
        }
        Ok(self.segments.pop_front())
    }
}

#[test]
fn muxed_h264_aac_survives_arbitrary_read_chunks() {
    let bytes = muxed_h264_aac_fixture(90_000);
    let input = DemuxInput::byte_stream(Box::new(ChunkedReader {
        bytes: Cursor::new(bytes),
        maximum_chunk: 7,
    }));
    let mut demuxer = open(input, DemuxHints::none()).expect("open generated TS");
    assert_eq!(demuxer.tracks().len(), 2);
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.kind == TrackKind::Video)
    );
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.codec_id == "A_AAC")
    );
    let events = drain(&mut *demuxer).expect("drain packets");
    assert!(events.iter().any(|event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video && packet.keyframe.is_known_keyframe())));
    assert!(events.iter().any(
        |event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio)
    ));
}

#[test]
fn audio_only_adts_is_playable_without_video() {
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x0f, AUDIO_PID)], 0)
        .pes(AUDIO_PID, 0, None, &adts_frame(&[0x11, 0x22]))
        .finish();
    let demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("audio-only TS");
    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].kind, TrackKind::Audio);
    assert_eq!(demuxer.tracks()[0].sample_rate, Some(44_100));
    assert_eq!(demuxer.tracks()[0].channels, Some(2));
}

#[test]
fn ordered_segment_continuity_restart_keeps_tracks_stable_and_emits_media_packets() {
    let first_segment = independent_muxed_h264_aac_segment_fixture(0);
    let second_segment = independent_muxed_h264_aac_segment_fixture(90_000);
    let segments = VecDeque::from([
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(0),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
            bytes: Bytes::from(first_segment),
        },
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(1),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
            bytes: Bytes::from(second_segment),
        },
    ]);
    let mut demuxer = open(
        DemuxInput::ordered_segments(Box::new(SegmentSource { segments })),
        DemuxHints::none(),
    )
    .expect("independent HLS-style TS segments");

    let events = drain(&mut *demuxer).expect("drain second independent segment");

    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
    );
    assert!(events.iter().any(
        |event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video)
    ));
    assert!(events.iter().any(
        |event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio)
    ));
    let video_positions = events
        .iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => Some(packet.pts),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        video_positions,
        [Duration::ZERO, Duration::from_secs(1)],
        "ordered boundary обязан flush-нуть video RAP каждого segment-а"
    );
}

#[test]
fn ordered_segment_boundary_rejects_incomplete_pes_before_packet_publication() {
    let mut oversized_access_unit = vec![0x00, 0x00, 0x01, 0x65, 0x80];
    oversized_access_unit.extend(std::iter::repeat_n(0x55, 400));
    let complete_transport = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &oversized_access_unit)
        .finish();
    let boundary = 188 * 3;
    let segments = VecDeque::from([
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(0),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
            bytes: Bytes::copy_from_slice(&complete_transport[..boundary]),
        },
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(1),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::StartsNewTimeline,
            bytes: Bytes::copy_from_slice(&complete_transport[boundary..]),
        },
    ]);
    let mut demuxer = open(
        DemuxInput::ordered_segments(Box::new(SegmentSource { segments })),
        DemuxHints::none(),
    )
    .expect("ordered TS topology до truncated PES валидна");
    let mut published_video_packet = false;
    let failure = loop {
        match demuxer.next_event() {
            Ok(DemuxReadEvent::Packet(packet)) => {
                published_video_packet |= packet.kind == TrackKind::Video;
            }
            Ok(
                DemuxReadEvent::TracksChanged(_)
                | DemuxReadEvent::MediaMetadataChanged(_)
                | DemuxReadEvent::TemporarilyUnavailable(_),
            ) => {}
            Ok(DemuxReadEvent::EndOfStream) => {
                panic!("incomplete PES не должен превратиться в clean EOF")
            }
            Err(error) => break error,
        }
    };

    assert!(!published_video_packet);
    assert!(matches!(
        failure.downcast_ref::<MpegTsDemuxError>(),
        Some(MpegTsDemuxError::Malformed { reason })
            if reason.contains("PES packet_length больше собранных bytes")
    ));
}

#[test]
fn muxed_h265_and_mpeg_audio_are_classified_from_pmt_and_frame_header() {
    let h265_access_unit = [0, 0, 1, 0x40, 0x01, 0x80, 0, 0, 1, 0x26, 0x01, 0x80];
    let mut mp3_frame = vec![0_u8; 417];
    mp3_frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x24, VIDEO_PID), (0x03, AUDIO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &h265_access_unit)
        .pes(AUDIO_PID, 0, None, &mp3_frame)
        .finish();

    let demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("H.265 + MPEG audio TS");

    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.codec_id == "V_MPEGH/ISO/HEVC")
    );
    assert!(
        demuxer
            .tracks()
            .iter()
            .any(|track| track.codec_id == "A_MP3")
    );
}

#[test]
fn adts_frame_split_across_two_pes_is_reassembled_once() {
    let frame = adts_frame(&[0x11, 0x22, 0x33, 0x44]);
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x0f, AUDIO_PID)], 0)
        .pes(AUDIO_PID, 0, None, &frame[..5])
        .pes(AUDIO_PID, 0, None, &frame[5..])
        .finish();
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("split ADTS TS");
    let packet_count = drain(&mut *demuxer)
        .expect("drain split ADTS")
        .iter()
        .filter(|event| matches!(event, DemuxReadEvent::Packet(_)))
        .count();
    assert_eq!(packet_count, 1);
}

#[test]
fn multiple_playable_programs_fail_closed() {
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID), (2, PMT_PID + 1)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .pmt(PMT_PID + 1, 2, &[(0x0f, AUDIO_PID)], 0)
        .finish();
    let error = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .err()
    .expect("ambiguous TS must fail");
    let DemuxOpenError::FactoryRejected { source, .. } = error else {
        panic!("expected factory rejection");
    };
    let demux_api::DemuxFactoryOpenError::Backend(source) = source else {
        panic!("expected backend rejection");
    };
    assert!(matches!(
        source.downcast_ref::<MpegTsDemuxError>(),
        Some(MpegTsDemuxError::MultiplePlayablePrograms { programs }) if programs == &vec![1, 2]
    ));
}

#[test]
fn conflicting_extension_cannot_override_ts_signature() {
    let bytes = muxed_h264_aac_fixture(0);
    let hints = DemuxHints::none().with_extension(DemuxSourceExtension::new("mp4").expect("hint"));
    let demuxer = open(DemuxInput::byte_stream(Box::new(Cursor::new(bytes))), hints)
        .expect("signature is authoritative");
    assert_eq!(demuxer.tracks().len(), 2);
}

#[test]
fn explicit_ordered_segment_discontinuity_precedes_next_packet() {
    let bytes = muxed_h264_aac_fixture(0);
    let split = 188 * 2;
    let segments = VecDeque::from([
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(0),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::Continuous,
            bytes: Bytes::copy_from_slice(&bytes[..split]),
        },
        OrderedSegment {
            sequence: OrderedSegmentSequence::new(1),
            kind: OrderedSegmentKind::Media,
            discontinuity: OrderedSegmentDiscontinuity::StartsNewTimeline,
            bytes: Bytes::copy_from_slice(&bytes[split..]),
        },
    ]);
    let mut demuxer = open(
        DemuxInput::ordered_segments(Box::new(SegmentSource { segments })),
        DemuxHints::none(),
    )
    .expect("segmented TS");
    let events = drain(&mut *demuxer).expect("drain segmented stream");
    let tracks_changed = events
        .iter()
        .position(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
        .expect("discontinuity lifecycle");
    let packet = events
        .iter()
        .position(|event| matches!(event, DemuxReadEvent::Packet(_)))
        .expect("dependent packet");
    assert!(tracks_changed < packet);
}

#[test]
fn in_band_discontinuity_resets_transport_state_without_changing_tracks() {
    let h264_access_unit = [0x00, 0x00, 0x01, 0x65, 0x88];
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .discontinuity(VIDEO_PID)
        .pes(VIDEO_PID, 0, None, &h264_access_unit)
        .finish();
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("in-band discontinuity fixture");
    let events = drain(&mut *demuxer).expect("drain in-band discontinuity");
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::Packet(_)))
    );
}

#[test]
fn cancelled_probe_never_opens_backend() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let registry = registry();
    let error = registry
        .open(
            DemuxInput::byte_stream(Box::new(Cursor::new(muxed_h264_aac_fixture(0)))),
            DemuxHints::none(),
            sniff_budget(),
            cancellation,
        )
        .err()
        .expect("cancelled open");
    assert!(matches!(error, DemuxOpenError::ProbeRejected(_)));
}

#[test]
fn cancellation_during_registry_probe_is_typed() {
    let cancellation = CancellationToken::new();
    let input = DemuxInput::byte_stream(Box::new(CancellingProbeReader {
        bytes: Cursor::new(muxed_h264_aac_fixture(0)),
        cancellation: cancellation.clone(),
        first_read: true,
    }));

    let error = match registry().open(input, DemuxHints::none(), sniff_budget(), cancellation) {
        Ok(_) => panic!("mid-probe cancellation must reject registry open"),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        DemuxOpenError::ProbeRejected(demux_api::DemuxProbeRejection::Cancelled)
    ));
}

#[test]
fn cancellation_during_runtime_read_is_typed() {
    let cancellation = CancellationToken::new();
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(many_h264_access_units(3)))),
        cancellation.clone(),
        MpegTsDemuxOptions::default(),
    )
    .expect("open streaming TS");
    cancellation.cancel();

    let error = demuxer.next_event().expect_err("runtime read cancellation");

    assert!(matches!(
        error.downcast_ref::<MpegTsDemuxError>(),
        Some(MpegTsDemuxError::Cancelled)
    ));
}

#[test]
fn m2ts_192_signature_is_explicitly_rejected() {
    let packet = [0xff_u8; 188];
    let mut bytes = Vec::new();
    for _ in 0..3 {
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let mut transport = packet;
        transport[0] = 0x47;
        bytes.extend_from_slice(&transport);
    }
    let error = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .err()
    .expect("M2TS unsupported");
    assert!(matches!(
        error,
        DemuxOpenError::ProbeRejected(demux_api::DemuxProbeRejection::Malformed { reason })
            if reason.contains("192-byte M2TS")
    ));
}

#[test]
fn psi_crc_and_version_are_validated() {
    let bytes = TsFixtureBuilder::new().pat(&[(1, PMT_PID)], 7).finish();
    let packet = &bytes[..188];
    let adaptation_length = usize::from(packet[4]);
    let payload = &packet[5 + adaptation_length..];
    let section = &payload[1..];
    let section_length = 3 + (((usize::from(section[1]) & 0x0f) << 8) | usize::from(section[2]));
    let (version, programs) = parse_pat(&section[..section_length]).expect("valid PAT");
    assert_eq!(version, 7);
    assert_eq!(programs[0].pmt_pid, PMT_PID);
}

#[test]
fn timestamp_unwrap_keeps_pts_and_dts_rollovers_independent() {
    let mut pts = TimestampUnwrapper::default();
    let mut dts = TimestampUnwrapper::default();
    assert_eq!(pts.unwrap((1_u64 << 33) - 4), (1_i64 << 33) - 4);
    assert_eq!(pts.unwrap(3), (1_i64 << 33) + 3);
    assert_eq!(dts.unwrap(9), 9);
}

#[test]
fn one_video_pes_with_two_auds_yields_two_access_units() {
    let aggregate = [
        0, 0, 1, 9, 0xf0, 0, 0, 1, 0x65, 0x80, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x41, 0x80,
    ];
    let access_units = crate::elementary::split_video_access_units(&aggregate, false)
        .expect("split Annex-B access units");
    assert_eq!(access_units.len(), 2);
    assert!(access_units[0].keyframe.is_known_keyframe());
    assert!(!access_units[1].keyframe.is_known_keyframe());
}

#[test]
fn h264_access_unit_split_between_pes_is_emitted_once_without_truncation() {
    let access_unit = [
        0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65, 0x80,
    ];
    let split = 8;
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &access_unit[..split])
        .pes(VIDEO_PID, 0, None, &access_unit[split..])
        .finish();
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("split H.264 AU");

    assert_eq!(
        demuxer.tracks()[0]
            .video
            .as_ref()
            .expect("video framing")
            .packet_framing,
        VideoPacketFraming::AnnexB
    );
    let packets: Vec<_> = drain(&mut *demuxer)
        .expect("drain split H.264")
        .into_iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => Some(packet),
            _ => None,
        })
        .collect();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].data.as_ref(), access_unit);
    assert!(packets[0].keyframe.is_known_keyframe());
}

#[test]
fn h265_config_and_irap_split_between_pes_keep_annex_b_evidence() {
    let access_unit = [
        0, 0, 1, 0x40, 0x01, 0x80, 0, 0, 1, 0x42, 0x01, 0x80, 0, 0, 1, 0x44, 0x01, 0x80, 0, 0, 1,
        0x26, 0x01, 0x80,
    ];
    let split = 11;
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x24, VIDEO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &access_unit[..split])
        .pes(VIDEO_PID, 0, None, &access_unit[split..])
        .finish();
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("split H.265 AU");

    assert_eq!(
        demuxer.tracks()[0]
            .video
            .as_ref()
            .expect("video framing")
            .packet_framing,
        VideoPacketFraming::AnnexB
    );
    let packets: Vec<_> = drain(&mut *demuxer)
        .expect("drain split H.265")
        .into_iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => Some(packet),
            _ => None,
        })
        .collect();
    assert_eq!(packets.len(), 1);
    assert_eq!(packets[0].data.as_ref(), access_unit);
    assert!(packets[0].keyframe.is_known_keyframe());
}

#[test]
fn separate_pcr_pid_and_rollover_produce_monotonic_seek_evidence() {
    const PCR_PID: u16 = 0x01ff;
    let before_wrap = (1_u64 << 33) - 45_000;
    let after_wrap = 45_000;
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt_with_pcr(PMT_PID, 1, PCR_PID, &[(0x0f, AUDIO_PID)], 0)
        .pcr(PCR_PID, before_wrap)
        .pes(AUDIO_PID, before_wrap, None, &adts_frame(&[1, 2]))
        .pcr(PCR_PID, after_wrap)
        .pes(AUDIO_PID, after_wrap, None, &adts_frame(&[3, 4]))
        .finish();
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("separate-pcr-rollover.ts");
    std::fs::write(&path, bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    let mut demuxer = open(
        DemuxInput::byte_source(Box::new(source)),
        DemuxHints::none(),
    )
    .expect("separate PCR PID");

    let target_units = before_wrap + 90_000;
    let target = Duration::from_secs_f64(target_units as f64 / 90_000.0);
    let result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(target))
        .expect("PCR rollover seek");

    assert!(result.actual_position.as_duration() >= Duration::from_secs(1));
}

#[test]
fn bounded_local_index_lands_decode_point_on_keyframe() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("seek.ts");
    std::fs::write(&path, muxed_h264_aac_fixture(90_000)).expect("write TS fixture");
    let source = LocalFileSource::open(&path).expect("open local source");
    let mut demuxer = open(
        DemuxInput::byte_source(Box::new(source)),
        DemuxHints::none(),
    )
    .expect("open seekable TS");

    let result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            1,
        )))
        .expect("decode-safe seek");

    assert_eq!(result.actual_position, result.requested_position);
}

#[test]
fn on_demand_index_expands_beyond_initial_window_and_lands_decode_safe() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("on-demand-index.ts");
    std::fs::write(&path, many_h264_access_units(8)).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    let options = MpegTsDemuxOptions {
        seek_scan_packets: MpegTsLimit::new(4, "seek_scan_packets").expect("limit"),
        ..MpegTsDemuxOptions::default()
    };
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        options,
    )
    .expect("bounded initial index");
    let initial_entries = demuxer.test_index_entries();

    let result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            4,
        )))
        .expect("bounded on-demand expansion");

    assert!(result.actual_position.as_duration() >= Duration::from_secs(3));
    assert!(result.actual_position.as_duration() <= Duration::from_secs(4));
    assert!(demuxer.test_index_entries() > initial_entries);
}

#[test]
fn on_demand_index_never_exceeds_configured_entry_cap() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("capped-index.ts");
    std::fs::write(&path, many_h264_access_units(10)).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    let options = MpegTsDemuxOptions {
        seek_scan_packets: MpegTsLimit::new(4, "seek_scan_packets").expect("limit"),
        index_entries: MpegTsLimit::new(2, "index_entries").expect("limit"),
        ..MpegTsDemuxOptions::default()
    };
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        options,
    )
    .expect("capped index");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            8,
        )))
        .expect_err("entry cap cannot claim uncovered target");

    assert_eq!(demuxer.test_index_entries(), 2);
    assert!(matches!(
        error.downcast_ref::<MpegTsDemuxError>(),
        Some(MpegTsDemuxError::SeekAnchorUnavailable { .. })
    ));
}

#[test]
fn index_continuation_preserves_access_unit_split_at_scan_window_boundary() {
    let access_unit = [
        0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65, 0x80,
    ];
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &access_unit[..8])
        .pes(VIDEO_PID, 0, None, &access_unit[8..])
        .pes(VIDEO_PID, 90_000, None, &[0, 0, 1, 0x65, 0x80])
        .finish();
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("scan-boundary-au.ts");
    std::fs::write(&path, bytes).expect("write fixture");
    let source = LocalFileSource::open(&path).expect("open source");
    let options = MpegTsDemuxOptions {
        seek_scan_packets: MpegTsLimit::new(2, "seek_scan_packets").expect("limit"),
        ..MpegTsDemuxOptions::default()
    };
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        CancellationToken::never_cancelled(),
        options,
    )
    .expect("initial window stores partial AU");

    let result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect("next bounded window completes split AU");

    assert_eq!(result.actual_position.as_duration(), Duration::ZERO);
}

#[test]
fn cancellation_during_seek_scan_rolls_back_reader_and_index() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("cancelled-seek-index.ts");
    std::fs::write(&path, many_h264_access_units(8)).expect("write fixture");
    let cancellation = CancellationToken::new();
    let armed = Arc::new(AtomicBool::new(false));
    let source = CancellingSeekSource {
        inner: LocalFileSource::open(&path).expect("open source"),
        cancellation: cancellation.clone(),
        armed: Arc::clone(&armed),
        cancel_after_position: None,
    };
    let options = MpegTsDemuxOptions {
        seek_scan_packets: MpegTsLimit::new(3, "seek_scan_packets").expect("limit"),
        ..MpegTsDemuxOptions::default()
    };
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        cancellation,
        options,
    )
    .expect("open before cancellation");
    let position_before = demuxer.test_reader_position();
    let entries_before = demuxer.test_index_entries();
    armed.store(true, Ordering::SeqCst);

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            4,
        )))
        .expect_err("scan cancellation");

    assert!(matches!(
        error.downcast_ref::<MpegTsDemuxError>(),
        Some(MpegTsDemuxError::Cancelled)
    ));
    assert_eq!(demuxer.test_reader_position(), position_before);
    assert_eq!(demuxer.test_index_entries(), entries_before);
}

#[test]
fn cancellation_during_initial_index_scan_is_typed() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("cancelled-open-index.ts");
    std::fs::write(&path, many_h264_access_units(8)).expect("write fixture");
    let cancellation = CancellationToken::new();
    let source = CancellingSeekSource {
        inner: LocalFileSource::open(&path).expect("open source"),
        cancellation: cancellation.clone(),
        armed: Arc::new(AtomicBool::new(true)),
        cancel_after_position: Some((188 * 3) as u64),
    };

    let error = match MpegTsDemuxer::open(
        DemuxInput::byte_source(Box::new(source)),
        cancellation,
        MpegTsDemuxOptions::default(),
    ) {
        Ok(_) => panic!("initial index cancellation must fail open"),
        Err(error) => error,
    };

    assert!(matches!(error, MpegTsDemuxError::Cancelled));
}

#[test]
fn bounded_local_index_uses_pcr_when_no_video_keyframe_exists() {
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x0f, AUDIO_PID)], 0)
        .pcr(AUDIO_PID, 90_000)
        .pes(AUDIO_PID, 90_000, None, &adts_frame(&[1, 2, 3, 4]))
        .finish();
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("pcr-index.ts");
    std::fs::write(&path, bytes).expect("write TS fixture");
    let source = LocalFileSource::open(&path).expect("open local source");
    let mut demuxer = open(
        DemuxInput::byte_source(Box::new(source)),
        DemuxHints::none(),
    )
    .expect("open seekable audio-only TS");

    let result = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::from_secs(1)))
        .expect("PCR-backed seek");

    assert_eq!(result.actual_position, result.requested_position);
}

#[test]
fn non_seekable_rejection_does_not_consume_next_packet() {
    let bytes = muxed_h264_aac_fixture(0);
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("streaming TS");
    let error = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
        .expect_err("stream must reject seek");
    assert!(error.downcast_ref::<MpegTsDemuxError>().is_some());
    assert!(
        drain(&mut *demuxer)
            .expect("read after failed seek")
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::Packet(_)))
    );
}

#[test]
fn pat_and_pmt_helpers_reject_corrupted_crc() {
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .finish();
    let mut corrupted = bytes.clone();
    corrupted[187] ^= 1;
    let result = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(corrupted))),
        DemuxHints::none(),
    );
    assert!(result.is_err());
}

#[test]
fn bounded_resync_skips_corruption_and_false_sync_without_reporting_eof() {
    let h264 = [0, 0, 1, 0x65, 0x80];
    let clean = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &h264)
        .pes(VIDEO_PID, 3_000, None, &h264)
        .pes(VIDEO_PID, 6_000, None, &h264)
        .finish();
    let mut corrupted = clean[..188 * 2].to_vec();
    corrupted.extend_from_slice(&[0x13, 0x47, 0x99, 0x00, 0x47]);
    corrupted.extend_from_slice(&clean[188 * 2..]);

    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(corrupted))),
        DemuxHints::none(),
    )
    .expect("resynchronized TS");
    let packets = drain(&mut *demuxer)
        .expect("corruption is recoverable")
        .iter()
        .filter(|event| matches!(event, DemuxReadEvent::Packet(_)))
        .count();
    assert_eq!(packets, 3);
}

#[test]
fn duplicate_continuity_counter_is_ignored_without_duplicate_video_packet() {
    let clean = many_h264_access_units(2);
    let mut duplicated = clean[..188 * 3].to_vec();
    duplicated.extend_from_slice(&clean[188 * 2..188 * 3]);
    duplicated.extend_from_slice(&clean[188 * 3..]);
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(duplicated))),
        DemuxHints::none(),
    )
    .expect("duplicate CC is recoverable");

    let video_packets = drain(&mut *demuxer)
        .expect("drain duplicate fixture")
        .into_iter()
        .filter(|event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video))
        .count();

    assert_eq!(video_packets, 2);
}

#[test]
fn continuity_gap_resets_only_affected_video_pid() {
    let video = [0, 0, 1, 0x65, 0x80];
    let audio = adts_frame(&[1, 2, 3]);
    let mut bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &video)
        .pes(AUDIO_PID, 0, None, &audio)
        .pes(VIDEO_PID, 90_000, None, &video)
        .pes(VIDEO_PID, 180_000, None, &video)
        .finish();
    bytes[188 * 4 + 3] = (bytes[188 * 4 + 3] & 0xf0) | 7;
    bytes[188 * 5 + 3] = (bytes[188 * 5 + 3] & 0xf0) | 8;
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("continuity gap fixture");
    let events = drain(&mut *demuxer).expect("recover affected PID");

    assert!(events.iter().any(
        |event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio)
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video))
            .count(),
        2
    );
}

#[test]
fn transport_error_indicator_drops_only_damaged_pid_and_recovers() {
    let mut bytes = many_h264_access_units(3);
    bytes[188 * 2 + 1] |= 0x80;
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("TEI recovery fixture");

    let video_packets = drain(&mut *demuxer)
        .expect("recover after TEI")
        .into_iter()
        .filter(|event| matches!(event, DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video))
        .count();

    assert_eq!(video_packets, 2);
}

#[test]
fn scrambled_payload_returns_typed_error() {
    let mut bytes = many_h264_access_units(2);
    bytes[188 * 2 + 3] |= 0x80;
    let mut demuxer = MpegTsDemuxer::open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        CancellationToken::never_cancelled(),
        MpegTsDemuxOptions::default(),
    )
    .expect("topology opens before scrambled payload");

    let error = demuxer.next_event().expect_err("scrambled packet");

    assert!(matches!(
        error.downcast_ref::<MpegTsDemuxError>(),
        Some(MpegTsDemuxError::Scrambled { pid: VIDEO_PID })
    ));
}

#[test]
fn video_config_change_emits_tracks_changed_before_dependent_packet() {
    let first = [
        0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 1, 0x68, 0xce, 0, 0, 1, 0x65, 0x80,
    ];
    let changed = [
        0, 0, 1, 0x67, 0x4d, 0, 0x28, 0, 0, 1, 0x68, 0xcf, 0, 0, 1, 0x65, 0x80,
    ];
    let tail = [0, 0, 1, 0x65, 0x80];
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &first)
        .pes(VIDEO_PID, 90_000, None, &changed)
        .pes(VIDEO_PID, 180_000, None, &tail)
        .finish();
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("config change fixture");
    let events = drain(&mut *demuxer).expect("config lifecycle");
    let changed_packet_index = events
        .iter()
        .position(|event| matches!(event, DemuxReadEvent::Packet(packet) if packet.data.as_ref() == changed))
        .expect("changed config packet");

    assert!(changed_packet_index > 0);
    assert!(matches!(
        events[changed_packet_index - 1],
        DemuxReadEvent::TracksChanged(_)
    ));
}

#[test]
fn pat_pmt_version_change_emits_tracks_changed_before_new_generation_packet() {
    let access_unit = [0, 0, 1, 0x65, 0x80];
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0)
        .pes(VIDEO_PID, 0, None, &access_unit)
        .pes(VIDEO_PID, 90_000, None, &access_unit)
        .pat(&[(1, PMT_PID)], 1)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 1)
        .pes(VIDEO_PID, 180_000, None, &access_unit)
        .pes(VIDEO_PID, 270_000, None, &access_unit)
        .finish();
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("versioned PAT/PMT fixture");
    let events = drain(&mut *demuxer).expect("topology lifecycle");
    let generation_packet = events
        .iter()
        .position(|event| matches!(event, DemuxReadEvent::Packet(packet) if packet.pts >= Duration::from_secs(2)))
        .expect("new generation packet");

    assert!(
        events[..generation_packet]
            .iter()
            .any(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
    );
}

#[test]
fn two_adts_frames_in_one_pes_are_emitted_individually() {
    let mut aggregate = adts_frame(&[1, 2]);
    aggregate.extend(adts_frame(&[3, 4, 5]));
    let bytes = TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x0f, AUDIO_PID)], 0)
        .pes(AUDIO_PID, 0, None, &aggregate)
        .finish();
    let mut demuxer = open(
        DemuxInput::byte_stream(Box::new(Cursor::new(bytes))),
        DemuxHints::none(),
    )
    .expect("aggregate ADTS");

    let packets: Vec<_> = drain(&mut *demuxer)
        .expect("drain ADTS aggregate")
        .into_iter()
        .filter_map(|event| match event {
            DemuxReadEvent::Packet(packet) => Some(packet.data),
            _ => None,
        })
        .collect();

    assert_eq!(
        packets,
        vec![Bytes::from_static(&[1, 2]), Bytes::from_static(&[3, 4, 5])]
    );
}

fn open(input: DemuxInput, hints: DemuxHints) -> Result<Box<dyn Demuxer + Send>, DemuxOpenError> {
    registry().open(
        input,
        hints,
        sniff_budget(),
        CancellationToken::never_cancelled(),
    )
}

fn registry() -> DemuxRegistry {
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(
            MpegTsDemuxFactory::new(Default::default()).expect("factory identity"),
        ))
        .expect("unique registration");
    registry
}

fn sniff_budget() -> DemuxSniffBudget {
    DemuxSniffBudget::new(
        NonZeroUsize::new(8 * 188).expect("non-zero"),
        NonZeroUsize::new(4).expect("non-zero"),
        Duration::from_secs(1),
    )
    .expect("positive duration")
}

fn drain(demuxer: &mut dyn Demuxer) -> anyhow::Result<Vec<DemuxReadEvent>> {
    let mut events = Vec::new();
    loop {
        let event = demuxer.next_event()?;
        if event == DemuxReadEvent::EndOfStream {
            break;
        }
        events.push(event);
    }
    Ok(events)
}

fn muxed_h264_aac_fixture(pts: u64) -> Vec<u8> {
    let h264_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01,
        0x65, 0x88,
    ];
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)], 0)
        .pes(
            VIDEO_PID,
            pts,
            Some(pts.saturating_sub(3_000)),
            &h264_access_unit,
        )
        .pes(AUDIO_PID, pts, None, &adts_frame(&[0x11, 0x22]))
        .finish()
}

/// Моделирует независимый HLS MPEG-TS segment с transport discontinuity на обоих ES PID.
fn independent_muxed_h264_aac_segment_fixture(pts: u64) -> Vec<u8> {
    let h264_access_unit = [
        0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1e, 0x00, 0x00, 0x01, 0x68, 0xce, 0x00, 0x00, 0x01,
        0x65, 0x88,
    ];
    TsFixtureBuilder::new()
        .pat(&[(1, PMT_PID)], 0)
        .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID), (0x0f, AUDIO_PID)], 0)
        .discontinuity(VIDEO_PID)
        .discontinuity(AUDIO_PID)
        .pes(
            VIDEO_PID,
            pts,
            Some(pts.saturating_sub(3_000)),
            &h264_access_unit,
        )
        .pes(AUDIO_PID, pts, None, &adts_frame(&[0x11, 0x22]))
        .finish()
}

fn many_h264_access_units(count: usize) -> Vec<u8> {
    let mut builder =
        TsFixtureBuilder::new()
            .pat(&[(1, PMT_PID)], 0)
            .pmt(PMT_PID, 1, &[(0x1b, VIDEO_PID)], 0);
    for index in 0..count {
        builder = builder.pes(
            VIDEO_PID,
            index as u64 * 90_000,
            None,
            &[0, 0, 1, 0x65, 0x80],
        );
    }
    builder.finish()
}

fn adts_frame(raw_aac: &[u8]) -> Vec<u8> {
    let frame_length = 7 + raw_aac.len();
    let mut frame = vec![
        0xff,
        0xf1,
        0x50,
        0x80 | ((frame_length >> 11) as u8 & 0x03),
        (frame_length >> 3) as u8,
        ((frame_length & 0x07) as u8) << 5 | 0x1f,
        0xfc,
    ];
    frame.extend_from_slice(raw_aac);
    frame
}

fn encode_timestamp(prefix: u8, timestamp: u64) -> [u8; 5] {
    let timestamp = timestamp & ((1_u64 << 33) - 1);
    [
        (prefix << 4) | (((timestamp >> 30) as u8 & 0x07) << 1) | 1,
        (timestamp >> 22) as u8,
        (((timestamp >> 15) as u8 & 0x7f) << 1) | 1,
        (timestamp >> 7) as u8,
        ((timestamp as u8 & 0x7f) << 1) | 1,
    ]
}

fn finalize_section(section: &mut Vec<u8>) {
    let section_length = section.len() - 3 + 4;
    section[1] = 0xb0 | ((section_length >> 8) as u8 & 0x0f);
    section[2] = section_length as u8;
    let crc = mpeg_crc32(section);
    section.extend_from_slice(&crc.to_be_bytes());
}

#[allow(dead_code)]
fn _assert_helpers_compile(program: ProgramMap) {
    let mut maps = std::collections::BTreeMap::new();
    maps.insert(program.program_number, program.clone());
    let _ = select_program(&maps);
    let _ = parse_pmt(&[]);
    let _ = MpegTsDemuxError::NoPlayableProgram;
}
