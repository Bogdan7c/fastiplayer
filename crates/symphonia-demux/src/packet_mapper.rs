use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use codec_core::{VideoCodec, VideoPacketKeyframeProbe, probe_video_packet_keyframe};
use media_core::{
    Packet as MediaPacket, PacketKeyframe, TrackDuration, TrackId, TrackKind, TrackTimestamp,
};
use symphonia::core::packet::Packet as SymphoniaPacket;
use symphonia::core::units::{TimeBase as SymphoniaTimeBase, Timestamp};

use crate::symphonia_api::{
    media_time_base_from_symphonia, symphonia_duration_to_duration, symphonia_timestamp_to_duration,
};
use crate::track_mapper::TrackEntry;

/// Результат packet-level validation до отдачи packet-а в player pipeline.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PacketConvertError {
    /// Container отдал packet для track-а, которого не было в metadata.
    UnknownTrack {
        /// Сырой Symphonia/container track id.
        track_id: u32,
    },

    /// Packet принадлежит track-у, который demuxer увидел при open, но намеренно не экспортировал.
    UnsupportedTrack {
        /// Track, к которому относится пропускаемый packet.
        track_id: TrackId,

        /// Человекочитаемая причина для trace diagnostics.
        reason: &'static str,
    },
}

/// Конвертирует Symphonia packet в neutral media-core packet.
pub(crate) fn convert_packet(
    packet: SymphoniaPacket,
    track_map: &HashMap<u32, TrackEntry>,
) -> Result<MediaPacket, PacketConvertError> {
    let packet_track_id = packet.track_id;
    let track_entry = track_map
        .get(&packet_track_id)
        .ok_or(PacketConvertError::UnknownTrack {
            track_id: packet_track_id,
        })?;
    let Some(track_kind) = track_entry.supported_kind() else {
        let unsupported_kind = track_entry
            .unsupported_kind()
            .expect("unsupported TrackEntry kind должен иметь причину");
        return Err(PacketConvertError::UnsupportedTrack {
            track_id: TrackId::new(packet_track_id),
            reason: unsupported_kind.diagnostic_reason(),
        });
    };
    let track_id = TrackId::new(packet_track_id);
    let track_pts = packet_track_timestamp(track_id, track_entry.time_base, packet.pts);
    let track_dts = packet_track_timestamp(track_id, track_entry.time_base, packet.dts);
    let pts = packet_presentation_timestamp(track_entry.time_base, packet.pts);
    let dts = packet_decode_timestamp(track_entry.time_base, packet.pts, packet.dts);
    let duration = packet_duration(track_entry.time_base, packet.dur);
    let track_duration = packet_track_duration(track_id, track_entry.time_base, packet.dur);
    let keyframe = packet_keyframe(track_entry, track_kind, &packet.data);

    Ok(MediaPacket {
        track_id,
        kind: track_kind,
        pts,
        track_pts,
        dts,
        track_dts,
        duration,
        track_duration,
        byte_offset: None,
        keyframe,
        data: Bytes::from(packet.data),
    })
}

/// Сохраняет packet timestamp в signed units исходного track-а, если metadata дала time base.
fn packet_track_timestamp(
    track_id: TrackId,
    time_base: Option<SymphoniaTimeBase>,
    timestamp: Timestamp,
) -> Option<TrackTimestamp> {
    time_base
        .and_then(media_time_base_from_symphonia)
        .map(|time_base| TrackTimestamp::new(track_id, timestamp.get(), time_base))
}

/// Нормализует presentation timestamp packet-а в неотрицательную media timeline.
fn packet_presentation_timestamp(
    time_base: Option<SymphoniaTimeBase>,
    timestamp: Timestamp,
) -> Duration {
    packet_timestamp_to_duration(time_base, timestamp)
}

/// Нормализует DTS только когда Symphonia сообщает его отдельно от PTS.
fn packet_decode_timestamp(
    time_base: Option<SymphoniaTimeBase>,
    pts_timestamp: Timestamp,
    dts_timestamp: Timestamp,
) -> Option<Duration> {
    // Symphonia 0.6 хранит DTS обязательным полем и default-ит его в PTS,
    // поэтому public Packet не позволяет отличить явно равный DTS от отсутствующего.
    if dts_timestamp == pts_timestamp {
        return None;
    }

    Some(packet_timestamp_to_duration(time_base, dts_timestamp))
}

/// Нормализует Symphonia packet duration в media-core duration.
fn packet_duration(
    time_base: Option<SymphoniaTimeBase>,
    duration: symphonia::core::units::Duration,
) -> Option<Duration> {
    time_base.map(|time_base| symphonia_duration_to_duration(time_base, duration))
}

/// Сохраняет packet duration в unsigned units исходного track-а, если metadata дала time base.
fn packet_track_duration(
    track_id: TrackId,
    time_base: Option<SymphoniaTimeBase>,
    duration: symphonia::core::units::Duration,
) -> Option<TrackDuration> {
    time_base
        .and_then(media_time_base_from_symphonia)
        .map(|time_base| TrackDuration::new(track_id, duration.get(), time_base))
}

/// Переводит signed Symphonia timestamp через media-core helpers без потери знака до clamp-а.
fn packet_timestamp_to_duration(
    time_base: Option<SymphoniaTimeBase>,
    timestamp: Timestamp,
) -> Duration {
    time_base
        .map(|time_base| symphonia_timestamp_to_duration(time_base, timestamp))
        .unwrap_or_default()
}

/// Определяет keyframe только через codec-aware probe, потому что Symphonia 0.6 Packet не несёт flag.
fn packet_keyframe(
    track_entry: &TrackEntry,
    track_kind: TrackKind,
    packet_data: &[u8],
) -> PacketKeyframe {
    if track_kind != TrackKind::Video {
        return PacketKeyframe::NotKeyframe;
    }

    match VideoCodec::from_container_codec_id(&track_entry.codec_id)
        .map(|codec| probe_video_packet_keyframe(codec, packet_data))
    {
        Some(VideoPacketKeyframeProbe::Keyframe(keyframe)) => PacketKeyframe::from_known(keyframe),
        Some(VideoPacketKeyframeProbe::Uncertain(_))
        | Some(VideoPacketKeyframeProbe::AdapterUnavailable { .. })
        | None => PacketKeyframe::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use media_core::{PacketKeyframe, TrackId, TrackKind};
    use symphonia::core::packet::{Packet as SymphoniaPacket, PacketBuilder};
    use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase, Timestamp};

    use super::{PacketConvertError, convert_packet};
    use crate::track_mapper::{TrackEntry, TrackEntryKind, UnsupportedTrackKind};

    fn track_entry(kind: TrackKind, codec_id: &str) -> TrackEntry {
        TrackEntry {
            kind: TrackEntryKind::Supported(kind),
            codec_id: codec_id.to_string(),
            time_base: Some(TimeBase::try_new(1, 48_000).expect("valid time base")),
            sample_rate: None,
            channels: None,
        }
    }

    fn track_entry_without_time_base(kind: TrackKind, codec_id: &str) -> TrackEntry {
        TrackEntry {
            kind: TrackEntryKind::Supported(kind),
            codec_id: codec_id.to_string(),
            time_base: None,
            sample_rate: None,
            channels: None,
        }
    }

    fn unsupported_track_entry(kind: UnsupportedTrackKind, codec_id: &str) -> TrackEntry {
        TrackEntry {
            kind: TrackEntryKind::Unsupported(kind),
            codec_id: codec_id.to_string(),
            time_base: Some(TimeBase::try_new(1, 48_000).expect("valid time base")),
            sample_rate: None,
            channels: None,
        }
    }

    fn packet(track_id: u32, pts: i64, bytes: &'static [u8]) -> SymphoniaPacket {
        SymphoniaPacket::new(
            track_id,
            Timestamp::new(pts),
            SymphoniaDuration::new(1),
            bytes.to_vec(),
        )
    }

    fn packet_with_dts(track_id: u32, pts: i64, dts: i64, bytes: &'static [u8]) -> SymphoniaPacket {
        PacketBuilder::new()
            .track_id(track_id)
            .pts(Timestamp::new(pts))
            .dts(Timestamp::new(dts))
            .dur(SymphoniaDuration::new(1))
            .data(bytes.to_vec())
            .build()
    }

    fn owned_packet(track_id: u32, pts: i64, bytes: Vec<u8>) -> SymphoniaPacket {
        SymphoniaPacket::new(
            track_id,
            Timestamp::new(pts),
            SymphoniaDuration::new(1),
            bytes,
        )
    }

    fn build_vp9_keyframe() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0b10, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(0);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0x498342, 24);
        push_bits(&mut bits, 1, 3);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn build_vp9_inter_frame() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0b10, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(1);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0, 2);
        push_bits(&mut bits, 0x01, 8);
        push_bits(&mut bits, 1, 3);
        bits.push(1);
        push_bits(&mut bits, 2, 3);
        bits.push(0);
        push_bits(&mut bits, 3, 3);
        bits.push(1);
        bits.push(0);
        bits.push(0);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                let mut byte = 0_u8;
                for (index, bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                byte
            })
            .collect()
    }

    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    fn push_profile(bits: &mut Vec<u8>, profile: u8) {
        bits.push(profile & 1);
        bits.push((profile >> 1) & 1);
        if profile == 3 {
            bits.push(0);
        }
    }

    #[test]
    fn converts_audio_packet_to_media_packet() {
        let track_map = HashMap::from([(7, track_entry(TrackKind::Audio, "A_OPUS"))]);

        let packet = convert_packet(packet(7, 48_000, b"opus"), &track_map)
            .expect("audio packet должен конвертироваться");

        assert_eq!(packet.track_id, TrackId::new(7));
        assert_eq!(packet.kind, TrackKind::Audio);
        assert_eq!(packet.pts, Duration::from_secs(1));
        assert_eq!(
            packet
                .track_pts
                .expect("raw PTS должен сохраняться")
                .units
                .get(),
            48_000
        );
        assert_eq!(packet.dts, None);
        assert_eq!(
            packet
                .track_dts
                .expect("Symphonia raw DTS должен сохраняться даже при DTS == PTS")
                .units
                .get(),
            48_000
        );
        assert_eq!(packet.duration, Some(Duration::from_nanos(20_833)));
        assert_eq!(
            packet
                .track_duration
                .expect("raw duration должен сохраняться")
                .units
                .get(),
            1
        );
        assert_eq!(&packet.data[..], b"opus");
        assert_eq!(packet.keyframe, PacketKeyframe::NotKeyframe);
    }

    #[test]
    fn converts_presentation_timestamp_from_packet_pts() {
        let track_map = HashMap::from([(7, track_entry(TrackKind::Audio, "A_OPUS"))]);

        let packet = convert_packet(packet(7, 96_000, b"opus"), &track_map)
            .expect("packet PTS должен стать media-core PTS");

        assert_eq!(packet.pts, Duration::from_secs(2));
    }

    #[test]
    fn converts_packet_duration_from_packet_dur() {
        let track_map = HashMap::from([(7, track_entry(TrackKind::Audio, "A_OPUS"))]);

        let packet = convert_packet(packet(7, 96_000, b"opus"), &track_map)
            .expect("packet duration должен стать media-core duration");

        assert_eq!(packet.duration, Some(Duration::from_nanos(20_833)));
        assert_eq!(
            packet
                .track_duration
                .expect("raw duration должен остаться в container units")
                .units
                .get(),
            1
        );
    }

    #[test]
    fn converts_decode_timestamp_when_dts_differs_from_pts() {
        let track_map = HashMap::from([(7, track_entry(TrackKind::Audio, "A_OPUS"))]);

        let packet = convert_packet(packet_with_dts(7, 96_000, 48_000, b"opus"), &track_map)
            .expect("packet DTS должен стать media-core DTS");

        assert_eq!(packet.pts, Duration::from_secs(2));
        assert_eq!(packet.dts, Some(Duration::from_secs(1)));
        assert_eq!(
            packet
                .track_dts
                .expect("raw DTS должен сохранять container units")
                .units
                .get(),
            48_000
        );
    }

    #[test]
    fn negative_presentation_timestamp_clamps_to_zero() {
        let track_map = HashMap::from([(7, track_entry(TrackKind::Audio, "A_OPUS"))]);

        let packet = convert_packet(packet(7, -24_000, b"opus"), &track_map)
            .expect("negative PTS должен нормализоваться без ошибки");

        assert_eq!(packet.pts, Duration::ZERO);
        let raw_pts = packet
            .track_pts
            .expect("negative raw PTS должен сохраниться");
        assert_eq!(raw_pts.units.get(), -24_000);
        assert!(raw_pts.units.is_negative());
        assert_eq!(packet.dts, None);
    }

    #[test]
    fn packet_without_track_time_base_keeps_no_raw_timestamp() {
        let track_map =
            HashMap::from([(7, track_entry_without_time_base(TrackKind::Audio, "A_OPUS"))]);

        let packet = convert_packet(packet(7, 48_000, b"opus"), &track_map)
            .expect("packet без time base должен оставаться readable");

        assert_eq!(packet.pts, Duration::ZERO);
        assert_eq!(packet.track_pts, None);
        assert_eq!(packet.track_dts, None);
        assert_eq!(packet.duration, None);
        assert_eq!(packet.track_duration, None);
    }

    #[test]
    fn packet_for_unknown_track_is_fatal_mapping_error() {
        let track_map = HashMap::new();

        let error = convert_packet(packet(99, 0, b"data"), &track_map)
            .expect_err("unknown track должен быть fatal");

        assert_eq!(error, PacketConvertError::UnknownTrack { track_id: 99 });
    }

    #[test]
    fn packet_for_unsupported_track_is_skipped_mapping_error() {
        let track_map = HashMap::from([(
            9,
            unsupported_track_entry(UnsupportedTrackKind::Subtitle, "S_TEXT/WEBVTT"),
        )]);

        let error = convert_packet(packet(9, 0, b"subtitle"), &track_map)
            .expect_err("unsupported track packet должен быть skip signal");

        assert_eq!(
            error,
            PacketConvertError::UnsupportedTrack {
                track_id: TrackId::new(9),
                reason: UnsupportedTrackKind::Subtitle.diagnostic_reason(),
            }
        );
    }

    #[test]
    fn uncertain_vp9_keyframe_probe_converts_with_unknown_keyframe() {
        let track_map = HashMap::from([(1, track_entry(TrackKind::Video, "V_VP9"))]);

        let packet = convert_packet(packet(1, 0, b"\x00"), &track_map)
            .expect("неуверенная keyframe-проба не должна ломать demux-конвертацию");

        assert_eq!(packet.keyframe, PacketKeyframe::Unknown);
    }

    #[test]
    fn unavailable_keyframe_adapter_converts_with_unknown_keyframe() {
        let track_map = HashMap::from([(1, track_entry(TrackKind::Video, "V_AV1"))]);

        let packet = convert_packet(packet(1, 0, b"av1"), &track_map)
            .expect("packet без keyframe adapter-а не должен молча становиться non-keyframe");

        assert_eq!(packet.keyframe, PacketKeyframe::Unknown);
    }

    #[test]
    fn valid_vp9_keyframe_packet_sets_keyframe_flag() {
        let track_map = HashMap::from([(1, track_entry(TrackKind::Video, "V_VP9"))]);

        let packet = convert_packet(owned_packet(1, 0, build_vp9_keyframe()), &track_map)
            .expect("валидный VP9 keyframe должен конвертироваться");

        assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
    }

    #[test]
    fn valid_vp9_inter_frame_packet_sets_non_keyframe_flag() {
        let track_map = HashMap::from([(1, track_entry(TrackKind::Video, "V_VP9"))]);

        let packet = convert_packet(owned_packet(1, 0, build_vp9_inter_frame()), &track_map)
            .expect("валидный VP9 inter-frame должен конвертироваться");

        assert_eq!(packet.keyframe, PacketKeyframe::NotKeyframe);
    }
}
