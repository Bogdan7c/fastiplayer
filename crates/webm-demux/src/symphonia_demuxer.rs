use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use media_core::{
    MediaTime, Packet as OurPacket, TimeBase as OurTimeBase, TimelineNotSeekableReason, TrackId,
    TrackInfo, TrackKind, TrackTimestamp, VideoTrackMetadata,
};
use source_core::{ByteSource, Seekability as SourceSeekability};
use symphonia::core::codecs::{CODEC_TYPE_OPUS, CODEC_TYPE_VORBIS};
use symphonia::core::errors::{Error as SymphoniaError, SeekErrorKind};
use symphonia::core::formats::{FormatOptions, FormatReader, Packet, SeekMode, SeekTo, Track};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};
use tracing::{info, warn};

use crate::byte_source::ByteSourceMediaSource;
use crate::demuxer::{DemuxSeekResult, DemuxSeekability, Demuxer};
use crate::error::DemuxError;
use crate::matroska_metadata::extract_video_track_metadata_from_file;

/// Demuxer на базе symphonia для WebM/MKV файлов.
pub struct SymphoniaDemuxer {
    format: Box<dyn FormatReader>,
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
    seekability: DemuxSeekability,
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

impl SymphoniaDemuxer {
    pub fn from_file(path: &Path) -> Result<Self, DemuxError> {
        if !path.exists() {
            return Err(DemuxError::FileNotFound(path.to_path_buf()));
        }

        let video_metadata_by_track = match extract_video_track_metadata_from_file(path) {
            Ok(metadata_by_track) => metadata_by_track,
            Err(error) => {
                warn!(
                    error = %error,
                    path = %path.display(),
                    "Matroska Colour metadata pre-scan failed"
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
            video_metadata_by_track,
            DemuxSeekability::Seekable,
        )
    }

    /// Открывает WebM/MKV из потокового reader-а без seek.
    pub fn from_stream<R>(reader: R, extension_hint: &str, label: &str) -> Result<Self, DemuxError>
    where
        R: std::io::Read + Send + Sync + 'static,
    {
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
            HashMap::new(),
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            },
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
        let source_seekability = source.seekability();
        let demux_seekability = source_seekability_to_demux_seekability(source_seekability);
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
            HashMap::new(),
            demux_seekability,
        )
    }

    /// Собирает metadata и track map из готового Symphonia format reader.
    fn from_format_reader(
        format: Box<dyn FormatReader>,
        label: &str,
        mut video_metadata_by_track: HashMap<TrackId, VideoTrackMetadata>,
        seekability: DemuxSeekability,
    ) -> Result<Self, DemuxError> {
        let mut tracks = Vec::new();
        let mut track_map = HashMap::new();

        for track in format.tracks() {
            let entry = build_track_entry(track);
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
                video: take_video_metadata_for_track_id(
                    TrackId::new(track.id),
                    entry.kind,
                    &mut video_metadata_by_track,
                ),
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
        })
    }

    fn convert_packet(&self, packet: &Packet) -> Option<OurPacket> {
        let entry = self.track_map.get(&packet.track_id())?;

        let pts = entry
            .time_base
            .map(|time_base| symphonia_timestamp_to_duration(time_base, packet.ts()))
            .unwrap_or_default();

        let keyframe = if entry.kind == TrackKind::Video && entry.codec_id == "V_VP9" {
            match vp9_parser::parse_uncompressed_header(packet.buf()) {
                Ok(info) => info.keyframe,
                Err(e) => {
                    tracing::warn!(error = %e, "VP9 packet header parse failed, skipping packet");
                    return None;
                }
            }
        } else {
            false
        };

        Some(OurPacket {
            track_id: TrackId::new(packet.track_id()),
            kind: entry.kind,
            pts,
            dts: None,
            keyframe,
            data: Bytes::copy_from_slice(packet.buf()),
        })
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

/// Достаёт Matroska video metadata для Symphonia track id.
///
/// Symphonia может использовать внутренний `track.id`, который не равен Matroska
/// `TrackNumber`. Если pre-scan нашёл ровно один video entry, fallback безопасен:
/// двусмысленности между несколькими видеотреками нет, а HDR metadata не теряется.
fn take_video_metadata_for_track_id(
    symphonia_track_id: TrackId,
    track_kind: TrackKind,
    metadata_by_track: &mut HashMap<TrackId, VideoTrackMetadata>,
) -> Option<VideoTrackMetadata> {
    if track_kind != TrackKind::Video {
        return None;
    }

    if let Some(metadata) = metadata_by_track.remove(&symphonia_track_id) {
        return Some(metadata);
    }

    if metadata_by_track.len() != 1 {
        return None;
    }

    let matroska_track_id = metadata_by_track.keys().next().copied()?;
    let metadata = metadata_by_track.remove(&matroska_track_id);
    if metadata.is_some() {
        warn!(
            symphonia_track_id = %symphonia_track_id,
            matroska_track_id = %matroska_track_id,
            "Matroska video metadata сопоставлена по единственному video track fallback"
        );
    }
    metadata
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
                Ok(packet) => {
                    if let Some(our_packet) = self.convert_packet(&packet) {
                        return Ok(Some(our_packet));
                    }
                    continue;
                }
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(symphonia::core::errors::Error::DecodeError(_))
                | Err(symphonia::core::errors::Error::IoError(_)) => {
                    warn!("Corrupted packet, skipping");
                    continue;
                }
                Err(symphonia::core::errors::Error::ResetRequired) => {
                    return Err(anyhow::anyhow!(
                        "Demux reset required: dynamic track changes are not supported yet"
                    ));
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("Demux error: {}", e));
                }
            }
        }
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        let target_time = duration_to_symphonia_time(timestamp);
        let seek_track_id = self.preferred_seek_track_id().map(TrackId::get);
        let seeked_to = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: target_time,
                    track_id: seek_track_id,
                },
            )
            .map_err(symphonia_seek_error_to_demux_error)?;

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

/// Определяет тип трека и codec_id из CodecParameters.
fn build_track_entry(track: &Track) -> TrackEntry {
    let params = &track.codec_params;

    // Пытаемся извлечь sample_rate/channels из codec params
    let mut sample_rate = params.sample_rate;
    let mut channels = params.channels.map(|c| c.count() as u32);

    // Для Opus в WebM symphonia 0.5 не заполняет params — парсим OpusHead вручную
    if sample_rate.is_none() || channels.is_none() {
        if let Some(ref codec_private) = params.extra_data {
            if let Some((sr, ch)) = parse_opus_head(codec_private) {
                if sample_rate.is_none() {
                    sample_rate = Some(sr);
                }
                if channels.is_none() {
                    channels = Some(ch);
                }
            }
        }
    }

    // Определяем kind по наличию audio params или codec_id
    let kind = if sample_rate.is_some() || channels.is_some() {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };

    // Определяем codec_id
    let codec_id = match params.codec {
        CODEC_TYPE_OPUS => "A_OPUS".to_string(),
        CODEC_TYPE_VORBIS => "A_VORBIS".to_string(),
        c if c == symphonia::core::codecs::CODEC_TYPE_NULL => {
            // Для video треков codec может быть NULL в symphonia
            // Определяем по наличию video-specific полей
            if kind == TrackKind::Video {
                "V_VP9".to_string() // Предполагаем VP9 для WebM
            } else {
                "unknown".to_string()
            }
        }
        c => format!("codec_{:?}", c),
    };

    TrackEntry {
        kind,
        codec_id,
        time_base: params.time_base,
        sample_rate,
        channels,
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
    use std::collections::HashMap;
    use std::time::Duration;

    use media_core::{TrackId, TrackKind, VideoTrackMetadata};
    use symphonia::core::units::TimeBase;

    use super::{
        duration_to_symphonia_time, symphonia_timestamp_to_duration,
        take_video_metadata_for_track_id,
    };

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
    fn video_metadata_exact_track_id_match_is_used_first() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(7),
            VideoTrackMetadata {
                coded_width: Some(3840),
                coded_height: None,
                profile: None,
                bit_depth: None,
                chroma: None,
                color: None,
            },
        )]);

        let metadata = take_video_metadata_for_track_id(
            TrackId::new(7),
            TrackKind::Video,
            &mut metadata_by_track,
        )
        .expect("exact metadata должна быть найдена");

        assert_eq!(metadata.coded_width, Some(3840));
        assert!(metadata_by_track.is_empty());
    }

    #[test]
    fn single_matroska_video_metadata_entry_can_fallback_to_symphonia_track_id() {
        let mut metadata_by_track = HashMap::from([(
            TrackId::new(1),
            VideoTrackMetadata {
                coded_width: Some(3840),
                coded_height: Some(2160),
                profile: None,
                bit_depth: None,
                chroma: None,
                color: None,
            },
        )]);

        let metadata = take_video_metadata_for_track_id(
            TrackId::new(0),
            TrackKind::Video,
            &mut metadata_by_track,
        )
        .expect("single video metadata fallback должен сработать");

        assert_eq!(metadata.coded_height, Some(2160));
        assert!(metadata_by_track.is_empty());
    }

    #[test]
    fn multiple_unmatched_video_metadata_entries_do_not_fallback() {
        let mut metadata_by_track = HashMap::from([
            (TrackId::new(1), VideoTrackMetadata::empty()),
            (TrackId::new(2), VideoTrackMetadata::empty()),
        ]);

        let metadata = take_video_metadata_for_track_id(
            TrackId::new(0),
            TrackKind::Video,
            &mut metadata_by_track,
        );

        assert!(metadata.is_none());
        assert_eq!(metadata_by_track.len(), 2);
    }
}
