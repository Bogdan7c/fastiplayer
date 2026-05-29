use capability_core::{SystemCapabilities, UnsupportedVideoRequirement, VideoCapabilityRejection};
use codec_core::{
    VideoCodec, VideoDecodeRequirement, VideoMetadataSource, resolve_video_metadata,
    unsupported_requirement_can_be_refined_by_packet_probe as codec_requirement_can_be_refined_by_packet_probe,
    video_requirement_needs_packet_refinement,
};
use media_core::{TrackInfo, TrackKind};
use tracing::info;

use crate::{PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult, TrackId};

use super::PlayerSession;

impl PlayerSession {
    /// Устанавливает capability report и публикует событие для UI/log layer.
    pub fn set_system_capabilities(&mut self, capabilities: SystemCapabilities) {
        let summary = capabilities.detailed_report_text();
        self.snapshot.capability_summary = Some(summary.clone());
        self.pending_events
            .push(PlayerEvent::CapabilityScanCompleted(
                crate::CapabilitySummary { summary },
            ));
        self.capabilities = Some(capabilities);
    }

    /// Ищет первый video track, который проходит capability-based selection.
    pub(super) fn select_default_video_track(
        &mut self,
        tracks: &[TrackInfo],
        missing_message: &str,
    ) -> PlayerResult<()> {
        let video_tracks = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .collect::<Vec<_>>();

        if video_tracks.is_empty() {
            info!("{missing_message}");
            return Ok(());
        }

        let mut last_rejection = None;
        for track in video_tracks {
            match self.accepted_video_requirement_for_track(track) {
                Ok(requirement) => {
                    self.activate_video_track(track, requirement);
                    return Ok(());
                }
                Err(error) => last_rejection = Some(error),
            }
        }

        Err(last_rejection.unwrap_or_else(|| {
            PlayerError::new(PlayerErrorKind::UnsupportedVideoCodec, missing_message)
        }))
    }

    /// Выбирает явно запрошенный video track только после fresh capability validation.
    pub(super) fn select_requested_video_track(&mut self, track_id: TrackId) -> PlayerResult<()> {
        let Some(track) = self
            .pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .cloned()
        else {
            return Err(PlayerError::new(
                PlayerErrorKind::InvalidCommand,
                format!("Video track `{track_id}` не найден в текущем media"),
            ));
        };

        let requirement = self.accepted_video_requirement_for_track(&track)?;
        self.activate_video_track(&track, requirement);
        Ok(())
    }

    /// Строит requirement из container metadata и принимает его до mutation selection state.
    fn accepted_video_requirement_for_track(
        &self,
        track: &TrackInfo,
    ) -> PlayerResult<VideoDecodeRequirement> {
        let Some(requirement) = video_requirement_from_track(track) else {
            return Err(PlayerError::new(
                PlayerErrorKind::UnsupportedVideoCodec,
                format!(
                    "Video codec `{}` не поддерживается текущей capability model",
                    track.codec_id
                ),
            ));
        };

        match self.validate_video_decode_requirement(&requirement) {
            Ok(()) => Ok(requirement),
            Err(error) => {
                if self.can_defer_packet_refinement(&requirement) {
                    info!(
                        track_id = %track.id,
                        requirement = %requirement.describe(),
                        "Video track выбран до bitstream refinement; strict capability check будет повторён перед decode"
                    );
                    return Ok(requirement);
                }

                Err(error)
            }
        }
    }

    /// Активирует video track после обычной проверки или разрешённого deferred refinement.
    fn activate_video_track(&mut self, track: &TrackInfo, requirement: VideoDecodeRequirement) {
        self.pipeline.select_video_track(track.id, requirement);
        self.snapshot.selected_tracks.video_track = Some(track.id);
        log_selected_video_track_metadata(track, self.pipeline.active_video_requirement());
    }

    /// Разрешает отложить codec validation до первого packet header-а, если container неполный.
    pub(super) fn can_defer_packet_refinement(&self, requirement: &VideoDecodeRequirement) -> bool {
        if !video_requirement_needs_packet_refinement(requirement) {
            return false;
        }

        self.capabilities.as_ref().is_some_and(|capabilities| {
            matches!(
                capabilities.check_video_requirement(requirement),
                Err(ref unsupported_requirement)
                    if unsupported_requirement_can_be_refined_by_packet_probe(
                        unsupported_requirement
                    )
            )
        })
    }

    /// Проверяет video stream requirement по последнему capability report.
    pub(super) fn validate_video_decode_requirement(
        &self,
        requirement: &VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        let Some(capabilities) = &self.capabilities else {
            return Ok(());
        };

        match capabilities.check_video_requirement(requirement) {
            Ok(_) => Ok(()),
            Err(error) => Err(player_error_from_unsupported_requirement(error)),
        }
    }

    /// Уточняет active video requirement после bitstream probe.
    pub(super) fn refine_active_video_requirement(
        &mut self,
        requirement: VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        self.validate_video_decode_requirement(&requirement)?;
        self.pipeline.set_active_video_requirement(requirement);
        Ok(())
    }

    /// Возвращает codec текущего video track по `TrackId`.
    pub(super) fn video_codec_for_track(&self, track_id: TrackId) -> Option<VideoCodec> {
        self.pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .and_then(|track| VideoCodec::from_container_codec_id(&track.codec_id))
    }

    /// Возвращает container metadata source для active track refinement.
    pub(super) fn video_metadata_source_for_track(
        &self,
        track_id: TrackId,
    ) -> Option<VideoMetadataSource> {
        self.pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .and_then(video_metadata_source_from_track)
    }
}

/// Строит минимальное decode requirement из container track metadata.
fn video_requirement_from_track(track: &TrackInfo) -> Option<VideoDecodeRequirement> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
    let Some(container_source) = video_metadata_source_from_track(track) else {
        return Some(VideoDecodeRequirement::new(codec));
    };

    Some(resolve_video_metadata(codec, Some(container_source), None).requirement)
}

/// Проверяет, что отказ относится к metadata, которую codec packet probe может уточнить.
fn unsupported_requirement_can_be_refined_by_packet_probe(
    unsupported_requirement: &UnsupportedVideoRequirement,
) -> bool {
    let is_metadata_rejection = matches!(
        unsupported_requirement.rejections.first(),
        Some(VideoCapabilityRejection::InvalidHdrMetadata { .. })
            | Some(VideoCapabilityRejection::InsufficientStreamMetadata { .. })
    );

    codec_requirement_can_be_refined_by_packet_probe(
        &unsupported_requirement.requirement,
        is_metadata_rejection,
    )
}

/// Собирает codec-neutral resolver source из typed video metadata track-а.
fn video_metadata_source_from_track(track: &TrackInfo) -> Option<VideoMetadataSource> {
    let codec = VideoCodec::from_container_codec_id(&track.codec_id)?;
    let video = track.video.as_ref()?;
    let mut source = VideoMetadataSource::container(codec);
    source.profile = video.profile;
    source.bit_depth = video.bit_depth;
    source.chroma = video.chroma;
    source.width = video.coded_width;
    source.height = video.coded_height;
    if let Some(color) = &video.color {
        source = source.with_color(color.clone());
    }
    Some(source)
}

/// Пишет resolved video/container metadata в logs без codec logic в UI.
fn log_selected_video_track_metadata(
    track: &TrackInfo,
    active_requirement: Option<&VideoDecodeRequirement>,
) {
    let Some(video_metadata) = track.video.as_ref() else {
        return;
    };

    info!(
        track_id = %track.id,
        codec = %track.codec_id,
        width = ?video_metadata.coded_width,
        height = ?video_metadata.coded_height,
        bit_depth = ?video_metadata.bit_depth,
        chroma = ?video_metadata.chroma,
        color = ?video_metadata.color,
        requirement = ?active_requirement,
        "Video track metadata resolved from container"
    );
}

/// Переводит structured capability error в player error model.
fn player_error_from_unsupported_requirement(error: UnsupportedVideoRequirement) -> PlayerError {
    let kind = match error.rejections.first() {
        Some(VideoCapabilityRejection::UnsupportedCodec { .. }) => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        Some(VideoCapabilityRejection::UnsupportedProfile { .. }) => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        Some(VideoCapabilityRejection::UnsupportedBitDepth { .. }) => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        Some(VideoCapabilityRejection::UnsupportedChroma { .. }) => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        Some(VideoCapabilityRejection::UnsupportedHdrRenderer { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::InvalidHdrMetadata { .. }) => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::P010NotRenderable { .. }) if error.requirement.hdr => {
            PlayerErrorKind::UnsupportedHdrMode
        }
        Some(VideoCapabilityRejection::NoAvailableRenderer)
        | Some(VideoCapabilityRejection::UnsupportedDeviceExportPath { .. })
        | Some(VideoCapabilityRejection::UnsupportedP010StorageLayout { .. })
        | Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat { .. })
        | Some(VideoCapabilityRejection::P010NotRenderable { .. }) => {
            PlayerErrorKind::UnsupportedRenderFormat
        }
        Some(VideoCapabilityRejection::NoAvailableBackend)
        | Some(VideoCapabilityRejection::UnsupportedDecodeFormat { .. })
        | Some(VideoCapabilityRejection::InsufficientStreamMetadata { .. })
        | None => PlayerErrorKind::HardwareDecoderUnavailable,
    };

    PlayerError::new(kind, error.user_message())
}
