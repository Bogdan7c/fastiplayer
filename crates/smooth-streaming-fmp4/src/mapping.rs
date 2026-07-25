//! Sealed mapping manifest stream/quality в точный F1 track contract.

use core::fmt;
use core::num::{NonZeroU32, NonZeroU64};

use smooth_streaming_manifest_core::{
    SmoothAudioQuality, SmoothCustomAttributeSet, SmoothManifest, SmoothQualityIndex,
    SmoothQualityLevel, SmoothStream, SmoothStreamKind, SmoothVideoQuality,
};
use symphonia_format_isomp4::{
    FragmentAacAudioSpecificConfig, FragmentAacChannelCount, FragmentAacLcConfiguration,
    FragmentAacSampleRate, FragmentH264Configuration, FragmentH264PictureParameterSet,
    FragmentH264SequenceParameterSet, FragmentInitializationCodec, FragmentTimescale,
    FragmentTrackId, FragmentVideoDimensions, FragmentVideoHeight, FragmentVideoWidth,
};

use crate::SmoothTrackMappingError;

/// Единственный track ID каждого independently reconstructed Smooth fragment-а.
const SMOOTH_FRAGMENT_TRACK_ID: NonZeroU32 = NonZeroU32::MIN;

/// Позиция stream-а в sealed manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SmoothStreamOrdinal(usize);

impl SmoothStreamOrdinal {
    /// Создаёт typed ordinal без смешивания с quality/fragment индексами.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Возвращает позицию для диагностик и UI selection state.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Позиция fragment-а в compact timeline выбранного stream-а.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SmoothFragmentIndex(usize);

impl SmoothFragmentIndex {
    /// Создаёт typed fragment index.
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Возвращает индекс для diagnostics/iteration.
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Явный выбор stream-а и manifest quality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmoothTrackSelection {
    /// Позиция stream-а в manifest.
    pub stream_ordinal: SmoothStreamOrdinal,
    /// Stable `Index` quality level-а, а не позиция в массиве.
    pub quality_index: SmoothQualityIndex,
}

impl SmoothTrackSelection {
    /// Группирует два typed selector-а.
    pub const fn new(
        stream_ordinal: SmoothStreamOrdinal,
        quality_index: SmoothQualityIndex,
    ) -> Self {
        Self {
            stream_ordinal,
            quality_index,
        }
    }
}

/// Media kind без зависимости downstream от manifest enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SmoothTrackMediaKind {
    /// H.264 video с обязательным proven RAP.
    Video,
    /// AAC-LC audio без искусственного RAP requirement.
    Audio,
}

/// Стабильная identity, переносимая через planning/reconstruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SmoothTrackIdentity {
    stream_ordinal: SmoothStreamOrdinal,
    quality_index: SmoothQualityIndex,
    media_kind: SmoothTrackMediaKind,
    bitrate: NonZeroU64,
}

impl SmoothTrackIdentity {
    /// Возвращает manifest stream ordinal.
    pub const fn stream_ordinal(self) -> SmoothStreamOrdinal {
        self.stream_ordinal
    }

    /// Возвращает manifest quality index.
    pub const fn quality_index(self) -> SmoothQualityIndex {
        self.quality_index
    }

    /// Возвращает media kind.
    pub const fn media_kind(self) -> SmoothTrackMediaKind {
        self.media_kind
    }

    /// Возвращает точный manifest bitrate.
    pub const fn bitrate(self) -> NonZeroU64 {
        self.bitrate
    }
}

/// Запрос выбора из sealed manifest.
pub struct SmoothTrackMappingRequest<'manifest, 'policy> {
    manifest: &'manifest SmoothManifest,
    selection: SmoothTrackSelection,
    cancellation: &'policy dyn Fn() -> bool,
}

impl<'manifest, 'policy> SmoothTrackMappingRequest<'manifest, 'policy> {
    /// Создаёт mapping request без quality policy и hidden defaults.
    pub const fn new(
        manifest: &'manifest SmoothManifest,
        selection: SmoothTrackSelection,
        cancellation: &'policy dyn Fn() -> bool,
    ) -> Self {
        Self {
            manifest,
            selection,
            cancellation,
        }
    }
}

/// Codec state, уже проверенный публичными F1 constructors.
enum SmoothMappedCodec<'manifest> {
    /// H.264 codec mapping.
    H264(FragmentH264Configuration<'manifest>),
    /// AAC-LC codec mapping вместе с manifest fields будущего clipping proof.
    AacLc {
        /// F1 initialization configuration.
        configuration: FragmentAacLcConfiguration<'manifest>,
        /// Exact manifest audio fields.
        audio_format: SmoothMappedAudioFormat,
    },
}

/// Именованные audio fields для будущего exact clipping proof.
#[derive(Clone, Copy)]
pub(crate) struct SmoothMappedAudioFormat {
    /// Manifest sampling rate.
    pub(crate) sample_rate_hz: u32,
    /// Manifest channel count.
    pub(crate) channel_count: u16,
}

/// Закрытое media state, которое не допускает video с audio fields.
#[derive(Clone, Copy)]
pub(crate) enum SmoothMappedMediaState {
    /// Video не несёт audio clipping metadata.
    Video,
    /// Audio всегда несёт полный формат будущего exact clipping proof.
    Audio(SmoothMappedAudioFormat),
}

/// Внутренний результат quality mapping-а без длинного positional tuple.
struct SmoothMappedQuality<'manifest> {
    media_kind: SmoothTrackMediaKind,
    codec: SmoothMappedCodec<'manifest>,
    bitrate: NonZeroU64,
    custom_attributes: &'manifest SmoothCustomAttributeSet,
}

/// Sealed mapped track: downstream не может подменить его поля по отдельности.
pub struct SmoothMappedTrack<'manifest> {
    identity: SmoothTrackIdentity,
    stream: &'manifest SmoothStream,
    timescale: FragmentTimescale,
    codec: SmoothMappedCodec<'manifest>,
    custom_attributes: &'manifest SmoothCustomAttributeSet,
}

impl fmt::Debug for SmoothMappedTrack<'_> {
    /// Не печатает codec bytes или manifest strings.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SmoothMappedTrack")
            .field("identity", &self.identity)
            .field("timescale", &self.timescale.get())
            .finish_non_exhaustive()
    }
}

impl<'manifest> SmoothMappedTrack<'manifest> {
    /// Возвращает безопасную identity.
    pub const fn identity(&self) -> SmoothTrackIdentity {
        self.identity
    }

    /// Возвращает точный stream timescale.
    pub const fn timescale_ticks_per_second(&self) -> u32 {
        self.timescale.get()
    }

    /// Возвращает F1 init codec только внутри crate boundary.
    pub(crate) const fn initialization_codec(&self) -> FragmentInitializationCodec<'manifest> {
        match self.codec {
            SmoothMappedCodec::H264(configuration) => {
                FragmentInitializationCodec::H264Avc1(configuration)
            }
            SmoothMappedCodec::AacLc { configuration, .. } => {
                FragmentInitializationCodec::AacLowComplexity(configuration)
            }
        }
    }

    /// Возвращает F1 timescale только внутри adapter-а.
    pub(crate) const fn fragment_timescale(&self) -> FragmentTimescale {
        self.timescale
    }

    /// Возвращает sealed stream для path/timeline planning-а.
    pub(crate) const fn stream(&self) -> &'manifest SmoothStream {
        self.stream
    }

    /// Возвращает авторитетный F1 track ID reconstructed resource-а.
    pub const fn reconstructed_track_id(&self) -> FragmentTrackId {
        FragmentTrackId::new(SMOOTH_FRAGMENT_TRACK_ID)
    }

    /// Выводит исчерпывающее media state из единственного codec owner-а.
    pub(crate) const fn media_state(&self) -> SmoothMappedMediaState {
        match self.codec {
            SmoothMappedCodec::H264(_) => SmoothMappedMediaState::Video,
            SmoothMappedCodec::AacLc { audio_format, .. } => {
                SmoothMappedMediaState::Audio(audio_format)
            }
        }
    }

    /// Возвращает custom attributes выбранного quality для template rendering.
    pub(crate) const fn custom_attributes(&self) -> &'manifest SmoothCustomAttributeSet {
        self.custom_attributes
    }
}

/// Выбирает и полностью проверяет manifest track до I/O.
pub fn map_smooth_track<'manifest>(
    request: SmoothTrackMappingRequest<'manifest, '_>,
) -> Result<SmoothMappedTrack<'manifest>, SmoothTrackMappingError> {
    if (request.cancellation)() {
        return Err(SmoothTrackMappingError::Cancelled);
    }
    let stream = request
        .manifest
        .streams()
        .get(request.selection.stream_ordinal.get())
        .ok_or(SmoothTrackMappingError::StreamNotFound)?;
    let quality = stream
        .qualities()
        .iter()
        .find(|quality| quality.index() == request.selection.quality_index)
        .ok_or(SmoothTrackMappingError::QualityNotFound)?;
    let timescale_value = u32::try_from(stream.timescale().get())
        .map_err(|_| SmoothTrackMappingError::TimescaleOutOfRange)?;
    let timescale = FragmentTimescale::new(
        NonZeroU32::new(timescale_value)
            .expect("validated Smooth timescale остаётся ненулевым после narrowing"),
    );
    let mapped_quality = map_quality(stream.kind(), quality)?;
    let identity = SmoothTrackIdentity {
        stream_ordinal: request.selection.stream_ordinal,
        quality_index: quality.index(),
        media_kind: mapped_quality.media_kind,
        bitrate: mapped_quality.bitrate,
    };
    let mapped_track = SmoothMappedTrack {
        identity,
        stream,
        timescale,
        codec: mapped_quality.codec,
        custom_attributes: mapped_quality.custom_attributes,
    };
    if (request.cancellation)() {
        return Err(SmoothTrackMappingError::Cancelled);
    }
    Ok(mapped_track)
}

/// Проверяет согласованность sealed stream kind и quality variant.
fn map_quality<'manifest>(
    stream_kind: SmoothStreamKind,
    quality: &'manifest SmoothQualityLevel,
) -> Result<SmoothMappedQuality<'manifest>, SmoothTrackMappingError> {
    match (stream_kind, quality) {
        (SmoothStreamKind::Video, SmoothQualityLevel::Video(video)) => Ok(SmoothMappedQuality {
            media_kind: SmoothTrackMediaKind::Video,
            codec: map_h264(video)?,
            bitrate: video.bitrate(),
            custom_attributes: video.custom_attributes(),
        }),
        (SmoothStreamKind::Audio, SmoothQualityLevel::Audio(audio)) => Ok(SmoothMappedQuality {
            media_kind: SmoothTrackMediaKind::Audio,
            codec: map_aac_lc(audio)?,
            bitrate: audio.bitrate(),
            custom_attributes: audio.custom_attributes(),
        }),
        _ => Err(SmoothTrackMappingError::QualityNotFound),
    }
}

/// Переводит canonical Smooth H.264 bytes и manifest dimensions в F1 values.
fn map_h264(
    quality: &SmoothVideoQuality,
) -> Result<SmoothMappedCodec<'_>, SmoothTrackMappingError> {
    let (sequence_parameter_set, picture_parameter_set) =
        split_h264_parameter_sets(quality.codec_configuration().as_bytes())?;
    let width = FragmentVideoWidth::try_new(quality.width().get())
        .map_err(SmoothTrackMappingError::InitializationContract)?;
    let height = FragmentVideoHeight::try_new(quality.height().get())
        .map_err(SmoothTrackMappingError::InitializationContract)?;
    let sequence_parameter_set = FragmentH264SequenceParameterSet::try_new(sequence_parameter_set)
        .map_err(SmoothTrackMappingError::InitializationContract)?;
    let picture_parameter_set = FragmentH264PictureParameterSet::try_new(picture_parameter_set)
        .map_err(SmoothTrackMappingError::InitializationContract)?;
    Ok(SmoothMappedCodec::H264(FragmentH264Configuration::new(
        FragmentVideoDimensions::new(width, height),
        sequence_parameter_set,
        picture_parameter_set,
    )))
}

/// Передаёт exact manifest ASC, включая derived bytes, без повторного вывода.
fn map_aac_lc(
    quality: &SmoothAudioQuality,
) -> Result<SmoothMappedCodec<'_>, SmoothTrackMappingError> {
    let sample_rate = FragmentAacSampleRate::try_new(quality.sampling_rate().get())
        .map_err(SmoothTrackMappingError::InitializationContract)?;
    let channel_count = FragmentAacChannelCount::try_new(u32::from(quality.channels().get()))
        .map_err(SmoothTrackMappingError::InitializationContract)?;
    let audio_specific_config =
        FragmentAacAudioSpecificConfig::try_new(quality.codec_configuration().as_bytes())
            .map_err(SmoothTrackMappingError::InitializationContract)?;
    let configuration =
        FragmentAacLcConfiguration::try_new(sample_rate, channel_count, audio_specific_config)
            .map_err(SmoothTrackMappingError::InitializationContract)?;
    Ok(SmoothMappedCodec::AacLc {
        configuration,
        audio_format: SmoothMappedAudioFormat {
            sample_rate_hz: quality.sampling_rate().get(),
            channel_count: quality.channels().get(),
        },
    })
}

/// Разбирает ровно две NAL units с canonical четырёхбайтовыми start codes.
fn split_h264_parameter_sets(
    codec_configuration: &[u8],
) -> Result<(&[u8], &[u8]), SmoothTrackMappingError> {
    const START_CODE: &[u8; 4] = b"\0\0\0\x01";
    if !codec_configuration.starts_with(START_CODE) {
        return Err(SmoothTrackMappingError::InvalidH264Configuration);
    }
    let remaining = &codec_configuration[START_CODE.len()..];
    let second_start = remaining
        .windows(START_CODE.len())
        .position(|window| window == START_CODE)
        .ok_or(SmoothTrackMappingError::InvalidH264Configuration)?;
    let first_nal = &remaining[..second_start];
    let second_nal = &remaining[second_start + START_CODE.len()..];
    if first_nal.is_empty()
        || second_nal.is_empty()
        || first_nal.windows(3).any(|window| window == b"\0\0\x01")
        || second_nal.windows(3).any(|window| window == b"\0\0\x01")
        || second_nal
            .windows(START_CODE.len())
            .any(|window| window == START_CODE)
    {
        return Err(SmoothTrackMappingError::InvalidH264Configuration);
    }
    match (first_nal[0] & 0x1f, second_nal[0] & 0x1f) {
        (7, 8) => Ok((first_nal, second_nal)),
        (8, 7) => Ok((second_nal, first_nal)),
        _ => Err(SmoothTrackMappingError::InvalidH264Configuration),
    }
}
