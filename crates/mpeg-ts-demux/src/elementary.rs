use bytes::Bytes;
use codec_core::{
    H264Packetization, H265PacketDecodeStartProbe, H265Packetization,
    probe_h264_packet_in_band_decode_start, probe_h264_packet_keyframe,
    probe_h265_packet_decode_start,
};
use media_core::{PacketDecodeStartInitialization, PacketKeyframe};

use crate::MpegTsDemuxError;

/// Результат frame splitting без container-specific state.
#[derive(Debug)]
pub(crate) struct ElementaryPacket {
    /// Decoder bytes одного audio frame или video access unit.
    pub(crate) bytes: Bytes,
    /// Точная/неизвестная decode-start классификация.
    pub(crate) keyframe: PacketKeyframe,
    /// Наличие required decoder configuration перед decode-start picture.
    pub(crate) decode_start_initialization: PacketDecodeStartInitialization,
    /// Sample rate, доказанный audio frame header-ом.
    pub(crate) sample_rate: Option<u32>,
    /// Channel count, доказанный ADTS header-ом.
    pub(crate) channels: Option<u32>,
    /// Exact stable audio codec ID после MPEG layer classification.
    pub(crate) audio_codec_id: Option<&'static str>,
    /// Duration одного audio frame в 90 kHz units.
    pub(crate) duration_90khz: Option<u64>,
}

/// Извлекает complete ADTS frames, оставляя split tail для следующего PES.
pub(crate) fn drain_adts_frames(
    payload: &mut Vec<u8>,
) -> Result<Vec<ElementaryPacket>, MpegTsDemuxError> {
    let mut frames = Vec::new();
    let mut cursor = 0_usize;
    while cursor < payload.len() {
        if payload.len() - cursor < 7 {
            break;
        }
        if payload[cursor] != 0xff || payload[cursor + 1] & 0xf6 != 0xf0 {
            return Err(malformed(
                "AAC stream_type 0x0f не содержит ADTS sync/header",
            ));
        }
        let protection_absent = payload[cursor + 1] & 0x01 != 0;
        let sample_rate_index = usize::from((payload[cursor + 2] >> 2) & 0x0f);
        let sample_rates = [
            96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025,
            8_000, 7_350,
        ];
        let Some(&sample_rate) = sample_rates.get(sample_rate_index) else {
            return Err(malformed("ADTS sample frequency index зарезервирован"));
        };
        let channels = u32::from(((payload[cursor + 2] & 0x01) << 2) | (payload[cursor + 3] >> 6));
        if channels == 0 {
            return Err(malformed(
                "ADTS program config element без channel config не поддержан",
            ));
        }
        let frame_length = (usize::from(payload[cursor + 3] & 0x03) << 11)
            | (usize::from(payload[cursor + 4]) << 3)
            | usize::from(payload[cursor + 5] >> 5);
        let header_length = if protection_absent { 7 } else { 9 };
        if frame_length < header_length {
            return Err(malformed("ADTS frame length меньше header"));
        }
        if cursor + frame_length > payload.len() {
            break;
        }
        let raw_frame =
            Bytes::copy_from_slice(&payload[cursor + header_length..cursor + frame_length]);
        frames.push(ElementaryPacket {
            bytes: raw_frame,
            keyframe: PacketKeyframe::NotKeyframe,
            decode_start_initialization:
                PacketDecodeStartInitialization::RequiresTrackConfiguration,
            sample_rate: Some(sample_rate),
            channels: Some(channels),
            audio_codec_id: Some("A_AAC"),
            duration_90khz: Some(1024_u64.saturating_mul(90_000) / u64::from(sample_rate)),
        });
        cursor += frame_length;
    }
    payload.drain(..cursor);
    Ok(frames)
}

/// Извлекает complete MPEG audio frames и удерживает split tail.
pub(crate) fn drain_mpeg_audio_frames(
    payload: &mut Vec<u8>,
) -> Result<Vec<ElementaryPacket>, MpegTsDemuxError> {
    let mut frames = Vec::new();
    let mut cursor = 0_usize;
    while cursor < payload.len() {
        if payload.len() - cursor < 4 {
            break;
        }
        let header = parse_mpeg_audio_header(&payload[cursor..])?;
        if cursor + header.frame_length > payload.len() {
            break;
        }
        frames.push(ElementaryPacket {
            bytes: Bytes::copy_from_slice(&payload[cursor..cursor + header.frame_length]),
            keyframe: PacketKeyframe::NotKeyframe,
            decode_start_initialization:
                PacketDecodeStartInitialization::RequiresTrackConfiguration,
            sample_rate: Some(header.sample_rate),
            channels: None,
            audio_codec_id: Some(header.codec_id),
            duration_90khz: Some(
                u64::from(header.samples_per_frame).saturating_mul(90_000)
                    / u64::from(header.sample_rate),
            ),
        });
        cursor += header.frame_length;
    }
    payload.drain(..cursor);
    Ok(frames)
}

/// Делит Annex-B PES на access units; codec-core владеет NAL/keyframe semantics.
#[cfg(test)]
pub(crate) fn split_video_access_units(
    payload: &[u8],
    is_h265: bool,
) -> Result<Vec<ElementaryPacket>, MpegTsDemuxError> {
    let boundaries = video_access_unit_boundaries(payload, is_h265)?;
    let mut packets = Vec::with_capacity(boundaries.len());
    for (start, end) in boundaries {
        packets.push(classify_video_access_unit(&payload[start..end], is_h265)?);
    }
    Ok(packets)
}

pub(crate) fn classify_video_access_unit(
    payload: &[u8],
    is_h265: bool,
) -> Result<ElementaryPacket, MpegTsDemuxError> {
    let (keyframe, decode_start_initialization) = if is_h265 {
        let keyframe = match probe_h265_packet_decode_start(payload, H265Packetization::AnnexB) {
            H265PacketDecodeStartProbe::DecodeStart => PacketKeyframe::Keyframe,
            H265PacketDecodeStartProbe::NotDecodeStart => PacketKeyframe::NotKeyframe,
            H265PacketDecodeStartProbe::Uncertain(error) => {
                return Err(malformed(&format!("H.265 Annex-B AU: {error}")));
            }
        };
        (
            keyframe,
            PacketDecodeStartInitialization::RequiresTrackConfiguration,
        )
    } else {
        let keyframe = probe_h264_packet_keyframe(payload, H264Packetization::AnnexB)
            .map(PacketKeyframe::from_known)
            .map_err(|error| malformed(&format!("H.264 Annex-B AU: {error}")))?;
        let includes_in_band_configuration = if keyframe.is_known_keyframe() {
            probe_h264_packet_in_band_decode_start(payload, H264Packetization::AnnexB)
                .map_err(|error| malformed(&format!("H.264 Annex-B AU: {error}")))?
        } else {
            false
        };
        let initialization = if includes_in_band_configuration {
            PacketDecodeStartInitialization::IncludesInBandConfiguration
        } else {
            PacketDecodeStartInitialization::RequiresTrackConfiguration
        };
        (keyframe, initialization)
    };
    Ok(ElementaryPacket {
        bytes: Bytes::copy_from_slice(payload),
        keyframe,
        decode_start_initialization,
        sample_rate: None,
        channels: None,
        audio_codec_id: None,
        duration_90khz: None,
    })
}

pub(crate) fn video_access_unit_boundaries(
    payload: &[u8],
    is_h265: bool,
) -> Result<Vec<(usize, usize)>, MpegTsDemuxError> {
    let nals = annex_b_nal_offsets(payload, is_h265)?;
    let mut starts = vec![0_usize];
    let mut saw_vcl = false;
    for nal in nals {
        let is_aud = if is_h265 {
            nal.nal_type == 35
        } else {
            nal.nal_type == 9
        };
        let is_vcl = if is_h265 {
            nal.nal_type <= 31
        } else {
            matches!(nal.nal_type, 1..=5)
        };
        let starts_picture = if !is_vcl {
            false
        } else if is_h265 {
            payload
                .get(nal.header_offset + 2)
                .is_some_and(|byte| byte & 0x80 != 0)
        } else {
            h264_first_mb_is_zero(&payload[nal.header_offset + 1..])
        };
        let starts_leading_non_vcl = saw_vcl
            && if is_h265 {
                matches!(nal.nal_type, 32..=35 | 39)
            } else {
                matches!(nal.nal_type, 6..=9 | 14..=18)
            };
        if (is_aud || starts_picture || starts_leading_non_vcl)
            && saw_vcl
            && nal.start_offset > *starts.last().unwrap_or(&0)
        {
            starts.push(nal.start_offset);
            saw_vcl = false;
        }
        saw_vcl |= is_vcl;
    }
    let mut ranges = Vec::with_capacity(starts.len());
    for (index, start) in starts.iter().copied().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(payload.len());
        if start < end {
            ranges.push((start, end));
        }
    }
    Ok(ranges)
}

#[derive(Debug, Clone, Copy)]
struct AnnexBNalOffset {
    start_offset: usize,
    header_offset: usize,
    nal_type: u8,
}

fn annex_b_nal_offsets(
    payload: &[u8],
    is_h265: bool,
) -> Result<Vec<AnnexBNalOffset>, MpegTsDemuxError> {
    let mut offsets = Vec::new();
    let mut cursor = 0_usize;
    while cursor + 3 <= payload.len() {
        let start_code_length = if payload[cursor..].starts_with(&[0, 0, 1]) {
            Some(3)
        } else if payload[cursor..].starts_with(&[0, 0, 0, 1]) {
            Some(4)
        } else {
            None
        };
        let Some(start_code_length) = start_code_length else {
            cursor += 1;
            continue;
        };
        let header_offset = cursor + start_code_length;
        let Some(&header) = payload.get(header_offset) else {
            // Stateful assembler может получить start code на самом конце PES;
            // header станет доступен после следующего PES, поэтому это не corruption.
            break;
        };
        offsets.push(AnnexBNalOffset {
            start_offset: cursor,
            header_offset,
            nal_type: if is_h265 {
                (header >> 1) & 0x3f
            } else {
                header & 0x1f
            },
        });
        cursor = header_offset + 1;
    }
    if offsets.is_empty() {
        return Err(malformed("video PES не содержит Annex-B NAL units"));
    }
    Ok(offsets)
}

fn h264_first_mb_is_zero(rbsp_with_emulation_prevention: &[u8]) -> bool {
    // Exp-Golomb `first_mb_in_slice == 0` кодируется первым RBSP bit-ом `1`.
    rbsp_with_emulation_prevention
        .first()
        .is_some_and(|byte| byte & 0x80 != 0)
}

#[derive(Debug)]
struct MpegAudioHeader {
    codec_id: &'static str,
    sample_rate: u32,
    samples_per_frame: u32,
    frame_length: usize,
}

fn parse_mpeg_audio_header(payload: &[u8]) -> Result<MpegAudioHeader, MpegTsDemuxError> {
    if payload.len() < 4 || payload[0] != 0xff || payload[1] & 0xe0 != 0xe0 {
        return Err(malformed("MPEG audio sync/header отсутствует"));
    }
    let version = (payload[1] >> 3) & 0x03;
    let layer_bits = (payload[1] >> 1) & 0x03;
    let bitrate_index = usize::from(payload[2] >> 4);
    let sample_rate_index = usize::from((payload[2] >> 2) & 0x03);
    if version == 0x01
        || layer_bits == 0
        || matches!(bitrate_index, 0 | 15)
        || sample_rate_index == 3
    {
        return Err(malformed("зарезервированный MPEG audio header"));
    }
    let mpeg1 = version == 0x03;
    let layer = usize::from(4 - layer_bits);
    let bitrate_table_mpeg1 = [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
    ];
    let bitrate_table_mpeg2 = [
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ];
    let bitrate_kbps = if mpeg1 {
        bitrate_table_mpeg1[layer - 1][bitrate_index]
    } else {
        bitrate_table_mpeg2[layer - 1][bitrate_index]
    };
    let base_sample_rate = [44_100_u32, 48_000, 32_000][sample_rate_index];
    let sample_rate = match version {
        0x03 => base_sample_rate,
        0x02 => base_sample_rate / 2,
        0x00 => base_sample_rate / 4,
        _ => unreachable!(),
    };
    let bitrate = usize::try_from(bitrate_kbps).unwrap_or(0) * 1_000;
    let padding = usize::from((payload[2] >> 1) & 1);
    let (codec_id, samples_per_frame, frame_length) = match layer {
        1 => (
            "A_MP1",
            384,
            ((12 * bitrate / sample_rate as usize) + padding) * 4,
        ),
        2 => (
            "A_MP2",
            1_152,
            144 * bitrate / sample_rate as usize + padding,
        ),
        3 if mpeg1 => (
            "A_MP3",
            1_152,
            144 * bitrate / sample_rate as usize + padding,
        ),
        3 => ("A_MP3", 576, 72 * bitrate / sample_rate as usize + padding),
        _ => unreachable!(),
    };
    Ok(MpegAudioHeader {
        codec_id,
        sample_rate,
        samples_per_frame,
        frame_length,
    })
}

fn malformed(reason: &str) -> MpegTsDemuxError {
    MpegTsDemuxError::Malformed {
        reason: reason.to_owned(),
    }
}
