//! Staged video preflight до запроса detached backend-а и atomic media commit-а.

use std::collections::{HashMap, VecDeque};

use bytes::Bytes;
use codec_core::{
    VideoCodec, VideoDecodeRequirement, VideoMetadataSource, VideoRequirementPreflight,
    VideoRequirementProbe, VideoRequirementUncertainty, preflight_video_requirement,
    probe_video_packet_requirement_with_codec_private, resolve_video_metadata,
};
use media_core::{DemuxReadEvent, DemuxRetryHint, TrackInfo, TrackKind};

use crate::{
    MediaInstallVideoBackendConstraint, PlayerError, PlayerErrorKind, PlayerResult, PreparedMedia,
    TrackId,
};

use super::PlayerSession;
use super::capability_selection::{
    can_try_next_video_track_after_error, fallback_frame_contract_for_unprobed_requirement,
    player_error_from_unsupported_requirement, video_metadata_source_from_track,
    video_stream_decode_config_from_track,
};
use super::staged_media_install::{StagedVideoBackendPlan, StagedVideoTrackPlan};
use super::video_requirement_error::player_error_from_requirement_rejection;

/// Максимум demux events, которые staged preflight имеет право удерживать до commit-а.
const MAX_STAGED_VIDEO_PROBE_EVENTS: usize = 512;

/// Максимум encoded payload replay queue во время staged preflight.
const MAX_STAGED_VIDEO_PROBE_ENCODED_BYTES: usize = 64 * 1024 * 1024;

/// Требование к полноте backend selection для конкретного media install ingress-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StagedVideoPlanningMode {
    /// Strong install обязан выбрать exact backend до запроса resource port-а.
    ExactBackendRequired,

    /// Временный compatibility ingress не создаёт detached backend до commit-а.
    CompatibilityDeferredAllowed,
}

/// Safety policy чтения candidate demuxer-а ради codec header-а.
#[derive(Debug, Clone, Copy)]
struct StagedVideoProbeBudget {
    /// Максимум событий защищает от stream-а, который бесконечно не отдаёт нужный track.
    max_events: usize,

    /// Максимум encoded bytes ограничивает память replay queue до commit-а.
    max_encoded_bytes: usize,

    /// Уже сохранённые demux events.
    observed_events: usize,

    /// Уже сохранённые encoded bytes всех packet kinds.
    observed_encoded_bytes: usize,
}

impl StagedVideoProbeBudget {
    /// Production ceiling: header должен появиться в начале stream-а, но interleaving допустим.
    const fn production() -> Self {
        Self {
            max_events: MAX_STAGED_VIDEO_PROBE_EVENTS,
            max_encoded_bytes: MAX_STAGED_VIDEO_PROBE_ENCODED_BYTES,
            observed_events: 0,
            observed_encoded_bytes: 0,
        }
    }

    /// Учитывает event до codec parsing и возвращает typed safety failure.
    fn observe(&mut self, event: &DemuxReadEvent) -> PlayerResult<()> {
        self.observed_events = self.observed_events.saturating_add(1);
        if let DemuxReadEvent::Packet(packet) = event {
            self.observed_encoded_bytes = self
                .observed_encoded_bytes
                .saturating_add(packet.data.len());
        }

        if self.observed_events > self.max_events
            || self.observed_encoded_bytes > self.max_encoded_bytes
        {
            return Err(PlayerError::new(
                PlayerErrorKind::DemuxError,
                format!(
                    "Video preflight превысил безопасный replay budget: {} events, {} bytes",
                    self.observed_events, self.observed_encoded_bytes
                ),
            ));
        }

        Ok(())
    }
}

/// Один проход чтения demuxer-а с packet cache для нескольких video tracks.
struct StagedVideoPacketProbeReader {
    /// Packets других video tracks, встреченные до выбора текущего candidate-а.
    packets_by_track: HashMap<TrackId, VecDeque<Bytes>>,

    /// Общий replay budget всех попыток выбора track-а.
    budget: StagedVideoProbeBudget,

    /// EOF уже был прочитан и сохранён в `PreparedMedia`.
    reached_end_of_stream: bool,
}

/// Результат одного resumable packet-probe прохода.
enum StagedPacketProbeOutcome {
    /// Exact codec requirement найден без потери prefetch replay events.
    Resolved(VideoDecodeRequirement),

    /// Source пока не готов; тот же reader должен продолжить после deadline-а.
    Pending(DemuxRetryHint),
}

/// Progress выбора video track-а, живущий между worker wakeup-ами.
pub(super) struct StagedVideoPlanner {
    /// Стабильный список candidate tracks из исходного prepared snapshot-а.
    video_tracks: Vec<TrackInfo>,

    /// Индекс candidate-а, который сейчас проверяется.
    next_track_index: usize,

    /// Общий reader/budget всех candidate tracks одного request-а.
    packet_probe_reader: StagedVideoPacketProbeReader,

    /// Последняя codec uncertainty текущего packet-probe candidate-а.
    active_last_uncertainty: Option<VideoRequirementUncertainty>,

    /// Последняя recoverable причина перехода к следующему candidate track-у.
    last_rejection: Option<PlayerError>,

    /// Source-specific сообщение при исчерпании всех candidate tracks.
    missing_message: String,
}

impl StagedVideoPlanner {
    /// Создаёт planning state без demux I/O.
    pub(super) fn new(prepared_media: &PreparedMedia) -> Self {
        Self {
            video_tracks: prepared_media
                .tracks()
                .iter()
                .filter(|track| track.kind == TrackKind::Video)
                .cloned()
                .collect(),
            next_track_index: 0,
            packet_probe_reader: StagedVideoPacketProbeReader::new(),
            active_last_uncertainty: None,
            last_rejection: None,
            missing_message: prepared_media.missing_video_track_message().to_owned(),
        }
    }
}

/// Результат resumable video planning owner-step-а.
pub(super) enum StagedVideoPlanningOutcome {
    /// Video plan полностью готов либо media не содержит video.
    Ready(Option<StagedVideoTrackPlan>),

    /// Тот же request/progress должен продолжиться после retry hint-а.
    Pending(DemuxRetryHint),
}

impl StagedVideoPacketProbeReader {
    /// Создаёт пустой reader для одного staged candidate media.
    fn new() -> Self {
        Self {
            packets_by_track: HashMap::new(),
            budget: StagedVideoProbeBudget::production(),
            reached_end_of_stream: false,
        }
    }

    /// Уточняет requirement выбранного track-а, не теряя packets соседних tracks.
    fn resolve_requirement_for_track(
        &mut self,
        prepared_media: &mut PreparedMedia,
        track: &TrackInfo,
        container_source: Option<VideoMetadataSource>,
        last_uncertainty: &mut Option<VideoRequirementUncertainty>,
    ) -> PlayerResult<StagedPacketProbeOutcome> {
        let codec = VideoCodec::from_container_codec_id(&track.codec_id).ok_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::UnsupportedVideoCodec,
                format!("Неизвестный video codec `{}`", track.codec_id),
            )
        })?;
        while let Some(packet_bytes) = self
            .packets_by_track
            .get_mut(&track.id)
            .and_then(VecDeque::pop_front)
        {
            if let Some(requirement) = probe_packet_requirement(
                codec,
                &packet_bytes,
                track.codec_private.as_deref(),
                container_source.clone(),
                last_uncertainty,
            )? {
                return Ok(StagedPacketProbeOutcome::Resolved(requirement));
            }
        }

        while !self.reached_end_of_stream {
            let event = prepared_media
                .prefetch_next_event_for_video_probe()
                .map_err(|error| {
                    PlayerError::new(
                        PlayerErrorKind::DemuxError,
                        format!("Не удалось прочитать video header candidate-а: {error}"),
                    )
                })?;
            if !matches!(event, DemuxReadEvent::TemporarilyUnavailable(_)) {
                self.budget.observe(&event)?;
            }
            match event {
                DemuxReadEvent::Packet(packet) if packet.track_id == track.id => {
                    if let Some(requirement) = probe_packet_requirement(
                        codec,
                        &packet.data,
                        track.codec_private.as_deref(),
                        container_source.clone(),
                        last_uncertainty,
                    )? {
                        return Ok(StagedPacketProbeOutcome::Resolved(requirement));
                    }
                }
                DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                    self.packets_by_track
                        .entry(packet.track_id)
                        .or_default()
                        .push_back(packet.data);
                }
                DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
                DemuxReadEvent::TemporarilyUnavailable(hint) => {
                    return Ok(StagedPacketProbeOutcome::Pending(hint));
                }
                DemuxReadEvent::TracksChanged(_) => {
                    return Err(PlayerError::new(
                        PlayerErrorKind::DemuxError,
                        "Список tracks изменился во время staged video preflight",
                    ));
                }
                DemuxReadEvent::EndOfStream => {
                    self.reached_end_of_stream = true;
                }
            }
        }

        let uncertainty_suffix = last_uncertainty
            .as_ref()
            .map(|uncertainty| format!(" Последний codec probe: {uncertainty:?}."))
            .unwrap_or_default();
        Err(PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            format!(
                "До конца stream-а не найден header с точным форматом {}.{uncertainty_suffix}",
                codec.display_name()
            ),
        ))
    }
}

/// Выбирает playable output внутри immutable app-owned constraint-а media install request-а.
fn select_staged_video_output(
    capabilities: &capability_core::SystemCapabilities,
    requirement: &VideoDecodeRequirement,
    backend_constraint: &MediaInstallVideoBackendConstraint,
) -> PlayerResult<capability_core::SupportedVideoOutput> {
    let default_output = capabilities
        .check_video_requirement(requirement)
        .map_err(player_error_from_unsupported_requirement)?;
    let required_backend_id = match backend_constraint {
        MediaInstallVideoBackendConstraint::AnyPlayable => return Ok(default_output.clone()),
        MediaInstallVideoBackendConstraint::RequireBackend(required_backend_id) => {
            required_backend_id
        }
    };

    capabilities
        .find_playable_video_output_for_backend(required_backend_id, requirement)
        .cloned()
        .ok_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::RequiredVideoBackendUnavailable,
                format!(
                    "Backend `{required_backend_id}` не имеет playable output для candidate video stream"
                ),
            )
        })
}

impl PlayerSession {
    /// Строит default video plan candidate-а, не читая active backend state.
    ///
    /// Exact mode сначала завершает codec evidence preflight, затем выбирает output
    /// из общего capability snapshot-а. Compatibility mode не делает demux I/O.
    pub(super) fn resume_staged_video_track_plan(
        &self,
        prepared_media: &mut PreparedMedia,
        planning_mode: StagedVideoPlanningMode,
        backend_constraint: &MediaInstallVideoBackendConstraint,
        planner: &mut StagedVideoPlanner,
    ) -> PlayerResult<StagedVideoPlanningOutcome> {
        if planner.video_tracks.is_empty() {
            return Ok(StagedVideoPlanningOutcome::Ready(None));
        }

        while let Some(track) = planner.video_tracks.get(planner.next_track_index) {
            let Some(codec) = VideoCodec::from_container_codec_id(&track.codec_id) else {
                planner.last_rejection = Some(PlayerError::new(
                    PlayerErrorKind::UnsupportedVideoCodec,
                    format!(
                        "Video codec `{}` не поддерживается candidate capability model",
                        track.codec_id
                    ),
                ));
                planner.next_track_index = planner.next_track_index.saturating_add(1);
                planner.active_last_uncertainty = None;
                continue;
            };
            let container_source = video_metadata_source_from_track(track);
            let preflight = preflight_video_requirement(
                codec,
                container_source.clone(),
                track.codec_private.as_deref(),
            );
            let requirement = match resolve_staged_preflight_requirement(
                preflight,
                planning_mode,
                &mut planner.packet_probe_reader,
                prepared_media,
                track,
                container_source,
                &mut planner.active_last_uncertainty,
            ) {
                Ok(StagedRequirementOutcome::Resolved(requirement)) => requirement,
                Ok(StagedRequirementOutcome::Pending(hint)) => {
                    return Ok(StagedVideoPlanningOutcome::Pending(hint));
                }
                Err(error) if can_try_next_video_track_after_error(&error.kind) => {
                    planner.last_rejection = Some(error);
                    planner.next_track_index = planner.next_track_index.saturating_add(1);
                    planner.active_last_uncertainty = None;
                    continue;
                }
                Err(error) => return Err(error),
            };

            let playable_output = match self.capabilities.as_ref() {
                Some(capabilities) => {
                    match select_staged_video_output(capabilities, &requirement, backend_constraint)
                    {
                        Ok(output) => Some(output),
                        Err(_)
                            if planning_mode
                                == StagedVideoPlanningMode::CompatibilityDeferredAllowed
                                && self.can_defer_packet_refinement(&requirement) =>
                        {
                            None
                        }
                        Err(error) => {
                            planner.last_rejection = Some(error);
                            planner.next_track_index = planner.next_track_index.saturating_add(1);
                            planner.active_last_uncertainty = None;
                            continue;
                        }
                    }
                }
                None if planning_mode == StagedVideoPlanningMode::CompatibilityDeferredAllowed => {
                    None
                }
                None => {
                    return Err(PlayerError::new(
                        PlayerErrorKind::HardwareDecoderUnavailable,
                        "Strong media install не получил system capability snapshot",
                    ));
                }
            };
            let frame_contract = playable_output.as_ref().map_or_else(
                || fallback_frame_contract_for_unprobed_requirement(&requirement),
                |output| output.frame_contract,
            );
            let stream_config =
                match video_stream_decode_config_from_track(track, &requirement, frame_contract) {
                    Ok(stream_config) => stream_config,
                    Err(error) if can_try_next_video_track_after_error(&error.kind) => {
                        planner.last_rejection = Some(error);
                        planner.next_track_index = planner.next_track_index.saturating_add(1);
                        planner.active_last_uncertainty = None;
                        continue;
                    }
                    Err(error) => return Err(error),
                };
            let backend_plan = match playable_output {
                Some(output) => StagedVideoBackendPlan::Exact {
                    backend_id: output.backend.as_str().to_owned(),
                },
                None => StagedVideoBackendPlan::CompatibilityDeferred,
            };

            return Ok(StagedVideoPlanningOutcome::Ready(Some(
                StagedVideoTrackPlan {
                    track_id: track.id,
                    requirement,
                    frame_contract,
                    stream_config,
                    backend_plan,
                },
            )));
        }

        Err(planner.last_rejection.take().unwrap_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::UnsupportedVideoCodec,
                planner.missing_message.clone(),
            )
        }))
    }
}

/// Requirement step отделяет readiness pause от настоящей preflight ошибки.
enum StagedRequirementOutcome {
    /// Requirement полностью доказан.
    Resolved(VideoDecodeRequirement),

    /// Demux source требует повторить тот же probe позже.
    Pending(DemuxRetryHint),
}

/// Превращает registry preflight в requirement с учётом strong/compatibility режима.
fn resolve_staged_preflight_requirement(
    preflight: VideoRequirementPreflight,
    planning_mode: StagedVideoPlanningMode,
    packet_probe_reader: &mut StagedVideoPacketProbeReader,
    prepared_media: &mut PreparedMedia,
    track: &TrackInfo,
    container_source: Option<VideoMetadataSource>,
    last_uncertainty: &mut Option<VideoRequirementUncertainty>,
) -> PlayerResult<StagedRequirementOutcome> {
    match preflight {
        VideoRequirementPreflight::Resolved(resolved) => {
            Ok(StagedRequirementOutcome::Resolved(resolved.requirement))
        }
        VideoRequirementPreflight::PacketProbeRequired(initial)
            if planning_mode == StagedVideoPlanningMode::CompatibilityDeferredAllowed =>
        {
            Ok(StagedRequirementOutcome::Resolved(initial.requirement))
        }
        VideoRequirementPreflight::PacketProbeRequired(_) => {
            match packet_probe_reader.resolve_requirement_for_track(
                prepared_media,
                track,
                container_source,
                last_uncertainty,
            )? {
                StagedPacketProbeOutcome::Resolved(requirement) => {
                    Ok(StagedRequirementOutcome::Resolved(requirement))
                }
                StagedPacketProbeOutcome::Pending(hint) => {
                    Ok(StagedRequirementOutcome::Pending(hint))
                }
            }
        }
        VideoRequirementPreflight::Rejected(rejection) => {
            Err(player_error_from_requirement_rejection(rejection))
        }
        VideoRequirementPreflight::Unavailable { initial, .. }
            if planning_mode == StagedVideoPlanningMode::CompatibilityDeferredAllowed =>
        {
            Ok(StagedRequirementOutcome::Resolved(initial.requirement))
        }
        VideoRequirementPreflight::Unavailable { reason, .. } => Err(PlayerError::new(
            PlayerErrorKind::UnsupportedVideoCodec,
            reason.user_message(),
        )),
    }
}

/// Пробует один encoded packet и merge-ит bitstream evidence с container metadata.
fn probe_packet_requirement(
    codec: VideoCodec,
    packet_bytes: &[u8],
    codec_private: Option<&[u8]>,
    container_source: Option<VideoMetadataSource>,
    last_uncertainty: &mut Option<VideoRequirementUncertainty>,
) -> PlayerResult<Option<VideoDecodeRequirement>> {
    match probe_video_packet_requirement_with_codec_private(codec, packet_bytes, codec_private) {
        VideoRequirementProbe::Candidate(candidate) => Ok(Some(
            resolve_video_metadata(codec, container_source, Some(candidate)).requirement,
        )),
        VideoRequirementProbe::Rejected(rejection) => {
            Err(player_error_from_requirement_rejection(rejection))
        }
        VideoRequirementProbe::Recoverable(uncertainty) => {
            *last_uncertainty = Some(uncertainty);
            Ok(None)
        }
    }
}
