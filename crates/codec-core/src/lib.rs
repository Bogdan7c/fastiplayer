//! Типизированная модель codec/profile/color для выбора аппаратного decode.
//!
//! Crate намеренно не зависит от VA-API, renderer-а или UI. Он описывает только
//! факты о видеопотоке и форматах, которые может поддерживать backend.

#![forbid(unsafe_code)]

mod adapter;
mod av1;
mod h264;
mod h265;
mod model;
mod profile;
mod requirement_preflight;
mod vp8;
mod vp9;
mod vp_configuration;

pub use adapter::{
    VideoMetadataSource, VideoPacketKeyframeProbe, VideoRequirementCandidate,
    VideoRequirementProbe, VideoRequirementRejection, VideoRequirementUncertainty,
    VideoResolvedMetadata, probe_video_packet_keyframe,
    probe_video_packet_keyframe_with_codec_private, probe_video_packet_requirement,
    probe_video_packet_requirement_with_codec_private, resolve_video_metadata,
    unsupported_requirement_can_be_refined_by_packet_probe,
    video_requirement_needs_packet_refinement, vp9_profile_from_video_profile,
};
pub use av1::{
    Av1DecoderConfigurationRecordError, Av1ObuError,
    av1_decode_requirement_from_decoder_configuration_record, probe_av1_packet_keyframe,
};
pub use h264::{
    AvcDecoderConfigurationRecord, AvcDecoderConfigurationRecordError, H264BitReaderError,
    H264ByteStreamError, H264NalLengthSize, H264NalUnit, H264Packetization, H264PacketizationError,
    H264ParameterSetInjection, H264ParameterSetKind, H264ProfileIndication, H264RequirementError,
    H264SpsError, H264SpsMetadata, UnsupportedH264ProfileIndication, h264_access_unit_to_annex_b,
    h264_access_unit_to_annex_b_into, h264_nal_units, h264_profile_from_indication,
    h264_sps_metadata_from_avc_decoder_configuration_record, h264_sps_metadata_from_packet,
    infer_h264_packetization, parse_avc_decoder_configuration_record, parse_h264_sps_metadata,
    probe_h264_packet_keyframe,
};
pub use h265::{
    H265BitReaderError, H265ByteStreamError, H265HeaderMetadata, H265NalLengthSize, H265NalUnit,
    H265PacketDecodeStartProbe, H265Packetization, H265PacketizationError,
    H265ParameterSetInjection, H265ParameterSetKind, H265ProfileTierLevel, H265RequirementError,
    H265SpsError, H265SpsMetadata, HevcDecoderConfigurationRecord,
    HevcDecoderConfigurationRecordError, h265_access_unit_to_annex_b,
    h265_access_unit_to_annex_b_into,
    h265_decode_requirement_from_hevc_decoder_configuration_record,
    h265_decode_requirement_from_packet,
    h265_header_metadata_from_hevc_decoder_configuration_record, h265_nal_units,
    h265_sps_metadata_from_hevc_decoder_configuration_record, h265_sps_metadata_from_packet,
    infer_h265_packetization, parse_h265_sps_metadata, parse_hevc_decoder_configuration_record,
    probe_h265_packet_decode_start,
};
pub use model::{
    AudioCodec, BitDepth, ChromaSubsampling, CodecLevel, ColorMetadataConfidence,
    ColorMetadataOrigin, ColorPipelineRequirement, ColorPrimaries, ColorRange, DecodeBackendId,
    FrameTimingContract, HdrMetadata, MatrixCoefficients, SupportedVideoDecodeFormat,
    TransferFunction, VideoCodec, VideoColorMetadata, VideoDecodeRequirement,
    VideoDisplayOrientation, video_frame_pixel_layout_from_codec_fields,
    video_frame_pixel_layout_from_decode_requirement,
    video_frame_pixel_layout_from_optional_codec_fields,
};
pub use profile::{Av1Profile, H264Profile, H265Profile, VideoProfile, Vp8Profile, Vp9Profile};
pub use requirement_preflight::{
    VideoRequirementEvidencePolicy, VideoRequirementPreflight,
    VideoRequirementPreflightUnavailable, preflight_video_requirement,
    video_requirement_evidence_policy,
};
pub use video_frame_contract::VideoFramePixelLayout;
pub use vp_configuration::{
    VpChromaSubsampling, VpCodecConfiguration, VpCodecConfigurationError,
    VpCodecConfigurationLayout, parse_vp_codec_configuration,
};
pub use vp8::{Vp8PacketHeaderError, probe_vp8_packet_keyframe};
pub use vp9::{
    Vp9DecodedFormatRequirement, Vp9MetadataConflict, Vp9MetadataDiagnostic, Vp9MetadataField,
    Vp9MetadataSource, Vp9RequirementCandidate, Vp9RequirementProbe, Vp9RequirementRejection,
    Vp9RequirementUncertainty, Vp9ResolvedMetadata, Vp9StrictHdrValidationError,
    probe_vp9_packet_requirement, resolve_vp9_metadata, validate_vp9_strict_hdr_core,
};
