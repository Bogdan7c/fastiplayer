use capability_core::{
    SupportedVideoOutput, SystemCapabilities, UnsupportedVideoRequirement, VideoCapabilityRejection,
};
use codec_core::{
    BitDepth, ChromaSubsampling, VideoCodec, VideoDecodeRequirement, VideoMetadataSource,
    parse_avc_decoder_configuration_record, parse_hevc_decoder_configuration_record,
    resolve_video_metadata,
    unsupported_requirement_can_be_refined_by_packet_probe as codec_requirement_can_be_refined_by_packet_probe,
    video_requirement_needs_packet_refinement,
};
use media_core::{TrackInfo, TrackKind};
use tracing::info;
use video_core::{
    VideoStreamConfigRejection, VideoStreamConfigResult, VideoStreamDecodeConfig,
    VideoStreamPacketization,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

use crate::{PlayerError, PlayerErrorKind, PlayerEvent, PlayerResult, TrackId};

use super::PlayerSession;

/// Принятый video stream после capability validation.
struct AcceptedVideoSelection {
    /// Codec-level stream requirement.
    requirement: VideoDecodeRequirement,

    /// Concrete backend output; отсутствует только в legacy test/no-capabilities path.
    matched_output: Option<SupportedVideoOutput>,
}

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
            match self.accepted_video_selection_for_track(track) {
                Ok(selection) => {
                    self.activate_video_track(track, selection)?;
                    return Ok(());
                }
                Err(error) if can_try_next_video_track_after_error(&error.kind) => {
                    last_rejection = Some(error);
                }
                Err(error) => return Err(error),
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

        let selection = self.accepted_video_selection_for_track(&track)?;
        self.activate_video_track(&track, selection)?;
        Ok(())
    }

    /// Строит requirement из container metadata и принимает его до mutation selection state.
    fn accepted_video_selection_for_track(
        &self,
        track: &TrackInfo,
    ) -> PlayerResult<AcceptedVideoSelection> {
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
            Ok(matched_output) => Ok(AcceptedVideoSelection {
                requirement,
                matched_output,
            }),
            Err(error) => {
                if self.can_defer_packet_refinement(&requirement) {
                    info!(
                        track_id = %track.id,
                        requirement = %requirement.describe(),
                        "Video track выбран до bitstream refinement; strict capability check будет повторён перед decode"
                    );
                    return Ok(AcceptedVideoSelection {
                        requirement,
                        matched_output: None,
                    });
                }

                Err(error)
            }
        }
    }

    /// Активирует video track после обычной проверки или разрешённого deferred refinement.
    fn activate_video_track(
        &mut self,
        track: &TrackInfo,
        selection: AcceptedVideoSelection,
    ) -> PlayerResult<()> {
        let frame_contract = selection
            .matched_output
            .as_ref()
            .map(|output| output.frame_contract)
            .unwrap_or_else(|| {
                fallback_frame_contract_for_unprobed_requirement(&selection.requirement)
            });
        self.configure_decoder_stream_for_track(track, &selection.requirement, frame_contract)?;
        self.pipeline.select_video_track_with_frame_contract(
            track.id,
            selection.requirement,
            frame_contract,
        );
        self.snapshot.selected_tracks.video_track = Some(track.id);
        log_selected_video_track_metadata(track, self.pipeline.active_video_requirement());
        Ok(())
    }

    /// Передаёт selected stream config decoder boundary до mutation selected-track state.
    fn configure_decoder_stream_for_track(
        &self,
        track: &TrackInfo,
        requirement: &VideoDecodeRequirement,
        frame_contract: VideoFrameContract,
    ) -> PlayerResult<()> {
        let config = video_stream_decode_config_from_track(track, requirement, frame_contract)?;
        player_result_from_stream_config_result(
            self.pipeline.configure_video_decoder_stream(config),
        )?;
        Ok(())
    }

    /// Повторно конфигурирует новый decoder backend под уже выбранный active video track.
    pub(super) fn configure_active_video_decoder_stream(&mut self) -> PlayerResult<()> {
        let Some(track_id) = self.pipeline.selected_video_track_id() else {
            return Ok(());
        };
        let Some(requirement) = self.pipeline.active_video_requirement().cloned() else {
            return Ok(());
        };
        let Some(frame_contract) = self.pipeline.active_video_frame_contract() else {
            return Ok(());
        };
        let Some(track) = self
            .pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
        else {
            return Err(PlayerError::new(
                PlayerErrorKind::InvalidCommand,
                format!("Active video track `{track_id}` отсутствует в текущем media"),
            ));
        };

        self.configure_decoder_stream_for_track(track, &requirement, frame_contract)?;
        self.pipeline
            .set_active_video_selection(requirement, frame_contract);
        Ok(())
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
    ) -> PlayerResult<Option<SupportedVideoOutput>> {
        let Some(capabilities) = &self.capabilities else {
            return Ok(None);
        };

        if let Some(active_backend_id) = self.active_video_backend_id.as_deref() {
            if let Some(output) = capabilities.playable_video_outputs.iter().find(|output| {
                output.backend.as_str() == active_backend_id && output.satisfies(requirement)
            }) {
                return Ok(Some(output.clone()));
            }

            return Err(self.active_backend_rejection(requirement, active_backend_id));
        }

        match capabilities.check_video_requirement(requirement) {
            Ok(output) => Ok(Some(output.clone())),
            Err(error) => Err(player_error_from_unsupported_requirement(error)),
        }
    }

    /// Возвращает typed rejection, когда общий system report содержит другой playable backend.
    fn active_backend_rejection(
        &self,
        requirement: &VideoDecodeRequirement,
        active_backend_id: &str,
    ) -> PlayerError {
        let other_backend_hint = self
            .capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.check_video_requirement(requirement).ok())
            .map(|output| {
                format!(
                    "; другой playable backend `{}` объявляет contract `{}`",
                    output.backend.as_str(),
                    output.frame_contract.diagnostic_label()
                )
            })
            .unwrap_or_default();

        PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            format!(
                "Active video backend `{active_backend_id}` не имеет playable output для {}; \
                 contract другого backend-а не используется{other_backend_hint}",
                requirement.describe()
            ),
        )
    }

    /// Уточняет active video requirement после bitstream probe.
    ///
    /// Если refined output contract отличается от текущего (например, поток
    /// оказался 10-bit P010, а стартовый fallback был NV12), decoder
    /// переинициализируется под новый contract ещё до отправки первого packet-а;
    /// иначе он продолжил бы ожидать старый contract и упал бы на mismatch при
    /// первом DMA-BUF export-е.
    pub(super) fn refine_active_video_requirement(
        &mut self,
        requirement: VideoDecodeRequirement,
    ) -> PlayerResult<()> {
        let matched_output = self.validate_video_decode_requirement(&requirement)?;
        let frame_contract = matched_output
            .as_ref()
            .map(|output| output.frame_contract)
            .unwrap_or_else(|| fallback_frame_contract_for_unprobed_requirement(&requirement));

        let contract_changed = match self.pipeline.active_video_frame_contract() {
            Some(active_contract) => active_contract != frame_contract,
            None => true,
        };

        if contract_changed {
            if let Some(track_id) = self.pipeline.selected_video_track_id() {
                let Some(track) = self
                    .pipeline
                    .tracks()
                    .iter()
                    .find(|track| track.id == track_id && track.kind == TrackKind::Video)
                else {
                    return Err(PlayerError::new(
                        PlayerErrorKind::InvalidCommand,
                        format!("Active video track `{track_id}` отсутствует в текущем media"),
                    ));
                };
                self.configure_decoder_stream_for_track(track, &requirement, frame_contract)?;
            }
        }

        self.pipeline
            .set_active_video_selection(requirement, frame_contract);
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

    /// Возвращает codec private data video track-а для codec-aware packet refinement.
    pub(super) fn video_codec_private_for_track(&self, track_id: TrackId) -> Option<&[u8]> {
        self.pipeline
            .tracks()
            .iter()
            .find(|track| track.id == track_id && track.kind == TrackKind::Video)
            .and_then(|track| track.codec_private.as_deref())
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

/// Строит decoder stream config из уже принятого track requirement.
fn video_stream_decode_config_from_track(
    track: &TrackInfo,
    requirement: &VideoDecodeRequirement,
    frame_contract: VideoFrameContract,
) -> PlayerResult<VideoStreamDecodeConfig> {
    let display_orientation = track
        .video
        .as_ref()
        .map(|metadata| metadata.orientation)
        .unwrap_or_default();
    let mut config =
        VideoStreamDecodeConfig::from_requirement(track.id, requirement, frame_contract)
            .with_codec_private(track.codec_private.clone())
            .with_display_orientation(display_orientation);

    match requirement.codec {
        VideoCodec::H264 => {
            config = config.with_packetization(h264_packetization_from_track(track)?);
        }
        VideoCodec::H265 => {
            config = config.with_packetization(h265_packetization_from_track(track)?);
        }
        _ => {}
    }

    Ok(config)
}

/// Возвращает explicit fallback contract только для no-capability legacy path.
///
/// Bit depth у VP9/HEVC часто неизвестен из container-а и приходит только из
/// bitstream probe. Для HDR/PQ 4:2:0 потоков это всегда 10-bit P010, поэтому
/// нельзя по умолчанию падать в NV12: иначе decoder сконфигурируется под NV12
/// и упадёт на реальном P010 DMA-BUF export-е до того, как refinement уточнит
/// bit depth.
fn fallback_frame_contract_for_unprobed_requirement(
    requirement: &VideoDecodeRequirement,
) -> VideoFrameContract {
    let chroma_allows_p010 = !matches!(
        requirement.chroma,
        Some(ChromaSubsampling::Yuv422 | ChromaSubsampling::Yuv444)
    );
    let prefers_p010 = chroma_allows_p010
        && match requirement.bit_depth {
            Some(bit_depth) => bit_depth == BitDepth::Ten,
            None => requirement.requires_hdr_processing(),
        };

    if prefers_p010 {
        VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers)
    } else {
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers)
    }
}

/// Достаёт H.264 packetization из codec-private, если adapter уже подтвердил `avcC`.
fn h264_packetization_from_track(
    track: &TrackInfo,
) -> PlayerResult<Option<VideoStreamPacketization>> {
    let Some(codec_private) = track
        .codec_private
        .as_ref()
        .filter(|bytes| !bytes.is_empty())
    else {
        return Ok(None);
    };

    let record = parse_avc_decoder_configuration_record(codec_private).map_err(|error| {
        PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            format!(
                "H.264 track `{}` codec_private не является поддержанным avcC: {error}",
                track.id
            ),
        )
    })?;

    Ok(Some(VideoStreamPacketization::H264(record.packetization())))
}

/// Достаёт H.265 packetization из `hvcC`, если container уже доказал framing.
fn h265_packetization_from_track(
    track: &TrackInfo,
) -> PlayerResult<Option<VideoStreamPacketization>> {
    let Some(codec_private) = track
        .codec_private
        .as_ref()
        .filter(|bytes| !bytes.is_empty())
    else {
        return Err(PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            format!(
                "H.265 track `{}` не содержит hvcC codec_private; packetization нельзя доказать до decoder config",
                track.id
            ),
        ));
    };

    let record = parse_hevc_decoder_configuration_record(codec_private).map_err(|error| {
        PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            format!(
                "H.265 track `{}` codec_private не является поддержанным hvcC: {error}",
                track.id
            ),
        )
    })?;

    Ok(Some(VideoStreamPacketization::H265(record.packetization())))
}

/// Переводит decoder configure outcome в player policy без мутации selection state.
fn player_result_from_stream_config_result(result: VideoStreamConfigResult) -> PlayerResult<()> {
    match result {
        VideoStreamConfigResult::AbsentDecoder
        | VideoStreamConfigResult::Configured
        | VideoStreamConfigResult::Unchanged
        | VideoStreamConfigResult::Cleared => Ok(()),
        VideoStreamConfigResult::Unsupported(rejection) => {
            Err(player_error_from_config_rejection(rejection))
        }
        VideoStreamConfigResult::Backpressure(reason) => Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("Video decoder stream configure backpressure: {reason}"),
        )),
        VideoStreamConfigResult::Fatal(error) => Err(PlayerError::new(
            PlayerErrorKind::RuntimeError,
            format!("Video decoder stream configure failed: {error}"),
        )),
    }
}

/// Сохраняет существующую policy: unsupported track можно пропустить, runtime failure — нет.
fn can_try_next_video_track_after_error(error_kind: &PlayerErrorKind) -> bool {
    matches!(
        error_kind,
        PlayerErrorKind::UnsupportedVideoCodec
            | PlayerErrorKind::UnsupportedVideoProfile
            | PlayerErrorKind::UnsupportedVideoBitDepth
            | PlayerErrorKind::UnsupportedVideoChroma
            | PlayerErrorKind::UnsupportedHdrMode
            | PlayerErrorKind::UnsupportedRenderFormat
    )
}

/// Мапит neutral decoder-stream отказ в существующие категории player errors.
fn player_error_from_config_rejection(rejection: VideoStreamConfigRejection) -> PlayerError {
    let kind = match &rejection {
        VideoStreamConfigRejection::UnsupportedCodec { .. }
        | VideoStreamConfigRejection::MissingPacketization { .. }
        | VideoStreamConfigRejection::InvalidCodecPrivate { .. }
        | VideoStreamConfigRejection::BackendUnsupported { .. } => {
            PlayerErrorKind::UnsupportedVideoCodec
        }
        VideoStreamConfigRejection::UnsupportedProfile { .. } => {
            PlayerErrorKind::UnsupportedVideoProfile
        }
        VideoStreamConfigRejection::UnsupportedBitDepth { .. } => {
            PlayerErrorKind::UnsupportedVideoBitDepth
        }
        VideoStreamConfigRejection::UnsupportedChroma { .. } => {
            PlayerErrorKind::UnsupportedVideoChroma
        }
        VideoStreamConfigRejection::UnsupportedSurfaceFormat { .. }
        | VideoStreamConfigRejection::UnsupportedFrameContract { .. } => {
            PlayerErrorKind::UnsupportedRenderFormat
        }
    };

    PlayerError::new(kind, rejection.to_string())
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
        display_orientation = %video_metadata.orientation,
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
        | Some(VideoCapabilityRejection::UnsupportedBackendFrameTransfer { .. })
        | Some(VideoCapabilityRejection::UnsupportedDmaBufImageLayout { .. })
        | Some(VideoCapabilityRejection::UnsupportedRenderFrameFormat { .. })
        | Some(VideoCapabilityRejection::UnsupportedRenderFrameTransfer { .. })
        | Some(VideoCapabilityRejection::RenderTextureSizeExceeded { .. })
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

#[cfg(test)]
mod tests {
    use super::*;
    use codec_core::{
        ColorPrimaries, ColorRange, MatrixCoefficients, TransferFunction, VideoColorMetadata,
    };
    use video_frame_contract::VideoFramePixelLayout;

    fn bt2020_pq_container() -> VideoColorMetadata {
        VideoColorMetadata::container(
            ColorRange::Limited,
            MatrixCoefficients::Bt2020,
            ColorPrimaries::Bt2020,
            TransferFunction::Pq,
            None,
        )
    }

    #[test]
    fn hdr_unprobed_requirement_falls_back_to_p010() {
        let requirement =
            VideoDecodeRequirement::new(VideoCodec::Vp9).with_color(bt2020_pq_container());

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::P010);
    }

    #[test]
    fn sdr_unprobed_requirement_falls_back_to_nv12() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::Nv12);
    }

    #[test]
    fn explicit_ten_bit_requirement_falls_back_to_p010() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::H265)
            .with_bit_depth(BitDepth::Ten)
            .with_chroma(ChromaSubsampling::Yuv420);

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::P010);
    }

    #[test]
    fn explicit_eight_bit_requirement_falls_back_to_nv12() {
        let requirement = VideoDecodeRequirement::new(VideoCodec::H265)
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420);

        let contract = fallback_frame_contract_for_unprobed_requirement(&requirement);

        assert_eq!(contract.pixel_layout, VideoFramePixelLayout::Nv12);
    }
}
