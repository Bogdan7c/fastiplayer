//! Private lifecycle API session-owned seek state; vocabulary и storage остаются у parent-модуля.

use super::*;

impl SeekRuntimeState {
    /// Проверяет, открыт ли seek commit.
    #[must_use]
    pub(crate) const fn has_active_commit(&self) -> bool {
        self.commit.is_some()
    }

    /// Возвращает active commit by value для gate/scheduler решений.
    #[must_use]
    pub(crate) const fn active_commit(&self) -> Option<SeekCommitState> {
        self.commit
    }

    /// Возвращает mutable active commit, когда command меняет только resume intent.
    pub(crate) fn active_commit_mut(&mut self) -> Option<&mut SeekCommitState> {
        self.commit.as_mut()
    }

    /// Открывает commit state после accepted demux seek.
    pub(crate) fn set_active_commit(&mut self, seek_commit: SeekCommitState) {
        self.acceptance_telemetry.begin_seek(seek_commit);
        self.commit = Some(seek_commit);
    }

    /// Закрывает commit state без изменения trace/scrub/fallback markers.
    pub(crate) fn clear_active_commit(&mut self) {
        self.commit = None;
        self.clear_seek_landing();
    }

    /// Запоминает, что decoder подтвердил Accurate output-floor для указанного generation.
    pub(crate) fn mark_decoder_output_floor_applied(
        &mut self,
        generation: u64,
        floor_pts: Duration,
    ) {
        self.decoder_output_floor = Some(SeekDecoderOutputFloorState {
            generation,
            floor_pts,
        });
    }

    /// Возвращает активный decoder-side floor marker без доступа к decoder internals.
    #[must_use]
    pub(crate) const fn decoder_output_floor(&self) -> Option<SeekDecoderOutputFloorState> {
        self.decoder_output_floor
    }

    /// Проверяет, подтверждён ли decoder-side floor для generation конкретного packet-а.
    #[must_use]
    pub(crate) const fn decoder_output_floor_applied_for_generation(
        &self,
        generation: u64,
    ) -> bool {
        matches!(
            self.decoder_output_floor,
            Some(floor) if floor.generation == generation
        )
    }

    /// Сбрасывает только marker decoder-side floor; decoder command вызывается отдельно.
    pub(crate) fn clear_decoder_output_floor(&mut self) {
        self.decoder_output_floor = None;
    }

    /// Перепривязывает active commit к новому packet generation после demux reset-а.
    pub(crate) fn rebase_active_commit_to_generation(
        &mut self,
        generation: u64,
    ) -> Option<SeekCommitState> {
        let active_commit = self.commit?;
        let rebased_commit = active_commit.rebased_to_generation(generation);
        self.commit = Some(rebased_commit);
        self.acceptance_telemetry.rebase_seek(rebased_commit);
        Some(rebased_commit)
    }

    /// Начинает новый trace для accepted seek generation-а.
    pub(crate) fn begin_trace(&mut self, generation: u64) {
        self.trace.begin(generation);
    }

    /// Очищает trace markers без изменения commit state.
    pub(crate) fn clear_trace(&mut self) {
        self.trace.clear();
        self.acceptance_telemetry.clear_active_presentation();
    }

    /// Учитывает реально presented pre-target frame текущей seek generation.
    pub(crate) fn record_presented_pre_target_frame_for_acceptance(
        &mut self,
        generation: u64,
        frame_pts: Duration,
    ) {
        self.acceptance_telemetry
            .record_presented_pre_target_frame(generation, frame_pts);
    }

    /// Возвращает one-shot evidence первого target/post-target presented frame-а.
    pub(crate) fn record_first_target_frame_presented_for_acceptance(
        &mut self,
        generation: u64,
        frame_pts: Duration,
    ) -> Option<SeekTargetPresentationEvidence> {
        self.acceptance_telemetry
            .record_first_target_frame_presented(generation, frame_pts)
    }

    /// Возвращает финальный presentation counter только для доказанного target frame-а.
    #[must_use]
    pub(crate) fn seek_commit_presentation_evidence(&self, generation: u64) -> Option<u64> {
        self.acceptance_telemetry
            .commit_presentation_evidence(generation)
    }

    /// Взводит bounded one-shot progress proof после успешного Playing commit-а.
    pub(crate) fn arm_post_commit_position_progress(
        &mut self,
        seek_commit: SeekCommitState,
        committed_position: Duration,
        committed_at: Instant,
    ) {
        self.acceptance_telemetry.arm_position_progress(
            seek_commit,
            committed_position,
            committed_at,
        );
    }

    /// Возвращает evidence только для первого положительного post-commit clock delta.
    pub(crate) fn observe_post_commit_position_progress(
        &mut self,
        observed_position: Duration,
        observed_at: Instant,
    ) -> Option<SeekPositionProgressEvidence> {
        self.acceptance_telemetry
            .observe_position_progress(observed_position, observed_at)
    }

    /// Учитывает post-seek demux packet для compact trace logging.
    pub(crate) fn record_post_seek_packet(
        &mut self,
        packet_kind: TrackKind,
    ) -> Option<PostSeekPacketTraceDecision> {
        self.trace.record_post_seek_packet(packet_kind)
    }

    /// Учитывает packet-level demux diagnostics active Accurate preroll-а.
    pub(crate) fn record_accurate_preroll_demux_packet(
        &mut self,
        packet_kind: TrackKind,
        target_or_after_selected_video: bool,
        elapsed: Duration,
    ) {
        self.trace.record_accurate_preroll_demux_packet(
            packet_kind,
            target_or_after_selected_video,
            elapsed,
        );
    }

    /// Учитывает lifecycle/error demux diagnostics active Accurate preroll-а.
    pub(crate) fn record_accurate_preroll_demux_event(
        &mut self,
        event_kind: AccuratePrerollDemuxEventKind,
    ) {
        self.trace.record_accurate_preroll_demux_event(event_kind);
    }

    /// Возвращает `true` только для первого decoded frame текущего trace-а.
    pub(crate) fn record_first_decoded_frame(&mut self) -> bool {
        self.trace.record_first_decoded_frame()
    }

    /// Учитывает target decoded frame для active Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_decoded_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        self.trace
            .record_accurate_preroll_decoded_frame(target_or_after_frame, elapsed);
    }

    /// Возвращает `true` только для первого queued frame текущего trace-а.
    pub(crate) fn record_first_queued_frame(&mut self) -> bool {
        self.trace.record_first_queued_frame()
    }

    /// Учитывает target queued frame для active Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_queued_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        self.trace
            .record_accurate_preroll_queued_frame(target_or_after_frame, elapsed);
    }

    /// Возвращает `true` только для первого presented frame текущего trace-а.
    pub(crate) fn record_first_presented_frame(&mut self, frame_pts: Duration) -> bool {
        self.trace.record_first_presented_frame(frame_pts)
    }

    /// Учитывает target presented frame для active Accurate preroll diagnostics.
    pub(crate) fn record_accurate_preroll_presented_frame(
        &mut self,
        target_or_after_frame: bool,
        elapsed: Duration,
    ) {
        self.trace
            .record_accurate_preroll_presented_frame(target_or_after_frame, elapsed);
    }

    /// Возвращает `true` только для первого TracksChanged marker-а текущего trace-а.
    pub(crate) fn record_first_track_list_update(&mut self) -> bool {
        self.trace.record_first_track_list_update()
    }

    /// Учитывает dropped audio preroll packet active Accurate seek-а.
    pub(crate) fn record_skipped_audio_preroll_packet(&mut self) {
        self.trace.record_skipped_audio_preroll_packet();
    }

    /// Учитывает pre-target video packet, отправленный decoder-у.
    pub(crate) fn record_video_preroll_packet_sent(&mut self) {
        self.trace.record_video_preroll_packet_sent();
    }

    /// Учитывает target-or-after video packet, отправленный decoder-у.
    pub(crate) fn record_target_or_after_video_packet_sent(&mut self) {
        self.trace.record_target_or_after_video_packet_sent();
    }

    /// Учитывает decoded pre-target frame, не попавший в обычный output path.
    pub(crate) fn record_decoded_pre_target_frame_dropped(&mut self) {
        self.trace.record_decoded_pre_target_frame_dropped();
    }

    /// Учитывает decoder/video admission backpressure active Accurate preroll-а.
    pub(crate) fn record_decoder_backpressure_pause(&mut self) {
        self.trace.record_decoder_backpressure_pause();
    }

    /// Возвращает read-only snapshot Accurate preroll diagnostics.
    #[must_use]
    pub(crate) fn accurate_preroll_snapshot(
        &self,
        active: bool,
    ) -> AccurateSeekPrerollDiagnosticsSnapshot {
        self.trace.accurate_preroll_snapshot(active)
    }

    /// Начинает lightweight scrub gesture.
    pub(crate) fn begin_simple_scrub(
        &mut self,
        confirmed_playback_state: PlaybackState,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) {
        self.visible_scrub_preview = None;
        self.simple_scrub
            .begin(confirmed_playback_state, live_scrub_diagnostics);
    }

    /// Возвращает monotonic span от принятого `BeginScrub` без раскрытия storage.
    #[must_use]
    pub(crate) fn simple_scrub_elapsed(&self) -> Option<Duration> {
        self.simple_scrub.elapsed_since_begin()
    }

    /// Запоминает stable identity/timing кадра в момент player-owned presentation.
    pub(crate) fn note_visible_scrub_preview(&mut self, preview: VisibleScrubPreview) {
        self.visible_scrub_preview = Some(preview);
    }

    /// Возвращает последний presented live-scrub preview без изменения lifecycle.
    #[must_use]
    pub(crate) const fn visible_scrub_preview(&self) -> Option<VisibleScrubPreview> {
        self.visible_scrub_preview
    }

    /// Запоминает latest scrub request по latest-wins policy.
    pub(crate) fn store_simple_scrub_request(
        &mut self,
        request: SeekRequest,
        confirmed_playback_state: PlaybackState,
        live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) {
        self.simple_scrub
            .store_request(request, confirmed_playback_state, live_scrub_diagnostics);
    }

    /// Возвращает playback state, подтверждённый до входа в active scrub gesture.
    #[must_use]
    pub(crate) const fn simple_scrub_confirmed_playback_state(&self) -> Option<PlaybackState> {
        self.simple_scrub.confirmed_playback_state()
    }

    /// Возвращает diagnostics active simple scrub-а, если он пришёл из live drag-а.
    #[must_use]
    pub(crate) const fn simple_scrub_live_diagnostics(&self) -> Option<LiveScrubDiagnostics> {
        self.simple_scrub.live_scrub_diagnostics()
    }

    /// Закрывает active scrub gesture и возвращает state для final/cancel route-а.
    pub(crate) fn finish_active_simple_scrub(&mut self) -> Option<FinishedSimpleScrub> {
        self.simple_scrub.finish_active()
    }

    /// Сбрасывает lightweight scrub state без изменения active commit-а.
    pub(crate) fn clear_simple_scrub(&mut self) {
        self.simple_scrub.clear();
    }

    /// Запоминает public seek параметры до того, как state-machine выдаст context.
    pub(crate) fn begin_seek_landing_request(
        &mut self,
        generation: ScrubGenerationToken,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
        route: SeekLandingRoute,
    ) {
        self.pending_seek_landing = Some(PendingSeekLandingState {
            generation,
            seek_mode,
            resume_intent,
            route,
        });
        self.active_seek_landing = None;
    }

    /// Возвращает planned scrub identity для pending SeekLanding-а.
    #[must_use]
    pub(crate) const fn pending_seek_landing_generation(&self) -> Option<ScrubGenerationToken> {
        match self.pending_seek_landing {
            Some(pending) => Some(pending.generation),
            None => None,
        }
    }

    /// Возвращает playback guard текущего SeekLanding route-а.
    ///
    /// Этот guard принадлежит scrub transaction identity и не обязан совпадать
    /// с decoder/pipeline seek generation.
    #[must_use]
    pub(crate) const fn seek_landing_playback_generation(&self) -> Option<PlaybackGeneration> {
        match (self.pending_seek_landing, self.active_seek_landing) {
            (Some(pending), _) => Some(pending.generation.playback_generation),
            (None, Some(active)) => Some(active.generation.playback_generation),
            (None, None) => None,
        }
    }

    /// Возвращает следующий nested scrub generation для replacement target-а.
    ///
    /// Playback generation остаётся решением `PlayerSession`; этот owner-метод
    /// отвечает только за nested S17 generation, которую нельзя вычислять через
    /// прямой доступ к `active_seek_landing`.
    #[must_use]
    pub(crate) fn next_seek_landing_scrub_generation_after_supersede(
        &self,
    ) -> Option<ScrubGeneration> {
        let active = self.active_seek_landing?;
        let next_generation = active.generation.scrub_generation.get().checked_add(1)?;

        Some(ScrubGeneration::new(next_generation))
    }

    /// Привязывает pending S17 SeekLanding request к concrete scrub generation.
    pub(crate) fn activate_seek_landing_generation(
        &mut self,
        generation: ScrubGenerationToken,
        execution: SeekLandingExecution,
        decode_seek_generation: Option<u64>,
    ) {
        let Some(pending) = self.pending_seek_landing.take() else {
            return;
        };

        debug_assert_eq!(pending.generation, generation);
        self.active_seek_landing = Some(ActiveSeekLandingState {
            generation,
            execution,
            seek_mode: pending.seek_mode,
            resume_intent: pending.resume_intent,
            route: pending.route,
            actual_decode_position: None,
            decode_seek_generation,
        });
    }

    /// Запоминает accepted demux position для active SeekLanding generation.
    pub(crate) fn record_seek_landing_demux_accept(
        &mut self,
        generation: ScrubGenerationToken,
        actual_decode_position: MediaTime,
    ) {
        let Some(active) = self.active_seek_landing.as_mut() else {
            return;
        };
        if active.matches_generation(generation) {
            active.actual_decode_position = Some(actual_decode_position);
        }
    }

    /// Возвращает active SeekLanding state только для совпавшей generation.
    #[must_use]
    pub(crate) fn active_seek_landing(
        &self,
        generation: ScrubGenerationToken,
    ) -> Option<ActiveSeekLandingState> {
        self.active_seek_landing
            .filter(|active| active.matches_generation(generation))
    }

    /// Возвращает active SeekLanding state для commit-а prepared/cold route-а.
    #[must_use]
    pub(crate) fn active_seek_landing_for_commit_generation(
        &self,
        commit_generation: u64,
    ) -> Option<ActiveSeekLandingState> {
        self.active_seek_landing
            .filter(|active| active.matches_commit_generation(commit_generation))
    }

    /// Возвращает resume intent active SeekLanding-а для cancel-first маршрутов.
    #[must_use]
    pub(crate) const fn active_seek_landing_resume_intent(&self) -> Option<PlaybackResumeIntent> {
        match self.active_seek_landing {
            Some(active) => Some(active.resume_intent()),
            None => None,
        }
    }

    /// Возвращает `true`, если active route принадлежит live scrub gesture.
    #[must_use]
    pub(crate) const fn active_seek_landing_is_live_scrub(&self) -> bool {
        match self.active_seek_landing {
            Some(active) => active.route().is_live_scrub(),
            None => false,
        }
    }

    /// Возвращает diagnostics active live route-а без раскрытия layout-а route enum.
    #[must_use]
    pub(crate) const fn active_seek_landing_live_diagnostics(
        &self,
    ) -> Option<LiveScrubDiagnostics> {
        match self.active_seek_landing {
            Some(active) => active.route().live_scrub_diagnostics(),
            None => None,
        }
    }

    /// Обновляет diagnostics active live route-а, если command принёс более свежий state.
    pub(crate) fn update_active_live_scrub_diagnostics(
        &mut self,
        diagnostics: Option<LiveScrubDiagnostics>,
    ) {
        let Some(diagnostics) = diagnostics else {
            return;
        };
        let Some(active) = self.active_seek_landing.as_mut() else {
            return;
        };
        let SeekLandingRoute::LiveScrub {
            commit_requested,
            diagnostics: _,
        } = active.route
        else {
            return;
        };
        active.route = SeekLandingRoute::LiveScrub {
            commit_requested,
            diagnostics: Some(diagnostics),
        };
    }

    /// Возвращает `true`, если active commit можно закрывать прямо сейчас.
    #[must_use]
    pub(crate) const fn active_seek_landing_commit_allowed(&self) -> bool {
        match self.active_seek_landing {
            Some(active) => active.route().commit_allowed(),
            None => true,
        }
    }

    /// Переводит active live scrub route в release/commit phase.
    pub(crate) fn request_live_scrub_commit(&mut self, release_started_at: Instant) {
        let Some(active) = self.active_seek_landing.as_mut() else {
            return;
        };
        if !active.route.is_live_scrub() {
            return;
        }

        active.route = SeekLandingRoute::LiveScrub {
            commit_requested: true,
            diagnostics: active.route.live_scrub_diagnostics(),
        };
        if let Some(commit) = self.commit.as_mut() {
            commit.started_at = release_started_at;
        }
    }

    /// Проверяет наличие любого active SeekLanding route-а.
    #[must_use]
    pub(crate) const fn seek_landing_active(&self) -> bool {
        self.active_seek_landing.is_some()
    }

    /// Decode loop должен работать в public `Scrubbing` только для S17A SeekLanding.
    #[must_use]
    pub(crate) const fn seek_landing_decode_active(&self) -> bool {
        match (self.commit, self.active_seek_landing) {
            (Some(_), Some(active)) => active.decode_active(),
            _ => false,
        }
    }

    /// Сбрасывает только S17 SeekLanding markers; simple scrub state не трогает.
    pub(crate) fn clear_seek_landing(&mut self) {
        self.pending_seek_landing = None;
        self.active_seek_landing = None;
    }

    /// Проверяет active scrub state для command/tests.
    #[must_use]
    pub(crate) const fn simple_scrub_active(&self) -> bool {
        self.simple_scrub.active()
    }

    /// Возвращает latest scrub request для diagnostics/tests.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn simple_scrub_latest_request(&self) -> Option<SeekRequest> {
        self.simple_scrub.latest_request()
    }

    /// Устанавливает lightweight scrub state напрямую только для focused boundary tests.
    #[cfg(test)]
    pub(crate) fn set_simple_scrub_state_for_tests(
        &mut self,
        active: bool,
        latest_request: Option<SeekRequest>,
    ) {
        self.simple_scrub.active = active;
        self.simple_scrub.latest_request = latest_request;
        self.simple_scrub.confirmed_playback_state = active.then_some(PlaybackState::Paused);
    }

    /// Проверяет, есть ли pending near-EOF fallback marker.
    #[must_use]
    pub(crate) const fn has_eof_fallback_video_position(&self) -> bool {
        self.eof_fallback_video_position.is_some()
    }

    /// Возвращает PTS presented EOF fallback frame-а.
    #[must_use]
    pub(crate) const fn eof_fallback_video_position(&self) -> Option<MediaTime> {
        self.eof_fallback_video_position
    }

    /// Запоминает PTS presented EOF fallback frame-а.
    pub(crate) fn set_eof_fallback_video_position(&mut self, position: MediaTime) {
        self.eof_fallback_video_position = Some(position);
    }

    /// Очищает EOF fallback marker без release pipeline-owned frame-а.
    pub(crate) fn clear_eof_fallback_video_position(&mut self) {
        self.eof_fallback_video_position = None;
    }
}
