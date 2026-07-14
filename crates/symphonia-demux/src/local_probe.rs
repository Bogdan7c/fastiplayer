//! Узкая Symphonia-specific граница чтения статического snapshot-а контейнера.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use media_core::{MediaContainerMetadata, MediaDuration, MediaMetadata};
use source_core::CancellationToken;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;

use crate::symphonia_api;
use crate::symphonia_demuxer::metadata::consume_media_metadata;

/// Exact Symphonia 0.6 sentinel emitted only when `Probe` found no reader.
const NO_SUITABLE_FORMAT_READER: &str = "core (probe): no suitable format reader found";

/// Количество audio/video tracks, объявленных самим container reader-ом.
///
/// Наличие декодера намеренно не участвует в этом snapshot-е: null/unknown codec
/// остаётся track-ом известного container type.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContainerTrackTopology {
    audio_track_count: usize,
    video_track_count: usize,
}

impl ContainerTrackTopology {
    /// Создаёт topology из точных container track counts.
    #[must_use]
    pub const fn new(audio_track_count: usize, video_track_count: usize) -> Self {
        Self {
            audio_track_count,
            video_track_count,
        }
    }

    /// Возвращает число audio tracks независимо от codec support.
    #[must_use]
    pub const fn audio_track_count(self) -> usize {
        self.audio_track_count
    }

    /// Возвращает число video tracks независимо от codec support.
    #[must_use]
    pub const fn video_track_count(self) -> usize {
        self.video_track_count
    }

    /// Показывает, содержит ли контейнер хотя бы один audio track.
    #[must_use]
    pub const fn has_audio(self) -> bool {
        self.audio_track_count > 0
    }

    /// Показывает, содержит ли контейнер хотя бы один video track.
    #[must_use]
    pub const fn has_video(self) -> bool {
        self.video_track_count > 0
    }
}

/// Статические сведения, доступные сразу после Symphonia probe/open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerProbeSnapshot {
    topology: ContainerTrackTopology,
    duration: Option<MediaDuration>,
    metadata: MediaMetadata,
}

impl ContainerProbeSnapshot {
    /// Возвращает container-level audio/video topology.
    #[must_use]
    pub const fn topology(&self) -> ContainerTrackTopology {
        self.topology
    }

    /// Возвращает duration, только когда container сообщил согласованные timebase и units.
    #[must_use]
    pub const fn duration(&self) -> Option<MediaDuration> {
        self.duration
    }

    /// Возвращает нормализованные neutral metadata.
    #[must_use]
    pub const fn metadata(&self) -> &MediaMetadata {
        &self.metadata
    }

    /// Передаёт metadata следующему owner-у без дополнительного clone.
    #[must_use]
    pub fn into_metadata(self) -> MediaMetadata {
        self.metadata
    }
}

/// Ошибка статического container probe без playback-specific error mapping.
#[derive(Debug, thiserror::Error)]
pub enum ContainerProbeError {
    /// Caller запросил cooperative cancellation.
    #[error("local media probe отменён")]
    Cancelled,

    /// Symphonia не нашла подходящий container reader.
    #[error("неподдерживаемый media container: {reason}")]
    UnsupportedContainer {
        /// Безопасная техническая причина от Symphonia.
        reason: String,
    },

    /// Source не удалось прочитать или переместить.
    #[error("ошибка I/O при чтении media container: {0}")]
    IoFailure(#[source] io::Error),

    /// Container reader найден, но статическую структуру прочитать не удалось.
    #[error("ошибка разбора media container: {reason}")]
    ProbeFailure {
        /// Безопасная техническая причина от Symphonia.
        reason: String,
    },
}

/// Читает только header/tracks/initial metadata уже открытого local file.
///
/// Функция не вызывает `FormatReader::next_packet`, не создаёт decoder и не
/// применяет playback-specific Matroska pre-scan/options. `extension_hint` лишь
/// ускоряет выбор reader-а: окончательное решение принимает содержимое файла.
pub fn probe_open_local_media_file(
    file: &mut File,
    extension_hint: Option<&str>,
    cancellation: &CancellationToken,
) -> Result<ContainerProbeSnapshot, ContainerProbeError> {
    ensure_not_cancelled(cancellation)?;

    let byte_len = file
        .metadata()
        .map_err(ContainerProbeError::IoFailure)?
        .len();
    let source = CooperativeFileSource {
        file,
        byte_len,
        cancellation: cancellation.clone(),
    };
    let media_source_stream = MediaSourceStream::new(Box::new(source), Default::default());
    let hint = extension_hint
        .map(symphonia_api::hint_from_extension)
        .unwrap_or_default();

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            media_source_stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| classify_symphonia_probe_error(error, cancellation))?;

    ensure_not_cancelled(cancellation)?;
    let snapshot = snapshot_from_format_reader(&mut format);
    ensure_not_cancelled(cancellation)?;
    Ok(snapshot)
}

/// Обёртка проверяет token на границе каждого фактического read/seek.
///
/// `Interrupted` сам по себе не считается отменой: классификатор дополнительно
/// проверяет shared token, поэтому настоящее I/O-прерывание не маскируется.
struct CooperativeFileSource<'file> {
    file: &'file mut File,
    byte_len: u64,
    cancellation: CancellationToken,
}

impl Read for CooperativeFileSource<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.ensure_active()?;
        self.file.read(buffer)
    }
}

impl Seek for CooperativeFileSource<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.ensure_active()?;
        self.file.seek(position)
    }
}

impl MediaSource for CooperativeFileSource<'_> {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.byte_len)
    }
}

impl CooperativeFileSource<'_> {
    fn ensure_active(&self) -> io::Result<()> {
        if self.cancellation.is_cancelled() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "local media probe cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<(), ContainerProbeError> {
    if cancellation.is_cancelled() {
        Err(ContainerProbeError::Cancelled)
    } else {
        Ok(())
    }
}

fn classify_symphonia_probe_error(
    error: SymphoniaError,
    cancellation: &CancellationToken,
) -> ContainerProbeError {
    match error {
        SymphoniaError::IoError(_) if cancellation.is_cancelled() => ContainerProbeError::Cancelled,
        // Symphonia использует `UnexpectedEof` и для преждевременно оборванного
        // container header. Файл уже успешно открыт, поэтому такой EOF описывает
        // malformed содержимое, а не недоступность local source.
        SymphoniaError::IoError(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            ContainerProbeError::ProbeFailure {
                reason: error.to_string(),
            }
        }
        SymphoniaError::IoError(error) => ContainerProbeError::IoFailure(error),
        SymphoniaError::Unsupported(reason) if reason == NO_SUITABLE_FORMAT_READER => {
            ContainerProbeError::UnsupportedContainer {
                reason: reason.to_owned(),
            }
        }
        other => ContainerProbeError::ProbeFailure {
            reason: other.to_string(),
        },
    }
}

fn snapshot_from_format_reader(
    format: &mut symphonia_api::FormatReaderBox<'_>,
) -> ContainerProbeSnapshot {
    let topology = topology_from_tracks(format.as_ref());
    let duration = duration_from_format_reader(format.as_ref());
    let format_name = format.format_info().short_name.to_owned();
    let mut metadata = MediaMetadata {
        container: Some(MediaContainerMetadata {
            format_name: Some(format_name),
        }),
        ..MediaMetadata::default()
    };

    consume_media_metadata(format, &mut metadata);

    ContainerProbeSnapshot {
        topology,
        duration,
        metadata,
    }
}

fn topology_from_tracks(format: &dyn FormatReader) -> ContainerTrackTopology {
    let mut topology = ContainerTrackTopology::default();

    for track in format.tracks() {
        match track.codec_params.as_ref() {
            Some(CodecParameters::Audio(_)) => topology.audio_track_count += 1,
            Some(CodecParameters::Video(_)) => topology.video_track_count += 1,
            _ => {}
        }
    }

    topology
}

fn duration_from_format_reader(format: &dyn FormatReader) -> Option<MediaDuration> {
    duration_from_time_base_units(format.media_info().time_base, format.media_info().duration)
        .or_else(|| {
            format
                .tracks()
                .iter()
                .filter(|track| {
                    matches!(
                        track.codec_params,
                        Some(CodecParameters::Audio(_) | CodecParameters::Video(_))
                    )
                })
                .filter_map(|track| duration_from_time_base_units(track.time_base, track.duration))
                .max()
        })
}

fn duration_from_time_base_units(
    time_base: Option<symphonia::core::units::TimeBase>,
    duration: Option<symphonia::core::units::Duration>,
) -> Option<MediaDuration> {
    time_base
        .zip(duration)
        .map(|(time_base, duration)| {
            symphonia_api::symphonia_duration_to_duration(time_base, duration)
        })
        .filter(|duration| !duration.is_zero())
        .map(MediaDuration::from_duration)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use media_core::{DiscNumber, TrackNumber, TvEpisodeNumber, TvSeasonNumber};
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::codecs::audio::AudioCodecParameters;
    use symphonia::core::codecs::video::VideoCodecParameters;
    use symphonia::core::formats::{
        FORMAT_ID_NULL, FormatInfo, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
    };
    use symphonia::core::meta::{
        METADATA_ID_NULL, Metadata, MetadataBuilder, MetadataInfo, MetadataLog, StandardTag, Tag,
    };
    use symphonia::core::packet::Packet;
    use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase};

    use super::*;

    static TEST_FORMAT_INFO: FormatInfo = FormatInfo {
        format: FORMAT_ID_NULL,
        short_name: "test-container",
        long_name: "Test container",
    };
    const TEST_METADATA_INFO: MetadataInfo = MetadataInfo {
        metadata: METADATA_ID_NULL,
        short_name: "test-metadata",
        long_name: "Test metadata",
    };

    struct FakeFormatReader {
        media_info: MediaInfo,
        tracks: Vec<Track>,
        metadata: MetadataLog,
        next_packet_calls: Arc<AtomicUsize>,
    }

    impl FormatReader for FakeFormatReader {
        fn format_info(&self) -> &FormatInfo {
            &TEST_FORMAT_INFO
        }

        fn media_info(&self) -> &MediaInfo {
            &self.media_info
        }

        fn metadata(&mut self) -> Metadata<'_> {
            self.metadata.metadata()
        }

        fn seek(
            &mut self,
            _mode: SeekMode,
            _to: SeekTo,
        ) -> symphonia::core::errors::Result<SeekedTo> {
            Err(SymphoniaError::Unsupported("fake seek"))
        }

        fn tracks(&self) -> &[Track] {
            &self.tracks
        }

        fn next_packet(&mut self) -> symphonia::core::errors::Result<Option<Packet>> {
            self.next_packet_calls.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }

        fn into_inner<'source>(self: Box<Self>) -> MediaSourceStream<'source>
        where
            Self: 'source,
        {
            MediaSourceStream::new(Box::new(Cursor::new(Vec::<u8>::new())), Default::default())
        }
    }

    #[test]
    fn topology_admits_null_codec_audio_and_video_without_reading_packets() {
        let next_packet_calls = Arc::new(AtomicUsize::new(0));
        let mut audio_track = Track::new(1);
        audio_track.with_codec_params(CodecParameters::Audio(AudioCodecParameters::new()));
        let mut video_track = Track::new(2);
        video_track.with_codec_params(CodecParameters::Video(VideoCodecParameters::default()));
        let mut format: symphonia_api::FormatReaderBox<'_> = Box::new(FakeFormatReader {
            media_info: MediaInfo::default(),
            tracks: vec![audio_track, video_track],
            metadata: MetadataLog::default(),
            next_packet_calls: Arc::clone(&next_packet_calls),
        });

        let snapshot = snapshot_from_format_reader(&mut format);

        assert_eq!(snapshot.topology().audio_track_count(), 1);
        assert_eq!(snapshot.topology().video_track_count(), 1);
        assert_eq!(next_packet_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn metadata_and_media_duration_are_extracted_without_decode_loop() {
        let time_base = TimeBase::try_new(1, 1_000).expect("valid time base");
        let mut media_info = MediaInfo::default();
        media_info
            .with_time_base(time_base)
            .with_duration(SymphoniaDuration::new(12_500));
        let mut metadata_builder = MetadataBuilder::new(TEST_METADATA_INFO);
        let tags = [
            StandardTag::TrackTitle(Arc::new("Название".to_owned())),
            StandardTag::Artist(Arc::new("Исполнитель".to_owned())),
            StandardTag::Album(Arc::new("Альбом".to_owned())),
            StandardTag::DiscNumber(2),
            StandardTag::TrackNumber(7),
            StandardTag::TvSeasonNumber(3),
            StandardTag::TvEpisodeNumber(9),
        ];
        for (index, standard_tag) in tags.into_iter().enumerate() {
            metadata_builder.add_tag(Tag::new_from_parts(
                format!("tag-{index}"),
                "ignored raw value",
                Some(standard_tag),
            ));
        }
        let mut metadata = MetadataLog::default();
        metadata.push(metadata_builder.build());
        let mut format: symphonia_api::FormatReaderBox<'_> = Box::new(FakeFormatReader {
            media_info,
            tracks: vec![],
            metadata,
            next_packet_calls: Arc::new(AtomicUsize::new(0)),
        });

        let snapshot = snapshot_from_format_reader(&mut format);
        let tags = &snapshot.metadata().tags;

        assert_eq!(
            snapshot.duration(),
            Some(MediaDuration::from_millis(12_500))
        );
        assert_eq!(
            snapshot
                .metadata()
                .container
                .as_ref()
                .and_then(|container| container.format_name.as_deref()),
            Some("test-container")
        );
        assert_eq!(tags.title.as_deref(), Some("Название"));
        assert_eq!(tags.artists, ["Исполнитель"]);
        assert_eq!(tags.album.as_deref(), Some("Альбом"));
        assert_eq!(tags.disc_number, Some(DiscNumber::new(2)));
        assert_eq!(tags.track_number, Some(TrackNumber::new(7)));
        assert_eq!(tags.tv_season_number, Some(TvSeasonNumber::new(3)));
        assert_eq!(tags.tv_episode_number, Some(TvEpisodeNumber::new(9)));
    }

    #[test]
    fn missing_duration_and_tags_remain_absent() {
        let mut format: symphonia_api::FormatReaderBox<'_> = Box::new(FakeFormatReader {
            media_info: MediaInfo::default(),
            tracks: vec![],
            metadata: MetadataLog::default(),
            next_packet_calls: Arc::new(AtomicUsize::new(0)),
        });

        let snapshot = snapshot_from_format_reader(&mut format);

        assert_eq!(snapshot.duration(), None);
        assert_eq!(snapshot.metadata().tags, Default::default());
    }

    #[test]
    fn interrupted_io_is_cancellation_only_when_token_confirms_it() {
        let token = CancellationToken::new();
        let interrupted = io::Error::new(io::ErrorKind::Interrupted, "external interrupt");

        let ordinary_error =
            classify_symphonia_probe_error(SymphoniaError::IoError(interrupted), &token);
        assert!(matches!(ordinary_error, ContainerProbeError::IoFailure(_)));

        token.cancel();
        let cancelled_error = classify_symphonia_probe_error(
            SymphoniaError::IoError(io::Error::new(
                io::ErrorKind::Interrupted,
                "cooperative cancellation",
            )),
            &token,
        );
        assert!(matches!(cancelled_error, ContainerProbeError::Cancelled));
    }

    #[test]
    fn unexpected_eof_is_malformed_probe_failure() {
        let token = CancellationToken::new();
        let unexpected_eof = io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "container header was truncated",
        );

        let error = classify_symphonia_probe_error(SymphoniaError::IoError(unexpected_eof), &token);

        assert!(matches!(error, ContainerProbeError::ProbeFailure { .. }));
    }
}
