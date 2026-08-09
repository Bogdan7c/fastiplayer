use codec_core::{
    H264Packetization, H265Packetization, parse_avc_decoder_configuration_record,
    parse_avc3_decoder_configuration_record, parse_hevc_decoder_configuration_record,
};
use media_core::{TrackInfo, VideoPacketFraming};
use video_core::VideoStreamPacketization;

use crate::{PlayerError, PlayerErrorKind, PlayerResult};

/// Разрешает neutral H.264 framing evidence в decoder-owned packetization.
pub(super) fn h264_packetization_from_track(
    track: &TrackInfo,
) -> PlayerResult<Option<VideoStreamPacketization>> {
    match packet_framing(track) {
        VideoPacketFraming::AnnexB => {
            reject_codec_configuration_conflict(track, "H.264")?;
            Ok(Some(VideoStreamPacketization::H264(
                H264Packetization::AnnexB,
            )))
        }
        VideoPacketFraming::Unspecified
        | VideoPacketFraming::LengthPrefixedFromCodecConfiguration => {
            h264_length_prefixed_packetization(track)
        }
        VideoPacketFraming::LengthPrefixedWithInBandParameterSets => {
            h264_in_band_parameter_set_packetization(track)
        }
    }
}

/// Разрешает `avc3`: length-prefix остаётся в `avcC`, а SPS/PPS приходят в packets.
fn h264_in_band_parameter_set_packetization(
    track: &TrackInfo,
) -> PlayerResult<Option<VideoStreamPacketization>> {
    let Some(codec_private) = track
        .codec_private
        .as_ref()
        .filter(|bytes| !bytes.is_empty())
    else {
        return Err(missing_codec_configuration_error(
            track,
            "H.264",
            "avc3 avcC",
        ));
    };
    let record = parse_avc3_decoder_configuration_record(codec_private)
        .map_err(|error| invalid_codec_configuration_error(track, "H.264", "avc3 avcC", error))?;
    Ok(Some(VideoStreamPacketization::H264(
        H264Packetization::from_avc3_decoder_configuration_record(&record),
    )))
}

/// Разрешает neutral H.265 framing evidence без container guessing-а.
pub(super) fn h265_packetization_from_track(
    track: &TrackInfo,
) -> PlayerResult<Option<VideoStreamPacketization>> {
    if packet_framing(track) == VideoPacketFraming::AnnexB {
        reject_codec_configuration_conflict(track, "H.265")?;
        return Ok(Some(VideoStreamPacketization::H265(
            H265Packetization::AnnexB,
        )));
    }
    let Some(codec_private) = track
        .codec_private
        .as_deref()
        .filter(|bytes| !bytes.is_empty())
    else {
        if packet_framing(track) == VideoPacketFraming::LengthPrefixedFromCodecConfiguration {
            return Err(missing_codec_configuration_error(track, "H.265", "hvcC"));
        }
        return Err(PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            format!(
                "H.265 track `{}` не содержит hvcC codec_private; packetization нельзя доказать до decoder config",
                track.id
            ),
        ));
    };
    let record = parse_hevc_decoder_configuration_record(codec_private)
        .map_err(|error| invalid_codec_configuration_error(track, "H.265", "hvcC", error))?;
    Ok(Some(VideoStreamPacketization::H265(record.packetization())))
}

fn h264_length_prefixed_packetization(
    track: &TrackInfo,
) -> PlayerResult<Option<VideoStreamPacketization>> {
    let Some(codec_private) = track
        .codec_private
        .as_ref()
        .filter(|bytes| !bytes.is_empty())
    else {
        if packet_framing(track) == VideoPacketFraming::LengthPrefixedFromCodecConfiguration {
            return Err(missing_codec_configuration_error(track, "H.264", "avcC"));
        }
        return Ok(None);
    };
    let record = parse_avc_decoder_configuration_record(codec_private)
        .map_err(|error| invalid_codec_configuration_error(track, "H.264", "avcC", error))?;
    Ok(Some(VideoStreamPacketization::H264(record.packetization())))
}

fn packet_framing(track: &TrackInfo) -> VideoPacketFraming {
    track
        .video
        .as_ref()
        .map_or(VideoPacketFraming::Unspecified, |metadata| {
            metadata.packet_framing
        })
}

/// Не позволяет двум authoritative framing evidence молча спорить друг с другом.
fn reject_codec_configuration_conflict(track: &TrackInfo, codec: &str) -> PlayerResult<()> {
    if track
        .codec_private
        .as_ref()
        .is_some_and(|bytes| !bytes.is_empty())
    {
        return Err(PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            format!(
                "{codec} track `{}` одновременно объявляет Annex-B packets и codec configuration для length-prefixed packets",
                track.id
            ),
        ));
    }
    Ok(())
}

fn missing_codec_configuration_error(
    track: &TrackInfo,
    codec: &str,
    configuration_name: &str,
) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::UnsupportedVideoCodec,
        format!(
            "{codec} track `{}` объявляет length-prefixed packets, но не содержит {configuration_name} codec_private",
            track.id
        ),
    )
}

fn invalid_codec_configuration_error(
    track: &TrackInfo,
    codec: &str,
    configuration_name: &str,
    error: impl std::fmt::Display,
) -> PlayerError {
    PlayerError::new(
        PlayerErrorKind::UnsupportedVideoCodec,
        format!(
            "{codec} track `{}` codec_private не является поддержанным {configuration_name}: {error}",
            track.id
        ),
    )
}
