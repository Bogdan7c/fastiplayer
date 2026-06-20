use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codec_core::{
    ColorPrimaries, ColorRange, H265Profile, MatrixCoefficients, TransferFunction,
    VideoDisplayOrientation, VideoProfile,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, DemuxTrackListUpdate, Demuxer,
    MediaDemuxError, PacketKeyframe, TimelineNotSeekableReason, TrackId, TrackKind,
};
use symphonia::core::audio::{Channels, Position};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioCodecParameters;
use symphonia::core::codecs::audio::well_known as audio_codec;
use symphonia::core::codecs::subtitle::SubtitleCodecParameters;
use symphonia::core::codecs::subtitle::well_known as subtitle_codec;
use symphonia::core::codecs::video::VideoCodecParameters;
use symphonia::core::codecs::video::well_known as video_codec;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{
    FORMAT_ID_NULL, FormatInfo, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{
    METADATA_ID_NULL, Metadata, MetadataBuilder, MetadataInfo, MetadataLog,
    PerTrackMetadataBuilder, Tag,
};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase, Timestamp};

use super::decode_point_before::{
    DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN, DECODE_POINT_BEFORE_MAX_RETRIES,
    DecodePointBeforeVerificationIssue, DecodePointBeforeVideoPacket,
    decode_point_before_initial_timestamp, decode_point_before_retry_timestamp_for_issue,
};
use super::{
    MATROSKA_STREAM_SCAN_LIMIT_BYTES, MatroskaVideoMetadataScanDecision,
    RUSTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG, RUSTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG,
    RUSTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG,
    RUSTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG,
    RUSTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG,
    RUSTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG, RUSTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG,
    RUSTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG, RUSTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG,
    SymphoniaDemuxer, decide_matroska_video_metadata_scan, read_stream_prefix,
};
use crate::error::DemuxError;
use crate::matroska_metadata::{MatroskaCueIndex, MatroskaVideoTrack};
use crate::options::DemuxerOptions;

const MAX_UNIT_EVENTS_BEFORE_EOF: usize = 16_384;
const MAX_UNIT_EVENTS_AFTER_SEEK: usize = 512;
const FAKE_METADATA_INFO: MetadataInfo = MetadataInfo {
    metadata: METADATA_ID_NULL,
    short_name: "fake",
    long_name: "Fake metadata",
};

struct FakeFormatReader {
    format_info: FormatInfo,
    media_info: MediaInfo,
    tracks: Vec<Track>,
    reset_track_updates: VecDeque<Vec<Track>>,
    metadata: MetadataLog,
    packets: VecDeque<std::result::Result<Packet, SymphoniaError>>,
    seek_packet_scripts: VecDeque<VecDeque<std::result::Result<Packet, SymphoniaError>>>,
    seek_mode_log: Option<Arc<Mutex<Vec<SeekMode>>>>,
    seek_track_log: Option<Arc<Mutex<Vec<u32>>>>,
    seek_timestamp_log: Option<Arc<Mutex<Vec<i64>>>>,
    next_packet_call_count: Option<Arc<Mutex<usize>>>,
    seek_response_policy: FakeSeekResponsePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeSeekResponsePolicy {
    Zero,
    CoarseAfterTargetAccurateBefore,
}

impl FakeFormatReader {
    fn new(tracks: Vec<Track>, packets: Vec<std::result::Result<Packet, SymphoniaError>>) -> Self {
        Self {
            format_info: FormatInfo {
                format: FORMAT_ID_NULL,
                short_name: "fake",
                long_name: "Fake FormatReader",
            },
            media_info: MediaInfo::default(),
            tracks,
            reset_track_updates: VecDeque::new(),
            metadata: MetadataLog::default(),
            packets: VecDeque::from(packets),
            seek_packet_scripts: VecDeque::new(),
            seek_mode_log: None,
            seek_track_log: None,
            seek_timestamp_log: None,
            next_packet_call_count: None,
            seek_response_policy: FakeSeekResponsePolicy::Zero,
        }
    }

    fn with_seek_mode_log(mut self, seek_mode_log: Arc<Mutex<Vec<SeekMode>>>) -> Self {
        self.seek_mode_log = Some(seek_mode_log);
        self
    }

    fn with_seek_track_log(mut self, seek_track_log: Arc<Mutex<Vec<u32>>>) -> Self {
        self.seek_track_log = Some(seek_track_log);
        self
    }

    fn with_seek_timestamp_log(mut self, seek_timestamp_log: Arc<Mutex<Vec<i64>>>) -> Self {
        self.seek_timestamp_log = Some(seek_timestamp_log);
        self
    }

    fn with_next_packet_call_count(mut self, call_count: Arc<Mutex<usize>>) -> Self {
        self.next_packet_call_count = Some(call_count);
        self
    }

    fn with_seek_response_policy(mut self, policy: FakeSeekResponsePolicy) -> Self {
        self.seek_response_policy = policy;
        self
    }

    fn with_seek_packet_scripts(
        mut self,
        scripts: Vec<Vec<std::result::Result<Packet, SymphoniaError>>>,
    ) -> Self {
        self.seek_packet_scripts = scripts
            .into_iter()
            .map(VecDeque::from)
            .collect::<VecDeque<_>>();
        self
    }

    fn with_media_info(mut self, media_info: MediaInfo) -> Self {
        self.media_info = media_info;
        self
    }

    fn with_display_orientation_metadata(
        mut self,
        track_id: u32,
        display_orientation: VideoDisplayOrientation,
    ) -> Self {
        let mut track_metadata = PerTrackMetadataBuilder::new(u64::from(track_id));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG,
            u64::from(display_orientation.clockwise_degrees()),
            None,
        ));

        let mut metadata = MetadataBuilder::new(FAKE_METADATA_INFO);
        metadata.add_track(track_metadata.build());
        self.metadata.push_front(metadata.build());
        self
    }

    fn with_mp4_hdr_color_metadata(mut self, track_id: u32) -> Self {
        let mut track_metadata = PerTrackMetadataBuilder::new(u64::from(track_id));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG,
            true,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG,
            9_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG,
            9_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG,
            16_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG,
            1_000.0_f64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG,
            0.005_f64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG,
            1_000_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            RUSTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG,
            400_u64,
            None,
        ));

        let mut metadata = MetadataBuilder::new(FAKE_METADATA_INFO);
        metadata.add_track(track_metadata.build());
        self.metadata.push_front(metadata.build());
        self
    }

    fn with_reset_track_update(mut self, tracks: Vec<Track>) -> Self {
        self.reset_track_updates.push_back(tracks);
        self
    }

    fn seek_response(&self, mode: SeekMode, target: SeekTo) -> SeekedTo {
        let track_id = seek_target_track_id(&self.tracks, &target);
        let required_ts = required_seek_timestamp(&self.tracks, &target);
        let actual_ts = match self.seek_response_policy {
            FakeSeekResponsePolicy::Zero => Timestamp::ZERO,
            FakeSeekResponsePolicy::CoarseAfterTargetAccurateBefore => match mode {
                SeekMode::Coarse => required_ts.saturating_add(SymphoniaDuration::new(250)),
                SeekMode::Accurate => required_ts.saturating_sub(SymphoniaDuration::new(250)),
            },
        };

        SeekedTo {
            track_id,
            required_ts,
            actual_ts,
        }
    }
}

impl FormatReader for FakeFormatReader {
    fn format_info(&self) -> &FormatInfo {
        &self.format_info
    }

    fn media_info(&self) -> &MediaInfo {
        &self.media_info
    }

    fn metadata(&mut self) -> Metadata<'_> {
        self.metadata.metadata()
    }

    fn seek(
        &mut self,
        mode: SeekMode,
        target: SeekTo,
    ) -> symphonia::core::errors::Result<SeekedTo> {
        if let Some(ref seek_mode_log) = self.seek_mode_log {
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .push(mode);
        }
        if let Some(ref seek_track_log) = self.seek_track_log {
            seek_track_log
                .lock()
                .expect("seek track log mutex should not be poisoned")
                .push(seek_target_track_id(&self.tracks, &target));
        }
        if let Some(ref seek_timestamp_log) = self.seek_timestamp_log {
            seek_timestamp_log
                .lock()
                .expect("seek timestamp log mutex should not be poisoned")
                .push(required_seek_timestamp(&self.tracks, &target).get());
        }
        if let Some(packets) = self.seek_packet_scripts.pop_front() {
            self.packets = packets;
        }

        Ok(self.seek_response(mode, target))
    }

    fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    fn next_packet(&mut self) -> symphonia::core::errors::Result<Option<Packet>> {
        if let Some(ref call_count) = self.next_packet_call_count {
            let mut call_count = call_count
                .lock()
                .expect("next_packet call count mutex should not be poisoned");
            *call_count += 1;
        }

        match self.packets.pop_front() {
            Some(Ok(packet)) => Ok(Some(packet)),
            Some(Err(SymphoniaError::ResetRequired)) => {
                if let Some(next_tracks) = self.reset_track_updates.pop_front() {
                    self.tracks = next_tracks;
                }
                Err(SymphoniaError::ResetRequired)
            }
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }

    fn into_inner<'source>(self: Box<Self>) -> MediaSourceStream<'source>
    where
        Self: 'source,
    {
        unreachable!("tests не возвращают MediaSourceStream из FakeFormatReader");
    }
}

fn seek_target_track_id(tracks: &[Track], target: &SeekTo) -> u32 {
    match target {
        SeekTo::Time { track_id, .. } => track_id
            .and_then(|id| tracks.iter().find(|track| track.id == id))
            .or_else(|| tracks.first())
            .map(|track| track.id)
            .unwrap_or_default(),
        SeekTo::Timestamp { track_id, .. } => *track_id,
    }
}

fn required_seek_timestamp(tracks: &[Track], target: &SeekTo) -> Timestamp {
    match target {
        SeekTo::Time { time, track_id } => track_id
            .and_then(|id| tracks.iter().find(|track| track.id == id))
            .or_else(|| tracks.first())
            .and_then(|track| track.time_base)
            .and_then(|time_base| time_base.calc_timestamp(*time))
            .unwrap_or(Timestamp::ZERO),
        SeekTo::Timestamp { ts, .. } => *ts,
    }
}

fn test_webm_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/VP9/test.webm")
}

fn audio_fixture_path(file_name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-assets/audio")
        .join(file_name)
}

fn optional_fixture_path(path: PathBuf, fixture_label: &str) -> Option<PathBuf> {
    if path.exists() {
        return Some(path);
    }

    eprintln!(
        "skipping {fixture_label} test fixture: {} is absent",
        path.display()
    );
    None
}

fn open_optional_demuxer_fixture(path: PathBuf, fixture_label: &str) -> Option<SymphoniaDemuxer> {
    let path = optional_fixture_path(path, fixture_label)?;

    Some(SymphoniaDemuxer::from_file(&path).unwrap_or_else(|error| {
        panic!(
            "{fixture_label} fixture должен открыться через Symphonia: {error}; path: {}",
            path.display()
        )
    }))
}

fn open_optional_file_fixture(path: PathBuf, fixture_label: &str) -> Option<File> {
    let path = optional_fixture_path(path, fixture_label)?;

    Some(File::open(&path).unwrap_or_else(|error| {
        panic!(
            "{fixture_label} fixture должен открыться как файл: {error}; path: {}",
            path.display()
        )
    }))
}

fn ios_h265_mov_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../<MEDIA_DIR>/ios-hevc-main10-aac-4k60.mov")
}

fn drain_demuxer_to_eof_for_unit_test(demuxer: &mut SymphoniaDemuxer) {
    for event_index in 0..MAX_UNIT_EVENTS_BEFORE_EOF {
        match demuxer
            .next_event()
            .unwrap_or_else(|error| panic!("EOF drain event #{event_index} failed: {error}"))
        {
            DemuxReadEvent::EndOfStream => return,
            DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
        }
    }

    panic!("demuxer did not reach EOF within {MAX_UNIT_EVENTS_BEFORE_EOF} events in unit test");
}

fn assert_first_packet_after_seek_for_unit_test(demuxer: &mut SymphoniaDemuxer) {
    for event_index in 0..MAX_UNIT_EVENTS_AFTER_SEEK {
        match demuxer.next_event().unwrap_or_else(|error| {
            panic!("post-seek event #{event_index} failed in unit test: {error}")
        }) {
            DemuxReadEvent::Packet(_) => return,
            DemuxReadEvent::TracksChanged(_) => {}
            DemuxReadEvent::EndOfStream => {
                panic!("post-seek EOF arrived before any packet in unit test")
            }
        }
    }

    panic!("demuxer did not return a packet within {MAX_UNIT_EVENTS_AFTER_SEEK} post-seek events");
}

fn vp9_video_track(track_id: u32) -> Track {
    let mut video_params = VideoCodecParameters::default();
    video_params.for_codec(video_codec::CODEC_ID_VP9);

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Video(video_params));
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

fn aac_audio_track_with_timing(track_id: u32, duration: SymphoniaDuration) -> Track {
    let mut audio_params = AudioCodecParameters::new();
    audio_params.for_codec(audio_codec::CODEC_ID_AAC);
    audio_params.with_sample_rate(48_000);
    audio_params.with_channels(Channels::from(Position::FRONT_LEFT | Position::FRONT_RIGHT));

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Audio(audio_params));
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track.with_duration(duration);
    track
}

fn media_info_with_duration(duration: SymphoniaDuration) -> MediaInfo {
    let mut media_info = MediaInfo::default();
    media_info.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    media_info.with_duration(duration);
    media_info
}

fn unknown_track(track_id: u32) -> Track {
    let mut track = Track::new(track_id);
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

fn subtitle_track(track_id: u32) -> Track {
    let mut subtitle_params = SubtitleCodecParameters::new();
    subtitle_params.for_codec(subtitle_codec::CODEC_ID_WEBVTT);

    let mut track = Track::new(track_id);
    track.with_codec_params(CodecParameters::Subtitle(subtitle_params));
    track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
    track
}

fn fake_packet(track_id: u32, timestamp: i64, packet_bytes: Vec<u8>) -> Packet {
    Packet::new(
        track_id,
        Timestamp::new(timestamp),
        SymphoniaDuration::new(1),
        packet_bytes,
    )
}

fn small_vp9_keyframe_packet(track_id: u32, timestamp: i64) -> Packet {
    fake_packet(track_id, timestamp, build_vp9_keyframe())
}

fn small_vp9_inter_frame_packet(track_id: u32, timestamp: i64) -> Packet {
    fake_packet(track_id, timestamp, build_vp9_inter_frame())
}

fn build_vp9_keyframe() -> Vec<u8> {
    let mut bits = Vec::new();
    push_bits(&mut bits, 0b10, 2);
    push_profile(&mut bits, 0);
    bits.push(0);
    bits.push(0);
    bits.push(1);
    bits.push(0);
    push_bits(&mut bits, 0x498342, 24);
    push_bits(&mut bits, 1, 3);
    bits.push(0);
    push_bits(&mut bits, 63, 16);
    push_bits(&mut bits, 63, 16);
    bits.push(0);
    bits_to_bytes(&bits)
}

fn build_vp9_inter_frame() -> Vec<u8> {
    let mut bits = Vec::new();
    push_bits(&mut bits, 0b10, 2);
    push_profile(&mut bits, 0);
    bits.push(0);
    bits.push(1);
    bits.push(1);
    bits.push(0);
    push_bits(&mut bits, 0, 2);
    push_bits(&mut bits, 0x01, 8);
    push_bits(&mut bits, 1, 3);
    bits.push(1);
    push_bits(&mut bits, 2, 3);
    bits.push(0);
    push_bits(&mut bits, 3, 3);
    bits.push(1);
    bits.push(0);
    bits.push(0);
    bits.push(0);
    push_bits(&mut bits, 63, 16);
    push_bits(&mut bits, 63, 16);
    bits.push(0);
    bits_to_bytes(&bits)
}

fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| {
            let mut byte = 0_u8;
            for (index, bit) in chunk.iter().enumerate() {
                byte |= bit << (7 - index);
            }
            byte
        })
        .collect()
}

fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
    for shift in (0..width).rev() {
        bits.push(((value >> shift) & 1) as u8);
    }
}

fn push_profile(bits: &mut Vec<u8>, profile: u8) {
    bits.push(profile & 1);
    bits.push((profile >> 1) & 1);
    if profile == 3 {
        bits.push(0);
    }
}

fn fake_demuxer_with_options(
    packets: Vec<std::result::Result<Packet, SymphoniaError>>,
    matroska_tracks: HashMap<TrackId, MatroskaVideoTrack>,
    options: DemuxerOptions,
) -> SymphoniaDemuxer {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], packets);
    SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        matroska_tracks,
        DemuxSeekability::Seekable,
        options,
    )
    .expect("fake demuxer должен открыться")
}

fn fake_demuxer_with_seek_mode_log() -> (SymphoniaDemuxer, Arc<Mutex<Vec<SeekMode>>>) {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 0))]])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    (demuxer, seek_mode_log)
}

fn assert_symphonia_seek_mode(request: DemuxSeekRequest, expected_mode: SeekMode) {
    let (mut demuxer, seek_mode_log) = fake_demuxer_with_seek_mode_log();

    demuxer
        .seek_with_request(request)
        .expect("fake seek должен завершиться без ошибки");

    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .as_slice(),
        &[expected_mode]
    );
}

fn next_video_packet(demuxer: &mut SymphoniaDemuxer) -> media_core::Packet {
    loop {
        let packet = demuxer
            .next_packet()
            .expect("packet read должен завершиться без ошибки")
            .expect("test asset должен содержать video packet");

        if packet.kind == TrackKind::Video {
            return packet;
        }
    }
}

struct BoundedPrefixReader {
    bytes_remaining: usize,
    bytes_read: usize,
}

impl BoundedPrefixReader {
    fn new(bytes_remaining: usize) -> Self {
        Self {
            bytes_remaining,
            bytes_read: 0,
        }
    }
}

impl std::io::Read for BoundedPrefixReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let bytes_to_read = self.bytes_remaining.min(output.len());
        if bytes_to_read == 0 {
            return Ok(0);
        }

        output[..bytes_to_read].fill(0);
        self.bytes_remaining -= bytes_to_read;
        self.bytes_read += bytes_to_read;
        Ok(bytes_to_read)
    }
}

#[test]
fn non_matroska_audio_extensions_skip_matroska_video_scan() {
    let audio_track = aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000));

    for extension in ["ogg", "mp3", "wav"] {
        assert_eq!(
            decide_matroska_video_metadata_scan(extension, std::slice::from_ref(&audio_track)),
            MatroskaVideoMetadataScanDecision::SkipNonMatroskaContainer
        );
    }
}

#[test]
fn audio_only_webm_skips_matroska_video_scan_after_symphonia_probe() {
    let audio_track = aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000));

    assert_eq!(
        decide_matroska_video_metadata_scan("webm", &[audio_track]),
        MatroskaVideoMetadataScanDecision::SkipNoVideoCandidates
    );
}

#[test]
fn stream_prefix_scan_limit_stays_bounded() {
    assert_eq!(MATROSKA_STREAM_SCAN_LIMIT_BYTES, 256 * 1024);

    let mut reader = BoundedPrefixReader::new(MATROSKA_STREAM_SCAN_LIMIT_BYTES * 2);
    let (prefix, video_tracks_by_track) =
        read_stream_prefix(&mut reader).expect("bounded prefix read works");

    assert_eq!(prefix.len(), MATROSKA_STREAM_SCAN_LIMIT_BYTES);
    assert_eq!(reader.bytes_read, MATROSKA_STREAM_SCAN_LIMIT_BYTES);
    assert!(video_tracks_by_track.is_empty());
}

#[test]
fn audio_only_demuxer_opens_and_uses_track_duration_metadata() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        Vec::new(),
    );

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "audio-only",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("audio-only demuxer должен открыться без video track-а");

    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].kind, TrackKind::Audio);
    assert_eq!(demuxer.tracks()[0].codec_id, "A_AAC");
    assert_eq!(demuxer.tracks()[0].sample_rate, Some(48_000));
    assert_eq!(demuxer.tracks()[0].channels, Some(2));
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(30)));
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
}

#[test]
fn ios_quicktime_h265_mov_fixture_opens_supported_tracks() {
    let fixture_path = ios_h265_mov_fixture_path();
    if !fixture_path.exists() {
        eprintln!(
            "skipping iOS QuickTime HEVC fixture test: {} is absent",
            fixture_path.display()
        );
        return;
    }

    let demuxer = SymphoniaDemuxer::from_file(&fixture_path)
        .expect("iOS QuickTime .mov fixture должен открыться через Symphonia");

    assert_eq!(demuxer.tracks().len(), 2);
    assert!(demuxer.duration().is_some());

    let video_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .expect("iOS .mov fixture должен содержать поддерживаемый HEVC video track");
    assert_eq!(video_track.codec_id, "V_MPEGH/ISO/HEVC");
    assert!(
        video_track
            .codec_private
            .as_ref()
            .is_some_and(|codec_private| !codec_private.is_empty())
    );

    let video_metadata = video_track
        .video
        .as_ref()
        .expect("HEVC video track должен сохранить container metadata");
    assert_eq!(video_metadata.coded_width, Some(3840));
    assert_eq!(video_metadata.coded_height, Some(2160));
    assert_eq!(
        video_metadata.profile,
        Some(VideoProfile::H265(H265Profile::Main10))
    );

    let audio_track = demuxer
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Audio)
        .expect("iOS .mov fixture должен содержать поддерживаемый AAC audio track");
    assert_eq!(audio_track.codec_id, "A_AAC");
    assert_eq!(audio_track.sample_rate, Some(48_000));
    assert_eq!(audio_track.channels, Some(2));
}

#[test]
fn demuxer_uses_media_info_duration_when_tracks_do_not_have_duration() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_media_info(media_info_with_duration(SymphoniaDuration::new(12_000)));

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "media-info-duration",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться с container-level duration");

    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].kind, TrackKind::Video);
    assert_eq!(demuxer.tracks()[0].duration, None);
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(12)));
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
}

#[test]
fn demuxer_maps_per_track_display_orientation_metadata() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_display_orientation_metadata(1, VideoDisplayOrientation::Rotate270Clockwise);

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "video-orientation",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыть video track с orientation metadata");
    let video_metadata = demuxer.tracks()[0]
        .video
        .as_ref()
        .expect("orientation должна создать video metadata");

    assert_eq!(
        video_metadata.orientation,
        VideoDisplayOrientation::Rotate270Clockwise
    );
}

#[test]
fn demuxer_maps_mp4_per_track_hdr_color_metadata() {
    let reader =
        FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new()).with_mp4_hdr_color_metadata(1);

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "video-color",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыть video track с MP4 color metadata");
    let color = demuxer.tracks()[0]
        .video
        .as_ref()
        .and_then(|metadata| metadata.color.as_ref())
        .expect("MP4 HDR color metadata должна попасть в VideoTrackMetadata.color");

    assert_eq!(color.range, ColorRange::Full);
    assert_eq!(color.matrix, MatrixCoefficients::Bt2020);
    assert_eq!(color.primaries, ColorPrimaries::Bt2020);
    assert_eq!(color.transfer, TransferFunction::Pq);
    assert_eq!(
        color
            .hdr_metadata
            .as_ref()
            .and_then(|metadata| metadata.max_content_light_level_nits),
        Some(1_000)
    );
    assert_eq!(
        color
            .hdr_metadata
            .as_ref()
            .and_then(|metadata| metadata.max_frame_average_light_level_nits),
        Some(400)
    );
}

#[test]
fn demuxer_prefers_track_duration_over_media_info_duration() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        Vec::new(),
    )
    .with_media_info(media_info_with_duration(SymphoniaDuration::new(12_000)));

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "track-duration-wins",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен сохранить track-level duration");

    assert_eq!(demuxer.duration(), Some(Duration::from_secs(30)));
    assert_eq!(demuxer.tracks()[0].duration, Some(Duration::from_secs(30)));
}

#[test]
fn reset_required_refreshes_track_list_as_lifecycle_event() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        vec![Err(SymphoniaError::ResetRequired)],
    )
    .with_reset_track_update(vec![aac_audio_track_with_timing(
        3,
        SymphoniaDuration::new(42_000),
    )]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "reset-event",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let event = demuxer
        .next_event()
        .expect("ResetRequired должен стать lifecycle event");

    match event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert_eq!(track_update.tracks.len(), 1);
            assert_eq!(track_update.tracks[0].id, TrackId::new(3));
            assert_eq!(track_update.duration, Some(Duration::from_secs(42)));
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    assert_eq!(demuxer.tracks()[0].id, TrackId::new(3));
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(42)));
}

#[test]
fn reset_required_keeps_media_info_duration_when_tracks_do_not_have_duration() {
    let reader = FakeFormatReader::new(
        vec![vp9_video_track(1)],
        vec![Err(SymphoniaError::ResetRequired)],
    )
    .with_media_info(media_info_with_duration(SymphoniaDuration::new(18_000)))
    .with_reset_track_update(vec![vp9_video_track(4)]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "reset-media-info-duration",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться с container-level duration");

    let event = demuxer
        .next_event()
        .expect("ResetRequired должен обновить track list без потери media duration");

    match event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert_eq!(track_update.tracks.len(), 1);
            assert_eq!(track_update.tracks[0].id, TrackId::new(4));
            assert_eq!(track_update.duration, Some(Duration::from_secs(18)));
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    assert_eq!(demuxer.tracks()[0].id, TrackId::new(4));
    assert_eq!(demuxer.tracks()[0].duration, None);
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(18)));
}

#[test]
fn next_packet_compatibility_skips_reset_lifecycle_event() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        vec![
            Err(SymphoniaError::ResetRequired),
            Ok(fake_packet(3, 5, b"audio".to_vec())),
        ],
    )
    .with_reset_track_update(vec![aac_audio_track_with_timing(
        3,
        SymphoniaDuration::new(42_000),
    )]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "reset-next-packet",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let packet = demuxer
        .next_packet()
        .expect("compat next_packet не должен считать ResetRequired ошибкой")
        .expect("следующий packet нового track-а должен быть доступен");

    assert_eq!(packet.track_id, TrackId::new(3));
    assert_eq!(packet.kind, TrackKind::Audio);
}

#[test]
fn accurate_demux_seek_uses_symphonia_accurate_mode() {
    assert_symphonia_seek_mode(
        DemuxSeekRequest::accurate(Duration::from_millis(500)),
        SeekMode::Accurate,
    );
}

#[test]
fn decode_point_before_demux_seek_uses_symphonia_accurate_mode() {
    assert_symphonia_seek_mode(
        DemuxSeekRequest::decode_point_before(Duration::from_millis(500)),
        SeekMode::Accurate,
    );
}

#[test]
fn preview_demux_seek_uses_symphonia_coarse_mode() {
    assert_symphonia_seek_mode(
        DemuxSeekRequest::preview(Duration::from_millis(500)),
        SeekMode::Coarse,
    );
}

#[test]
fn preview_seek_clears_decode_point_prebuffer_without_verification() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 400))],
            vec![Ok(small_vp9_keyframe_packet(1, 11_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "preview-clears-prebuffer",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("первый seek должен создать verification prebuffer");
    demuxer
        .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
        .expect("preview-mode seek не должен запускать DecodePointBefore verification");
    let packet = demuxer
        .next_packet()
        .expect("preview packet должен читаться после seek")
        .expect("preview-mode seek не должен съесть post-seek packet");

    assert_eq!(packet.pts, Duration::from_secs(11));
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .as_slice(),
        &[SeekMode::Accurate, SeekMode::Coarse]
    );
}

#[test]
fn seek_result_uses_selected_video_track_timestamp_when_audio_track_is_first() {
    let reader = FakeFormatReader::new(
        vec![
            aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
            vp9_video_track(1),
        ],
        Vec::new(),
    );
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "selected-video-track-timestamp",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
        .expect("preview-mode seek должен использовать selected video track");

    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("seek result должен сохранить raw timestamp")
            .track_id,
        TrackId::new(1)
    );
}

#[test]
fn decode_point_before_seek_rejects_coarse_after_target_position() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 0))]])
        .with_seek_response_policy(FakeSeekResponsePolicy::CoarseAfterTargetAccurateBefore);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "coarse-after-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");
    let target = Duration::from_millis(100);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("decode-point seek должен завершиться без ошибки");

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "DecodePointBefore не должен принимать backend-позицию после target"
    );
}

#[test]
fn decode_point_before_seek_retries_when_first_video_packet_overshoots_target() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 11_000))],
            vec![Ok(small_vp9_keyframe_packet(1, 4_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "packet-after-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");
    let target = Duration::from_secs(10);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("decode-point seek должен retry-нуться до packet перед target");

    assert!(
        seek_result.actual_position.as_duration() <= target,
        "retry должен вернуть packet-level actual position не позже target"
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .as_slice(),
        &[SeekMode::Accurate, SeekMode::Accurate]
    );
}

#[test]
fn decode_point_before_after_target_retry_expands_existing_preroll() {
    let issue = DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
        packet: DecodePointBeforeVideoPacket {
            pts: Duration::from_millis(29_233),
            track_pts: None,
            keyframe: PacketKeyframe::Keyframe,
        },
    };

    let retry_timestamp = decode_point_before_retry_timestamp_for_issue(
        Duration::from_millis(24_225),
        Duration::from_millis(29_225),
        issue,
        0,
        Duration::from_secs(5),
        Duration::from_secs(10),
    )
    .expect("after-target packet должен дать retry timestamp");

    assert_eq!(
        retry_timestamp,
        Duration::from_millis(19_225),
        "retry должен расширить pre-roll, а не отступить только на маленький overshoot"
    );
}

#[test]
fn decode_point_before_too_far_rescue_targets_accepted_preroll_window() {
    let issue = DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget {
        packet: DecodePointBeforeVideoPacket {
            pts: Duration::from_millis(41_224),
            track_pts: None,
            keyframe: PacketKeyframe::NotKeyframe,
        },
    };

    let retry_timestamp = decode_point_before_retry_timestamp_for_issue(
        Duration::from_millis(31_931),
        Duration::from_millis(66_932),
        issue,
        3,
        Duration::from_secs(5),
        Duration::from_secs(10),
    )
    .expect("too-far packet должен дать rescue retry timestamp");

    assert_eq!(
        retry_timestamp,
        Duration::from_millis(56_932),
        "rescue должен прыгать к началу допустимого окна, а не в сам target"
    );
}

#[test]
fn decode_point_before_initial_seek_targets_requested_not_far_preroll() {
    // RC1: initial backend seek целится практически в сам target, чтобы stss/cues
    // приземлились на ближайший keyframe ≤ target, а не на GOP за несколько секунд раньше.
    let requested = Duration::from_millis(94_351);
    let initial =
        decode_point_before_initial_timestamp(requested, DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN);

    assert!(
        initial < requested,
        "initial seek должен оставаться не позже target: {initial:?} < {requested:?}"
    );
    assert!(
        requested.saturating_sub(initial) <= Duration::from_millis(2),
        "initial seek должен быть почти в target, а не на целый pre-roll раньше: {:?}",
        requested.saturating_sub(initial)
    );
}

#[test]
fn decode_point_before_matroska_cue_index_overrides_initial_backend_target() {
    let seek_timestamp_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 8_000))]])
        .with_seek_timestamp_log(Arc::clone(&seek_timestamp_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "matroska-cue-anchor",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_secs(2), Duration::from_secs(8)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10,
        )))
        .expect("cue-backed DecodePointBefore должен принять keyframe перед target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[8_000],
        "первый backend seek должен идти к ближайшему Matroska cue, а не к target-1ms"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_secs(8),
        "actual остаётся verified decode anchor, public target хранится отдельно"
    );
    assert_eq!(
        seek_result.requested_position.as_duration(),
        Duration::from_secs(10),
        "requested_position не должен превращаться в keyframe-before target"
    );
}

#[test]
fn decode_point_before_matroska_retry_uses_previous_cue_before_backoff() {
    let seek_timestamp_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_inter_frame_packet(1, 3_040))],
            vec![
                Ok(small_vp9_keyframe_packet(1, 37)),
                Ok(small_vp9_keyframe_packet(1, 2_039)),
            ],
        ])
        .with_seek_timestamp_log(Arc::clone(&seek_timestamp_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "matroska-previous-cue-retry",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_millis(37), Duration::from_millis(2_039)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            3,
        )))
        .expect("previous cue retry должен найти keyframe перед target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[2_039, 37],
        "rejected nearest cue должен перейти к предыдущему cue, а не к 5s backoff"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_millis(2_039),
        "actual должен стать verified packet из previous-cue attempt"
    );
}

#[test]
fn decode_point_before_matroska_too_far_uses_rescue_not_previous_cue() {
    let seek_timestamp_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 8_000))],
            vec![Ok(small_vp9_keyframe_packet(1, 9_500))],
        ])
        .with_seek_timestamp_log(Arc::clone(&seek_timestamp_log));
    let options = DemuxerOptions::default()
        .with_decode_point_before_preroll(Duration::from_secs(1))
        .with_decode_point_before_max_accepted_preroll(Duration::from_secs(1));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "matroska-too-far-rescue",
        HashMap::new(),
        DemuxSeekability::Seekable,
        options,
    )
    .expect("fake demuxer должен открыться");
    demuxer.matroska_cue_index = MatroskaCueIndex::from_track_cues_for_tests(
        TrackId::new(1),
        [Duration::from_secs(2), Duration::from_secs(8)],
    );

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10,
        )))
        .expect("too-far cue anchor должен перейти в rescue retry ближе к target");

    assert_eq!(
        seek_timestamp_log
            .lock()
            .expect("seek timestamp log lock")
            .as_slice(),
        &[8_000, 9_000],
        "too-far ошибка должна использовать rescue window, а не предыдущий Matroska cue"
    );
    assert_eq!(
        seek_result.actual_position.as_duration(),
        Duration::from_millis(9_500),
        "rescue retry должен принять keyframe внутри разрешённого pre-roll окна"
    );
}

#[test]
fn decode_point_before_seek_fails_when_actual_is_before_but_video_packet_is_after_target() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let scripts = (0..=DECODE_POINT_BEFORE_MAX_RETRIES)
        .map(|_| vec![Ok(small_vp9_keyframe_packet(1, 11_000))])
        .collect::<Vec<_>>();
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(scripts)
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "actual-before-packet-after",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect_err("video packet после target должен отклонить DecodePointBefore");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure должен быть typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
    assert!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len()
            > 1
    );
}

#[test]
fn decode_point_before_seek_success_prebuffers_verified_video_packet() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 400))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "packet-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("keyframe packet до target должен принять DecodePointBefore");
    let packet = demuxer
        .next_packet()
        .expect("prebuffered packet должен читаться без ошибки")
        .expect("verification не должна терять packet");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("video packet raw timestamp должен обновить actual")
            .track_id,
        TrackId::new(1)
    );
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(400));
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

#[test]
fn decode_point_before_seek_accepts_startup_keyframe_after_zero() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 33))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "startup-keyframe-after-zero",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect("стартовый keyframe сразу после zero seek должен приниматься");
    let packet = demuxer
        .next_packet()
        .expect("startup packet должен остаться в prebuffer")
        .expect("verification не должна терять startup packet");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(33)
    );
    assert_eq!(packet.kind, TrackKind::Video);
    assert_eq!(packet.pts, Duration::from_millis(33));
    assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
}

#[test]
fn decode_point_before_seek_rejects_startup_keyframe_beyond_lead_window() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 500))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "startup-keyframe-too-late",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::ZERO))
        .expect_err("слишком поздний startup keyframe не должен считаться началом");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("verification failure должен быть typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
}

#[test]
fn decode_point_before_returns_verification_events_in_read_order() {
    let reader = FakeFormatReader::new(
        vec![
            vp9_video_track(1),
            aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
        ],
        Vec::new(),
    )
    .with_seek_packet_scripts(vec![vec![
        Ok(fake_packet(2, 100, b"audio".to_vec())),
        Ok(small_vp9_keyframe_packet(1, 400)),
    ]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "verification-order",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("verification должен принять selected video packet");

    let first_event = demuxer
        .next_event()
        .expect("первый buffered event должен читаться");
    let second_event = demuxer
        .next_event()
        .expect("второй buffered event должен читаться");

    match first_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(2));
            assert_eq!(packet.kind, TrackKind::Audio);
            assert_eq!(packet.pts, Duration::from_millis(100));
        }
        unexpected_event => panic!("ожидали audio packet, получили {unexpected_event:?}"),
    }
    match second_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(1));
            assert_eq!(packet.kind, TrackKind::Video);
            assert_eq!(packet.pts, Duration::from_millis(400));
        }
        unexpected_event => panic!("ожидали video packet, получили {unexpected_event:?}"),
    }
}

#[test]
fn decode_point_before_verifies_selected_video_track_not_any_video_track() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1), vp9_video_track(2)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Ok(small_vp9_keyframe_packet(2, 11_000)),
            Ok(small_vp9_keyframe_packet(1, 400)),
        ]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "selected-video-verification",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("unselected video packet не должен решать verification selected track-а");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("verified actual должен быть timestamp selected video track-а")
            .track_id,
        TrackId::new(1)
    );
}

#[test]
fn decode_point_before_seek_accepts_unknown_keyframe_before_target() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![Ok(fake_packet(1, 400, b"\x00".to_vec()))]]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "unknown-keyframe-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("unknown keyframe до target не должен блокировать seek полностью");
    let packet = demuxer
        .next_packet()
        .expect("prebuffered packet должен читаться")
        .expect("packet должен вернуться pipeline");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(400)
    );
    assert_eq!(packet.keyframe, PacketKeyframe::Unknown);
}

#[test]
fn reset_required_after_preview_seek_is_returned_as_tracks_changed_event() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Err(SymphoniaError::ResetRequired),
            Ok(small_vp9_keyframe_packet(4, 600)),
        ]])
        .with_reset_track_update(vec![vp9_video_track(4)]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "post-seek-reset",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    demuxer
        .seek_with_request(DemuxSeekRequest::preview(Duration::from_millis(500)))
        .expect("preview-mode seek должен завершиться до post-seek read");

    let reset_event = demuxer
        .next_event()
        .expect("ResetRequired после seek должен стать lifecycle event");
    let packet_event = demuxer
        .next_event()
        .expect("packet после TracksChanged должен остаться доступным");

    match reset_event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert_eq!(track_update.tracks[0].id, TrackId::new(4));
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(600));
        }
        unexpected_event => {
            panic!("ожидали packet нового track-а, получили {unexpected_event:?}")
        }
    }
}

#[test]
fn reset_required_during_decode_point_verification_is_buffered_before_packet() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Err(SymphoniaError::ResetRequired),
            Ok(small_vp9_keyframe_packet(4, 400)),
        ]])
        .with_reset_track_update(vec![vp9_video_track(4)]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "verification-reset",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(500),
        ))
        .expect("verification должен пережить ResetRequired и принять новый video track");
    let reset_event = demuxer
        .next_event()
        .expect("TracksChanged должен вернуться перед verified packet-ом");
    let packet_event = demuxer
        .next_event()
        .expect("verified packet должен вернуться после TracksChanged");

    assert_eq!(
        seek_result
            .actual_track_timestamp
            .expect("verified actual должен принадлежать новому video track-у")
            .track_id,
        TrackId::new(4)
    );
    assert!(matches!(reset_event, DemuxReadEvent::TracksChanged(_)));
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(400));
        }
        unexpected_event => panic!("ожидали verified packet, получили {unexpected_event:?}"),
    }
}

#[test]
fn tracks_changed_from_failed_decode_point_verification_survives_retry() {
    let seek_track_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![
                Err(SymphoniaError::ResetRequired),
                Ok(small_vp9_inter_frame_packet(4, 400)),
            ],
            vec![Ok(small_vp9_keyframe_packet(4, 300))],
        ])
        .with_reset_track_update(vec![
            aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
            vp9_video_track(4),
        ])
        .with_seek_track_log(Arc::clone(&seek_track_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "verification-reset-before-retry",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("retry должен найти keyframe после ResetRequired");
    let reset_event = demuxer
        .next_event()
        .expect("TracksChanged из rejected attempt должен сохраниться");
    let packet_event = demuxer
        .next_event()
        .expect("verified packet успешной retry-попытки должен остаться доступен");

    match reset_event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert!(
                track_update
                    .tracks
                    .iter()
                    .any(|track| track.kind == TrackKind::Video && track.id == TrackId::new(4))
            );
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    match packet_event {
        DemuxReadEvent::Packet(packet) => {
            assert_eq!(packet.track_id, TrackId::new(4));
            assert_eq!(packet.pts, Duration::from_millis(300));
            assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
        }
        unexpected_event => panic!("ожидали verified packet, получили {unexpected_event:?}"),
    }
    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(300)
    );
    assert_eq!(
        seek_track_log
            .lock()
            .expect("seek track log mutex should not be poisoned")
            .as_slice(),
        &[1, 4]
    );
}

#[test]
fn decode_point_before_seek_retries_when_first_video_packet_is_not_keyframe() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_inter_frame_packet(1, 400))],
            vec![Ok(small_vp9_keyframe_packet(1, 300))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "inter-frame-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("retry должен найти keyframe до target");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(300)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        2
    );
}

#[test]
fn decode_point_before_accepts_keyframe_after_initial_inter_frame_in_prefix() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![vec![
            Ok(small_vp9_inter_frame_packet(1, 111_445)),
            Ok(small_vp9_keyframe_packet(1, 112_145)),
        ]])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "inter-frame-then-keyframe-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(116_449),
        ))
        .expect("verification должен принять keyframe внутри bounded prefix-а");
    let first_packet = demuxer
        .next_packet()
        .expect("prebuffered inter-frame должен читаться")
        .expect("verification prefix должен сохранить первый packet");
    let accepted_packet = demuxer
        .next_packet()
        .expect("prebuffered keyframe должен читаться")
        .expect("verification prefix должен сохранить accepted keyframe");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(112_145)
    );
    assert_eq!(first_packet.keyframe, PacketKeyframe::NotKeyframe);
    assert_eq!(accepted_packet.keyframe, PacketKeyframe::Keyframe);
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        1
    );
}

#[test]
fn decode_point_before_default_prefix_limit_reaches_later_keyframe() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let mut long_prefix = Vec::new();
    for frame_index in 0..180 {
        long_prefix.push(Ok(small_vp9_inter_frame_packet(
            1,
            65_181 + frame_index * 16,
        )));
    }
    long_prefix.push(Ok(small_vp9_keyframe_packet(1, 68_061)));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![long_prefix])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "long-prefix-keyframe-before-target",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(69_143),
        ))
        .expect("default verification limit должен дойти до keyframe текущего GOP");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(68_061)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        1
    );
}

#[test]
fn decode_point_before_retries_when_prefix_reaches_target_before_keyframe() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![
                Ok(small_vp9_inter_frame_packet(1, 9_900)),
                Ok(small_vp9_inter_frame_packet(1, 10_100)),
            ],
            vec![Ok(small_vp9_keyframe_packet(1, 9_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "inter-frame-crosses-target-before-keyframe",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect("after-target packet до keyframe должен вызвать retry");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(9_000)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        2
    );
}

#[test]
fn decode_point_before_retries_when_first_video_packet_is_too_far_before_target() {
    let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 0))],
            vec![Ok(small_vp9_keyframe_packet(1, 96_000))],
        ])
        .with_seek_mode_log(Arc::clone(&seek_mode_log));
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "too-far-before-target-rescue",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(96_784),
        ))
        .expect("too-far decode point должен retry-нуться ближе к target");

    assert_eq!(
        seek_result.actual_position,
        media_core::MediaTime::from_millis(96_000)
    );
    assert_eq!(
        seek_mode_log
            .lock()
            .expect("seek mode log mutex should not be poisoned")
            .len(),
        2
    );
}

#[test]
fn decode_point_before_fails_instead_of_accepting_start_of_file_for_middle_seek() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_seek_packet_scripts(vec![
            vec![Ok(small_vp9_keyframe_packet(1, 0))],
            vec![Ok(small_vp9_keyframe_packet(1, 0))],
        ]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "too-far-before-target-failure",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(96_784),
        ))
        .expect_err("seek в середину файла не должен принимать packet с начала файла");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("too-far verification failure должен быть typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_too_far_before_target",
            ..
        }
    ));
}

#[test]
fn decode_point_before_uses_video_packet_when_audio_actual_is_earlier() {
    let scripts = (0..=DECODE_POINT_BEFORE_MAX_RETRIES)
        .map(|_| {
            vec![
                Ok(fake_packet(1, 100, b"audio".to_vec())),
                Ok(small_vp9_keyframe_packet(2, 11_000)),
            ]
        })
        .collect::<Vec<_>>();
    let reader = FakeFormatReader::new(
        vec![
            aac_audio_track_with_timing(1, SymphoniaDuration::new(30_000)),
            vp9_video_track(2),
        ],
        Vec::new(),
    )
    .with_seek_packet_scripts(scripts);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "audio-actual-video-after",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(
            Duration::from_millis(10_000),
        ))
        .expect_err("ранний audio actual не должен маскировать video packet после target");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("video verification failure должен быть typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::DecodePointBeforeVerificationFailed {
            reason: "first_video_after_target",
            ..
        }
    ));
}

#[test]
fn normal_eof_returns_none_without_error() {
    let mut demuxer =
        fake_demuxer_with_options(Vec::new(), HashMap::new(), DemuxerOptions::default());

    let packet = demuxer
        .next_packet()
        .expect("normal EOF не должен быть ошибкой");

    assert!(packet.is_none());
}

#[test]
fn seek_after_eof_rebuilds_seekable_reader_and_preserves_track_layout() {
    let Some(mut demuxer) =
        open_optional_demuxer_fixture(audio_fixture_path("music_sample.m4a"), "seekable m4a")
    else {
        return;
    };
    let tracks_before_eof = demuxer.tracks().to_vec();
    let duration_before_eof = demuxer.duration();

    drain_demuxer_to_eof_for_unit_test(&mut demuxer);

    assert!(demuxer.end_of_stream_reached);
    assert!(demuxer.format.is_some());

    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
        .expect("seek after EOF должен rebuild-ить seekable reader");

    assert!(!demuxer.end_of_stream_reached);
    assert_eq!(demuxer.tracks(), tracks_before_eof.as_slice());
    assert_eq!(demuxer.duration(), duration_before_eof);
    assert!(demuxer.format.is_some());

    assert_first_packet_after_seek_for_unit_test(&mut demuxer);
}

#[test]
fn unseekable_stream_seek_after_eof_does_not_rebuild_or_become_seekable() {
    let Some(fixture_file) =
        open_optional_file_fixture(audio_fixture_path("music_sample.mp3"), "mp3")
    else {
        return;
    };
    let mut demuxer = SymphoniaDemuxer::from_stream(fixture_file, "mp3", "unit unseekable mp3")
        .expect("unseekable stream fixture должен открыться");
    let tracks_before_eof = demuxer.tracks().to_vec();
    let duration_before_eof = demuxer.duration();

    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable
        }
    ));

    drain_demuxer_to_eof_for_unit_test(&mut demuxer);

    let seek_error = demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
        .expect_err("unseekable stream не должен превращаться в seekable после EOF");

    assert!(
        seek_error.downcast_ref::<MediaDemuxError>().is_some()
            || seek_error.downcast_ref::<DemuxError>().is_some(),
        "seek failure должен остаться typed demux error: {seek_error}"
    );
    assert!(demuxer.end_of_stream_reached);
    assert!(demuxer.format.is_some());
    assert_eq!(demuxer.tracks(), tracks_before_eof.as_slice());
    assert_eq!(demuxer.duration(), duration_before_eof);
    assert!(matches!(
        demuxer.seekability(),
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable
        }
    ));
}

#[test]
fn seek_reprobe_preserves_pending_tracks_changed_event() {
    let Some(mut demuxer) =
        open_optional_demuxer_fixture(audio_fixture_path("music_sample.m4a"), "seekable m4a")
    else {
        return;
    };

    drain_demuxer_to_eof_for_unit_test(&mut demuxer);

    let retained_update = DemuxTrackListUpdate::new(demuxer.tracks().to_vec(), demuxer.duration());
    demuxer
        .pending_events
        .push_back(DemuxReadEvent::TracksChanged(retained_update.clone()));

    demuxer
        .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
        .expect("seek after EOF должен сохранить pending lifecycle event");

    match demuxer
        .next_event()
        .expect("retained TracksChanged должен читаться после seek")
    {
        DemuxReadEvent::TracksChanged(actual_update) => {
            assert_eq!(actual_update, retained_update);
        }
        unexpected_event => {
            panic!("ожидали retained TracksChanged, получили {unexpected_event:?}")
        }
    }

    assert_first_packet_after_seek_for_unit_test(&mut demuxer);
}

#[test]
fn unknown_track_does_not_break_open() {
    let reader = FakeFormatReader::new(vec![unknown_track(9)], Vec::new());
    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("unknown track не должен ломать open");

    assert!(demuxer.tracks().is_empty());
}

#[test]
fn subtitle_packets_are_skipped_without_unknown_track_error() {
    let reader = FakeFormatReader::new(
        vec![subtitle_track(9)],
        vec![Ok(fake_packet(9, 0, b"subtitle".to_vec()))],
    );
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("subtitle track не должен ломать open");

    let packet = demuxer
        .next_packet()
        .expect("subtitle packet должен быть пропущен без fatal ошибки");

    assert!(packet.is_none());
}

#[test]
fn unexpected_eof_error_is_kept_as_defensive_eof_fallback() {
    let mut demuxer = fake_demuxer_with_options(
        vec![Err(SymphoniaError::IoError(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "legacy eof",
        )))],
        HashMap::new(),
        DemuxerOptions::default(),
    );

    let packet = demuxer
        .next_packet()
        .expect("defensive UnexpectedEof fallback должен остаться EOF");

    assert!(packet.is_none());
}

#[test]
fn decode_error_from_format_reader_is_parse_error_without_retry() {
    let options =
        DemuxerOptions::from_max_consecutive_corrupted_packets(2).expect("test limit ненулевой");
    let next_packet_call_count = Arc::new(Mutex::new(0));
    let reader = FakeFormatReader::new(
        vec![vp9_video_track(1)],
        vec![
            Err(SymphoniaError::DecodeError("isomp4: no atom pending read")),
            Ok(small_vp9_keyframe_packet(1, 10)),
        ],
    )
    .with_next_packet_call_count(next_packet_call_count.clone());
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "fake",
        HashMap::new(),
        DemuxSeekability::Seekable,
        options,
    )
    .expect("fake demuxer должен открыться");

    let error = demuxer
        .next_packet()
        .expect_err("structural DecodeError из next_packet должен быть fatal");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("fatal должен остаться typed DemuxError");

    match demux_error {
        DemuxError::Parse(SymphoniaError::DecodeError(reason)) => {
            assert_eq!(*reason, "isomp4: no atom pending read");
        }
        unexpected_error => panic!("ожидали Parse(DecodeError), получили {unexpected_error:?}"),
    }

    assert_eq!(
        *next_packet_call_count
            .lock()
            .expect("next_packet call count mutex should not be poisoned"),
        1
    );
}

#[test]
fn packet_for_unknown_track_is_fatal() {
    let mut demuxer = fake_demuxer_with_options(
        vec![Ok(fake_packet(99, 0, b"\x00".to_vec()))],
        HashMap::new(),
        DemuxerOptions::default(),
    );

    let error = demuxer
        .next_packet()
        .expect_err("unknown track должен быть fatal");
    let demux_error = error
        .downcast_ref::<DemuxError>()
        .expect("fatal должен быть typed DemuxError");

    assert!(matches!(
        demux_error,
        DemuxError::UnknownPacketTrack { track_id: 99 }
    ));
}

#[test]
fn uncertain_vp9_keyframe_probe_is_returned_without_demux_error() {
    let matroska_tracks = HashMap::from([(
        TrackId::new(1),
        MatroskaVideoTrack {
            codec_id: Some("V_VP9".to_string()),
            metadata: None,
        },
    )]);
    let mut demuxer = fake_demuxer_with_options(
        vec![
            Ok(fake_packet(1, 0, b"\x00".to_vec())),
            Ok(fake_packet(1, 10, b"\x00".to_vec())),
            Ok(small_vp9_keyframe_packet(1, 20)),
        ],
        matroska_tracks,
        DemuxerOptions::default(),
    );

    let first_packet = demuxer
        .next_packet()
        .expect("неуверенная keyframe-проба не должна становиться fatal corruption")
        .expect("packet с неизвестным keyframe должен быть возвращён");
    let second_packet = demuxer
        .next_packet()
        .expect("повторная неуверенная keyframe-проба не должна копить corruption counter")
        .expect("второй packet с неизвестным keyframe должен быть возвращён");

    assert_eq!(first_packet.pts, Duration::ZERO);
    assert_eq!(first_packet.keyframe, PacketKeyframe::Unknown);
    assert_eq!(second_packet.pts, Duration::from_millis(10));
    assert_eq!(second_packet.keyframe, PacketKeyframe::Unknown);
}

#[test]
fn decode_point_before_seek_starts_video_before_target() {
    let Some(mut demuxer) =
        open_optional_demuxer_fixture(test_webm_path(), "VP9 WebM decode-point")
    else {
        return;
    };
    let target = Duration::from_millis(500);

    let seek_result = demuxer
        .seek_with_request(DemuxSeekRequest::decode_point_before(target))
        .expect("decode-point seek должен завершиться без ошибки");
    let packet = next_video_packet(&mut demuxer);

    assert_eq!(
        seek_result.requested_position,
        media_core::MediaTime::from_duration(target)
    );
    assert!(
        seek_result.actual_position.as_duration() <= target,
        "container должен поставить чтение не позже user target"
    );
    assert!(
        packet.pts <= target,
        "первый video packet после bootstrap seek должен быть pre-roll кадром"
    );
    // Keyframe flag теперь восстанавливается только codec-aware packet mapper-ом:
    // Symphonia 0.6 больше не отдаёт container keyframe flag в public Packet.
}

#[test]
fn decode_point_before_seek_actual_position_stays_before_video_targets() {
    let targets = [
        Duration::ZERO,
        Duration::from_secs(106),
        Duration::from_millis(212_500),
    ];

    for target in targets {
        let Some(mut demuxer) =
            open_optional_demuxer_fixture(test_webm_path(), "VP9 WebM decode-point")
        else {
            return;
        };

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .expect("decode-point seek должен завершиться без ошибки");

        assert!(
            seek_result.actual_position.as_duration() <= target,
            "actual demux position {:?} не должна быть после requested target {:?}",
            seek_result.actual_position.as_duration(),
            target
        );
    }
}
