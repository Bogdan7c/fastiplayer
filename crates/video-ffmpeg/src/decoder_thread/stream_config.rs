use codec_core::{H264Packetization, H265Packetization, VideoDecodeRequirement};
use video_core::{VideoStreamConfigRejection, VideoStreamDecodeConfig, VideoStreamPacketization};

use super::FfmpegOpenDecoderError;

#[cfg(feature = "ffmpeg")]
pub(super) fn video_decode_requirement_from_stream_config(
    config: &VideoStreamDecodeConfig,
) -> VideoDecodeRequirement {
    let mut requirement = VideoDecodeRequirement::new(config.codec);

    if let Some(profile) = config.profile {
        requirement = requirement.with_profile(profile);
    }

    if let Some(bit_depth) = config.bit_depth {
        requirement = requirement.with_bit_depth(bit_depth);
    }

    if let Some(chroma) = config.chroma {
        requirement = requirement.with_chroma(chroma);
    }

    if let (Some(width), Some(height)) = (config.coded_width, config.coded_height) {
        requirement = requirement.with_resolution(width, height);
    }

    requirement
}

/// Решает, какие codec-private bytes нужно установить как FFmpeg extradata.
///
/// Length-prefixed (`avcC`/`hvcC`) H.264/H.265 streams требуют extradata, иначе
/// FFmpeg парсит length-prefixed NAL units как Annex B и падает с
/// `AVERROR_INVALIDDATA`. Annex B потоки несут SPS/PPS in-band, поэтому extradata
/// им не передаётся (иначе decoder ошибочно переключится в length-prefixed режим).
/// Для остальных кодеков codec-private передаётся как есть, если он присутствует.
#[cfg(feature = "ffmpeg")]
pub(super) fn extradata_for_stream_config(
    config: &VideoStreamDecodeConfig,
) -> Result<Option<Vec<u8>>, FfmpegOpenDecoderError> {
    let length_prefixed = matches!(
        config.packetization,
        Some(VideoStreamPacketization::H264(
            H264Packetization::AvccLengthPrefixed { .. }
                | H264Packetization::AvccLengthPrefixedWithInBandParameterSets { .. }
        )) | Some(VideoStreamPacketization::H265(
            H265Packetization::HvccLengthPrefixed { .. }
        ))
    );

    if length_prefixed {
        let codec_private = config
            .codec_private
            .as_ref()
            .filter(|bytes| !bytes.is_empty())
            .ok_or_else(|| {
                FfmpegOpenDecoderError::Unsupported(
                    VideoStreamConfigRejection::InvalidCodecPrivate {
                        codec: config.codec,
                        reason:
                            "length-prefixed H.264/H.265 stream requires avcC/hvcC codec-private \
                             extradata, but it is missing or empty"
                                .to_string(),
                    },
                )
            })?;
        return Ok(Some(codec_private.to_vec()));
    }

    if matches!(
        config.packetization,
        Some(VideoStreamPacketization::H264(H264Packetization::AnnexB))
            | Some(VideoStreamPacketization::H265(H265Packetization::AnnexB))
    ) {
        return Ok(None);
    }

    Ok(config
        .codec_private
        .as_ref()
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| bytes.to_vec()))
}
