use player_core::{
    MatchedPlaybackEvent, PlayerEvent, PlayerSnapshot, PreviewFrameReadyEvent, ScrubCommittedEvent,
    ScrubEvent, ScrubEventFrameIdentity, ScrubFrameTiming, ScrubRequestKind, ScrubTargetContext,
};
use video_present_core::{VideoFrameLease, VideoPresentFrameIdentity};

use super::{AppState, RenderablePresentFrame};

/// Тип main-video override, который app имеет право показывать в S16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MainVisualOverrideKind {
    SeekLanding,
    LiveScrub,
}

impl MainVisualOverrideKind {
    /// Мапит neutral scrub request kind в visual override kind.
    fn from_request_kind(request_kind: ScrubRequestKind) -> Self {
        match request_kind {
            ScrubRequestKind::SeekLanding => Self::SeekLanding,
            ScrubRequestKind::LiveScrub => Self::LiveScrub,
        }
    }
}

/// Identity active override-а, по которому app сравнивает stale/commit/match события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MainVisualOverrideMetadata {
    kind: MainVisualOverrideKind,
    context: ScrubTargetContext,
    timing: ScrubFrameTiming,
    frame_identity: VideoPresentFrameIdentity,
}

impl MainVisualOverrideMetadata {
    /// Собирает metadata только для разрешённых main-video override kinds.
    fn from_preview_frame_event(event: &PreviewFrameReadyEvent) -> Self {
        let kind = MainVisualOverrideKind::from_request_kind(event.context.request_kind());
        Self {
            kind,
            context: event.context,
            timing: event.frame.timing,
            frame_identity: event.frame.frame_identity,
        }
    }

    /// Проверяет, что lease относится к тому же decoded frame, что и scrub event.
    fn matches_lease(&self, lease: &VideoFrameLease) -> bool {
        self.frame_identity
            == VideoPresentFrameIdentity::from_decoded_frame(
                lease.render_generation(),
                lease.decoded_frame(),
            )
    }

    /// Проверяет, что commit/match event относится именно к active override.
    fn matches_clear_event(
        &self,
        context: ScrubTargetContext,
        timing: ScrubFrameTiming,
        frame_identity: ScrubEventFrameIdentity,
    ) -> bool {
        self.context == context
            && self.timing == timing
            && frame_identity == ScrubEventFrameIdentity::Video(self.frame_identity)
    }

    /// Проверяет, что incoming scrub context уже новее active override-а.
    fn superseded_by_context(&self, context: ScrubTargetContext) -> bool {
        let active_generation = self.context.generation();
        let incoming_generation = context.generation();

        incoming_generation.playback_generation > active_generation.playback_generation
            || (incoming_generation.playback_generation == active_generation.playback_generation
                && incoming_generation.scrub_generation > active_generation.scrub_generation)
    }

    /// Проверяет render generation guard до materialization/render.
    fn stale_for_snapshot(&self, player_snapshot: &PlayerSnapshot) -> bool {
        self.frame_identity.render_generation() != player_snapshot.render_generation
    }
}

/// Active main-video override: metadata появляется по event, renderable frame после S15 materialization.
#[derive(Clone)]
pub(super) struct MainVisualOverride {
    metadata: MainVisualOverrideMetadata,
    renderable_frame: Option<RenderablePresentFrame>,
}

impl MainVisualOverride {
    /// Создаёт active override без renderable frame: lease ещё нужно получить/materialize-ить.
    fn pending(metadata: MainVisualOverrideMetadata) -> Self {
        Self {
            metadata,
            renderable_frame: None,
        }
    }

    /// Возвращает already-materialized frame только если snapshot generation ещё совпадает.
    fn renderable_for_snapshot(
        &self,
        player_snapshot: &PlayerSnapshot,
    ) -> Option<RenderablePresentFrame> {
        let renderable_frame = self.renderable_frame.as_ref()?;
        if self.metadata.stale_for_snapshot(player_snapshot) {
            return None;
        }
        if renderable_frame.present_frame.render_generation() != player_snapshot.render_generation {
            return None;
        }
        Some(renderable_frame.clone())
    }
}

/// Результат app-level acquisition перед materialization stage.
pub(crate) enum MainVisualOverrideAcquisition {
    NoOverride,
    WaitingForExactFrame,
    Ready(RenderablePresentFrame),
    Lease {
        metadata: MainVisualOverrideMetadata,
        lease: VideoFrameLease,
    },
}

impl MainVisualOverrideAcquisition {
    /// Диагностическое имя source state для render timing/telemetry.
    pub(crate) const fn metric_name(&self) -> &'static str {
        match self {
            Self::NoOverride => "scrub_override_none",
            Self::WaitingForExactFrame => "scrub_override_waiting_for_exact_frame",
            Self::Ready(_) => "scrub_override_cached_renderable",
            Self::Lease { .. } => "scrub_override_lease_acquired",
        }
    }
}

/// App-owned state main-video visual override-а.
#[derive(Default)]
pub(super) struct MainVisualOverrideState {
    active: Option<MainVisualOverride>,
}

impl MainVisualOverrideState {
    /// Запоминает exact preview как candidate для main-video override-а.
    pub(super) fn note_preview_frame_ready(&mut self, event: &PreviewFrameReadyEvent) {
        let metadata = MainVisualOverrideMetadata::from_preview_frame_event(event);
        let stale_preview = self.active.as_ref().is_some_and(|active_override| {
            metadata.superseded_by_context(active_override.metadata.context)
        });
        if stale_preview {
            return;
        }

        let preserved_renderable = self
            .active
            .as_ref()
            .filter(|active_override| active_override.metadata == metadata)
            .and_then(|active_override| active_override.renderable_frame.clone());

        self.active = Some(match preserved_renderable {
            Some(renderable_frame) => MainVisualOverride {
                metadata,
                renderable_frame: Some(renderable_frame),
            },
            None => MainVisualOverride::pending(metadata),
        });
    }

    /// Обрабатывает scrub event без timeline actions и без heuristic clear.
    pub(super) fn handle_scrub_event(&mut self, event: &ScrubEvent) {
        self.clear_if_superseded_by_context(scrub_event_context(event));

        match event {
            ScrubEvent::PreviewFrameReady(event) => self.note_preview_frame_ready(event),
            ScrubEvent::Committed(event) => self.clear_if_committed_matches(event),
            ScrubEvent::MatchedPlayback(event) => self.clear_if_matched_playback_matches(event),
            ScrubEvent::Cancelled(event) => self.clear_if_context_matches(event.context),
            ScrubEvent::Failed(event) => self.clear_if_context_matches(event.context),
            ScrubEvent::Started(_) | ScrubEvent::Progress(_) | ScrubEvent::ResumePending(_) => {}
        }
    }

    /// Чистит override на source/backend/media lifecycle boundaries.
    pub(super) fn handle_player_event(&mut self, event: &PlayerEvent) {
        if player_event_invalidates_main_visual_override(event) {
            self.clear();
        }
    }

    /// Чистит stale override перед render, если render generation уже сменился.
    pub(super) fn drop_stale_for_snapshot(&mut self, player_snapshot: &PlayerSnapshot) {
        let should_clear = self.active.as_ref().is_some_and(|active_override| {
            active_override.metadata.stale_for_snapshot(player_snapshot)
        });
        if should_clear {
            self.clear();
        }
    }

    /// Возвращает active metadata, если override ещё не стал stale.
    pub(super) fn active_metadata_for_snapshot(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> Option<MainVisualOverrideMetadata> {
        self.drop_stale_for_snapshot(player_snapshot);
        self.active
            .as_ref()
            .map(|active_override| active_override.metadata)
    }

    /// Возвращает уже materialized override frame, если его можно рендерить сейчас.
    pub(super) fn renderable_for_snapshot(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> Option<RenderablePresentFrame> {
        self.drop_stale_for_snapshot(player_snapshot);
        self.active
            .as_ref()
            .and_then(|active_override| active_override.renderable_for_snapshot(player_snapshot))
    }

    /// Сохраняет S15-materialized frame отдельно от playback frame cache.
    pub(super) fn remember_renderable(
        &mut self,
        metadata: MainVisualOverrideMetadata,
        renderable_frame: RenderablePresentFrame,
    ) {
        let Some(active_override) = &mut self.active else {
            return;
        };
        if active_override.metadata == metadata {
            active_override.renderable_frame = Some(renderable_frame);
        }
    }

    /// Удаляет active override без touching playback cache.
    pub(super) fn clear(&mut self) {
        self.active = None;
    }

    fn clear_if_committed_matches(&mut self, event: &ScrubCommittedEvent) {
        self.clear_if_matches(
            event.context,
            event.committed_frame_timing,
            event.frame_identity,
        );
    }

    fn clear_if_matched_playback_matches(&mut self, event: &MatchedPlaybackEvent) {
        self.clear_if_matches(
            event.context,
            event.matched_frame_timing,
            event.frame_identity,
        );
    }

    fn clear_if_matches(
        &mut self,
        context: ScrubTargetContext,
        timing: ScrubFrameTiming,
        frame_identity: ScrubEventFrameIdentity,
    ) {
        let should_clear = self.active.as_ref().is_some_and(|active_override| {
            active_override
                .metadata
                .matches_clear_event(context, timing, frame_identity)
        });
        if should_clear {
            self.clear();
        }
    }

    fn clear_if_context_matches(&mut self, context: ScrubTargetContext) {
        let should_clear = self
            .active
            .as_ref()
            .is_some_and(|active_override| active_override.metadata.context == context);
        if should_clear {
            self.clear();
        }
    }

    fn clear_if_superseded_by_context(&mut self, context: ScrubTargetContext) {
        let should_clear = self
            .active
            .as_ref()
            .is_some_and(|active_override| active_override.metadata.superseded_by_context(context));
        if should_clear {
            self.clear();
        }
    }
}

fn scrub_event_context(event: &ScrubEvent) -> ScrubTargetContext {
    match event {
        ScrubEvent::Started(event) => event.context,
        ScrubEvent::Progress(event) => event.context,
        ScrubEvent::PreviewFrameReady(event) => event.context,
        ScrubEvent::ResumePending(event) => event.context,
        ScrubEvent::Committed(event) => event.context,
        ScrubEvent::MatchedPlayback(event) => event.context,
        ScrubEvent::Cancelled(event) => event.context,
        ScrubEvent::Failed(event) => event.context,
    }
}

impl AppState {
    /// Переносит scrub event stream в app-owned main visual override state.
    pub(crate) fn handle_main_visual_override_scrub_event(&mut self, event: &ScrubEvent) {
        self.main_visual_override_state.handle_scrub_event(event);
    }

    /// Синхронизирует override lifecycle с player lifecycle events.
    pub(crate) fn handle_main_visual_override_player_event(&mut self, event: &PlayerEvent) {
        self.main_visual_override_state.handle_player_event(event);
    }

    /// Выбирает active override source перед playback frame cache.
    pub(crate) fn acquire_main_visual_override_for_render(
        &mut self,
        player_snapshot: &PlayerSnapshot,
    ) -> MainVisualOverrideAcquisition {
        if let Some(renderable_frame) = self
            .main_visual_override_state
            .renderable_for_snapshot(player_snapshot)
        {
            return MainVisualOverrideAcquisition::Ready(renderable_frame);
        }

        let Some(metadata) = self
            .main_visual_override_state
            .active_metadata_for_snapshot(player_snapshot)
        else {
            return MainVisualOverrideAcquisition::NoOverride;
        };

        let Some(lease) = self.player_worker.try_acquire_scrub_visual_override_frame() else {
            return MainVisualOverrideAcquisition::WaitingForExactFrame;
        };
        if !metadata.matches_lease(&lease) {
            return MainVisualOverrideAcquisition::WaitingForExactFrame;
        }

        MainVisualOverrideAcquisition::Lease { metadata, lease }
    }

    /// Запоминает materialized override frame отдельно от playback cache.
    pub(crate) fn remember_main_visual_override_renderable(
        &mut self,
        metadata: MainVisualOverrideMetadata,
        renderable_frame: RenderablePresentFrame,
    ) {
        self.main_visual_override_state
            .remember_renderable(metadata, renderable_frame);
    }

    /// Сбрасывает visual override при owner/source/backend lifecycle boundary.
    pub(crate) fn clear_main_visual_override(&mut self) {
        self.main_visual_override_state.clear();
    }
}

fn player_event_invalidates_main_visual_override(event: &PlayerEvent) -> bool {
    matches!(
        event,
        PlayerEvent::MediaOpenRequested(_)
            | PlayerEvent::MediaOpened(_)
            | PlayerEvent::VideoBackendSelectionRequested(_)
            | PlayerEvent::VideoTrackSelected(_)
            | PlayerEvent::PlaybackStateChanged(player_core::PlaybackState::Stopped)
            | PlayerEvent::PlaybackStateChanged(player_core::PlaybackState::Failed)
            | PlayerEvent::ShutdownRequested
            | PlayerEvent::FatalError(_)
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
    use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};
    use player_core::{
        BackendRevision, PlaybackGeneration, ScrubDriverOutcomeKind, ScrubEvent,
        ScrubEventDiagnostics, ScrubEventFrameIdentity, ScrubExactnessPolicy, ScrubGeneration,
        ScrubGenerationToken, ScrubNoVideoFrameReason, ScrubPreviewFrame, ScrubTarget,
        ScrubTrackSelection, SourceRevision,
    };
    use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::*;

    fn generation_token(playback: u64, scrub: u64) -> ScrubGenerationToken {
        ScrubGenerationToken::new(
            PlaybackGeneration::new(playback),
            ScrubGeneration::new(scrub),
        )
    }

    fn track_timestamp(track_id: TrackId, millis: u64) -> TrackTimestamp {
        let time_base = TimeBase::new(1, 1_000).expect("валидная test timebase");
        TrackTimestamp::new(track_id, millis as i64, time_base)
    }

    fn context_for_tests(
        request_kind: ScrubRequestKind,
        source_revision: u64,
        backend_revision: u64,
        generation: ScrubGenerationToken,
    ) -> ScrubTargetContext {
        let video_track = TrackId::new(7);
        ScrubTargetContext::new(
            SourceRevision::new(source_revision),
            BackendRevision::new(backend_revision),
            ScrubTrackSelection::with_audio(video_track, TrackId::new(8)),
            ScrubTarget::new(
                MediaTime::from_millis(1_250),
                track_timestamp(video_track, 1_250),
            ),
            ScrubExactnessPolicy::TargetOrAfter,
            request_kind,
            generation,
        )
    }

    fn decoded_frame_for_tests(resource_handle: FrameResourceHandle) -> DecodedFrame {
        DecodedFrame {
            generation: 30,
            pts: Duration::from_millis(1_250),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: VideoFrameDiagnostics::default(),
        }
    }

    fn preview_event_for_tests(
        context: ScrubTargetContext,
        resource_handle: FrameResourceHandle,
    ) -> PreviewFrameReadyEvent {
        let decoded_frame = decoded_frame_for_tests(resource_handle);
        PreviewFrameReadyEvent {
            context,
            frame: ScrubPreviewFrame {
                generation: context.generation(),
                timing: ScrubFrameTiming::new(
                    context.target().media_time,
                    context.target().target_pts,
                ),
                frame_identity: VideoPresentFrameIdentity::from_decoded_frame(2, &decoded_frame),
                resource:
                    video_present_core::VideoPresentFrameResourceDescriptor::from_decoded_frame(
                        2,
                        &decoded_frame,
                    ),
            },
            diagnostics: ScrubEventDiagnostics::new(ScrubDriverOutcomeKind::ExactFrameReady),
        }
    }

    #[test]
    fn preview_frame_ready_installs_main_video_override_kind() {
        let mut state = MainVisualOverrideState::default();
        let generation = generation_token(1, 1);
        let seek_context = context_for_tests(ScrubRequestKind::SeekLanding, 10, 20, generation);

        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(preview_event_for_tests(
            seek_context,
            FrameResourceHandle(41),
        )));
        assert!(matches!(
            state
                .active
                .as_ref()
                .map(|active_override| active_override.metadata.kind),
            Some(MainVisualOverrideKind::SeekLanding)
        ));
    }

    #[test]
    fn mismatched_commit_or_match_does_not_clear_newer_override() {
        let mut state = MainVisualOverrideState::default();
        let old_context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 1),
        );
        let new_context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 2),
        );
        let old_event = preview_event_for_tests(old_context, FrameResourceHandle(40));
        let new_event = preview_event_for_tests(new_context, FrameResourceHandle(41));
        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(new_event));

        state.handle_scrub_event(&ScrubEvent::Committed(ScrubCommittedEvent {
            context: old_context,
            committed_position: old_context.target().media_time,
            committed_frame_timing: old_event.frame.timing,
            frame_identity: ScrubEventFrameIdentity::Video(old_event.frame.frame_identity),
            diagnostics: ScrubEventDiagnostics::new(ScrubDriverOutcomeKind::Finished),
        }));

        assert_eq!(
            state
                .active
                .as_ref()
                .map(|active_override| active_override.metadata.context),
            Some(new_context)
        );
    }

    #[test]
    fn matching_commit_clears_override_without_timer_fallback() {
        let mut state = MainVisualOverrideState::default();
        let context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 1),
        );
        let event = preview_event_for_tests(context, FrameResourceHandle(40));
        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(event));

        state.handle_scrub_event(&ScrubEvent::Committed(ScrubCommittedEvent {
            context,
            committed_position: context.target().media_time,
            committed_frame_timing: event.frame.timing,
            frame_identity: ScrubEventFrameIdentity::Video(event.frame.frame_identity),
            diagnostics: ScrubEventDiagnostics::new(ScrubDriverOutcomeKind::Finished),
        }));

        assert!(state.active.is_none());
    }

    #[test]
    fn matching_playback_match_clears_override_without_timer_fallback() {
        let mut state = MainVisualOverrideState::default();
        let context =
            context_for_tests(ScrubRequestKind::LiveScrub, 10, 20, generation_token(1, 1));
        let event = preview_event_for_tests(context, FrameResourceHandle(40));
        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(event));

        state.handle_scrub_event(&ScrubEvent::MatchedPlayback(MatchedPlaybackEvent {
            context,
            playback_position: context.target().media_time,
            matched_frame_timing: event.frame.timing,
            frame_identity: ScrubEventFrameIdentity::Video(event.frame.frame_identity),
            diagnostics: ScrubEventDiagnostics::new(ScrubDriverOutcomeKind::MatchedPlayback),
        }));

        assert!(state.active.is_none());
    }

    #[test]
    fn newer_scrub_generation_clears_old_override_without_frame_match() {
        let mut state = MainVisualOverrideState::default();
        let old_context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 1),
        );
        let new_context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 2),
        );
        let event = preview_event_for_tests(old_context, FrameResourceHandle(40));
        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(event));

        state.handle_scrub_event(&ScrubEvent::Committed(ScrubCommittedEvent {
            context: new_context,
            committed_position: new_context.target().media_time,
            committed_frame_timing: ScrubFrameTiming::new(
                new_context.target().media_time,
                new_context.target().target_pts,
            ),
            frame_identity: ScrubEventFrameIdentity::NoVideoFrame(
                ScrubNoVideoFrameReason::CurrentFrameUnavailable,
            ),
            diagnostics: ScrubEventDiagnostics::new(ScrubDriverOutcomeKind::Finished),
        }));

        assert!(state.active.is_none());
    }

    #[test]
    fn stale_preview_frame_ready_does_not_replace_newer_override() {
        let mut state = MainVisualOverrideState::default();
        let old_context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 1),
        );
        let new_context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 2),
        );
        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(preview_event_for_tests(
            new_context,
            FrameResourceHandle(41),
        )));

        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(preview_event_for_tests(
            old_context,
            FrameResourceHandle(40),
        )));

        assert_eq!(
            state
                .active
                .as_ref()
                .map(|active_override| active_override.metadata.context),
            Some(new_context)
        );
    }

    #[test]
    fn stale_generation_snapshot_prevents_rendering_override() {
        let mut state = MainVisualOverrideState::default();
        let context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 1),
        );
        let event = preview_event_for_tests(context, FrameResourceHandle(40));
        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(event));
        let mut player_snapshot = PlayerSnapshot::empty();
        player_snapshot.render_generation = 3;

        assert!(
            state
                .active_metadata_for_snapshot(&player_snapshot)
                .is_none()
        );
        assert!(state.active.is_none());
    }

    #[test]
    fn source_backend_or_track_switch_event_clears_override() {
        let mut state = MainVisualOverrideState::default();
        let context = context_for_tests(
            ScrubRequestKind::SeekLanding,
            10,
            20,
            generation_token(1, 1),
        );
        state.handle_scrub_event(&ScrubEvent::PreviewFrameReady(preview_event_for_tests(
            context,
            FrameResourceHandle(40),
        )));

        state.handle_player_event(&PlayerEvent::VideoTrackSelected(TrackId::new(99)));

        assert!(state.active.is_none());
    }
}
