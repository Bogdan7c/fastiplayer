use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use codec_core::{VideoCodec, VideoPacketKeyframeProbe, probe_video_packet_keyframe};
use media_core::{
    MediaTime, Packet as OurPacket, TimeBase as OurTimeBase, TimelineNotSeekableReason, TrackId,
    TrackInfo, TrackKind, TrackTimestamp,
};
use source_core::{
    ByteSource, CancellationToken, Seekability as SourceSeekability, SourceError, SourceResult,
};
use symphonia::core::codecs::{CODEC_TYPE_NULL, CODEC_TYPE_OPUS, CODEC_TYPE_VORBIS, CodecType};
use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
use symphonia::core::formats::{
    FormatOptions, FormatReader, Packet, SeekMode as SymphoniaSeekMode, SeekTo, Track,
};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};
use tracing::{info, warn};

use crate::byte_source::ByteSourceMediaSource;
use crate::demuxer::{DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer};
use crate::error::DemuxError;
use crate::matroska_metadata::{
    MatroskaVideoTrack, extract_video_tracks_from_file, scan_video_tracks_from_bytes,
};
use crate::options::DemuxerOptions;

/// Верхняя граница prefix scan-а для seekable byte source-ов.
const MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Более короткая граница для unseekable stream, чтобы open не ждал большой network prefix.
const MATROSKA_STREAM_SCAN_LIMIT_BYTES: usize = 256 * 1024;

/// Codec id для video track-а, когда контейнер не дал доказательства конкретного codec-а.
const UNKNOWN_VIDEO_CODEC_ID: &str = "unknown_video";

/// Codec id для audio track-а, когда контейнер не дал доказательства конкретного codec-а.
const UNKNOWN_AUDIO_CODEC_ID: &str = "unknown_audio";

/// Demuxer на базе symphonia для WebM/MKV файлов.
pub struct SymphoniaDemuxer {
    format: Box<dyn FormatReader>,
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
    seekability: DemuxSeekability,
    options: DemuxerOptions,
    consecutive_corrupted_packets: usize,
}

/// Внутренняя структура для хранения данных о треке
#[derive(Clone)]
struct TrackEntry {
    kind: TrackKind,
    codec_id: String,
    time_base: Option<TimeBase>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
}

/// Результат packet-level validation до отдачи packet-а в player pipeline.
#[derive(Debug)]
enum PacketConvertError {
    /// Container отдал packet для track-а, которого не было в metadata.
    UnknownTrack {
        /// Сырой Symphonia/container track id.
        track_id: u32,
    },

    /// Packet можно пропустить, если fail-safe лимит ещё не исчерпан.
    CorruptedPacket {
        /// Track, к которому относится повреждённый packet.
        track_id: TrackId,

        /// Человекочитаемая причина для logs/fatal error.
        reason: String,
    },
}

impl SymphoniaDemuxer {
    pub fn from_file(path: &Path) -> Result<Self, DemuxError> {
        Self::from_file_with_options(path, DemuxerOptions::default())
    }

    pub fn from_file_with_options(
        path: &Path,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        if !path.exists() {
            return Err(DemuxError::FileNotFound(path.to_path_buf()));
        }

        let video_tracks_by_track = match extract_video_tracks_from_file(path) {
            Ok(video_tracks_by_track) => video_tracks_by_track,
            Err(error) => {
                warn!(
                    error = %error,
                    path = %path.display(),
                    "Matroska video track pre-scan failed"
                );
                HashMap::new()
            }
        };

        let file = File::open(path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let fmt_opts = FormatOptions::default();

        let probe_result = symphonia::default::get_probe()
            .format(&hint, mss, &fmt_opts, &MetadataOptions::default())
            .map_err(|e| DemuxError::UnsupportedFormat(format!("{}", e)))?;

        Self::from_format_reader(
            probe_result.format,
            &path.display().to_string(),
            video_tracks_by_track,
            DemuxSeekability::Seekable,
            options,
        )
    }

    /// Открывает WebM/MKV из потокового reader-а без seek.
    pub fn from_stream<R>(reader: R, extension_hint: &str, label: &str) -> Result<Self, DemuxError>
    where
        R: Read + Send + Sync + 'static,
    {
        Self::from_stream_with_options(reader, extension_hint, label, DemuxerOptions::default())
    }

    /// Открывает WebM/MKV из потокового reader-а без seek с явной fail-safe политикой.
    pub fn from_stream_with_options<R>(
        reader: R,
        extension_hint: &str,
        label: &str,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError>
    where
        R: Read + Send + Sync + 'static,
    {
        let mut reader = reader;
        let (stream_prefix, video_tracks_by_track) = read_stream_prefix(&mut reader)?;
        let reader = io::Cursor::new(stream_prefix).chain(reader);

        // ReadOnlySource объявляет источник как unseekable для Symphonia.
        let media_source = ReadOnlySource::new(reader);
        let media_source_stream =
            MediaSourceStream::new(Box::new(media_source), Default::default());

        // Hint нужен probe-еру, потому что URL/файлового расширения у streaming source нет.
        let mut hint = Hint::new();
        hint.with_extension(extension_hint);

        let format_options = FormatOptions::default();

        let probe_result = symphonia::default::get_probe()
            .format(
                &hint,
                media_source_stream,
                &format_options,
                &MetadataOptions::default(),
            )
            .map_err(|error| DemuxError::UnsupportedFormat(format!("{}", error)))?;

        Self::from_format_reader(
            probe_result.format,
            label,
            video_tracks_by_track,
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            },
            options,
        )
    }

    /// Открывает WebM/MKV из нейтрального seekable byte source-а.
    pub fn from_byte_source<S>(
        source: S,
        extension_hint: &str,
        label: &str,
    ) -> Result<Self, DemuxError>
    where
        S: ByteSource + 'static,
    {
        Self::from_byte_source_with_options(
            source,
            extension_hint,
            label,
            DemuxerOptions::default(),
        )
    }

    /// Открывает WebM/MKV из нейтрального byte source-а с явной fail-safe политикой.
    pub fn from_byte_source_with_options<S>(
        mut source: S,
        extension_hint: &str,
        label: &str,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError>
    where
        S: ByteSource + 'static,
    {
        let source_seekability = source.seekability();
        let demux_seekability = source_seekability_to_demux_seekability(source_seekability);
        let video_tracks_by_track = extract_video_tracks_from_byte_source(&mut source, label)?;
        let media_source = ByteSourceMediaSource::new(Box::new(source));
        let media_source_stream =
            MediaSourceStream::new(Box::new(media_source), Default::default());

        let mut hint = Hint::new();
        hint.with_extension(extension_hint);

        let format_options = FormatOptions::default();

        let probe_result = symphonia::default::get_probe()
            .format(
                &hint,
                media_source_stream,
                &format_options,
                &MetadataOptions::default(),
            )
            .map_err(|error| DemuxError::UnsupportedFormat(format!("{}", error)))?;

        Self::from_format_reader(
            probe_result.format,
            label,
            video_tracks_by_track,
            demux_seekability,
            options,
        )
    }

    /// Собирает metadata и track map из готового Symphonia format reader.
    fn from_format_reader(
        format: Box<dyn FormatReader>,
        label: &str,
        mut video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
        seekability: DemuxSeekability,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        let mut tracks = Vec::new();
        let mut track_map = HashMap::new();

        for track in format.tracks() {
            let provisional_kind = infer_track_kind(track);
            let matroska_video_track = take_matroska_video_track_for_track_id(
                TrackId::new(track.id),
                provisional_kind,
                &mut video_tracks_by_track,
            );
            let entry = build_track_entry(track, matroska_video_track.as_ref());
            track_map.insert(track.id, entry.clone());

            let duration =
                entry
                    .time_base
                    .zip(track.codec_params.n_frames)
                    .map(|(time_base, frame_count)| {
                        symphonia_timestamp_to_duration(time_base, frame_count)
                    });

            tracks.push(TrackInfo {
                id: TrackId::new(track.id),
                kind: entry.kind,
                codec_id: entry.codec_id.clone(),
                codec_private: track
                    .codec_params
                    .extra_data
                    .as_ref()
                    .map(|codec_private_bytes| Bytes::copy_from_slice(codec_private_bytes)),
                time_base: entry
                    .time_base
                    .and_then(|time_base| OurTimeBase::new(time_base.numer, time_base.denom)),
                duration,
                sample_rate: entry.sample_rate,
                channels: entry.channels,
                video: matroska_video_track.and_then(|video_track| video_track.metadata),
            });
        }

        let global_duration = tracks.iter().filter_map(|track| track.duration).max();

        info!(
            source = %label,
            tracks = tracks.len(),
            duration = ?global_duration,
            "WebM source открыт"
        );

        Ok(Self {
            format,
            tracks,
            duration: global_duration,
            track_map,
            seekability,
            options,
            consecutive_corrupted_packets: 0,
        })
    }

    fn convert_packet(&self, packet: Packet) -> std::result::Result<OurPacket, PacketConvertError> {
        let packet_track_id = packet.track_id();
        let entry =
            self.track_map
                .get(&packet_track_id)
                .ok_or(PacketConvertError::UnknownTrack {
                    track_id: packet_track_id,
                })?;

        let pts = entry
            .time_base
            .map(|time_base| symphonia_timestamp_to_duration(time_base, packet.ts()))
            .unwrap_or_default();

        let keyframe = if let Some(container_keyframe) = packet.keyframe {
            container_keyframe
        } else if entry.kind == TrackKind::Video {
            match VideoCodec::from_container_codec_id(&entry.codec_id)
                .map(|codec| probe_video_packet_keyframe(codec, packet.buf()))
            {
                Some(VideoPacketKeyframeProbe::Keyframe(keyframe)) => keyframe,
                Some(VideoPacketKeyframeProbe::Uncertain(uncertainty)) => {
                    return Err(PacketConvertError::CorruptedPacket {
                        track_id: TrackId::new(packet_track_id),
                        reason: format!("video packet keyframe probe failed: {uncertainty:?}"),
                    });
                }
                Some(VideoPacketKeyframeProbe::AdapterUnavailable { .. }) | None => false,
            }
        } else {
            false
        };

        Ok(OurPacket {
            track_id: TrackId::new(packet_track_id),
            kind: entry.kind,
            pts,
            dts: None,
            byte_offset: packet.source_byte_offset,
            keyframe,
            data: Bytes::from(packet.data),
        })
    }

    /// Сбрасывает счётчик corrupted packets после доказанного нормального продвижения.
    fn record_successful_packet(&mut self) {
        self.consecutive_corrupted_packets = 0;
    }

    /// Учитывает recoverable corrupted packet и возвращает fatal после configured лимита.
    fn record_corrupted_packet(
        &mut self,
        track_id: Option<TrackId>,
        reason: impl Into<String>,
    ) -> Result<()> {
        let reason = reason.into();
        self.consecutive_corrupted_packets = self.consecutive_corrupted_packets.saturating_add(1);
        let skipped = self.consecutive_corrupted_packets;
        let limit = self.options.max_consecutive_corrupted_packets();

        warn!(
            ?track_id,
            skipped,
            limit,
            reason = %reason,
            "Corrupted packet skipped"
        );

        if skipped <= limit {
            return Ok(());
        }

        Err(DemuxError::TooManyCorruptedPackets {
            limit,
            skipped,
            last_error: reason,
        }
        .into())
    }

    /// Выбирает track для Symphonia seek: video предпочтительнее audio.
    fn preferred_seek_track_id(&self) -> Option<TrackId> {
        self.tracks
            .iter()
            .find(|track| track.kind == TrackKind::Video)
            .or_else(|| {
                self.tracks
                    .iter()
                    .find(|track| track.kind == TrackKind::Audio)
            })
            .map(|track| track.id)
    }

    /// Конвертирует `SeekedTo.actual_ts` в нейтральную timeline-позицию.
    fn seeked_to_timeline_result(
        &self,
        requested_position: Duration,
        seeked_to: symphonia::core::formats::SeekedTo,
    ) -> DemuxSeekResult {
        let actual_track_id = TrackId::new(seeked_to.track_id);
        let actual_track_timestamp = self
            .track_map
            .get(&seeked_to.track_id)
            .and_then(|entry| {
                entry
                    .time_base
                    .and_then(|time_base| OurTimeBase::new(time_base.numer, time_base.denom))
            })
            .map(|time_base| TrackTimestamp::new(actual_track_id, seeked_to.actual_ts, time_base));
        let actual_position = actual_track_timestamp
            .map(TrackTimestamp::to_media_time)
            .unwrap_or_else(|| MediaTime::from_duration(requested_position));

        DemuxSeekResult {
            requested_position: MediaTime::from_duration(requested_position),
            actual_position,
            actual_track_timestamp,
        }
    }
}

/// Достаёт Matroska video track metadata для Symphonia track id.
///
/// Symphonia может использовать внутренний `track.id`, который не равен Matroska
/// `TrackNumber`. Если pre-scan нашёл ровно один video entry, fallback безопасен:
/// двусмысленности между несколькими видеотреками нет, а HDR metadata не теряется.
fn take_matroska_video_track_for_track_id(
    symphonia_track_id: TrackId,
    track_kind: TrackKind,
    video_tracks_by_track: &mut HashMap<TrackId, MatroskaVideoTrack>,
) -> Option<MatroskaVideoTrack> {
    if track_kind != TrackKind::Video {
        return None;
    }

    if let Some(video_track) = video_tracks_by_track.remove(&symphonia_track_id) {
        return Some(video_track);
    }

    if video_tracks_by_track.len() != 1 {
        return None;
    }

    let matroska_track_id = video_tracks_by_track.keys().next().copied()?;
    let video_track = video_tracks_by_track.remove(&matroska_track_id);
    if video_track.is_some() {
        warn!(
            symphonia_track_id = %symphonia_track_id,
            matroska_track_id = %matroska_track_id,
            "Matroska video track metadata сопоставлена по единственному video track fallback"
        );
    }
    video_track
}

impl Demuxer for SymphoniaDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    fn next_packet(&mut self) -> Result<Option<OurPacket>> {
        loop {
            match self.format.next_packet() {
                Ok(packet) => match self.convert_packet(packet) {
                    Ok(our_packet) => {
                        self.record_successful_packet();
                        return Ok(Some(our_packet));
                    }
                    Err(PacketConvertError::UnknownTrack { track_id }) => {
                        return Err(DemuxError::UnknownPacketTrack { track_id }.into());
                    }
                    Err(PacketConvertError::CorruptedPacket { track_id, reason }) => {
                        self.record_corrupted_packet(Some(track_id), reason)?;
                        continue;
                    }
                },
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(symphonia::core::errors::Error::DecodeError(reason)) => {
                    self.record_corrupted_packet(None, reason)?;
                    continue;
                }
                Err(symphonia::core::errors::Error::IoError(error)) => {
                    return Err(DemuxError::Io(error).into());
                }
                Err(symphonia::core::errors::Error::ResetRequired) => {
                    return Err(DemuxError::ResetRequired.into());
                }
                Err(e) => {
                    return Err(DemuxError::Parse(e).into());
                }
            }
        }
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let timestamp = request.timestamp;
        let target_time = duration_to_symphonia_time(timestamp);
        let seek_track_id = self.preferred_seek_track_id().map(TrackId::get);
        let seek_mode = match request.mode {
            DemuxSeekMode::Accurate => SymphoniaSeekMode::Accurate,
            DemuxSeekMode::DecodePointBefore => SymphoniaSeekMode::Coarse,
            DemuxSeekMode::Preview => SymphoniaSeekMode::Coarse,
        };
        let seeked_to = self
            .format
            .seek(
                seek_mode,
                SeekTo::Time {
                    time: target_time,
                    track_id: seek_track_id,
                },
            )
            .map_err(symphonia_seek_error_to_demux_error)?;

        self.consecutive_corrupted_packets = 0;

        Ok(self.seeked_to_timeline_result(timestamp, seeked_to))
    }
}

/// Конвертирует source seekability в neutral demux seekability.
fn source_seekability_to_demux_seekability(seekability: SourceSeekability) -> DemuxSeekability {
    match seekability {
        SourceSeekability::Seekable => DemuxSeekability::Seekable,
        SourceSeekability::NotSeekable { reason } => match reason {
            source_core::NotSeekableReason::HttpRangeStatus { .. } => {
                DemuxSeekability::NotSeekable {
                    reason: TimelineNotSeekableReason::SourceNotSeekable,
                }
            }
            source_core::NotSeekableReason::Unknown => DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline,
            },
        },
    }
}

/// Читает Matroska prefix из seekable byte source-а и возвращает source cursor назад.
fn extract_video_tracks_from_byte_source<S>(
    source: &mut S,
    label: &str,
) -> Result<HashMap<TrackId, MatroskaVideoTrack>, DemuxError>
where
    S: ByteSource,
{
    if !source.seekability().is_seekable() {
        return Ok(HashMap::new());
    }

    let original_position = source.position();
    let scan_result = read_byte_source_video_tracks(source);
    let reset_result = source.seek(original_position);

    if let Err(error) = reset_result {
        return Err(source_error_to_demux_error(error));
    }

    match scan_result {
        Ok(video_tracks_by_track) => Ok(video_tracks_by_track),
        Err(error) => {
            warn!(
                error = %error,
                source = %label,
                "Matroska video track byte-source pre-scan failed"
            );
            Ok(HashMap::new())
        }
    }
}

/// Читает короткий prefix unseekable stream-а и потом replay-ит его перед основным reader-ом.
fn read_stream_prefix<R>(
    reader: &mut R,
) -> io::Result<(Vec<u8>, HashMap<TrackId, MatroskaVideoTrack>)>
where
    R: Read,
{
    let mut metadata_prefix = Vec::new();
    let mut read_buffer = [0_u8; 64 * 1024];

    while metadata_prefix.len() < MATROSKA_STREAM_SCAN_LIMIT_BYTES {
        let remaining_bytes = MATROSKA_STREAM_SCAN_LIMIT_BYTES - metadata_prefix.len();
        let read_size = remaining_bytes.min(read_buffer.len());
        let bytes_read = reader.read(&mut read_buffer[..read_size])?;

        if bytes_read == 0 {
            break;
        }

        metadata_prefix.extend_from_slice(&read_buffer[..bytes_read]);

        let scan = scan_video_tracks_from_bytes(&metadata_prefix);
        if scan.tracks_found {
            return Ok((metadata_prefix, scan.video_tracks));
        }
    }

    let scan = scan_video_tracks_from_bytes(&metadata_prefix);
    Ok((metadata_prefix, scan.video_tracks))
}

/// Читает prefix seekable byte source-а только до первого найденного Matroska `Tracks`.
fn read_byte_source_video_tracks<S>(
    source: &mut S,
) -> SourceResult<HashMap<TrackId, MatroskaVideoTrack>>
where
    S: ByteSource,
{
    let cancellation = CancellationToken::never_cancelled();
    let mut metadata_prefix = Vec::new();
    let mut read_buffer = [0_u8; 64 * 1024];

    while metadata_prefix.len() < MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES {
        let remaining_bytes = MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES - metadata_prefix.len();
        let read_size = remaining_bytes.min(read_buffer.len());
        let bytes_read = source.read(&mut read_buffer[..read_size], &cancellation)?;

        if bytes_read == 0 {
            break;
        }

        metadata_prefix.extend_from_slice(&read_buffer[..bytes_read]);

        let scan = scan_video_tracks_from_bytes(&metadata_prefix);
        if scan.tracks_found {
            return Ok(scan.video_tracks);
        }
    }

    Ok(scan_video_tracks_from_bytes(&metadata_prefix).video_tracks)
}

/// Конвертирует source-layer ошибку pre-scan-а в demux-level IO ошибку.
fn source_error_to_demux_error(error: SourceError) -> DemuxError {
    DemuxError::Io(io::Error::other(error))
}

/// Определяет тип трека по audio признакам; всё не-audio остаётся video для текущего MVP.
fn infer_track_kind(track: &Track) -> TrackKind {
    let (sample_rate, channels) = audio_properties_from_codec_params(track);

    if sample_rate.is_some() || channels.is_some() {
        TrackKind::Audio
    } else {
        TrackKind::Video
    }
}

/// Определяет тип трека и codec_id из CodecParameters и Matroska CodecID.
fn build_track_entry(
    track: &Track,
    matroska_video_track: Option<&MatroskaVideoTrack>,
) -> TrackEntry {
    let params = &track.codec_params;
    let (sample_rate, channels) = audio_properties_from_codec_params(track);
    let kind = if sample_rate.is_some() || channels.is_some() {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    let matroska_codec_id = matroska_video_track.and_then(|video_track| {
        video_track
            .codec_id
            .as_deref()
            .and_then(normalize_matroska_codec_id)
    });
    let codec_id = resolve_track_codec_id(params.codec, kind, matroska_codec_id);

    TrackEntry {
        kind,
        codec_id,
        time_base: params.time_base,
        sample_rate,
        channels,
    }
}

/// Достаёт audio sample rate/channels, включая ручной OpusHead fallback для WebM.
fn audio_properties_from_codec_params(track: &Track) -> (Option<u32>, Option<u32>) {
    let params = &track.codec_params;
    let mut sample_rate = params.sample_rate;
    let mut channels = params.channels.map(|channels| channels.count() as u32);

    if (sample_rate.is_none() || channels.is_none())
        && let Some(ref codec_private) = params.extra_data
        && let Some((opus_sample_rate, opus_channels)) = parse_opus_head(codec_private)
    {
        sample_rate = sample_rate.or(Some(opus_sample_rate));
        channels = channels.or(Some(opus_channels));
    }

    (sample_rate, channels)
}

/// Нормализует Matroska CodecID без предположений о том, поддерживаем ли codec.
fn normalize_matroska_codec_id(codec_id: &str) -> Option<String> {
    let trimmed_codec_id = codec_id.trim();
    if trimmed_codec_id.is_empty() {
        None
    } else {
        Some(trimmed_codec_id.to_ascii_uppercase())
    }
}

/// Возвращает container codec id с приоритетом явного Matroska CodecID.
fn resolve_track_codec_id(
    symphonia_codec: CodecType,
    kind: TrackKind,
    matroska_codec_id: Option<String>,
) -> String {
    if let Some(codec_id) = matroska_codec_id {
        return codec_id;
    }

    if let Some(codec_id) = codec_id_from_symphonia_codec(symphonia_codec) {
        return codec_id.to_string();
    }

    if symphonia_codec == CODEC_TYPE_NULL {
        return unknown_codec_id_for_kind(kind).to_string();
    }

    format!("codec_{symphonia_codec}")
}

/// Таблица Symphonia codec id, которую можно расширять без переписывания demux policy.
fn codec_id_from_symphonia_codec(codec: CodecType) -> Option<&'static str> {
    match codec {
        CODEC_TYPE_OPUS => Some("A_OPUS"),
        CODEC_TYPE_VORBIS => Some("A_VORBIS"),
        _ => None,
    }
}

/// Возвращает стабильный unknown codec id для diagnostics и capability layer.
fn unknown_codec_id_for_kind(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => UNKNOWN_VIDEO_CODEC_ID,
        TrackKind::Audio => UNKNOWN_AUDIO_CODEC_ID,
    }
}

/// Парсит OpusHead из codec private data.
///
/// OpusHead структура (RFC 7845):
/// 0-7:  "OpusHead" magic
/// 8:    version
/// 9:    channel count
/// 10-11: pre-skip
/// 12-15: input sample rate (LE u32)
/// 16-17: output gain
/// 18:   channel mapping family
///
/// Возвращает (sample_rate, channels) если OpusHead валиден.
fn parse_opus_head(codec_private_bytes: &[u8]) -> Option<(u32, u32)> {
    // Минимальный размер OpusHead = 19 bytes
    if codec_private_bytes.len() < 19 {
        return None;
    }

    // Проверяем magic "OpusHead"
    if &codec_private_bytes[0..8] != b"OpusHead" {
        return None;
    }

    let channel_count = codec_private_bytes[9] as u32;
    if channel_count == 0 || channel_count > 255 {
        return None;
    }

    // Sample rate — u32 little-endian по смещению 12
    let sample_rate = u32::from_le_bytes([
        codec_private_bytes[12],
        codec_private_bytes[13],
        codec_private_bytes[14],
        codec_private_bytes[15],
    ]);
    if sample_rate == 0 {
        return None;
    }

    tracing::debug!(
        sample_rate,
        channels = channel_count,
        "OpusHead распарсен из codec private data"
    );

    Some((sample_rate, channel_count))
}

/// Конвертирует Symphonia timestamp units в [`Duration`] без размазывания формулы по demuxer-у.
fn symphonia_timestamp_to_duration(time_base: TimeBase, timestamp_units: u64) -> Duration {
    OurTimeBase::new(time_base.numer, time_base.denom)
        .map(|media_time_base| media_time_base.timestamp_to_duration(timestamp_units))
        .unwrap_or_default()
}

/// Конвертирует Rust `Duration` в Symphonia `Time` без потери целых секунд.
fn duration_to_symphonia_time(duration: Duration) -> Time {
    Time::new(
        duration.as_secs(),
        f64::from(duration.subsec_nanos()) / 1_000_000_000.0,
    )
}

/// Мапит Symphonia seek failures в typed demux errors для player-core.
fn symphonia_seek_error_to_demux_error(error: SymphoniaError) -> DemuxError {
    match error {
        SymphoniaError::SeekError(SeekErrorKind::Unseekable)
        | SymphoniaError::SeekError(SeekErrorKind::ForwardOnly) => {
            DemuxError::SeekUnavailable(error.to_string())
        }
        SymphoniaError::SeekError(_) => DemuxError::SeekFailed(error.to_string()),
        other_error => DemuxError::Parse(other_error),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use media_core::{TrackId, TrackKind, VideoTrackMetadata};
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::{
        Cue, FormatOptions, FormatReader, Packet, SeekMode, SeekTo, SeekedTo, Track,
    };
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{Metadata, MetadataLog};
    use symphonia::core::units::TimeBase;

    use super::{
        SymphoniaDemuxer, build_track_entry, duration_to_symphonia_time,
        symphonia_timestamp_to_duration, take_matroska_video_track_for_track_id,
    };
    use crate::demuxer::{DemuxSeekRequest, Demuxer};
    use crate::error::DemuxError;
    use crate::matroska_metadata::MatroskaVideoTrack;
    use crate::options::DemuxerOptions;

    struct FakeFormatReader {
        tracks: Vec<Track>,
        cues: Vec<Cue>,
        metadata: MetadataLog,
        packets: VecDeque<std::result::Result<Packet, SymphoniaError>>,
        seek_mode_log: Option<Arc<Mutex<Vec<SeekMode>>>>,
    }

    impl FakeFormatReader {
        fn new(
            tracks: Vec<Track>,
            packets: Vec<std::result::Result<Packet, SymphoniaError>>,
        ) -> Self {
            Self {
                tracks,
                cues: Vec::new(),
                metadata: MetadataLog::default(),
                packets: VecDeque::from(packets),
                seek_mode_log: None,
            }
        }

        fn with_seek_mode_log(mut self, seek_mode_log: Arc<Mutex<Vec<SeekMode>>>) -> Self {
            self.seek_mode_log = Some(seek_mode_log);
            self
        }
    }

    impl FormatReader for FakeFormatReader {
        fn try_new(
            _source: MediaSourceStream,
            _options: &FormatOptions,
        ) -> symphonia::core::errors::Result<Self>
        where
            Self: Sized,
        {
            unreachable!("tests создают FakeFormatReader напрямую");
        }

        fn cues(&self) -> &[Cue] {
            &self.cues
        }

        fn metadata(&mut self) -> Metadata<'_> {
            self.metadata.metadata()
        }

        fn seek(
            &mut self,
            mode: SeekMode,
            _to: SeekTo,
        ) -> symphonia::core::errors::Result<SeekedTo> {
            if let Some(ref seek_mode_log) = self.seek_mode_log {
                seek_mode_log
                    .lock()
                    .expect("seek mode log mutex should not be poisoned")
                    .push(mode);
            }

            let track_id = self
                .tracks
                .first()
                .map(|track| track.id)
                .unwrap_or_default();
            Ok(SeekedTo {
                track_id,
                required_ts: 0,
                actual_ts: 0,
            })
        }

        fn tracks(&self) -> &[Track] {
            &self.tracks
        }

        fn next_packet(&mut self) -> symphonia::core::errors::Result<Packet> {
            self.packets.pop_front().unwrap_or_else(|| {
                Err(SymphoniaError::IoError(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "fake eof",
                )))
            })
        }

        fn into_inner(self: Box<Self>) -> MediaSourceStream {
            unreachable!("tests не возвращают MediaSourceStream из FakeFormatReader");
        }
    }

    fn test_webm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/test.webm")
    }

    fn null_video_track(track_id: u32) -> Track {
        let mut codec_params = CodecParameters::new();
        codec_params.time_base = Some(TimeBase::new(1, 1_000));
        Track::new(track_id, codec_params)
    }

    fn keyframe_packet(track_id: u32, timestamp: u64) -> Packet {
        Packet::new_from_slice(track_id, timestamp, 1, b"\x00").with_keyframe(true)
    }

    fn fake_demuxer_with_options(
        packets: Vec<std::result::Result<Packet, SymphoniaError>>,
        matroska_tracks: HashMap<TrackId, MatroskaVideoTrack>,
        options: DemuxerOptions,
    ) -> SymphoniaDemuxer {
        let reader = FakeFormatReader::new(vec![null_video_track(1)], packets);
        SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "fake",
            matroska_tracks,
            crate::demuxer::DemuxSeekability::Seekable,
            options,
        )
        .expect("fake demuxer должен открыться")
    }

    fn fake_demuxer_with_seek_mode_log() -> (SymphoniaDemuxer, Arc<Mutex<Vec<SeekMode>>>) {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![null_video_track(1)], Vec::new())
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "fake",
            HashMap::new(),
            crate::demuxer::DemuxSeekability::Seekable,
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

    fn video_track_metadata(width: u32, height: Option<u32>) -> VideoTrackMetadata {
        VideoTrackMetadata {
            coded_width: Some(width),
            coded_height: height,
            profile: None,
            bit_depth: None,
            chroma: None,
            color: None,
        }
    }

    fn matroska_video_track(metadata: VideoTrackMetadata) -> MatroskaVideoTrack {
        MatroskaVideoTrack {
            codec_id: Some("V_VP9".to_string()),
            metadata: Some(metadata),
        }
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

    #[test]
    fn converts_packet_timestamp_to_duration() {
        let time_base = TimeBase::new(1, 1_000);

        let duration = symphonia_timestamp_to_duration(time_base, 2_750);

        assert_eq!(duration, Duration::from_millis(2_750));
    }

    #[test]
    fn converts_duration_to_symphonia_time() {
        let time = duration_to_symphonia_time(Duration::new(12, 250_000_000));

        assert_eq!(time.seconds, 12);
        assert!((time.frac - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn accurate_demux_seek_uses_symphonia_accurate_mode() {
        assert_symphonia_seek_mode(
            DemuxSeekRequest::accurate(Duration::from_millis(500)),
            SeekMode::Accurate,
        );
    }

    #[test]
    fn decode_point_before_demux_seek_uses_symphonia_coarse_mode() {
        assert_symphonia_seek_mode(
            DemuxSeekRequest::decode_point_before(Duration::from_millis(500)),
            SeekMode::Coarse,
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
    fn video_metadata_exact_track_id_match_is_used_first() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(7),
            matroska_video_track(video_track_metadata(3840, None)),
        )]);

        let video_track = take_matroska_video_track_for_track_id(
            TrackId::new(7),
            TrackKind::Video,
            &mut metadata_by_track,
        )
        .expect("exact video track metadata должна быть найдена");
        let metadata = video_track.metadata.expect("video metadata должна быть");

        assert_eq!(metadata.coded_width, Some(3840));
        assert!(metadata_by_track.is_empty());
    }

    #[test]
    fn single_matroska_video_metadata_entry_can_fallback_to_symphonia_track_id() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(1),
            matroska_video_track(video_track_metadata(3840, Some(2160))),
        )]);

        let video_track = take_matroska_video_track_for_track_id(
            TrackId::new(0),
            TrackKind::Video,
            &mut metadata_by_track,
        )
        .expect("single video track metadata fallback должен сработать");
        let metadata = video_track.metadata.expect("video metadata должна быть");

        assert_eq!(metadata.coded_height, Some(2160));
        assert!(metadata_by_track.is_empty());
    }

    #[test]
    fn multiple_unmatched_video_metadata_entries_do_not_fallback() {
        let mut metadata_by_track = HashMap::from([
            (
                TrackId::new(1),
                matroska_video_track(VideoTrackMetadata::empty()),
            ),
            (
                TrackId::new(2),
                matroska_video_track(VideoTrackMetadata::empty()),
            ),
        ]);

        let metadata = take_matroska_video_track_for_track_id(
            TrackId::new(0),
            TrackKind::Video,
            &mut metadata_by_track,
        );

        assert!(metadata.is_none());
        assert_eq!(metadata_by_track.len(), 2);
    }

    #[test]
    fn unknown_video_codec_is_not_assumed_to_be_vp9() {
        let track = null_video_track(1);

        let entry = build_track_entry(&track, None);

        assert_eq!(entry.kind, TrackKind::Video);
        assert_eq!(entry.codec_id, "unknown_video");
    }

    #[test]
    fn explicit_matroska_video_codec_id_wins_over_symphonia_null_codec() {
        let track = null_video_track(1);
        let matroska_video_track = MatroskaVideoTrack {
            codec_id: Some("v_vp9".to_string()),
            metadata: None,
        };

        let entry = build_track_entry(&track, Some(&matroska_video_track));

        assert_eq!(entry.codec_id, "V_VP9");
    }

    #[test]
    fn unsupported_matroska_video_codec_stays_visible_to_capability_layer() {
        let track = null_video_track(1);
        let matroska_video_track = MatroskaVideoTrack {
            codec_id: Some("V_AV1".to_string()),
            metadata: None,
        };

        let entry = build_track_entry(&track, Some(&matroska_video_track));

        assert_eq!(entry.codec_id, "V_AV1");
    }

    #[test]
    fn demuxer_stops_after_configured_corrupted_packet_limit() {
        let options = DemuxerOptions::from_max_consecutive_corrupted_packets(2)
            .expect("test limit ненулевой");
        let mut demuxer = fake_demuxer_with_options(
            vec![
                Err(SymphoniaError::DecodeError("bad packet 1")),
                Err(SymphoniaError::DecodeError("bad packet 2")),
                Err(SymphoniaError::DecodeError("bad packet 3")),
            ],
            HashMap::new(),
            options,
        );

        let error = demuxer
            .next_packet()
            .expect_err("третья corrupted ошибка должна стать fatal");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("fatal должен быть typed DemuxError");

        assert!(matches!(
            demux_error,
            DemuxError::TooManyCorruptedPackets {
                limit: 2,
                skipped: 3,
                ..
            }
        ));
    }

    #[test]
    fn successful_packet_resets_corrupted_packet_counter() {
        let options = DemuxerOptions::from_max_consecutive_corrupted_packets(2)
            .expect("test limit ненулевой");
        let mut demuxer = fake_demuxer_with_options(
            vec![
                Err(SymphoniaError::DecodeError("bad packet 1")),
                Err(SymphoniaError::DecodeError("bad packet 2")),
                Ok(keyframe_packet(1, 10)),
                Err(SymphoniaError::DecodeError("bad packet 3")),
                Err(SymphoniaError::DecodeError("bad packet 4")),
                Ok(keyframe_packet(1, 20)),
            ],
            HashMap::new(),
            options,
        );

        let first_packet = demuxer
            .next_packet()
            .expect("первое чтение должно пережить две corrupted ошибки")
            .expect("успешный packet должен быть возвращён");
        let second_packet = demuxer
            .next_packet()
            .expect("счётчик должен сброситься после первого packet")
            .expect("второй успешный packet должен быть возвращён");

        assert_eq!(first_packet.pts, Duration::from_millis(10));
        assert_eq!(second_packet.pts, Duration::from_millis(20));
    }

    #[test]
    fn packet_for_unknown_track_is_fatal() {
        let mut demuxer = fake_demuxer_with_options(
            vec![Ok(keyframe_packet(99, 0))],
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
    fn invalid_vp9_header_is_counted_as_corrupted_packet() {
        let options = DemuxerOptions::from_max_consecutive_corrupted_packets(1)
            .expect("test limit ненулевой");
        let matroska_tracks = HashMap::from([(
            TrackId::new(1),
            MatroskaVideoTrack {
                codec_id: Some("V_VP9".to_string()),
                metadata: None,
            },
        )]);
        let mut demuxer = fake_demuxer_with_options(
            vec![
                Ok(Packet::new_from_slice(1, 0, 1, b"\x00")),
                Ok(keyframe_packet(1, 10)),
            ],
            matroska_tracks,
            options,
        );

        let packet = demuxer
            .next_packet()
            .expect("один битый VP9 packet можно пропустить")
            .expect("следующий packet должен быть возвращён");

        assert_eq!(packet.pts, Duration::from_millis(10));
        assert!(packet.keyframe);
    }

    #[test]
    fn decode_point_before_seek_starts_video_from_keyframe_before_target() {
        let mut demuxer =
            SymphoniaDemuxer::from_file(&test_webm_path()).expect("test webm должен открыться");
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
        assert!(
            packet.keyframe,
            "первый video packet после decoder flush должен быть keyframe"
        );
    }
}
