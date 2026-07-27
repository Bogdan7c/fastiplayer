//! Pure static mapping одной serialized yt-dlp format row.

use web_media_core::{
    AudioComponentDescriptor, AudioTrackDescriptor, Bitrate, ChannelCount, CodecFamily, CodecKind,
    CodecMediaKind, ContainerFamily, ContainerIdentity, DynamicRange, FrameRate, LanguageTag,
    MuxedComponentDescriptor, NormalizedCodec, NormalizedTransport, ProfileExclusionReason,
    RawCodecIdentity, RawContainerIdentity, RawExtensionIdentity, RawTransportIdentity, SampleRate,
    StaticCompatibilityRejection, StaticDescriptorField, StaticMetadataViolation, StreamLayout,
    TransportFamily, VideoComponentDescriptor, VideoHeight, VideoTrackDescriptor, VideoWidth,
};

use super::model::{YtDlpCandidateNormalizationRejection, YtDlpVideoColorEvidence};
use super::raw::YtDlpSerializedFormat;
use super::request_material::{YtDlpRequestMaterial, normalize_request_material};

/// Точность rational conversion для JSON decimal FPS.
const FRAME_RATE_SCALE: u32 = 1_000;
/// Kbit/s от yt-dlp переводится в bits/s через explicit SI multiplier.
const KILOBITS_TO_BITS: f64 = 1_000.0;

/// Owner-local result не выпускает request material в neutral crate.
pub(super) struct NormalizedFormatParts {
    /// Нейтральный static layout.
    pub(super) layout: StreamLayout,
    /// Exact provider evidence остаётся вне neutral descriptor-а.
    pub(super) video_color_evidence: Option<YtDlpVideoColorEvidence>,
    /// Service-owned transient material.
    pub(super) request: YtDlpRequestMaterial,
}

/// Проверяет static S00 profile и строит один component layout.
pub(super) fn normalize_format_parts(
    format: &YtDlpSerializedFormat,
) -> Result<NormalizedFormatParts, YtDlpCandidateNormalizationRejection> {
    if format.has_drm == Some(true) {
        return Err(static_rejection(
            StaticCompatibilityRejection::ProfileExcluded {
                reason: ProfileExclusionReason::Drm,
            },
        ));
    }
    if format.url.is_none() {
        return Err(invalid_metadata(
            StaticDescriptorField::Transport,
            StaticMetadataViolation::Missing,
        ));
    }

    let transport = normalize_transport(format)?;
    let container = normalize_container(format, transport.family())?;
    let video_codec = normalize_codec(format.vcodec.as_deref(), CodecMediaKind::Video)?;
    let audio_codec = normalize_codec(format.acodec.as_deref(), CodecMediaKind::Audio)?;
    let layout = normalize_layout(format, transport, container, video_codec, audio_codec)?;
    let video_color_evidence = if matches!(&layout, StreamLayout::AudioOnly(_)) {
        None
    } else {
        normalize_video_color_evidence(format.dynamic_range.as_deref())
    };
    let request = normalize_request_material(format)
        .map_err(YtDlpCandidateNormalizationRejection::RequestMaterial)?;

    Ok(NormalizedFormatParts {
        layout,
        video_color_evidence,
        request,
    })
}

/// Парсит raw transport и сохраняет unknown/profile exclusions typed.
fn normalize_transport(
    format: &YtDlpSerializedFormat,
) -> Result<NormalizedTransport, YtDlpCandidateNormalizationRejection> {
    let raw_protocol = format.protocol.clone().ok_or_else(|| {
        invalid_metadata(
            StaticDescriptorField::Transport,
            StaticMetadataViolation::Missing,
        )
    })?;
    let raw_transport = RawTransportIdentity::new(raw_protocol).map_err(|_| {
        invalid_metadata(
            StaticDescriptorField::Transport,
            StaticMetadataViolation::OutOfBounds,
        )
    })?;
    let transport = NormalizedTransport::parse(raw_transport);

    match transport.family() {
        TransportFamily::Unknown => Err(static_rejection(
            StaticCompatibilityRejection::UnknownTransport {
                transport: transport.clone(),
            },
        )),
        TransportFamily::KnownExcluded(exclusion) => {
            let reason = match exclusion {
                web_media_core::KnownExcludedTransport::PrivateLiveState
                | web_media_core::KnownExcludedTransport::DashGenerator => {
                    ProfileExclusionReason::RequiresLiveExtractorState
                }
                web_media_core::KnownExcludedTransport::NonMedia => {
                    ProfileExclusionReason::NonMedia
                }
            };
            Err(static_rejection(
                StaticCompatibilityRejection::ProfileExcluded { reason },
            ))
        }
        _ => Ok(transport),
    }
}

/// Парсит ext/container независимо и разрешает только approved relationships.
fn normalize_container(
    format: &YtDlpSerializedFormat,
    transport_family: TransportFamily,
) -> Result<ContainerIdentity, YtDlpCandidateNormalizationRejection> {
    let raw_extension = format
        .ext
        .clone()
        .map(RawExtensionIdentity::new)
        .transpose()
        .map_err(|_| {
            invalid_metadata(
                StaticDescriptorField::Container,
                StaticMetadataViolation::OutOfBounds,
            )
        })?;
    let raw_container = format
        .container
        .clone()
        .map(RawContainerIdentity::new)
        .transpose()
        .map_err(|_| {
            invalid_metadata(
                StaticDescriptorField::Container,
                StaticMetadataViolation::OutOfBounds,
            )
        })?;
    let container = ContainerIdentity::parse(raw_extension, raw_container);
    let family = match container.consistent_family() {
        Ok(family) => family,
        Err(conflict) if hls_ts_output_hint_is_compatible(transport_family, conflict) => {
            Some(ContainerFamily::MpegTs)
        }
        Err(conflict) => {
            return Err(static_rejection(
                StaticCompatibilityRejection::ContainerHintsConflict { conflict },
            ));
        }
    };

    match family {
        None if container.raw_extension().is_some() || container.raw_container().is_some() => Err(
            static_rejection(StaticCompatibilityRejection::UnknownContainer {
                container: container.clone(),
            }),
        ),
        None => Err(invalid_metadata(
            StaticDescriptorField::Container,
            StaticMetadataViolation::Missing,
        )),
        Some(ContainerFamily::Unknown) => Err(static_rejection(
            StaticCompatibilityRejection::UnknownContainer {
                container: container.clone(),
            },
        )),
        Some(ContainerFamily::MpegProgramStream | ContainerFamily::Avi | ContainerFamily::Asf) => {
            Err(static_rejection(
                StaticCompatibilityRejection::ProfileExcluded {
                    reason: ProfileExclusionReason::ProvisionalContainer,
                },
            ))
        }
        Some(_) => Ok(container),
    }
}

/// yt-dlp HLS row может сообщать planned output `ext=mp4` при реальном MPEG-TS.
const fn hls_ts_output_hint_is_compatible(
    transport_family: TransportFamily,
    conflict: web_media_core::ContainerHintConflict,
) -> bool {
    matches!(transport_family, TransportFamily::Hls)
        && matches!(conflict.extension, ContainerFamily::IsoBmff)
        && matches!(conflict.container, ContainerFamily::MpegTs)
}

/// Парсит обязательный codec hint и проверяет media kind.
fn normalize_codec(
    raw_codec: Option<&str>,
    expected_media: CodecMediaKind,
) -> Result<NormalizedCodec, YtDlpCandidateNormalizationRejection> {
    let field = codec_field(expected_media);
    let raw_codec =
        raw_codec.ok_or_else(|| invalid_metadata(field, StaticMetadataViolation::Missing))?;
    let codec = NormalizedCodec::parse(
        RawCodecIdentity::new(raw_codec)
            .map_err(|_| invalid_metadata(field, StaticMetadataViolation::OutOfBounds))?,
    );

    match codec.kind() {
        CodecKind::Unknown => Err(static_rejection(
            StaticCompatibilityRejection::UnsupportedCodec {
                expected_media,
                codec: codec.clone(),
            },
        )),
        CodecKind::Known(CodecFamily::IsoBmffAudio) => Err(invalid_metadata(
            field,
            StaticMetadataViolation::Insufficient,
        )),
        CodecKind::Known(family) if family.media_kind() != expected_media => Err(invalid_metadata(
            field,
            StaticMetadataViolation::WrongMediaKind,
        )),
        _ => Ok(codec),
    }
}

/// Строит shape без Option-комбинаций и synthetic pair generation.
fn normalize_layout(
    format: &YtDlpSerializedFormat,
    transport: NormalizedTransport,
    container: ContainerIdentity,
    video_codec: NormalizedCodec,
    audio_codec: NormalizedCodec,
) -> Result<StreamLayout, YtDlpCandidateNormalizationRejection> {
    match (video_codec.kind(), audio_codec.kind()) {
        (CodecKind::Known(_), CodecKind::Known(_)) => {
            let video = normalize_video_track(format, video_codec)?;
            let audio = normalize_audio_track(format, audio_codec)?;
            Ok(StreamLayout::Muxed(MuxedComponentDescriptor::new(
                transport, container, video, audio,
            )))
        }
        (CodecKind::Known(_), CodecKind::Absent) => {
            let video = normalize_video_track(format, video_codec)?;
            Ok(StreamLayout::VideoOnly(VideoComponentDescriptor::new(
                transport, container, video,
            )))
        }
        (CodecKind::Absent, CodecKind::Known(_)) => {
            let audio = normalize_audio_track(format, audio_codec)?;
            Ok(StreamLayout::AudioOnly(AudioComponentDescriptor::new(
                transport, container, audio,
            )))
        }
        _ => Err(YtDlpCandidateNormalizationRejection::InvalidStreamLayout),
    }
}

/// Строит bounded video descriptor и conservative HDR hint.
fn normalize_video_track(
    format: &YtDlpSerializedFormat,
    codec: NormalizedCodec,
) -> Result<VideoTrackDescriptor, YtDlpCandidateNormalizationRejection> {
    let width = format
        .width
        .map(VideoWidth::new)
        .transpose()
        .map_err(|_| invalid_video_dimensions())?;
    let height = format
        .height
        .map(VideoHeight::new)
        .transpose()
        .map_err(|_| invalid_video_dimensions())?;
    let frame_rate = format.fps.map(normalize_frame_rate).transpose()?;
    let bitrate = normalize_bitrate(format.vbr.or(format.tbr))?;
    let dynamic_range = normalize_dynamic_range(format.dynamic_range.as_deref());

    Ok(VideoTrackDescriptor::new(
        codec,
        width,
        height,
        frame_rate,
        bitrate,
        dynamic_range,
    ))
}

/// Строит bounded audio descriptor.
fn normalize_audio_track(
    format: &YtDlpSerializedFormat,
    codec: NormalizedCodec,
) -> Result<AudioTrackDescriptor, YtDlpCandidateNormalizationRejection> {
    let sample_rate = format.asr.map(normalize_sample_rate).transpose()?;
    let channels = format
        .audio_channels
        .map(ChannelCount::new)
        .transpose()
        .map_err(|_| {
            invalid_metadata(
                StaticDescriptorField::AudioChannels,
                StaticMetadataViolation::OutOfBounds,
            )
        })?;
    let bitrate = normalize_bitrate(format.abr.or(format.tbr))?;
    let language = format
        .language
        .as_deref()
        .map(LanguageTag::new)
        .transpose()
        .map_err(|_| {
            invalid_metadata(
                StaticDescriptorField::AudioCodec,
                StaticMetadataViolation::OutOfBounds,
            )
        })?;

    Ok(AudioTrackDescriptor::new(
        codec,
        sample_rate,
        channels,
        bitrate,
        language,
    ))
}

/// Переводит decimal FPS в bounded rational с deterministic reduction.
fn normalize_frame_rate(
    frames_per_second: f64,
) -> Result<FrameRate, YtDlpCandidateNormalizationRejection> {
    if !frames_per_second.is_finite() || frames_per_second <= 0.0 {
        return Err(invalid_frame_rate());
    }
    let scaled = (frames_per_second * f64::from(FRAME_RATE_SCALE)).round();
    if scaled <= 0.0 || scaled > f64::from(u32::MAX) {
        return Err(invalid_frame_rate());
    }
    let numerator = scaled as u32;
    let divisor = greatest_common_divisor(numerator, FRAME_RATE_SCALE);
    FrameRate::new(numerator / divisor, FRAME_RATE_SCALE / divisor)
        .map_err(|_| invalid_frame_rate())
}

/// Проверяет integer sample rate из JSON number.
fn normalize_sample_rate(
    sample_rate: f64,
) -> Result<SampleRate, YtDlpCandidateNormalizationRejection> {
    if !sample_rate.is_finite()
        || sample_rate <= 0.0
        || sample_rate.fract() != 0.0
        || sample_rate > f64::from(u32::MAX)
    {
        return Err(invalid_metadata(
            StaticDescriptorField::AudioSampleRate,
            StaticMetadataViolation::OutOfBounds,
        ));
    }
    SampleRate::new(sample_rate as u32).map_err(|_| {
        invalid_metadata(
            StaticDescriptorField::AudioSampleRate,
            StaticMetadataViolation::OutOfBounds,
        )
    })
}

/// Переводит positive Kbit/s hint в bits/s.
fn normalize_bitrate(
    kilobits_per_second: Option<f64>,
) -> Result<Option<Bitrate>, YtDlpCandidateNormalizationRejection> {
    let Some(kilobits_per_second) = kilobits_per_second else {
        return Ok(None);
    };
    let bits_per_second = (kilobits_per_second * KILOBITS_TO_BITS).round();
    if !bits_per_second.is_finite() || bits_per_second <= 0.0 || bits_per_second > u64::MAX as f64 {
        return Err(invalid_layout_bounds());
    }
    Bitrate::new(bits_per_second as u64)
        .map(Some)
        .map_err(|_| invalid_layout_bounds())
}

/// Признаёт только exact typed dynamic-range labels, не descriptions.
fn normalize_dynamic_range(raw_dynamic_range: Option<&str>) -> DynamicRange {
    let Some(raw_dynamic_range) = raw_dynamic_range else {
        return DynamicRange::Unknown;
    };
    match raw_dynamic_range.trim().to_ascii_uppercase().as_str() {
        "SDR" => DynamicRange::Sdr,
        "HDR" | "HDR10" | "HDR10+" | "HLG" | "PQ" => DynamicRange::Hdr,
        _ => DynamicRange::Unknown,
    }
}

/// Сохраняет только labels с однозначным transfer; общий `HDR` остаётся без evidence.
fn normalize_video_color_evidence(
    raw_dynamic_range: Option<&str>,
) -> Option<YtDlpVideoColorEvidence> {
    match raw_dynamic_range?.trim().to_ascii_uppercase().as_str() {
        "HDR10" | "HDR10+" => Some(YtDlpVideoColorEvidence::Bt2020PqLimited),
        "HLG" => Some(YtDlpVideoColorEvidence::Bt2020HlgLimited),
        _ => None,
    }
}

/// Euclidean reduction rational FPS.
const fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

/// Выбирает neutral descriptor field по expected media kind.
const fn codec_field(expected_media: CodecMediaKind) -> StaticDescriptorField {
    match expected_media {
        CodecMediaKind::Video => StaticDescriptorField::VideoCodec,
        CodecMediaKind::Audio => StaticDescriptorField::AudioCodec,
    }
}

/// Сокращает boilerplate neutral rejection wrapper-а.
const fn static_rejection(
    rejection: StaticCompatibilityRejection,
) -> YtDlpCandidateNormalizationRejection {
    YtDlpCandidateNormalizationRejection::Static(rejection)
}

/// Строит typed invalid-metadata rejection.
const fn invalid_metadata(
    field: StaticDescriptorField,
    violation: StaticMetadataViolation,
) -> YtDlpCandidateNormalizationRejection {
    static_rejection(StaticCompatibilityRejection::InvalidMetadata { field, violation })
}

/// Повторяемая ошибка dimensions остаётся named.
const fn invalid_video_dimensions() -> YtDlpCandidateNormalizationRejection {
    invalid_metadata(
        StaticDescriptorField::VideoDimensions,
        StaticMetadataViolation::OutOfBounds,
    )
}

/// Повторяемая ошибка FPS остаётся named.
const fn invalid_frame_rate() -> YtDlpCandidateNormalizationRejection {
    invalid_metadata(
        StaticDescriptorField::FrameRate,
        StaticMetadataViolation::OutOfBounds,
    )
}

/// Повторяемая ошибка bitrate/layout bounds остаётся named.
const fn invalid_layout_bounds() -> YtDlpCandidateNormalizationRejection {
    invalid_metadata(
        StaticDescriptorField::Layout,
        StaticMetadataViolation::OutOfBounds,
    )
}
