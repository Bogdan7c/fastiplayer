//! Demuxer для YouTube adaptive streaming.
//!
//! YouTube отдаёт video-only и audio-only WebM отдельно. Этот demuxer объединяет два
//! независимых `SymphoniaDemuxer` в один поток packets, чтобы app layer мог остаться простым.

use std::cmp::Ordering;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaTime, Packet, TrackId, TrackInfo, TrackKind,
    TrackTimestamp,
};
use tracing::debug;

use crate::symphonia_demuxer::SymphoniaDemuxer;

/// Track id для video после remap.
const REMAPPED_VIDEO_TRACK_ID: u32 = 1;

/// Track id для audio после remap.
const REMAPPED_AUDIO_TRACK_ID: u32 = 2;

/// Результат заполнения pending slot-а из одного stream demuxer-а.
enum PendingFillOutcome {
    /// Pending slot заполнен packet-ом, stream дошёл до EOF или уже был готов.
    Ready,

    /// Внутренний demuxer сообщил новый track list, и composite boundary должен отдать event.
    TracksChanged(DemuxTrackListUpdate),
}

/// Одноразовая post-seek policy для вывода audio после video-safe seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostSeekAudioBootstrap {
    /// Обычный interleave по PTS без специальных правил.
    Inactive,

    /// После `DecodePointBefore` один готовый audio packet можно отдать раньше video preroll.
    DecodePointBeforePending,
}

impl PostSeekAudioBootstrap {
    /// Возвращает policy, которую нужно включить после успешного seek-а двух stream-ов.
    fn for_seek_results(
        request: DemuxSeekRequest,
        video_seek: DemuxSeekResult,
        audio_seek: DemuxSeekResult,
    ) -> Self {
        if request.mode == DemuxSeekMode::DecodePointBefore
            && audio_seek.actual_position > video_seek.actual_position
        {
            Self::DecodePointBeforePending
        } else {
            Self::Inactive
        }
    }
}

/// Demuxer, который интерливит packets из отдельных video/audio demuxer-ов по PTS.
pub struct DualStreamDemuxer {
    /// Demuxer video-only WebM.
    video_demuxer: SymphoniaDemuxer,

    /// Demuxer audio-only WebM.
    audio_demuxer: SymphoniaDemuxer,

    /// Информация о remapped tracks.
    tracks: Vec<TrackInfo>,

    /// Общая duration, если удалось извлечь хотя бы из одного stream.
    duration: Option<Duration>,

    /// Следующий video packet, уже прочитанный для сравнения PTS.
    pending_video_packet: Option<Packet>,

    /// Следующий audio packet, уже прочитанный для сравнения PTS.
    pending_audio_packet: Option<Packet>,

    /// Video stream дошёл до EOF.
    video_eof: bool,

    /// Audio stream дошёл до EOF.
    audio_eof: bool,

    /// Bounded policy, которая не даёт первому audio после seek-а застрять за video preroll.
    post_seek_audio_bootstrap: PostSeekAudioBootstrap,
}

impl DualStreamDemuxer {
    /// Создаёт объединённый demuxer из video-only и audio-only WebM demuxer-ов.
    pub fn new(video_demuxer: SymphoniaDemuxer, audio_demuxer: SymphoniaDemuxer) -> Result<Self> {
        // Выбираем первый VP9 video track.
        let video_track = video_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Video && track.codec_id == "V_VP9")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Streaming video WebM не содержит VP9 video track"))?;

        // Выбираем первый audio track с параметрами для Opus decoder.
        let audio_track = audio_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Streaming audio WebM не содержит audio track"))?;

        // Remap нужен, потому что в двух отдельных WebM оба track id часто равны 1.
        let mut remapped_video_track = video_track;
        remapped_video_track.id = TrackId::new(REMAPPED_VIDEO_TRACK_ID);

        let mut remapped_audio_track = audio_track;
        remapped_audio_track.id = TrackId::new(REMAPPED_AUDIO_TRACK_ID);

        let duration = remapped_video_track
            .duration
            .or(remapped_audio_track.duration)
            .or_else(|| video_demuxer.duration())
            .or_else(|| audio_demuxer.duration());

        Ok(Self {
            video_demuxer,
            audio_demuxer,
            tracks: vec![remapped_video_track, remapped_audio_track],
            duration,
            pending_video_packet: None,
            pending_audio_packet: None,
            video_eof: false,
            audio_eof: false,
            post_seek_audio_bootstrap: PostSeekAudioBootstrap::Inactive,
        })
    }

    /// Сбрасывает local read-state перед новой попыткой seek-а.
    fn clear_post_seek_read_state(&mut self) {
        self.pending_video_packet = None;
        self.pending_audio_packet = None;
        self.video_eof = false;
        self.audio_eof = false;
        self.post_seek_audio_bootstrap = PostSeekAudioBootstrap::Inactive;
    }

    /// Читает следующий video packet или возвращает lifecycle update внутреннего stream-а.
    fn fill_pending_video_event(&mut self) -> Result<PendingFillOutcome> {
        // Уже есть packet для сравнения PTS.
        if self.pending_video_packet.is_some() || self.video_eof {
            return Ok(PendingFillOutcome::Ready);
        }

        // Пропускаем не-video packets на всякий случай, хотя stream должен быть video-only.
        loop {
            match self.video_demuxer.next_event()? {
                DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                    let packet = packet.with_track_id(TrackId::new(REMAPPED_VIDEO_TRACK_ID));
                    self.pending_video_packet = Some(packet);
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::Packet(_) => continue,
                DemuxReadEvent::EndOfStream => {
                    self.video_eof = true;
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::TracksChanged(_) => {
                    return self.refresh_tracks_after_inner_reset();
                }
            }
        }
    }

    /// Читает следующий audio packet или возвращает lifecycle update внутреннего stream-а.
    fn fill_pending_audio_event(&mut self) -> Result<PendingFillOutcome> {
        // Уже есть packet для сравнения PTS.
        if self.pending_audio_packet.is_some() || self.audio_eof {
            return Ok(PendingFillOutcome::Ready);
        }

        // Пропускаем не-audio packets на всякий случай, хотя stream должен быть audio-only.
        loop {
            match self.audio_demuxer.next_event()? {
                DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio => {
                    let packet = packet.with_track_id(TrackId::new(REMAPPED_AUDIO_TRACK_ID));
                    self.pending_audio_packet = Some(packet);
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::Packet(_) => continue,
                DemuxReadEvent::EndOfStream => {
                    self.audio_eof = true;
                    return Ok(PendingFillOutcome::Ready);
                }
                DemuxReadEvent::TracksChanged(_) => {
                    return self.refresh_tracks_after_inner_reset();
                }
            }
        }
    }

    /// Перестраивает remapped composite tracks после reset одного из внутренних demuxer-ов.
    fn refresh_tracks_after_inner_reset(&mut self) -> Result<PendingFillOutcome> {
        let video_track = self
            .video_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Video && track.codec_id == "V_VP9")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Streaming video demuxer потерял VP9 video track"))?;
        let audio_track = self
            .audio_demuxer
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Streaming audio demuxer потерял audio track"))?;
        let mut remapped_video_track = video_track;
        let mut remapped_audio_track = audio_track;

        remapped_video_track.id = TrackId::new(REMAPPED_VIDEO_TRACK_ID);
        remapped_audio_track.id = TrackId::new(REMAPPED_AUDIO_TRACK_ID);

        self.duration = remapped_video_track
            .duration
            .or(remapped_audio_track.duration)
            .or_else(|| self.video_demuxer.duration())
            .or_else(|| self.audio_demuxer.duration());
        self.tracks = vec![remapped_video_track, remapped_audio_track];
        self.pending_video_packet = None;
        self.pending_audio_packet = None;
        self.post_seek_audio_bootstrap = PostSeekAudioBootstrap::Inactive;

        Ok(PendingFillOutcome::TracksChanged(
            DemuxTrackListUpdate::new(self.tracks.clone(), self.duration),
        ))
    }

    /// Забирает первый post-seek audio packet раньше PTS-interleave-а, если policy arm-нута.
    fn take_post_seek_audio_bootstrap_packet(&mut self) -> Option<Packet> {
        if self.post_seek_audio_bootstrap == PostSeekAudioBootstrap::Inactive {
            return None;
        }

        self.post_seek_audio_bootstrap = PostSeekAudioBootstrap::Inactive;
        self.pending_audio_packet.take()
    }

    /// Собирает seek result двух stream-ов в один результат merged timeline.
    fn composite_seek_result(
        request: DemuxSeekRequest,
        video_seek: DemuxSeekResult,
        audio_seek: DemuxSeekResult,
    ) -> DemuxSeekResult {
        let video_seek = remap_seek_result_track(video_seek, TrackId::new(REMAPPED_VIDEO_TRACK_ID));
        let audio_seek = remap_seek_result_track(audio_seek, TrackId::new(REMAPPED_AUDIO_TRACK_ID));
        let composite_seek = match request.mode {
            DemuxSeekMode::DecodePointBefore => video_seek,
            DemuxSeekMode::Accurate | DemuxSeekMode::Preview => {
                earliest_stream_seek_result(video_seek, audio_seek)
            }
        };

        if video_seek.actual_position != audio_seek.actual_position {
            debug!(
                mode = ?request.mode,
                requested_ms = request.timestamp.as_millis(),
                video_actual_ms = video_seek.actual_position.as_duration().as_millis(),
                audio_actual_ms = audio_seek.actual_position.as_duration().as_millis(),
                composite_actual_ms = composite_seek.actual_position.as_duration().as_millis(),
                "Dual stream seek вернул разные actual positions"
            );
        }

        DemuxSeekResult {
            requested_position: MediaTime::from_duration(request.timestamp),
            actual_position: composite_seek.actual_position,
            actual_track_timestamp: composite_seek.actual_track_timestamp,
        }
    }
}

/// Переводит raw seek timestamp внутреннего stream-а в remapped track id merged demuxer-а.
fn remap_seek_result_track(
    mut seek_result: DemuxSeekResult,
    remapped_track_id: TrackId,
) -> DemuxSeekResult {
    seek_result.actual_track_timestamp =
        seek_result
            .actual_track_timestamp
            .map(|actual_track_timestamp| TrackTimestamp {
                track_id: remapped_track_id,
                ..actual_track_timestamp
            });
    seek_result
}

/// Выбирает stream, который реально начнёт interleaved packet stream раньше остальных.
fn earliest_stream_seek_result(
    video_seek: DemuxSeekResult,
    audio_seek: DemuxSeekResult,
) -> DemuxSeekResult {
    if audio_seek.actual_position < video_seek.actual_position {
        audio_seek
    } else {
        video_seek
    }
}

impl Demuxer for DualStreamDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn seekability(&self) -> DemuxSeekability {
        match (
            self.video_demuxer.seekability(),
            self.audio_demuxer.seekability(),
        ) {
            (DemuxSeekability::Seekable, DemuxSeekability::Seekable) => DemuxSeekability::Seekable,
            (DemuxSeekability::NotSeekable { reason }, _) => {
                DemuxSeekability::NotSeekable { reason }
            }
            (_, DemuxSeekability::NotSeekable { reason }) => {
                DemuxSeekability::NotSeekable { reason }
            }
        }
    }

    fn next_packet(&mut self) -> Result<Option<Packet>> {
        loop {
            match self.next_event()? {
                DemuxReadEvent::Packet(packet) => return Ok(Some(packet)),
                DemuxReadEvent::EndOfStream => return Ok(None),
                DemuxReadEvent::TracksChanged(_) => continue,
            }
        }
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        // Подготавливаем по одному packet с каждой стороны, чтобы выбрать ранний PTS.
        if let PendingFillOutcome::TracksChanged(track_update) = self.fill_pending_video_event()? {
            return Ok(DemuxReadEvent::TracksChanged(track_update));
        }
        if let PendingFillOutcome::TracksChanged(track_update) = self.fill_pending_audio_event()? {
            return Ok(DemuxReadEvent::TracksChanged(track_update));
        }

        if let Some(audio_packet) = self.take_post_seek_audio_bootstrap_packet() {
            return Ok(DemuxReadEvent::Packet(audio_packet));
        }

        match (&self.pending_video_packet, &self.pending_audio_packet) {
            (Some(video_packet), Some(audio_packet)) => {
                if video_packet.presentation_order_cmp(audio_packet) != Ordering::Greater {
                    Ok(DemuxReadEvent::Packet(
                        self.pending_video_packet
                            .take()
                            .expect("pending video packet checked above"),
                    ))
                } else {
                    Ok(DemuxReadEvent::Packet(
                        self.pending_audio_packet
                            .take()
                            .expect("pending audio packet checked above"),
                    ))
                }
            }
            (Some(_), None) => Ok(DemuxReadEvent::Packet(
                self.pending_video_packet
                    .take()
                    .expect("pending video packet checked above"),
            )),
            (None, Some(_)) => Ok(DemuxReadEvent::Packet(
                self.pending_audio_packet
                    .take()
                    .expect("pending audio packet checked above"),
            )),
            (None, None) => Ok(DemuxReadEvent::EndOfStream),
        }
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        self.clear_post_seek_read_state();

        let video_seek = self.video_demuxer.seek_with_request(request)?;
        let audio_seek = self.audio_demuxer.seek_with_request(request)?;
        self.post_seek_audio_bootstrap =
            PostSeekAudioBootstrap::for_seek_results(request, video_seek, audio_seek);

        Ok(Self::composite_seek_result(request, video_seek, audio_seek))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use bytes::Bytes;
    use media_core::{Packet, TimeBase as MediaTimeBase, TrackId, TrackKind, TrackTimestamp};
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

    fn test_webm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/VP9/test.webm")
    }

    fn open_dual_demuxer_from_test_asset() -> DualStreamDemuxer {
        let path = test_webm_path();
        let video_demuxer = SymphoniaDemuxer::from_file(&path).expect("video demuxer opens");
        let audio_demuxer = SymphoniaDemuxer::from_file(&path).expect("audio demuxer opens");

        DualStreamDemuxer::new(video_demuxer, audio_demuxer).expect("dual demuxer opens")
    }

    fn marker_packet(kind: TrackKind) -> Packet {
        Packet::new(
            TrackId::new(99),
            kind,
            Duration::from_millis(1),
            None,
            kind == TrackKind::Video,
            Bytes::from_static(b"marker"),
        )
    }

    fn marker_packet_with_raw_pts(kind: TrackKind, raw_pts_units: i64) -> Packet {
        let time_base = MediaTimeBase::new(1, 1_000).expect("valid test time base");

        Packet::new(
            TrackId::new(99),
            kind,
            Duration::ZERO,
            None,
            kind == TrackKind::Video,
            Bytes::from_static(b"marker"),
        )
        .with_track_timestamps(
            Some(TrackTimestamp::new(
                TrackId::new(99),
                raw_pts_units,
                time_base,
            )),
            None,
        )
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
    fn seek_seeks_both_streams_and_clears_pending_state() {
        let mut demuxer = open_dual_demuxer_from_test_asset();
        demuxer.pending_video_packet = Some(marker_packet(TrackKind::Video));
        demuxer.pending_audio_packet = Some(marker_packet(TrackKind::Audio));
        demuxer.video_eof = true;
        demuxer.audio_eof = true;

        let result = demuxer
            .seek(Duration::from_millis(500))
            .expect("dual seek succeeds");

        assert_eq!(
            result.requested_position,
            media_core::MediaTime::from_duration(Duration::from_millis(500))
        );
        assert!(demuxer.pending_video_packet.is_none());
        assert!(demuxer.pending_audio_packet.is_none());
        assert!(!demuxer.video_eof);
        assert!(!demuxer.audio_eof);
    }

    #[test]
    fn failed_audio_seek_after_video_success_clears_composite_pending_state() {
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

        demuxer.pending_video_packet = Some(marker_packet(TrackKind::Video));
        demuxer.pending_audio_packet = Some(marker_packet(TrackKind::Audio));
        demuxer.video_eof = true;
        demuxer.audio_eof = true;

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
            .expect_err("audio seek failure должен прервать composite seek");

        assert!(format!("{error}").contains("fake audio seek failure"));
        assert!(demuxer.pending_video_packet.is_none());
        assert!(demuxer.pending_audio_packet.is_none());
        assert!(!demuxer.video_eof);
        assert!(!demuxer.audio_eof);
        assert_eq!(
            video_seek_log
                .lock()
                .expect("video seek log mutex should not be poisoned")
                .len(),
            1
        );
        assert_eq!(
            audio_seek_log
                .lock()
                .expect("audio seek log mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[test]
    fn interleaving_orders_pending_packets_by_raw_signed_pts_before_media_pts() {
        let (mut demuxer, _video_seek_log, _audio_seek_log) = fake_dual_stream_demuxer(0, 0);
        demuxer.pending_video_packet = Some(marker_packet_with_raw_pts(TrackKind::Video, -25));
        demuxer.pending_audio_packet = Some(marker_packet_with_raw_pts(TrackKind::Audio, -10));

        let packet = demuxer
            .next_packet()
            .expect("pending packet read should not fail")
            .expect("one pending packet should be returned");

        assert_eq!(packet.kind, TrackKind::Video);
    }

    #[test]
    fn decode_point_before_seek_is_forwarded_to_both_streams() {
        let (mut demuxer, video_seek_log, audio_seek_log) = fake_dual_stream_demuxer(4_000, 4_000);

        demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
                10,
            )))
            .expect("dual decode-point seek succeeds");

        // RC1: initial backend seek целится почти в сам target (10 s − 1 ms margin), а не на
        // target − 5 s, чтобы каждый поток приземлился на ближайший keyframe перед target.
        let expected_seek_call = FakeSeekCall {
            mode: SeekMode::Accurate,
            required_timestamp_units: 9_999,
        };
        assert_eq!(
            video_seek_log
                .lock()
                .expect("video seek log mutex should not be poisoned")
                .as_slice(),
            &[expected_seek_call]
        );
        assert_eq!(
            audio_seek_log
                .lock()
                .expect("audio seek log mutex should not be poisoned")
                .as_slice(),
            &[expected_seek_call]
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
