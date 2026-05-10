//! Типизированная модель codec/profile/color для выбора аппаратного decode.
//!
//! Crate намеренно не зависит от VA-API, renderer-а или UI. Он описывает только
//! факты о видеопотоке и форматах, которые может поддерживать backend.

#![forbid(unsafe_code)]

mod model;
mod profile;
mod vp9;

pub use model::{
    AudioCodec, BitDepth, ChromaSubsampling, CodecLevel, ColorMetadataConfidence,
    ColorMetadataOrigin, ColorPrimaries, ColorRange, DecodeBackendId, HdrMetadata,
    MatrixCoefficients, SupportedVideoDecodeFormat, TransferFunction, VideoCodec,
    VideoColorMetadata, VideoDecodeRequirement,
};
pub use profile::{Av1Profile, H264Profile, H265Profile, VideoProfile, Vp8Profile, Vp9Profile};
pub use vp9::{
    Vp9DecodedFormatRequirement, Vp9MetadataConflict, Vp9MetadataDiagnostic, Vp9MetadataField,
    Vp9MetadataSource, Vp9RequirementCandidate, Vp9RequirementProbe, Vp9RequirementRejection,
    Vp9RequirementUncertainty, Vp9ResolvedMetadata, Vp9StrictHdrValidationError,
    probe_vp9_packet_requirement, resolve_vp9_metadata, validate_vp9_strict_hdr_core,
};
