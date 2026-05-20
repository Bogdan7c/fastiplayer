use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer, Packet as OurPacket,
    TimelineNotSeekableReason, TrackId, TrackInfo,
};
use source_core::{
    ByteSource, CancellationToken, Seekability as SourceSeekability, SourceError, SourceResult,
};
use tracing::{info, warn};

use crate::byte_source::ByteSourceMediaSource;
use crate::error::DemuxError;
use crate::matroska_metadata::{
    MatroskaVideoTrack, extract_video_tracks_from_file, scan_video_tracks_from_bytes,
};
use crate::options::DemuxerOptions;
use crate::packet_mapper::{PacketConvertError, convert_packet};
use crate::seek_mapper::{
    preferred_seek_track_id, seeked_to_timeline_result, symphonia_seek_error_to_demux_error,
    symphonia_seek_mode, symphonia_seek_target,
};
use crate::symphonia_api::{
    self, FormatReaderBox, MediaSourceStream, ReadOnlySource, SymphoniaError,
};
use crate::track_mapper::{TrackEntry, map_tracks};

/// Верхняя граница prefix scan-а для seekable byte source-ов.
const MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Более короткая граница для unseekable stream, чтобы open не ждал большой network prefix.
const MATROSKA_STREAM_SCAN_LIMIT_BYTES: usize = 256 * 1024;

/// Demuxer на базе symphonia для WebM/MKV файлов.
pub struct SymphoniaDemuxer {
    format: FormatReaderBox<'static>,
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
    seekability: DemuxSeekability,
    options: DemuxerOptions,
    consecutive_corrupted_packets: usize,
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
        let media_source_stream = MediaSourceStream::new(Box::new(file), Default::default());
        let hint = symphonia_api::hint_from_path(path);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)?;

        Self::from_format_reader(
            format,
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

        let hint = symphonia_api::hint_from_extension(extension_hint);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)?;

        Self::from_format_reader(
            format,
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

        let hint = symphonia_api::hint_from_extension(extension_hint);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)?;

        Self::from_format_reader(
            format,
            label,
            video_tracks_by_track,
            demux_seekability,
            options,
        )
    }

    /// Собирает metadata и track map из готового Symphonia format reader.
    fn from_format_reader(
        format: FormatReaderBox<'static>,
        label: &str,
        mut video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
        seekability: DemuxSeekability,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        let track_mapping = map_tracks(format.tracks(), &mut video_tracks_by_track);

        info!(
            source = %label,
            tracks = track_mapping.tracks.len(),
            duration = ?track_mapping.duration,
            "WebM source открыт"
        );

        Ok(Self {
            format,
            tracks: track_mapping.tracks,
            duration: track_mapping.duration,
            track_map: track_mapping.track_map,
            seekability,
            options,
            consecutive_corrupted_packets: 0,
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
                Ok(Some(packet)) => match convert_packet(packet, &self.track_map) {
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
                Ok(None) => {
                    return Ok(None);
                }
                Err(SymphoniaError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None);
                }
                Err(SymphoniaError::DecodeError(reason)) => {
                    self.record_corrupted_packet(None, reason)?;
                    continue;
                }
                Err(SymphoniaError::IoError(error)) => {
                    return Err(DemuxError::Io(error).into());
                }
                Err(SymphoniaError::ResetRequired) => {
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
        let seek_track_id = preferred_seek_track_id(&self.tracks);
        let seek_mode = symphonia_seek_mode(request.mode);
        let seek_target = symphonia_seek_target(request, seek_track_id);
        let seeked_to = self
            .format
            .seek(seek_mode, seek_target)
            .map_err(symphonia_seek_error_to_demux_error)?;

        self.consecutive_corrupted_packets = 0;

        Ok(seeked_to_timeline_result(
            timestamp,
            seeked_to,
            &self.track_map,
        ))
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use media_core::{DemuxSeekRequest, DemuxSeekability, Demuxer, TrackId, TrackKind};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::{
        FORMAT_ID_NULL, FormatInfo, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
    };
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{Metadata, MetadataLog};
    use symphonia::core::packet::Packet;
    use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase, Timestamp};

    use super::SymphoniaDemuxer;
    use crate::error::DemuxError;
    use crate::matroska_metadata::MatroskaVideoTrack;
    use crate::options::DemuxerOptions;

    struct FakeFormatReader {
        format_info: FormatInfo,
        media_info: MediaInfo,
        tracks: Vec<Track>,
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
                format_info: FormatInfo {
                    format: FORMAT_ID_NULL,
                    short_name: "fake",
                    long_name: "Fake FormatReader",
                },
                media_info: MediaInfo::default(),
                tracks,
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
                required_ts: Timestamp::ZERO,
                actual_ts: Timestamp::ZERO,
            })
        }

        fn tracks(&self) -> &[Track] {
            &self.tracks
        }

        fn next_packet(&mut self) -> symphonia::core::errors::Result<Option<Packet>> {
            match self.packets.pop_front() {
                Some(Ok(packet)) => Ok(Some(packet)),
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

    fn test_webm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/test.webm")
    }

    fn null_video_track(track_id: u32) -> Track {
        let mut track = Track::new(track_id);
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
        let reader = FakeFormatReader::new(vec![null_video_track(1)], packets);
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
        let reader = FakeFormatReader::new(vec![null_video_track(1)], Vec::new())
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
    fn normal_eof_returns_none_without_error() {
        let mut demuxer =
            fake_demuxer_with_options(Vec::new(), HashMap::new(), DemuxerOptions::default());

        let packet = demuxer
            .next_packet()
            .expect("normal EOF не должен быть ошибкой");

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
                Ok(fake_packet(1, 10, b"\x00".to_vec())),
                Err(SymphoniaError::DecodeError("bad packet 3")),
                Err(SymphoniaError::DecodeError("bad packet 4")),
                Ok(fake_packet(1, 20, b"\x00".to_vec())),
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
                Ok(fake_packet(1, 0, b"\x00".to_vec())),
                Ok(small_vp9_keyframe_packet(1, 10)),
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
    fn decode_point_before_seek_starts_video_before_target() {
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
        // Keyframe flag теперь восстанавливается только codec-aware packet mapper-ом:
        // Symphonia 0.6 больше не отдаёт container keyframe flag в public Packet.
    }
}
