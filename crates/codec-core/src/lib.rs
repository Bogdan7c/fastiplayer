//! Типизированная модель codec/profile/color для выбора аппаратного decode.
//!
//! Crate намеренно не зависит от VA-API, renderer-а или UI. Он описывает только
//! факты о видеопотоке и форматах, которые может поддерживать backend.

#![forbid(unsafe_code)]

mod adapter;
mod model;
mod profile;
mod vp9;

pub use adapter::{
    VideoMetadataSource, VideoPacketKeyframeProbe, VideoRequirementCandidate,
    VideoRequirementProbe, VideoRequirementRejection, VideoRequirementUncertainty,
    VideoResolvedMetadata, probe_video_packet_keyframe, probe_video_packet_requirement,
    resolve_video_metadata, unsupported_requirement_can_be_refined_by_packet_probe,
    video_requirement_needs_packet_refinement, vp9_profile_from_video_profile,
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
