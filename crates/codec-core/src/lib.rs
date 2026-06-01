//! Типизированная модель codec/profile/color для выбора аппаратного decode.
//!
//! Crate намеренно не зависит от VA-API, renderer-а или UI. Он описывает только
//! факты о видеопотоке и форматах, которые может поддерживать backend.

#![forbid(unsafe_code)]

mod adapter;
mod h264;
mod model;
mod profile;
mod vp9;

pub use adapter::{
    VideoMetadataSource, VideoPacketKeyframeProbe, VideoRequirementCandidate,
    VideoRequirementProbe, VideoRequirementRejection, VideoRequirementUncertainty,
    VideoResolvedMetadata, probe_video_packet_keyframe,
    probe_video_packet_keyframe_with_codec_private, probe_video_packet_requirement,
    probe_video_packet_requirement_with_codec_private, resolve_video_metadata,
    unsupported_requirement_can_be_refined_by_packet_probe,
    video_requirement_needs_packet_refinement, vp9_profile_from_video_profile,
};
pub use h264::{
    AvcDecoderConfigurationRecord, AvcDecoderConfigurationRecordError, H264BitReaderError,
    H264ByteStreamError, H264NalLengthSize, H264NalUnit, H264Packetization, H264PacketizationError,
    H264ParameterSetInjection, H264ParameterSetKind, H264RequirementError, H264SpsError,
    H264SpsMetadata, h264_access_unit_to_annex_b, h264_nal_units,
    h264_sps_metadata_from_avc_decoder_configuration_record, h264_sps_metadata_from_packet,
    infer_h264_packetization, parse_avc_decoder_configuration_record, parse_h264_sps_metadata,
    probe_h264_packet_keyframe,
};
pub use model::{
    AudioCodec, BitDepth, ChromaSubsampling, CodecLevel, ColorMetadataConfidence,
    ColorMetadataOrigin, ColorPipelineRequirement, ColorPrimaries, ColorRange, DecodeBackendId,
    FrameTimingContract, HdrMetadata, MatrixCoefficients, SupportedVideoDecodeFormat,
    TransferFunction, VideoCodec, VideoColorMetadata, VideoDecodeRequirement, VideoMemoryContract,
    VideoSurfaceFormat, ZeroCopyExportRequirement,
};
pub use profile::{Av1Profile, H264Profile, H265Profile, VideoProfile, Vp8Profile, Vp9Profile};
pub use vp9::{
    Vp9DecodedFormatRequirement, Vp9MetadataConflict, Vp9MetadataDiagnostic, Vp9MetadataField,
    Vp9MetadataSource, Vp9RequirementCandidate, Vp9RequirementProbe, Vp9RequirementRejection,
    Vp9RequirementUncertainty, Vp9ResolvedMetadata, Vp9StrictHdrValidationError,
    probe_vp9_packet_requirement, resolve_vp9_metadata, validate_vp9_strict_hdr_core,
};
