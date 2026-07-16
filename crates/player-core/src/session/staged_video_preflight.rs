//! Staged video preflight до запроса detached backend-а и atomic media commit-а.

use std::collections::{HashMap, VecDeque};

use bytes::Bytes;
use codec_core::{
    VideoCodec, VideoDecodeRequirement, VideoMetadataSource, VideoRequirementPreflight,
    VideoRequirementProbe, VideoRequirementUncertainty, preflight_video_requirement,
    probe_video_packet_requirement_with_codec_private, resolve_video_metadata,
};
use media_core::{DemuxReadEvent, TrackInfo, TrackKind};

use crate::{PlayerError, PlayerErrorKind, PlayerResult, PreparedMedia, TrackId};

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
    ) -> PlayerResult<VideoDecodeRequirement> {
        let codec = VideoCodec::from_container_codec_id(&track.codec_id).ok_or_else(|| {
            PlayerError::new(
                PlayerErrorKind::UnsupportedVideoCodec,
                format!("Неизвестный video codec `{}`", track.codec_id),
            )
        })?;
        let mut last_uncertainty = None;

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
                &mut last_uncertainty,
            )? {
                return Ok(requirement);
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
            self.budget.observe(&event)?;

            match event {
                DemuxReadEvent::Packet(packet) if packet.track_id == track.id => {
                    if let Some(requirement) = probe_packet_requirement(
                        codec,
                        &packet.data,
                        track.codec_private.as_deref(),
                        container_source.clone(),
                        &mut last_uncertainty,
                    )? {
                        return Ok(requirement);
                    }
                }
                DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video => {
                    self.packets_by_track
                        .entry(packet.track_id)
                        .or_default()
                        .push_back(packet.data);
                }
                DemuxReadEvent::Packet(_) | DemuxReadEvent::MediaMetadataChanged(_) => {}
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

impl PlayerSession {
    /// Строит default video plan candidate-а, не читая active backend state.
    ///
    /// Exact mode сначала завершает codec evidence preflight, затем выбирает output
    /// из общего capability snapshot-а. Compatibility mode не делает demux I/O.
    pub(super) fn plan_staged_video_track(
        &self,
        prepared_media: &mut PreparedMedia,
        planning_mode: StagedVideoPlanningMode,
    ) -> PlayerResult<Option<StagedVideoTrackPlan>> {
        let video_tracks = prepared_media
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .cloned()
            .collect::<Vec<_>>();
        if video_tracks.is_empty() {
            return Ok(None);
        }

        let missing_message = prepared_media.missing_video_track_message();
        let mut packet_probe_reader = StagedVideoPacketProbeReader::new();
        let mut last_rejection = None;

        for track in &video_tracks {
            let Some(codec) = VideoCodec::from_container_codec_id(&track.codec_id) else {
                last_rejection = Some(PlayerError::new(
                    PlayerErrorKind::UnsupportedVideoCodec,
                    format!(
                        "Video codec `{}` не поддерживается candidate capability model",
                        track.codec_id
                    ),
                ));
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
                &mut packet_probe_reader,
                prepared_media,
                track,
                container_source,
            ) {
                Ok(requirement) => requirement,
                Err(error) if can_try_next_video_track_after_error(&error.kind) => {
                    last_rejection = Some(error);
                    continue;
                }
                Err(error) => return Err(error),
            };

            let playable_output = match self.capabilities.as_ref() {
                Some(capabilities) => match capabilities.check_video_requirement(&requirement) {
                    Ok(output) => Some(output.clone()),
                    Err(_)
                        if planning_mode
                            == StagedVideoPlanningMode::CompatibilityDeferredAllowed
                            && self.can_defer_packet_refinement(&requirement) =>
                    {
                        None
                    }
                    Err(error) => {
                        last_rejection = Some(player_error_from_unsupported_requirement(error));
                        continue;
                    }
                },
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
                        last_rejection = Some(error);
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

            return Ok(Some(StagedVideoTrackPlan {
                track_id: track.id,
                requirement,
                frame_contract,
                stream_config,
                backend_plan,
            }));
        }

        Err(last_rejection.unwrap_or_else(|| {
            PlayerError::new(PlayerErrorKind::UnsupportedVideoCodec, missing_message)
        }))
    }
}

/// Превращает registry preflight в requirement с учётом strong/compatibility режима.
fn resolve_staged_preflight_requirement(
    preflight: VideoRequirementPreflight,
    planning_mode: StagedVideoPlanningMode,
    packet_probe_reader: &mut StagedVideoPacketProbeReader,
    prepared_media: &mut PreparedMedia,
    track: &TrackInfo,
    container_source: Option<VideoMetadataSource>,
) -> PlayerResult<VideoDecodeRequirement> {
    match preflight {
        VideoRequirementPreflight::Resolved(resolved) => Ok(resolved.requirement),
        VideoRequirementPreflight::PacketProbeRequired(initial)
            if planning_mode == StagedVideoPlanningMode::CompatibilityDeferredAllowed =>
        {
            Ok(initial.requirement)
        }
        VideoRequirementPreflight::PacketProbeRequired(_) => packet_probe_reader
            .resolve_requirement_for_track(prepared_media, track, container_source),
        VideoRequirementPreflight::Rejected(rejection) => {
            Err(player_error_from_requirement_rejection(rejection))
        }
        VideoRequirementPreflight::Unavailable { initial, .. }
            if planning_mode == StagedVideoPlanningMode::CompatibilityDeferredAllowed =>
        {
            Ok(initial.requirement)
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
