//! Packet-level validation и decode-start policy до decoder send boundary.
//!
//! Модуль владеет codec probe/refinement, typed validation outcome, keyframe
//! framing и stale-generation решением. Фактическая отправка, in-flight
//! accounting, decoder drain и release decoded frames остаются у родителя.

use std::time::Duration;

use bytes::Bytes;
use codec_core::{
    VideoCodec, VideoDecodeRequirement, VideoRequirementProbe,
    probe_video_packet_requirement_with_codec_private, resolve_video_metadata,
    video_requirement_needs_packet_refinement,
};
use media_core::{PacketKeyframe, TrackId};
use tracing::{debug, trace, warn};

use crate::session::capability_selection::ActiveVideoRequirementRefinement;
use crate::session::video_requirement_error::player_error_from_requirement_rejection;
use crate::{PendingVideoPacket, PlaybackPipeline, PlayerError, session::PlayerSession};

use super::PlayerVideoDropReason;

/// Проверяет, нужен ли front packet-у decoder capacity, или tick может списать его локально.
pub(in crate::session::tick) fn pending_video_packet_requires_decoder_send_capacity(
    session: &PlayerSession,
    packet: &PendingVideoPacket,
) -> bool {
    if !session
        .pipeline
        .packet_generation_is_current(packet.generation)
    {
        return false;
    }

    if !session
        .pipeline
        .video_packet_belongs_to_selected_track(packet.track_id)
    {
        return false;
    }

    if !session.pipeline.video_decoder_needs_keyframe() {
        return true;
    }

    match packet.keyframe {
        PacketKeyframe::Keyframe => true,
        PacketKeyframe::NotKeyframe => false,
        PacketKeyframe::Unknown => !active_video_codec_requires_proven_decode_start(session),
    }
}

/// Пропускает inter-frames, пока decoder после flush ждёт новый keyframe.
pub(in crate::session::tick) fn accept_video_packet_for_decoder_bootstrap(
    session: &mut PlayerSession,
    packet_keyframe: PacketKeyframe,
    packet_pts: Duration,
) -> bool {
    if !session.pipeline.video_decoder_needs_keyframe() {
        return true;
    }

    match packet_keyframe {
        PacketKeyframe::Keyframe => {
            let bootstrap = session.record_video_decoder_bootstrap_accepted(packet_keyframe);
            debug!(
                pts_ms = packet_pts.as_millis(),
                dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                first_accepted_keyframe = ?bootstrap.first_accepted_keyframe,
                "Accepted post-flush video decoder bootstrap packet"
            );
            session.pipeline.mark_video_decoder_bootstrapped();
            true
        }
        PacketKeyframe::NotKeyframe => {
            let bootstrap = session.record_video_packet_dropped_until_keyframe();
            debug!(
                pts_ms = packet_pts.as_millis(),
                dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                "Dropping video packet until decoder receives post-flush keyframe"
            );
            false
        }
        PacketKeyframe::Unknown => {
            if active_video_codec_requires_proven_decode_start(session) {
                let bootstrap = session.record_video_packet_dropped_until_keyframe();
                warn!(
                    pts_ms = packet_pts.as_millis(),
                    dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                    "Dropping video packet with unknown keyframe state until codec-aware decode-start proof"
                );
                return false;
            }

            let bootstrap = session.record_video_decoder_bootstrap_accepted(packet_keyframe);
            warn!(
                pts_ms = packet_pts.as_millis(),
                dropped_until_keyframe = bootstrap.dropped_until_keyframe,
                first_accepted_keyframe = ?bootstrap.first_accepted_keyframe,
                "Accepting video packet with unknown keyframe state as post-flush decode start"
            );
            session.pipeline.mark_video_decoder_bootstrapped();
            true
        }
    }
}

/// Проверяет, требует ли активный codec доказанный decode-start после seek/flush.
fn active_video_codec_requires_proven_decode_start(session: &PlayerSession) -> bool {
    session
        .pipeline
        .active_video_requirement()
        .is_some_and(|requirement| matches!(requirement.codec, VideoCodec::H264 | VideoCodec::H265))
}

/// Отделяет stale seek generation от late-drop policy.
pub(in crate::session::tick) fn pending_video_packet_generation_drop_reason(
    pipeline: &PlaybackPipeline,
    packet_generation: u64,
) -> Option<PlayerVideoDropReason> {
    (!pipeline.packet_generation_is_current(packet_generation))
        .then_some(PlayerVideoDropReason::StaleGeneration)
}

/// Переводит typed demux-классификацию в текущий bool contract decoder-а.
///
/// `Unknown` становится decode-start hint только если codec policy уже разрешила
/// такой bootstrap. Для H.264/H.265 этот путь закрыт выше codec-aware проверкой.
pub(super) fn video_decode_packet_keyframe_hint(
    packet_keyframe: PacketKeyframe,
    accepted_as_decode_start: bool,
) -> bool {
    matches!(packet_keyframe, PacketKeyframe::Keyframe)
        || (accepted_as_decode_start && packet_keyframe == PacketKeyframe::Unknown)
}

/// Минимальный view pending packet-а для bitstream capability validation.
pub(super) struct PendingVideoPacketProbe {
    /// Track ID нужен, чтобы найти container codec.
    pub(super) track_id: TrackId,

    /// Codec payload нужен adapter-у для чтения header-level requirement.
    pub(super) encoded_bytes: Bytes,
}

/// Решение packet admission после codec requirement probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingVideoPacketValidation {
    /// Packet можно отправлять текущему decoder-у.
    DecoderReady,

    /// Packet остаётся первым в очереди до завершения backend reselection.
    BackendReselectionPending,

    /// Stream отклонён, а fatal error уже опубликован session-ом.
    Rejected,
}

/// Проверяет profile/format до отправки packet-а в hardware decoder.
pub(super) fn validate_pending_video_packet_before_decode(
    session: &mut PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> PendingVideoPacketValidation {
    if session.has_pending_video_backend_reselection() {
        return PendingVideoPacketValidation::BackendReselectionPending;
    }

    if session
        .pipeline
        .active_video_requirement()
        .is_some_and(|requirement| !video_requirement_needs_packet_refinement(requirement))
    {
        return PendingVideoPacketValidation::DecoderReady;
    }

    let requirement = match video_requirement_from_packet(session, packet) {
        Ok(Some(requirement)) => requirement,
        Ok(None) => return PendingVideoPacketValidation::DecoderReady,
        Err(error) => {
            warn!(error = %error, "Video stream rejected by packet requirement probe");
            session.mark_fatal_error(error);
            session.pipeline.clear_pending_video_packets();
            return PendingVideoPacketValidation::Rejected;
        }
    };

    match session.refine_active_video_requirement(requirement) {
        Ok(ActiveVideoRequirementRefinement::DecoderReady) => {
            PendingVideoPacketValidation::DecoderReady
        }
        Ok(ActiveVideoRequirementRefinement::BackendReselectionRequested) => {
            PendingVideoPacketValidation::BackendReselectionPending
        }
        Err(error) => {
            warn!(error = %error, "Video stream rejected before hardware decode");
            session.mark_fatal_error(error);
            session.pipeline.clear_pending_video_packets();
            PendingVideoPacketValidation::Rejected
        }
    }
}

/// Читает codec header через adapter registry и строит уточнённое requirement.
fn video_requirement_from_packet_data(
    codec: VideoCodec,
    packet_data: &[u8],
    codec_private: Option<&[u8]>,
    container_source: Option<codec_core::VideoMetadataSource>,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    match probe_video_packet_requirement_with_codec_private(codec, packet_data, codec_private) {
        VideoRequirementProbe::Candidate(candidate) => Ok(Some(
            resolve_video_metadata(codec, container_source, Some(candidate)).requirement,
        )),
        VideoRequirementProbe::Rejected(rejection) => {
            Err(player_error_from_requirement_rejection(rejection))
        }
        VideoRequirementProbe::Recoverable(uncertainty) => {
            trace!(
                ?uncertainty,
                "Video requirement probe skipped before decode"
            );
            Ok(None)
        }
    }
}

/// Возвращает codec-specific requirement probe через adapter registry.
fn video_requirement_from_packet(
    session: &PlayerSession,
    packet: &PendingVideoPacketProbe,
) -> Result<Option<VideoDecodeRequirement>, PlayerError> {
    let Some(codec) = session.video_codec_for_track(packet.track_id) else {
        return Ok(None);
    };

    video_requirement_from_packet_data(
        codec,
        &packet.encoded_bytes,
        session.video_codec_private_for_track(packet.track_id),
        session.video_metadata_source_for_track(packet.track_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{
        BitDepth, ChromaSubsampling, H264Profile, VideoProfile,
        video_frame_pixel_layout_from_decode_requirement,
    };
    use media_core::{
        DemuxSeekResult, Demuxer, TimeBase, TrackInfo, TrackKind, VideoTrackMetadata,
    };
    use video_frame_contract::VideoFramePixelLayout;

    use crate::PlayerErrorKind;

    #[test]
    fn h264_packet_refinement_uses_codec_private_for_avcc_packets() {
        let codec_private = supported_h264_high_avcc_codec_private();
        let ambiguous_avcc_packet = avcc_packet_with_annex_b_like_length_prefix();

        let false_rejection = video_requirement_from_packet_data(
            VideoCodec::H264,
            &ambiguous_avcc_packet,
            None,
            None,
        )
        .expect_err("без avcC AVCC packet ошибочно выглядит как unsupported SPS");
        assert_eq!(
            false_rejection.kind,
            PlayerErrorKind::UnsupportedVideoProfile
        );

        let refined_requirement = video_requirement_from_packet_data(
            VideoCodec::H264,
            &ambiguous_avcc_packet,
            Some(&codec_private),
            None,
        )
        .expect("valid avcC должен уточнить H.264 requirement")
        .expect("valid avcC должен вернуть candidate requirement");

        assert_eq!(
            refined_requirement.profile,
            Some(VideoProfile::H264(H264Profile::High))
        );
        assert_eq!(refined_requirement.bit_depth, Some(BitDepth::Eight));
        assert_eq!(refined_requirement.chroma, Some(ChromaSubsampling::Yuv420));
        assert_eq!(refined_requirement.width, Some(1280));
        assert_eq!(refined_requirement.height, Some(720));
        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&refined_requirement),
            Some(VideoFramePixelLayout::Nv12)
        );
    }

    #[test]
    fn h264_packet_refinement_reads_codec_private_from_session_track() {
        let track_id = TrackId::new(42);
        let codec_private = supported_h264_high_avcc_codec_private();
        let video_track = h264_video_track(track_id, codec_private.clone());
        let mut session = PlayerSession::new();
        session.pipeline.install_opened_media(
            Box::new(NoopDemuxer {
                tracks: vec![video_track.clone()],
            }),
            None,
            None,
            vec![video_track],
        );

        let packet = PendingVideoPacketProbe {
            track_id,
            encoded_bytes: Bytes::from(avcc_packet_with_annex_b_like_length_prefix()),
        };
        let refined_requirement = video_requirement_from_packet(&session, &packet)
            .expect("session track codec_private должен быть передан в H.264 probe")
            .expect("valid avcC должен вернуть refined requirement");

        assert_eq!(
            refined_requirement.profile,
            Some(VideoProfile::H264(H264Profile::High))
        );
        assert_eq!(
            video_frame_pixel_layout_from_decode_requirement(&refined_requirement),
            Some(VideoFramePixelLayout::Nv12)
        );
    }

    struct NoopDemuxer {
        tracks: Vec<TrackInfo>,
    }

    impl Demuxer for NoopDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(1))
        }

        fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
            Ok(media_core::DemuxReadEvent::EndOfStream)
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            Err(anyhow::anyhow!(
                "NoopDemuxer не поддерживает seek в этом тесте"
            ))
        }
    }

    fn h264_video_track(track_id: TrackId, codec_private: Vec<u8>) -> TrackInfo {
        TrackInfo {
            id: track_id,
            kind: TrackKind::Video,
            codec_id: "V_MPEG4/ISO/AVC".to_owned(),
            codec_private: Some(Bytes::from(codec_private)),
            time_base: TimeBase::new(1, 1_000),
            duration: Some(Duration::from_secs(1)),
            sample_rate: None,
            channels: None,
            video: Some(VideoTrackMetadata {
                packet_framing: media_core::VideoPacketFraming::Unspecified,
                coded_width: Some(1280),
                coded_height: Some(720),
                profile: Some(VideoProfile::H264(H264Profile::High)),
                bit_depth: None,
                chroma: None,
                color: None,
                orientation: codec_core::VideoDisplayOrientation::Identity,
            }),
        }
    }

    fn supported_h264_high_avcc_codec_private() -> Vec<u8> {
        vec![
            0x01, 0x64, 0x00, 0x20, 0xff, 0xe1, 0x00, 0x1b, 0x67, 0x64, 0x00, 0x20, 0xac, 0xd1,
            0x00, 0x50, 0x05, 0xbb, 0x01, 0x6a, 0x02, 0x02, 0x02, 0x80, 0x00, 0x01, 0xf4, 0x80,
            0x00, 0xea, 0x60, 0x07, 0x8c, 0x18, 0x89, 0x01, 0x00, 0x04, 0x68, 0xeb, 0x8f, 0x2c,
        ]
    }

    fn avcc_packet_with_annex_b_like_length_prefix() -> Vec<u8> {
        vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x00, 0x00, 0x00, 0x02]
    }
}
