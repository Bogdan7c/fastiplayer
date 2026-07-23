use bytes::Bytes;
use codec_core::{
    VideoCodec, VideoPacketKeyframeProbe, VpCodecConfigurationLayout,
    av1_decode_requirement_from_decoder_configuration_record,
    parse_avc_decoder_configuration_record, parse_hevc_decoder_configuration_record,
    parse_vp_codec_configuration, probe_video_packet_keyframe_with_codec_private,
};
use media_core::{
    PacketKeyframe, TimeBase, TrackId, TrackInfo, TrackKind, VideoPacketFraming, VideoTrackMetadata,
};

use crate::FlvDemuxError;

pub(crate) const VIDEO_TRACK_ID: TrackId = TrackId::new(1);
pub(crate) const AUDIO_TRACK_ID: TrackId = TrackId::new(2);

/// Validated config, который целиком заменяет прежний track snapshot.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrackConfiguration {
    pub(crate) track: TrackInfo,
    pub(crate) video_codec: Option<VideoCodec>,
}

/// Packet-level результат разбора одного codec tag-а.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EncodedTagPacket {
    pub(crate) track_id: TrackId,
    pub(crate) kind: TrackKind,
    pub(crate) composition_offset_ms: i32,
    pub(crate) keyframe: PacketKeyframe,
    pub(crate) bytes: Bytes,
}

/// Lifecycle event codec payload-а.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CodecTagEvent {
    Configuration(TrackConfiguration),
    Packet(EncodedTagPacket),
    SequenceEnd { track_id: TrackId },
}

/// Разбирает legacy/enhanced video header и codec-specific packet type.
pub(crate) fn parse_video_tag(
    payload: &Bytes,
    active_configuration: Option<&TrackConfiguration>,
) -> Result<CodecTagEvent, FlvDemuxError> {
    let first = *payload
        .first()
        .ok_or_else(|| malformed("пустой video tag"))?;
    if first & 0x80 != 0 {
        parse_enhanced_video_tag(payload, active_configuration)
    } else {
        parse_legacy_video_tag(payload, active_configuration)
    }
}

/// Разбирает selected legacy audio codecs без alias expansion.
pub(crate) fn parse_audio_tag(payload: &Bytes) -> Result<CodecTagEvent, FlvDemuxError> {
    let header = *payload
        .first()
        .ok_or_else(|| malformed("пустой audio tag"))?;
    let format = header >> 4;
    let is_16_bit = header & 0x02 != 0;
    match format {
        0 if is_16_bit => Err(unsupported(
            "platform-endian 16-bit PCM неоднозначен и намеренно отклонён",
        )),
        0 => audio_packet("A_PCM_U8", payload.slice(1..)),
        1 => audio_packet("A_ADPCM_SWF", payload.slice(1..)),
        2 => audio_packet("A_MP3", payload.slice(1..)),
        3 if is_16_bit => audio_packet("A_PCM_S16LE", payload.slice(1..)),
        3 => audio_packet("A_PCM_U8", payload.slice(1..)),
        10 => parse_aac_tag(payload),
        7 | 8 => Err(unsupported("G.711 вне выбранного S30 codec scope")),
        unsupported_format => Err(unsupported(format!(
            "legacy audio SoundFormat={unsupported_format} не поддерживается"
        ))),
    }
}

fn parse_legacy_video_tag(
    payload: &Bytes,
    active_configuration: Option<&TrackConfiguration>,
) -> Result<CodecTagEvent, FlvDemuxError> {
    let first = payload[0];
    let frame_type = first >> 4;
    validate_video_frame_type(frame_type)?;
    match first & 0x0f {
        7 if matches!(frame_type, 3 | 4) => Err(unsupported(format!(
            "legacy AVC не допускает FLV FrameType={frame_type}"
        ))),
        7 => parse_video_packet_body(
            VideoCodec::H264,
            "V_MPEG4/ISO/AVC",
            frame_type,
            payload.slice(1..),
            active_configuration,
            false,
        ),
        12 => Err(unsupported(
            "legacy HEVC codec id 12 не является доказанным Enhanced RTMP path",
        )),
        codec_id => Err(unsupported(format!(
            "legacy video CodecID={codec_id} (H263/Screen/VP6/unknown) не поддерживается"
        ))),
    }
}

fn parse_enhanced_video_tag(
    payload: &Bytes,
    active_configuration: Option<&TrackConfiguration>,
) -> Result<CodecTagEvent, FlvDemuxError> {
    let header = payload[0];
    let packet_type = header & 0x0f;
    if packet_type == 6 {
        return Err(unsupported(
            "Enhanced RTMP multitrack video не поддерживается",
        ));
    }
    if packet_type == 7 {
        return Err(unsupported("Enhanced RTMP ModEx не поддерживается"));
    }
    if packet_type == 5 {
        return Err(unsupported(
            "Enhanced RTMP MPEG2TS packet type не поддерживается",
        ));
    }
    let fourcc = payload
        .get(1..5)
        .ok_or_else(|| malformed("Enhanced video FourCC обрезан"))?;
    let (codec, codec_id) = match fourcc {
        b"vp08" => (VideoCodec::Vp8, "V_VP8"),
        b"vp09" => (VideoCodec::Vp9, "V_VP9"),
        b"av01" => (VideoCodec::Av1, "V_AV1"),
        b"avc1" => (VideoCodec::H264, "V_MPEG4/ISO/AVC"),
        b"hvc1" => (VideoCodec::H265, "V_MPEGH/ISO/HEVC"),
        b"vvc1" | b"vvi1" => return Err(unsupported("VVC вне выбранного S30 codec scope")),
        other => {
            return Err(unsupported(format!(
                "Enhanced video FourCC `{}` не поддерживается",
                String::from_utf8_lossy(other)
            )));
        }
    };
    let frame_type = (header >> 4) & 0x07;
    validate_video_frame_type(frame_type)?;
    let body = payload.slice(5..);
    parse_video_packet_body_with_type(
        codec,
        codec_id,
        frame_type,
        packet_type,
        body,
        active_configuration,
        true,
    )
}

fn parse_video_packet_body(
    codec: VideoCodec,
    codec_id: &'static str,
    frame_type: u8,
    body: Bytes,
    active_configuration: Option<&TrackConfiguration>,
    enhanced: bool,
) -> Result<CodecTagEvent, FlvDemuxError> {
    let packet_type = *body
        .first()
        .ok_or_else(|| malformed("video packet type обрезан"))?;
    if !enhanced && packet_type == 3 {
        return Err(unsupported(
            "legacy AVC packet type 3 зарезервирован для Enhanced RTMP",
        ));
    }
    let body = body.slice(1..);
    let body = if matches!(packet_type, 0 | 2) {
        if body.len() < 3 {
            return Err(malformed("legacy AVC composition-time field обрезан"));
        }
        body.slice(3..)
    } else {
        body
    };
    parse_video_packet_body_with_type(
        codec,
        codec_id,
        frame_type,
        packet_type,
        body,
        active_configuration,
        enhanced,
    )
}

fn parse_video_packet_body_with_type(
    codec: VideoCodec,
    codec_id: &'static str,
    frame_type: u8,
    packet_type: u8,
    body: Bytes,
    active_configuration: Option<&TrackConfiguration>,
    enhanced: bool,
) -> Result<CodecTagEvent, FlvDemuxError> {
    match packet_type {
        0 => validate_video_configuration(codec, codec_id, body, enhanced)
            .map(CodecTagEvent::Configuration),
        1 | 3 => {
            let (composition_offset_ms, packet_bytes) =
                if matches!(codec, VideoCodec::H264 | VideoCodec::H265) {
                    if packet_type == 3 {
                        (0, body)
                    } else {
                        let composition = body
                            .get(..3)
                            .ok_or_else(|| malformed("signed SI24 composition offset обрезан"))?;
                        (read_si24(composition), body.slice(3..))
                    }
                } else {
                    (0, body)
                };
            if packet_bytes.is_empty() {
                return Err(malformed("coded video frame payload пуст"));
            }
            let configuration = active_configuration
                .filter(|configuration| configuration.video_codec == Some(codec));
            let codec_private = configuration.and_then(|configuration| {
                configuration
                    .track
                    .codec_private
                    .as_ref()
                    .map(Bytes::as_ref)
            });
            let probed_keyframe = match probe_video_packet_keyframe_with_codec_private(
                codec,
                &packet_bytes,
                codec_private,
            ) {
                VideoPacketKeyframeProbe::Keyframe(true) if frame_type == 1 => {
                    PacketKeyframe::Keyframe
                }
                VideoPacketKeyframeProbe::Keyframe(_) => PacketKeyframe::NotKeyframe,
                VideoPacketKeyframeProbe::AdapterUnavailable { .. }
                | VideoPacketKeyframeProbe::Uncertain(_) => PacketKeyframe::Unknown,
            };
            Ok(CodecTagEvent::Packet(EncodedTagPacket {
                track_id: VIDEO_TRACK_ID,
                kind: TrackKind::Video,
                composition_offset_ms,
                keyframe: probed_keyframe,
                bytes: packet_bytes,
            }))
        }
        2 => Ok(CodecTagEvent::SequenceEnd {
            track_id: VIDEO_TRACK_ID,
        }),
        4 => Err(unsupported(
            "Enhanced video metadata packet type не является media packet",
        )),
        unknown => Err(unsupported(format!(
            "Enhanced/AVC video packet type {unknown} не поддерживается"
        ))),
    }
}

fn validate_video_configuration(
    codec: VideoCodec,
    codec_id: &'static str,
    bytes: Bytes,
    enhanced: bool,
) -> Result<TrackConfiguration, FlvDemuxError> {
    if bytes.is_empty() {
        return Err(invalid_config(codec_id, "configuration payload пуст"));
    }
    match codec {
        VideoCodec::H264 => parse_avc_decoder_configuration_record(&bytes)
            .map(|_| ())
            .map_err(|error| invalid_config(codec_id, error.to_string()))?,
        VideoCodec::H265 => parse_hevc_decoder_configuration_record(&bytes)
            .map(|_| ())
            .map_err(|error| invalid_config(codec_id, error.to_string()))?,
        VideoCodec::Av1 => av1_decode_requirement_from_decoder_configuration_record(&bytes)
            .map(|_| ())
            .map_err(|error| invalid_config(codec_id, error.to_string()))?,
        VideoCodec::Vp8 | VideoCodec::Vp9 => {
            let layout = if enhanced {
                VpCodecConfigurationLayout::FfmpegEnhancedRtmpSequenceStart
            } else {
                VpCodecConfigurationLayout::Record
            };
            parse_vp_codec_configuration(codec, layout, &bytes)
                .map(|_| ())
                .map_err(|error| invalid_config(codec_id, error.to_string()))?;
        }
    }
    let mut video = VideoTrackMetadata::empty();
    video.packet_framing = match codec {
        VideoCodec::H264 | VideoCodec::H265 => {
            VideoPacketFraming::LengthPrefixedFromCodecConfiguration
        }
        VideoCodec::Vp8 | VideoCodec::Vp9 | VideoCodec::Av1 => VideoPacketFraming::Unspecified,
    };
    Ok(TrackConfiguration {
        track: TrackInfo {
            id: VIDEO_TRACK_ID,
            kind: TrackKind::Video,
            codec_id: codec_id.to_owned(),
            codec_private: Some(bytes),
            time_base: TimeBase::new(1, 1_000),
            duration: None,
            sample_rate: None,
            channels: None,
            video: Some(video),
        },
        video_codec: Some(codec),
    })
}

fn parse_aac_tag(payload: &Bytes) -> Result<CodecTagEvent, FlvDemuxError> {
    let packet_type = *payload
        .get(1)
        .ok_or_else(|| malformed("AACPacketType обрезан"))?;
    let body = payload.slice(2..);
    match packet_type {
        0 => {
            let (sample_rate, channels) = parse_audio_specific_config(&body)?;
            Ok(CodecTagEvent::Configuration(TrackConfiguration {
                track: TrackInfo {
                    id: AUDIO_TRACK_ID,
                    kind: TrackKind::Audio,
                    codec_id: "A_AAC".to_owned(),
                    codec_private: Some(body),
                    time_base: TimeBase::new(1, 1_000),
                    duration: None,
                    sample_rate: Some(sample_rate),
                    channels: Some(channels),
                    video: None,
                },
                video_codec: None,
            }))
        }
        1 if body.is_empty() => Err(malformed("AAC raw packet пуст")),
        1 => Ok(EncodedTagPacket {
            track_id: AUDIO_TRACK_ID,
            kind: TrackKind::Audio,
            composition_offset_ms: 0,
            keyframe: PacketKeyframe::NotKeyframe,
            bytes: body,
        }
        .into()),
        unknown => Err(unsupported(format!("AAC packet type {unknown} неизвестен"))),
    }
}

impl From<EncodedTagPacket> for CodecTagEvent {
    fn from(packet: EncodedTagPacket) -> Self {
        Self::Packet(packet)
    }
}

fn audio_packet(codec_id: &'static str, bytes: Bytes) -> Result<CodecTagEvent, FlvDemuxError> {
    if bytes.is_empty() {
        return Err(malformed(format!(
            "legacy audio payload для {codec_id} пуст"
        )));
    }
    Ok(CodecTagEvent::Packet(EncodedTagPacket {
        track_id: AUDIO_TRACK_ID,
        kind: TrackKind::Audio,
        composition_offset_ms: 0,
        keyframe: PacketKeyframe::NotKeyframe,
        bytes,
    }))
}

/// Отсекает reserved/command frame identities до codec probe и decoder boundary.
fn validate_video_frame_type(frame_type: u8) -> Result<(), FlvDemuxError> {
    match frame_type {
        1..=4 => Ok(()),
        5 => Err(unsupported(
            "video command/info frame не содержит coded media",
        )),
        reserved => Err(unsupported(format!(
            "зарезервированный FLV video FrameType={reserved}"
        ))),
    }
}

/// Возвращает implicit track config для self-describing legacy audio tag-а.
pub(crate) fn legacy_audio_configuration(
    payload: &Bytes,
) -> Result<Option<TrackConfiguration>, FlvDemuxError> {
    let header = *payload
        .first()
        .ok_or_else(|| malformed("пустой audio tag"))?;
    let format = header >> 4;
    if format == 10 {
        return Ok(None);
    }
    let rate = [5_500, 11_025, 22_050, 44_100][usize::from((header >> 2) & 3)];
    let channels = if header & 1 != 0 { 2 } else { 1 };
    let codec_id = match (format, header & 2 != 0) {
        (0, true) => return Err(unsupported("platform-endian 16-bit PCM неоднозначен")),
        (0 | 3, false) => "A_PCM_U8",
        (3, true) => "A_PCM_S16LE",
        (1, _) => "A_ADPCM_SWF",
        (2, _) => "A_MP3",
        _ => return Ok(None),
    };
    Ok(Some(TrackConfiguration {
        track: TrackInfo {
            id: AUDIO_TRACK_ID,
            kind: TrackKind::Audio,
            codec_id: codec_id.to_owned(),
            codec_private: None,
            time_base: TimeBase::new(1, 1_000),
            duration: None,
            sample_rate: Some(rate),
            channels: Some(channels),
            video: None,
        },
        video_codec: None,
    }))
}

fn parse_audio_specific_config(bytes: &[u8]) -> Result<(u32, u32), FlvDemuxError> {
    let mut bits = AacBitReader::new(bytes);
    let audio_object_type = read_audio_object_type(&mut bits)?;
    if !matches!(audio_object_type, 2 | 5 | 29) {
        return Err(invalid_config(
            "AAC",
            format!("audio object type {audio_object_type} не входит в profile"),
        ));
    }
    let core_sample_rate = read_aac_sample_rate(&mut bits)?;
    let channel_configuration = bits.read(4)? as u8;
    let sample_rate = if matches!(audio_object_type, 5 | 29) {
        let extension_sample_rate = read_aac_sample_rate(&mut bits)?;
        let extension_object_type = read_audio_object_type(&mut bits)?;
        if extension_object_type != 2 {
            return Err(invalid_config(
                "AAC",
                format!("SBR/PS core object type {extension_object_type} не поддерживается"),
            ));
        }
        extension_sample_rate
    } else {
        core_sample_rate
    };
    let channels = match channel_configuration {
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 5,
        6 => 6,
        7 => 8,
        other => {
            return Err(invalid_config(
                "AAC",
                format!("channel configuration {other} unsupported"),
            ));
        }
    };
    Ok((sample_rate, channels))
}

fn read_audio_object_type(bits: &mut AacBitReader<'_>) -> Result<u8, FlvDemuxError> {
    let base = bits.read(5)? as u8;
    if base == 31 {
        Ok(32_u8.saturating_add(bits.read(6)? as u8))
    } else {
        Ok(base)
    }
}

fn read_aac_sample_rate(bits: &mut AacBitReader<'_>) -> Result<u32, FlvDemuxError> {
    let frequency_index = bits.read(4)? as u8;
    let sample_rate = match frequency_index {
        0 => 96_000,
        1 => 88_200,
        2 => 64_000,
        3 => 48_000,
        4 => 44_100,
        5 => 32_000,
        6 => 24_000,
        7 => 22_050,
        8 => 16_000,
        9 => 12_000,
        10 => 11_025,
        11 => 8_000,
        12 => 7_350,
        15 => {
            let explicit = bits.read(24)?;
            if explicit == 0 {
                return Err(invalid_config(
                    "AAC",
                    "explicit sample rate не может быть 0",
                ));
            }
            explicit
        }
        reserved => {
            return Err(invalid_config(
                "AAC",
                format!("reserved frequency index {reserved}"),
            ));
        }
    };
    Ok(sample_rate)
}

struct AacBitReader<'a> {
    bytes: &'a [u8],
    bit_offset: usize,
}

impl<'a> AacBitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_offset: 0,
        }
    }

    fn read(&mut self, bit_count: usize) -> Result<u32, FlvDemuxError> {
        if bit_count > 32
            || self
                .bit_offset
                .checked_add(bit_count)
                .is_none_or(|end| end > self.bytes.len().saturating_mul(8))
        {
            return Err(invalid_config(
                "AAC",
                "AudioSpecificConfig bit field обрезан",
            ));
        }
        let mut value = 0_u32;
        for _ in 0..bit_count {
            let byte = self.bytes[self.bit_offset / 8];
            let shift = 7 - (self.bit_offset % 8);
            value = value << 1 | u32::from((byte >> shift) & 1);
            self.bit_offset += 1;
        }
        Ok(value)
    }
}

fn read_si24(bytes: &[u8]) -> i32 {
    let raw = i32::from(bytes[0]) << 16 | i32::from(bytes[1]) << 8 | i32::from(bytes[2]);
    if raw & 0x0080_0000 != 0 {
        raw | !0x00ff_ffff
    } else {
        raw
    }
}

fn malformed(reason: impl Into<String>) -> FlvDemuxError {
    FlvDemuxError::MalformedTag {
        offset: 0,
        reason: reason.into(),
    }
}

fn unsupported(reason: impl Into<String>) -> FlvDemuxError {
    FlvDemuxError::UnsupportedCodec {
        reason: reason.into(),
    }
}

fn invalid_config(codec: &'static str, reason: impl Into<String>) -> FlvDemuxError {
    FlvDemuxError::InvalidConfiguration {
        codec,
        reason: reason.into(),
    }
}
