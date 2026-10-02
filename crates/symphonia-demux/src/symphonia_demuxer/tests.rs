use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codec_core::{
    ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoDisplayOrientation,
};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, DemuxTrackListUpdate, Demuxer,
    PacketKeyframe, TrackId, TrackKind,
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
    METADATA_ID_NULL, Metadata, MetadataBuilder, MetadataInfo, MetadataLog, MetadataRevision,
    PerTrackMetadataBuilder, StandardTag, Tag,
};
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase, Timestamp};

use super::decode_point_before::{
    DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN, DECODE_POINT_BEFORE_MAX_RETRIES,
    DecodePointBeforeVerificationIssue, DecodePointBeforeVideoPacket,
    decode_point_before_initial_timestamp, decode_point_before_retry_timestamp_for_issue,
};
use super::matroska_source_probe::{
    MATROSKA_STREAM_SCAN_LIMIT_BYTES, MatroskaVideoMetadataScanDecision,
    decide_matroska_video_metadata_scan, read_stream_prefix,
};
use super::{
    FASTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG, FASTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG,
    FASTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG,
    FASTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG,
    FASTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG,
    FASTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG, FASTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG,
    FASTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG, FASTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG,
    SymphoniaDemuxer,
};
use crate::error::DemuxError;
use crate::matroska_metadata::{MatroskaCueIndex, MatroskaVideoTrack};
use crate::options::DemuxerOptions;

mod byte_source_failure;
mod decode_point_before;
mod events_errors;
mod open_metadata;
mod seek_modes;

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
    metadata_revisions_after_packets: VecDeque<MetadataRevision>,
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
            metadata_revisions_after_packets: VecDeque::new(),
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

    /// Публикует по одной metadata revision после чтения каждого следующего packet-а.
    fn with_metadata_revisions_after_packets(mut self, revisions: Vec<MetadataRevision>) -> Self {
        self.metadata_revisions_after_packets = VecDeque::from(revisions);
        self
    }

    fn with_display_orientation_metadata(
        mut self,
        track_id: u32,
        display_orientation: VideoDisplayOrientation,
    ) -> Self {
        let mut track_metadata = PerTrackMetadataBuilder::new(u64::from(track_id));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG,
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
            FASTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG,
            true,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG,
            9_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG,
            9_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG,
            16_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG,
            1_000.0_f64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG,
            0.005_f64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG,
            1_000_u64,
            None,
        ));
        track_metadata.add_tag(Tag::new_from_parts(
            FASTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG,
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
            Some(Ok(packet)) => {
                if let Some(revision) = self.metadata_revisions_after_packets.pop_front() {
                    // Live revision добавляется в конец time-ordered log-а; `current()` остаётся
                    // на старой revision, пока adapter не вызовет `pop()`.
                    self.metadata.push(revision);
                }
                Ok(Some(packet))
            }
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

/// Собирает media-level revision из typed Symphonia tags; raw payload намеренно не парсится.
fn metadata_revision(standard_tags: Vec<StandardTag>) -> MetadataRevision {
    let mut metadata = MetadataBuilder::new(FAKE_METADATA_INFO);
    for (index, standard_tag) in standard_tags.into_iter().enumerate() {
        metadata.add_tag(Tag::new_from_parts(
            format!("test-tag-{index}"),
            "ignored raw value",
            Some(standard_tag),
        ));
    }
    metadata.build()
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
