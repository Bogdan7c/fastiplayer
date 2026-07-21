//! Demuxer для adaptive streaming с раздельными video/audio источниками.
//!
//! Этот demuxer объединяет независимые video-only и audio-only `SymphoniaDemuxer`
//! в один поток packets, чтобы app layer не зависел от происхождения streams.

use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, Packet,
    TrackInfo, TrackKind,
};

use crate::symphonia_demuxer::SymphoniaDemuxer;

/// Track id для video после remap.
const REMAPPED_VIDEO_TRACK_ID: u32 = 1;

/// Track id для audio после remap.
const REMAPPED_AUDIO_TRACK_ID: u32 = 2;

/// Максимальный временной разрыв одного заранее считанного component packet-а.
const DUAL_STREAM_MAX_COMPONENT_LEAD: Duration = Duration::from_secs(5 * 60);

/// Жёсткий memory ceiling для одного pending packet до S21R readiness migration.
const DUAL_STREAM_PENDING_BYTE_LIMIT: usize = 16 * 1024 * 1024;

/// Compatibility adapter для прежнего VP9+audio WebM open path.
///
/// Codec-specific admission остаётся здесь, а track remap/interleave/seek/lifecycle
/// принадлежат neutral `demux-api::CompositeAvDemuxer`.
pub struct DualStreamDemuxer {
    /// Neutral owner всей A/V composition state и invariants.
    inner: demux_api::CompositeAvDemuxer,
}

impl DualStreamDemuxer {
    /// Сохраняет прежний VP9+audio admission и передаёт composition neutral owner-у.
    pub fn new(video_demuxer: SymphoniaDemuxer, audio_demuxer: SymphoniaDemuxer) -> Result<Self> {
        // Этот adapter намеренно остаётся VP9-specific ради compatibility текущего yt-dlp path.
        let video_track_id = video_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Video && track.codec_id == "V_VP9")
            .map(|track| track.id)
            .ok_or_else(|| anyhow::anyhow!("Streaming video WebM не содержит VP9 video track"))?;
        // Audio codec admission по-прежнему принадлежит existing downstream decoder selection.
        let audio_track_id = audio_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.id)
            .ok_or_else(|| anyhow::anyhow!("Streaming audio WebM не содержит audio track"))?;
        // S21 сохраняет один pending packet на component; S21R применит timestamp lead к readiness.
        let bootstrap_byte_limit = std::num::NonZeroUsize::new(DUAL_STREAM_PENDING_BYTE_LIMIT)
            .ok_or_else(|| {
                anyhow::anyhow!("composite bootstrap byte limit должен быть ненулевым")
            })?;
        let lead_policy = demux_api::CompositeComponentLeadPolicy::single_pending_packet(
            DUAL_STREAM_MAX_COMPONENT_LEAD,
            bootstrap_byte_limit,
        )?;
        let selection = demux_api::CompositeAvTrackSelection::new(video_track_id, audio_track_id);
        // Старый public mapping 1=video/2=audio является observable player behavior.
        let public_track_ids = demux_api::CompositeAvPublicTrackIds::new(
            media_core::TrackId::new(REMAPPED_VIDEO_TRACK_ID),
            media_core::TrackId::new(REMAPPED_AUDIO_TRACK_ID),
        );
        let inner = demux_api::CompositeAvDemuxer::new_with_public_track_ids(
            Box::new(video_demuxer),
            Box::new(audio_demuxer),
            selection,
            public_track_ids,
            lead_policy,
        )?;
        Ok(Self { inner })
    }
}

impl Demuxer for DualStreamDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        self.inner.duration()
    }

    fn media_metadata(&self) -> Option<media_core::MediaMetadata> {
        self.inner.media_metadata()
    }

    fn seekability(&self) -> DemuxSeekability {
        self.inner.seekability()
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        self.inner.next_packet()
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        self.inner.next_event()
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.inner.seek(timestamp)
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        self.inner.seek_with_request(request)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use media_core::{MediaTime, TrackId, TrackKind};
    use symphonia::core::audio::{Channels, Position};
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::codecs::audio::AudioCodecParameters;
    use symphonia::core::codecs::audio::well_known as audio_codec;
    use symphonia::core::codecs::video::VideoCodecParameters;
    use symphonia::core::codecs::video::well_known as video_codec;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::{
        FORMAT_ID_NULL, FormatInfo, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
    };
    use symphonia::core::meta::{Metadata, MetadataLog};
    use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase, Timestamp};

    use super::*;
    use crate::options::DemuxerOptions;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeSeekCall {
        mode: SeekMode,
        required_timestamp_units: i64,
    }

    type FakeSeekLog = Arc<Mutex<Vec<FakeSeekCall>>>;
    type FakeDualStreamDemuxer = (DualStreamDemuxer, FakeSeekLog, FakeSeekLog);

    struct FakeSeekReader {
        format_info: FormatInfo,
        media_info: MediaInfo,
        tracks: Vec<Track>,
        metadata: MetadataLog,
        actual_timestamp_units: i64,
        post_seek_packet_timestamp_units: Vec<i64>,
        packets: VecDeque<symphonia::core::packet::Packet>,
        seek_log: FakeSeekLog,
        seek_error: Option<SymphoniaError>,
    }

    impl FakeSeekReader {
        fn new(tracks: Vec<Track>, actual_timestamp_units: i64, seek_log: FakeSeekLog) -> Self {
            Self {
                format_info: FormatInfo {
                    format: FORMAT_ID_NULL,
                    short_name: "fake",
                    long_name: "Fake Seek Reader",
                },
                media_info: MediaInfo::default(),
                tracks,
                metadata: MetadataLog::default(),
                actual_timestamp_units,
                post_seek_packet_timestamp_units: vec![actual_timestamp_units],
                packets: VecDeque::new(),
                seek_log,
                seek_error: None,
            }
        }

        fn with_post_seek_packet_timestamp_sequence(
            mut self,
            post_seek_packet_timestamp_units: Vec<i64>,
        ) -> Self {
            self.post_seek_packet_timestamp_units = post_seek_packet_timestamp_units;
            self
        }

        fn with_seek_error(mut self, seek_error: SymphoniaError) -> Self {
            self.seek_error = Some(seek_error);
            self
        }
    }

    impl FormatReader for FakeSeekReader {
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
            let required_timestamp = required_seek_timestamp(&self.tracks, target);
            self.seek_log
                .lock()
                .expect("seek log mutex should not be poisoned")
                .push(FakeSeekCall {
                    mode,
                    required_timestamp_units: required_timestamp.get(),
                });
            if let Some(error) = self.seek_error.take() {
                return Err(error);
            }
            self.packets = self
                .post_seek_packet_timestamp_units
                .iter()
                .copied()
                .map(|timestamp_units| post_seek_packet(&self.tracks, timestamp_units))
                .collect();

            Ok(SeekedTo {
                track_id: self
                    .tracks
                    .first()
                    .map(|track| track.id)
                    .unwrap_or_default(),
                required_ts: required_timestamp,
                actual_ts: Timestamp::new(self.actual_timestamp_units),
            })
        }

        fn tracks(&self) -> &[Track] {
            &self.tracks
        }

        fn next_packet(
            &mut self,
        ) -> symphonia::core::errors::Result<Option<symphonia::core::packet::Packet>> {
            Ok(self.packets.pop_front())
        }

        fn into_inner<'source>(self: Box<Self>) -> symphonia::core::io::MediaSourceStream<'source>
        where
            Self: 'source,
        {
            unreachable!("fake reader не возвращает MediaSourceStream")
        }
    }

    fn required_seek_timestamp(tracks: &[Track], target: SeekTo) -> Timestamp {
        match target {
            SeekTo::Time { time, track_id } => track_id
                .and_then(|id| tracks.iter().find(|track| track.id == id))
                .or_else(|| tracks.first())
                .and_then(|track| track.time_base)
                .and_then(|time_base| time_base.calc_timestamp(time))
                .unwrap_or(Timestamp::ZERO),
            SeekTo::Timestamp { ts, .. } => ts,
        }
    }

    fn post_seek_packet(tracks: &[Track], timestamp_units: i64) -> symphonia::core::packet::Packet {
        let track_id = tracks.first().map(|track| track.id).unwrap_or_default();

        symphonia::core::packet::Packet::new(
            track_id,
            Timestamp::new(timestamp_units),
            SymphoniaDuration::new(1),
            b"\x00".to_vec(),
        )
    }

    fn vp9_video_track(track_id: u32) -> Track {
        let mut video_params = VideoCodecParameters::default();
        video_params.for_codec(video_codec::CODEC_ID_VP9);

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Video(video_params));
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid video time base"));
        track.with_duration(SymphoniaDuration::new(30_000));
        track
    }

    fn aac_audio_track(track_id: u32) -> Track {
        let mut audio_params = AudioCodecParameters::new();
        audio_params.for_codec(audio_codec::CODEC_ID_AAC);
        audio_params.with_sample_rate(48_000);
        audio_params.with_channels(Channels::from(Position::FRONT_LEFT | Position::FRONT_RIGHT));

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Audio(audio_params));
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid audio time base"));
        track.with_duration(SymphoniaDuration::new(30_000));
        track
    }

    fn fake_stream_demuxer(
        track: Track,
        actual_timestamp_units: i64,
        seek_log: FakeSeekLog,
        label: &str,
    ) -> SymphoniaDemuxer {
        fake_stream_demuxer_with_post_seek_packet(
            track,
            actual_timestamp_units,
            Some(actual_timestamp_units),
            seek_log,
            label,
        )
    }

    fn fake_stream_demuxer_with_post_seek_packet(
        track: Track,
        actual_timestamp_units: i64,
        post_seek_packet_timestamp_units: Option<i64>,
        seek_log: FakeSeekLog,
        label: &str,
    ) -> SymphoniaDemuxer {
        let post_seek_packet_timestamp_units =
            post_seek_packet_timestamp_units.into_iter().collect();

        fake_stream_demuxer_with_post_seek_packet_sequence(
            track,
            actual_timestamp_units,
            post_seek_packet_timestamp_units,
            seek_log,
            label,
        )
    }

    fn fake_stream_demuxer_with_post_seek_packet_sequence(
        track: Track,
        actual_timestamp_units: i64,
        post_seek_packet_timestamp_units: Vec<i64>,
        seek_log: FakeSeekLog,
        label: &str,
    ) -> SymphoniaDemuxer {
        fake_stream_demuxer_with_post_seek_packet_sequence_and_options(
            track,
            actual_timestamp_units,
            post_seek_packet_timestamp_units,
            seek_log,
            label,
            DemuxerOptions::default(),
        )
    }

    fn fake_stream_demuxer_with_post_seek_packet_sequence_and_options(
        track: Track,
        actual_timestamp_units: i64,
        post_seek_packet_timestamp_units: Vec<i64>,
        seek_log: FakeSeekLog,
        label: &str,
        options: DemuxerOptions,
    ) -> SymphoniaDemuxer {
        let reader = FakeSeekReader::new(vec![track], actual_timestamp_units, seek_log)
            .with_post_seek_packet_timestamp_sequence(post_seek_packet_timestamp_units);

        SymphoniaDemuxer::from_test_format_reader(
            Box::new(reader),
            label,
            DemuxSeekability::Seekable,
            options,
        )
        .expect("fake stream demuxer должен открыться")
    }

    fn fake_dual_stream_demuxer(
        video_actual_timestamp_units: i64,
        audio_actual_timestamp_units: i64,
    ) -> FakeDualStreamDemuxer {
        let video_seek_log = Arc::new(Mutex::new(Vec::new()));
        let audio_seek_log = Arc::new(Mutex::new(Vec::new()));
        let video_demuxer = fake_stream_demuxer(
            vp9_video_track(10),
            video_actual_timestamp_units,
            Arc::clone(&video_seek_log),
            "fake-video",
        );
        let audio_demuxer = fake_stream_demuxer(
            aac_audio_track(20),
            audio_actual_timestamp_units,
            Arc::clone(&audio_seek_log),
            "fake-audio",
        );
        let demuxer =
            DualStreamDemuxer::new(video_demuxer, audio_demuxer).expect("dual demuxer opens");

        (demuxer, video_seek_log, audio_seek_log)
    }

    #[test]
    fn seek_seeks_both_streams_through_neutral_composite() {
        let (mut demuxer, video_seek_log, audio_seek_log) = fake_dual_stream_demuxer(500, 500);
        let result = demuxer
            .seek(Duration::from_millis(500))
            .expect("dual seek succeeds");
        assert_eq!(
            result.requested_position,
            media_core::MediaTime::from_duration(Duration::from_millis(500))
        );
        assert_eq!(video_seek_log.lock().expect("video seek log").len(), 1);
        assert_eq!(audio_seek_log.lock().expect("audio seek log").len(), 1);
    }

    #[test]
    fn failed_audio_seek_after_video_success_is_typed_partial_failure() {
        let video_seek_log = Arc::new(Mutex::new(Vec::new()));
        let audio_seek_log = Arc::new(Mutex::new(Vec::new()));
        let video_demuxer = fake_stream_demuxer(
            vp9_video_track(10),
            4_000,
            Arc::clone(&video_seek_log),
            "fake-video-success",
        );
        let audio_reader = FakeSeekReader::new(
            vec![aac_audio_track(20)],
            4_000,
            Arc::clone(&audio_seek_log),
        )
        .with_seek_error(SymphoniaError::Unsupported("fake audio seek failure"));
        let audio_demuxer = SymphoniaDemuxer::from_test_format_reader(
            Box::new(audio_reader),
            "fake-audio-failure",
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake audio demuxer должен открыться");
        let mut demuxer =
            DualStreamDemuxer::new(video_demuxer, audio_demuxer).expect("dual demuxer opens");
        let error = demuxer
            .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
            .expect_err("audio seek failure должен прервать composite seek");
        let typed = error
            .downcast_ref::<demux_api::CompositeComponentSeekError>()
            .expect("typed composite seek error");
        assert_eq!(typed.component, demux_api::CompositeComponent::Audio);
        assert!(typed.video_seek_completed);
        assert!(format!("{error:#}").contains("fake audio seek failure"));
        assert_eq!(video_seek_log.lock().expect("video seek log").len(), 1);
        assert_eq!(audio_seek_log.lock().expect("audio seek log").len(), 1);
    }

    #[test]
    fn decode_point_before_uses_video_anchor_and_audio_accurate_seek() {
        let (mut demuxer, video_seek_log, audio_seek_log) = fake_dual_stream_demuxer(4_000, 4_000);

        demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
                10,
            )))
            .expect("dual decode-point seek succeeds");

        // RC1: video seek целится почти в сам target (10 s − 1 ms margin), а не на
        // target − 5 s, чтобы выбрать ближайший decode-safe keyframe перед target.
        // Audio не требует video decode anchor и сохраняет исходную Accurate-цель 10 s.
        let expected_video_seek_call = FakeSeekCall {
            mode: SeekMode::Accurate,
            required_timestamp_units: 9_999,
        };
        let expected_audio_seek_call = FakeSeekCall {
            mode: SeekMode::Accurate,
            required_timestamp_units: 10_000,
        };
        assert_eq!(
            video_seek_log
                .lock()
                .expect("video seek log mutex should not be poisoned")
                .as_slice(),
            &[expected_video_seek_call]
        );
        assert_eq!(
            audio_seek_log
                .lock()
                .expect("audio seek log mutex should not be poisoned")
                .as_slice(),
            &[expected_audio_seek_call]
        );
    }

    #[test]
    fn decode_point_before_accepts_audio_packet_granularity_after_target() {
        let requested_timestamp = Duration::from_millis(10_000);
        let (mut demuxer, video_seek_log, audio_seek_log) = fake_dual_stream_demuxer(9_000, 10_010);

        let result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(requested_timestamp))
            .expect("audio packet granularity after target не должна ломать video seek");

        assert_eq!(result.requested_position, MediaTime::from_secs(10));
        assert_eq!(result.actual_position, MediaTime::from_secs(9));
        assert_eq!(
            video_seek_log
                .lock()
                .expect("video seek log mutex should not be poisoned")
                .as_slice(),
            &[FakeSeekCall {
                mode: SeekMode::Accurate,
                required_timestamp_units: 9_999,
            }]
        );
        assert_eq!(
            audio_seek_log
                .lock()
                .expect("audio seek log mutex should not be poisoned")
                .as_slice(),
            &[FakeSeekCall {
                mode: SeekMode::Accurate,
                required_timestamp_units: 10_000,
            }]
        );
    }

    #[test]
    fn preview_mode_returns_earliest_actual_position_when_streams_differ() {
        let (mut demuxer, _video_seek_log, _audio_seek_log) =
            fake_dual_stream_demuxer(9_000, 7_000);

        let result = demuxer
            .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
            .expect("dual preview-mode seek succeeds");

        assert_eq!(result.requested_position, MediaTime::from_secs(10));
        assert_eq!(result.actual_position, MediaTime::from_secs(7));
        assert_eq!(
            result
                .actual_track_timestamp
                .expect("composite seek должен сохранить raw timestamp")
                .track_id,
            TrackId::new(REMAPPED_AUDIO_TRACK_ID)
        );
    }

    #[test]
    fn decode_point_before_seek_result_reports_video_actual_when_audio_is_earlier() {
        let (mut demuxer, _video_seek_log, _audio_seek_log) =
            fake_dual_stream_demuxer(9_000, 7_000);

        let result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
                10,
            )))
            .expect("dual decode-point seek succeeds");

        assert_eq!(result.requested_position, MediaTime::from_secs(10));
        assert_eq!(result.actual_position, MediaTime::from_secs(9));
        assert_eq!(
            result
                .actual_track_timestamp
                .expect("decode-point composite actual должен быть video timestamp")
                .track_id,
            TrackId::new(REMAPPED_VIDEO_TRACK_ID)
        );
    }

    #[test]
    fn decode_point_before_bootstraps_audio_before_long_video_preroll() {
        let video_seek_log = Arc::new(Mutex::new(Vec::new()));
        let audio_seek_log = Arc::new(Mutex::new(Vec::new()));
        let early_video_packet_timestamps = (0_i64..240_i64)
            .map(|packet_index| packet_index * 400)
            .collect();
        let video_demuxer_options = DemuxerOptions::default()
            .with_decode_point_before_max_accepted_preroll(Duration::from_secs(120));
        let video_demuxer = fake_stream_demuxer_with_post_seek_packet_sequence_and_options(
            vp9_video_track(10),
            0,
            early_video_packet_timestamps,
            Arc::clone(&video_seek_log),
            "fake-video-long-preroll",
            video_demuxer_options,
        );
        let audio_demuxer = fake_stream_demuxer(
            aac_audio_track(20),
            96_000,
            Arc::clone(&audio_seek_log),
            "fake-audio-near-target",
        );
        let mut demuxer =
            DualStreamDemuxer::new(video_demuxer, audio_demuxer).expect("dual demuxer opens");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(96_784),
            ))
            .expect("dual DecodePointBefore seek succeeds");

        assert_eq!(
            seek_result.actual_position,
            MediaTime::from_duration(Duration::ZERO)
        );
        assert_eq!(
            seek_result
                .actual_track_timestamp
                .expect("DecodePointBefore actual должен остаться video timestamp")
                .track_id,
            TrackId::new(REMAPPED_VIDEO_TRACK_ID)
        );

        let mut video_packets_before_audio = 0_usize;
        let mut audio_packet_seen = false;

        for _ in 0..8 {
            match demuxer
                .next_event()
                .expect("post-seek packet read should not fail")
            {
                DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio => {
                    audio_packet_seen = true;
                    break;
                }
                DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                    video_packets_before_audio = video_packets_before_audio.saturating_add(1);
                }
                DemuxReadEvent::Packet(packet) => {
                    panic!("unexpected packet kind after seek: {:?}", packet.kind);
                }
                DemuxReadEvent::TracksChanged(_) => continue,
                DemuxReadEvent::MediaMetadataChanged(_) => continue,
                DemuxReadEvent::EndOfStream => panic!("audio bootstrap ended before audio packet"),
            }
        }

        assert!(audio_packet_seen);
        assert_eq!(video_packets_before_audio, 0);
    }

    #[test]
    fn decode_point_before_keeps_normal_interleave_when_audio_is_not_after_video() {
        let (mut demuxer, _video_seek_log, _audio_seek_log) =
            fake_dual_stream_demuxer(4_000, 4_000);

        demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
                10,
            )))
            .expect("dual DecodePointBefore seek succeeds");
        let packet = demuxer
            .next_packet()
            .expect("post-seek packet read should not fail")
            .expect("post-seek packet should exist");

        assert_eq!(packet.kind, TrackKind::Video);
        assert_eq!(packet.pts, Duration::from_secs(4));
    }

    #[test]
    fn decode_point_before_video_failure_is_not_masked_by_audio_success() {
        let video_seek_log = Arc::new(Mutex::new(Vec::new()));
        let audio_seek_log = Arc::new(Mutex::new(Vec::new()));
        let video_demuxer = fake_stream_demuxer_with_post_seek_packet(
            vp9_video_track(10),
            4_000,
            Some(11_000),
            Arc::clone(&video_seek_log),
            "fake-video-after-target",
        );
        let audio_demuxer = fake_stream_demuxer(
            aac_audio_track(20),
            4_000,
            Arc::clone(&audio_seek_log),
            "fake-audio-success",
        );
        let mut demuxer =
            DualStreamDemuxer::new(video_demuxer, audio_demuxer).expect("dual demuxer opens");

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
                10,
            )))
            .expect_err("video DecodePointBefore failure должен прервать composite seek");

        assert!(format!("{error}").contains("DecodePointBefore verification failed"));
        assert!(
            audio_seek_log
                .lock()
                .expect("audio seek log mutex should not be poisoned")
                .is_empty()
        );
        assert!(
            !video_seek_log
                .lock()
                .expect("video seek log mutex should not be poisoned")
                .is_empty()
        );
    }
}
