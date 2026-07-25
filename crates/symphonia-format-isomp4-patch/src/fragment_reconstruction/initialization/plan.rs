//! Checked planning всех box sizes до единственной allocation.

use super::error::{
    FragmentBoxType, FragmentInitializationError, FragmentInitializationField,
    FragmentInitializationLimitKind,
};
use super::model::{
    FragmentAacLcConfiguration, FragmentH264Configuration, FragmentInitializationCodec,
    FragmentInitializationRequest, FragmentTimescale,
};
use crate::fragment_reconstruction::FragmentTrackId;

const MOVIE_HEADER_SIZE: u32 = 108;
const TRACK_HEADER_SIZE: u32 = 92;
const MEDIA_HEADER_SIZE: u32 = 32;
const HANDLER_SIZE: u32 = 32;
const VIDEO_MEDIA_HEADER_SIZE: u32 = 20;
const SOUND_MEDIA_HEADER_SIZE: u32 = 16;
const DATA_INFORMATION_SIZE: u32 = 36;
const TIME_TO_SAMPLE_SIZE: u32 = 16;
const SAMPLE_TO_CHUNK_SIZE: u32 = 16;
const SAMPLE_SIZE_SIZE: u32 = 20;
const CHUNK_OFFSET_SIZE: u32 = 16;
const TRACK_EXTENDS_SIZE: u32 = 32;
const AVC_SAMPLE_ENTRY_BASE_SIZE: u32 = 86;
const AAC_SAMPLE_ENTRY_BASE_SIZE: u32 = 36;
const ELEMENTARY_STREAM_DESCRIPTOR_SIZE: u32 = 39;

/// Codec-specific proven plan.
#[derive(Clone, Copy, Debug)]
pub(super) enum PlannedInitializationCodec<'codec> {
    H264 {
        configuration: FragmentH264Configuration<'codec>,
        avc_configuration_size: u32,
        sample_entry_size: u32,
    },
    Aac {
        configuration: FragmentAacLcConfiguration<'codec>,
        elementary_stream_descriptor_size: u32,
        sample_entry_size: u32,
    },
}

impl PlannedInitializationCodec<'_> {
    pub(super) const fn sample_entry_size(self) -> u32 {
        match self {
            Self::H264 {
                sample_entry_size, ..
            }
            | Self::Aac {
                sample_entry_size, ..
            } => sample_entry_size,
        }
    }

    pub(super) const fn media_header_size(self) -> u32 {
        match self {
            Self::H264 { .. } => VIDEO_MEDIA_HEADER_SIZE,
            Self::Aac { .. } => SOUND_MEDIA_HEADER_SIZE,
        }
    }
}

/// Полное дерево заранее доказанных размеров.
#[derive(Clone, Copy, Debug)]
pub(super) struct InitializationBoxSizes {
    pub(super) file_type: u32,
    pub(super) movie: u32,
    pub(super) track: u32,
    pub(super) media: u32,
    pub(super) media_information: u32,
    pub(super) sample_table: u32,
    pub(super) sample_description: u32,
    pub(super) movie_extends: u32,
}

/// Immutable plan, достаточный для serializer-а без новых решений.
#[derive(Clone, Copy, Debug)]
pub(super) struct FragmentInitializationPlan<'codec> {
    track_id: FragmentTrackId,
    next_track_id: u32,
    timescale: FragmentTimescale,
    codec: PlannedInitializationCodec<'codec>,
    sizes: InitializationBoxSizes,
    total_size: usize,
}

impl<'codec> FragmentInitializationPlan<'codec> {
    pub(super) const fn track_id(self) -> FragmentTrackId {
        self.track_id
    }

    pub(super) const fn next_track_id(self) -> u32 {
        self.next_track_id
    }

    pub(super) const fn timescale(self) -> FragmentTimescale {
        self.timescale
    }

    pub(super) const fn codec(self) -> PlannedInitializationCodec<'codec> {
        self.codec
    }

    pub(super) const fn sizes(self) -> InitializationBoxSizes {
        self.sizes
    }

    pub(super) const fn total_size(self) -> usize {
        self.total_size
    }
}

/// Планирует codec и всё box tree без allocation.
pub(super) fn plan_initialization_segment<'codec>(
    request: &FragmentInitializationRequest<'codec, '_>,
) -> Result<FragmentInitializationPlan<'codec>, FragmentInitializationError> {
    let codec = plan_codec(request.codec(), request)?;
    let sample_description = checked_box_size(
        FragmentBoxType::SampleDescription,
        u64::from(8_u32)
            .checked_add(u64::from(codec.sample_entry_size()))
            .ok_or(FragmentInitializationError::SizeArithmeticOverflow)?,
    )?;
    let sample_table_content = [
        sample_description,
        TIME_TO_SAMPLE_SIZE,
        SAMPLE_TO_CHUNK_SIZE,
        SAMPLE_SIZE_SIZE,
        CHUNK_OFFSET_SIZE,
    ]
    .into_iter()
    .try_fold(0_u64, checked_add_size)?;
    let sample_table = checked_box_size(FragmentBoxType::SampleTable, sample_table_content)?;
    let media_information_content = [
        codec.media_header_size(),
        DATA_INFORMATION_SIZE,
        sample_table,
    ]
    .into_iter()
    .try_fold(0_u64, checked_add_size)?;
    let media_information =
        checked_box_size(FragmentBoxType::MediaInformation, media_information_content)?;
    let media_content = [MEDIA_HEADER_SIZE, HANDLER_SIZE, media_information]
        .into_iter()
        .try_fold(0_u64, checked_add_size)?;
    let media = checked_box_size(FragmentBoxType::Media, media_content)?;
    let track_content = [TRACK_HEADER_SIZE, media]
        .into_iter()
        .try_fold(0_u64, checked_add_size)?;
    let track = checked_box_size(FragmentBoxType::Track, track_content)?;
    let movie_extends =
        checked_box_size(FragmentBoxType::MovieExtends, u64::from(TRACK_EXTENDS_SIZE))?;
    let movie_content = [MOVIE_HEADER_SIZE, track, movie_extends]
        .into_iter()
        .try_fold(0_u64, checked_add_size)?;
    let movie = checked_box_size(FragmentBoxType::Movie, movie_content)?;
    let file_type = checked_box_size(FragmentBoxType::FileType, 20)?;
    let total_size_u64 = u64::from(file_type)
        .checked_add(u64::from(movie))
        .ok_or(FragmentInitializationError::SizeArithmeticOverflow)?;
    let total_size = usize::try_from(total_size_u64)
        .map_err(|_| FragmentInitializationError::SizeArithmeticOverflow)?;
    enforce_limit(
        total_size,
        request.limits().maximum_output_bytes(),
        FragmentInitializationLimitKind::OutputBytes,
    )?;

    let track_id = request.track_id();
    let next_track_id =
        track_id
            .get()
            .checked_add(1)
            .ok_or(FragmentInitializationError::FieldOverflow {
                field: FragmentInitializationField::NextTrackId,
                value: u64::from(track_id.get()),
            })?;

    Ok(FragmentInitializationPlan {
        track_id,
        next_track_id,
        timescale: request.timescale(),
        codec,
        sizes: InitializationBoxSizes {
            file_type,
            movie,
            track,
            media,
            media_information,
            sample_table,
            sample_description,
            movie_extends,
        },
        total_size,
    })
}

fn plan_codec<'codec>(
    codec: FragmentInitializationCodec<'codec>,
    request: &FragmentInitializationRequest<'codec, '_>,
) -> Result<PlannedInitializationCodec<'codec>, FragmentInitializationError> {
    match codec {
        FragmentInitializationCodec::H264Avc1(configuration) => {
            let sequence_parameter_set = configuration.sequence_parameter_set().as_bytes();
            let picture_parameter_set = configuration.picture_parameter_set().as_bytes();
            let codec_byte_count = sequence_parameter_set
                .len()
                .checked_add(picture_parameter_set.len())
                .ok_or(FragmentInitializationError::SizeArithmeticOverflow)?;
            enforce_codec_limit(codec_byte_count, request)?;
            let avc_configuration_content = u64::try_from(codec_byte_count)
                .map_err(|_| FragmentInitializationError::SizeArithmeticOverflow)?
                .checked_add(11)
                .ok_or(FragmentInitializationError::SizeArithmeticOverflow)?;
            let avc_configuration_size =
                checked_box_size(FragmentBoxType::AvcConfiguration, avc_configuration_content)?;
            let sample_entry_size = checked_box_size(
                FragmentBoxType::AvcSampleEntry,
                u64::from(AVC_SAMPLE_ENTRY_BASE_SIZE - 8)
                    .checked_add(u64::from(avc_configuration_size))
                    .ok_or(FragmentInitializationError::SizeArithmeticOverflow)?,
            )?;
            Ok(PlannedInitializationCodec::H264 {
                configuration,
                avc_configuration_size,
                sample_entry_size,
            })
        }
        FragmentInitializationCodec::AacLowComplexity(configuration) => {
            let codec_byte_count = configuration.audio_specific_config().as_bytes().len();
            enforce_codec_limit(codec_byte_count, request)?;
            let sample_entry_size = checked_box_size(
                FragmentBoxType::AacSampleEntry,
                u64::from(AAC_SAMPLE_ENTRY_BASE_SIZE - 8)
                    .checked_add(u64::from(ELEMENTARY_STREAM_DESCRIPTOR_SIZE))
                    .ok_or(FragmentInitializationError::SizeArithmeticOverflow)?,
            )?;
            Ok(PlannedInitializationCodec::Aac {
                configuration,
                elementary_stream_descriptor_size: ELEMENTARY_STREAM_DESCRIPTOR_SIZE,
                sample_entry_size,
            })
        }
    }
}

fn enforce_codec_limit(
    observed: usize,
    request: &FragmentInitializationRequest<'_, '_>,
) -> Result<(), FragmentInitializationError> {
    enforce_limit(
        observed,
        request.limits().maximum_codec_configuration_bytes(),
        FragmentInitializationLimitKind::CodecConfigurationBytes,
    )
}

fn enforce_limit(
    observed: usize,
    limit: usize,
    kind: FragmentInitializationLimitKind,
) -> Result<(), FragmentInitializationError> {
    if observed <= limit {
        return Ok(());
    }
    let limit =
        u64::try_from(limit).map_err(|_| FragmentInitializationError::SizeArithmeticOverflow)?;
    let observed =
        u64::try_from(observed).map_err(|_| FragmentInitializationError::SizeArithmeticOverflow)?;
    Err(FragmentInitializationError::LimitExceeded {
        kind,
        limit,
        observed,
    })
}

pub(super) fn checked_box_size(
    box_type: FragmentBoxType,
    content_size: u64,
) -> Result<u32, FragmentInitializationError> {
    let size = content_size
        .checked_add(8)
        .ok_or(FragmentInitializationError::SizeArithmeticOverflow)?;
    u32::try_from(size).map_err(|_| FragmentInitializationError::BoxSizeOverflow { box_type, size })
}

fn checked_add_size(accumulator: u64, value: u32) -> Result<u64, FragmentInitializationError> {
    accumulator
        .checked_add(u64::from(value))
        .ok_or(FragmentInitializationError::SizeArithmeticOverflow)
}
