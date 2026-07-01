use super::telemetry_panel::TelemetryPanelState;
use super::timeline_hover_leave_grace::{
    TimelineHoverLeaveGraceReleaseReason, TimelineHoverLeaveGraceStartOutcome,
};
use super::*;
use crate::timeline_hover_intent::{TimelineHoverFrameCoalescer, TimelineHoverFrameOutcome};
use crate::ui::timeline::{TimelineHoverTarget, TimelineHoverVisualTarget};
use frame_server_core::TimelineHoverPrepareSessionEndReleaseReason;
use tracing::{trace, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimelineHoverPrepareUiOutcome {
    request_accepted: bool,
    preview_load_state: TimelineHoverPreviewLoadState,
}

/// Diagnostic route пользовательского timeline intent-а на границе app-egui -> player-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimelineCommandRoute {
    /// Одиночный click ушёл в exact final seek без scrub generation.
    ClickSeek,

    /// Pointer drag release ушёл как exact final seek без scrub generation.
    DragSeek,

    /// Pointer-down начал live scrub gesture.
    LiveScrubBegin,

    /// Live scrub latest target ушёл в preview route.
    LiveScrubPreview,

    /// Release завершил active live scrub route.
    LiveScrubEnd,
}

impl TimelineCommandRoute {
    /// Возвращает стабильное имя route-а для logs/diagnostics.
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ClickSeek => "click-seek",
            Self::DragSeek => "drag-seek",
            Self::LiveScrubBegin => "live-scrub-begin",
            Self::LiveScrubPreview => "live-scrub-preview",
            Self::LiveScrubEnd => "live-scrub-end",
        }
    }
}

/// Конвертирует timeline action в player command и diagnostic route.
pub(super) fn timeline_command_from_action(
    action: TimelineAction,
) -> Option<(PlayerCommand, TimelineCommandRoute)> {
    match action {
        TimelineAction::ClickSeek(position) => Some((
            PlayerCommand::Seek(SeekRequest::absolute(position)),
            TimelineCommandRoute::ClickSeek,
        )),
        TimelineAction::CommitDragSeek(position) => Some((
            PlayerCommand::Seek(SeekRequest::absolute(position)),
            TimelineCommandRoute::DragSeek,
        )),
        TimelineAction::BeginLiveScrub(_)
        | TimelineAction::PreviewLiveScrub(_)
        | TimelineAction::EndLiveScrub(_)
        | TimelineAction::CancelLiveScrub => None,
    }
}

/// Проверяет, что action не принадлежит timeline и должен оборвать pending leave grace.
pub(super) fn control_action_cancels_timeline_hover_leave_grace(action: &ControlAction) -> bool {
    !matches!(
        action,
        ControlAction::Timeline(_) | ControlAction::TimelineHover(_)
    )
}

/// Проверяет, принадлежит ли primary press текущему timeline target-у/seek-у.
pub(super) fn control_actions_include_timeline_pointer_target(actions: &[ControlAction]) -> bool {
    actions.iter().any(|action| match action {
        ControlAction::Timeline(_) => true,
        ControlAction::TimelineHover(crate::ui::timeline::TimelineHoverIntent::Target(_)) => true,
        ControlAction::TimelineHover(crate::ui::timeline::TimelineHoverIntent::Clear) => false,
        _ => false,
    })
}

/// Ищет raw primary press, который может быть пассивным click-ом вне timeline.
pub(super) fn raw_input_has_primary_pointer_press(egui_input: &egui::RawInput) -> bool {
    egui_input.events.iter().any(|event| {
        matches!(
            event,
            egui::Event::PointerButton {
                button: egui::PointerButton::Primary,
                pressed: true,
                ..
            }
        )
    })
}

/// Мапит app-owned UX reason в neutral working-set cleanup reason.
fn timeline_hover_prepare_session_release_reason(
    reason: TimelineHoverLeaveGraceReleaseReason,
) -> TimelineHoverPrepareSessionEndReleaseReason {
    match reason {
        TimelineHoverLeaveGraceReleaseReason::ImmediateTimelineLeave => {
            TimelineHoverPrepareSessionEndReleaseReason::ImmediateTimelineLeave
        }
        TimelineHoverLeaveGraceReleaseReason::LeaveGraceExpired => {
            TimelineHoverPrepareSessionEndReleaseReason::LeaveGraceExpired
        }
        TimelineHoverLeaveGraceReleaseReason::NonTimelineAction => {
            TimelineHoverPrepareSessionEndReleaseReason::NonTimelineAction
        }
    }
}

/// Мапит neutral titlebar intent в старый settings UI action boundary.
pub(super) const fn settings_action_from_titlebar_icon_action(
    action: TitlebarIconAreaAction,
) -> SettingsUiAction {
    match action {
        TitlebarIconAreaAction::ToggleSettingsSidebar => SettingsUiAction::ToggleOpen,
    }
}

/// Строит S18 prepare target из player-owned guarded snapshot context-а.
pub(super) fn timeline_hover_prepare_target_from_snapshot(
    player_snapshot: &PlayerSnapshot,
    target: TimelineHoverTarget,
) -> Option<TimelineHoverPrepareTarget> {
    let prepare_snapshot = player_snapshot.timeline_hover_prepare?;
    let target_pts = prepare_snapshot.target_pts(target.position());
    let target_bucket = player_core::TimelineHoverPrepareSnapshot::target_bucket(target_pts);
    let target_context = TimelineHoverPrepareTargetContext::new(
        prepare_snapshot.source_revision(),
        prepare_snapshot.backend_revision(),
        prepare_snapshot.track_selection(),
        prepare_snapshot.hover_generation(),
        prepare_snapshot.exactness_policy(),
    );

    Some(TimelineHoverPrepareTarget::unresolved(
        target_context,
        target_pts,
        target_bucket,
        timeline_hover_prepare_playback_mode(
            player_snapshot.playback_state,
            prepare_snapshot.interaction(),
        ),
    ))
}

/// Runtime playback mode влияет только на typed executor degrade/admission.
pub(super) const fn timeline_hover_prepare_playback_mode(
    playback_state: PlaybackState,
    interaction: player_core::TimelineHoverPrepareInteraction,
) -> TimelineHoverPreparePlaybackMode {
    match interaction {
        player_core::TimelineHoverPrepareInteraction::LiveScrubActive => {
            return TimelineHoverPreparePlaybackMode::LiveScrubActive;
        }
        player_core::TimelineHoverPrepareInteraction::OneShotSeekLandingResumePending {
            spare_capacity_available,
        } => {
            return TimelineHoverPreparePlaybackMode::ResumePendingAfterSeek {
                spare_capacity_available,
            };
        }
        player_core::TimelineHoverPrepareInteraction::Ordinary => {}
    }

    match playback_state {
        PlaybackState::Paused
        | PlaybackState::Stopped
        | PlaybackState::Idle
        | PlaybackState::Ended => TimelineHoverPreparePlaybackMode::PausedOrStopped,
        PlaybackState::Scrubbing => TimelineHoverPreparePlaybackMode::LiveScrubActive,
        PlaybackState::Opening
        | PlaybackState::Playing
        | PlaybackState::Buffering
        | PlaybackState::Seeking
        | PlaybackState::Draining
        | PlaybackState::Failed => TimelineHoverPreparePlaybackMode::ActivePlayback,
    }
}

/// Проверяет, можно ли visual HoverPreview брать lease из shared prepared working set-а.
pub(super) const fn timeline_hover_prepare_allows_preview_borrow(
    playback_mode: TimelineHoverPreparePlaybackMode,
) -> bool {
    !matches!(
        playback_mode,
        TimelineHoverPreparePlaybackMode::LiveScrubActive
    )
}

const fn timeline_hover_preview_load_state(
    target: TimelineHoverTarget,
    executor_outcome: TimelineHoverPrepareExecutorOutcome,
) -> TimelineHoverPreviewLoadState {
    match executor_outcome {
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason:
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening { .. },
        } => TimelineHoverPreviewLoadState::NetworkOpening { target },
        _ => TimelineHoverPreviewLoadState::Idle,
    }
}

impl AppState {
    /// Рендерит egui UI поверх видео.
    ///
    /// UI читает только `PlayerSnapshot`, а действия после egui closure отправляет worker-у.
    #[instrument(skip(self, window, frame_context, egui_input, settings_ui_model))]
    pub fn render_ui(
        &mut self,
        window: &Window,
        egui_input: egui::RawInput,
        frame_context: &AppFrameContext,
        settings_ui_model: &SettingsUiModel,
    ) -> RenderedAppUi {
        let render_ui_started_at = Instant::now();

        let pre_ui_setup_started_at = Instant::now();
        let player_snapshot = frame_context.player_snapshot();
        let is_playing = player_snapshot.playback_state == PlaybackState::Playing;
        let timeline_inline_status = self.timeline_inline_status_message(Instant::now());

        if is_playing {
            self.next_frame();
        }

        let backend_name = player_snapshot
            .active_backend
            .name
            .clone()
            .unwrap_or_else(|| "Synthetic (test)".to_string());
        let telemetry = Arc::clone(&self.telemetry);
        let start_time = self.start_time;
        let frame_duration_estimate_ms =
            player_snapshot.video_frame_duration_estimate.as_secs_f64() * 1000.0;
        let player_error_message = player_snapshot
            .last_error
            .as_ref()
            .map(std::string::ToString::to_string);
        let error_message = player_error_message
            .as_deref()
            .or(self.startup_error.as_deref());
        let pending_message = if error_message.is_none() {
            self.startup_pending.as_deref()
        } else {
            None
        };
        let render_diagnostics = frame_context.render_diagnostics();
        let selected_skin =
            skin::skin_from_config(self.committed_config_snapshot.ui_skin()).unwrap_or_else(|| {
            warn!(
                skin = %self.committed_config_snapshot.ui_skin(),
                "Config validation должна была отклонить неизвестный UI skin; используем minimal"
            );
            skin::MinimalSkin
        });
        let animation_state = AnimationState::from_timeline(&player_snapshot.timeline);
        // Позиция анимации продвинута раньше в prepare_ui_frame; здесь только чтение.
        let sidebar_slide_progress = self.sidebar_slide.eased_progress(Easing::EaseInOutCubic);
        let show_telemetry = self.committed_config_snapshot.show_telemetry();
        let titlebar_height_points = self.committed_config_snapshot.titlebar_height_points();
        let window_is_maximized = window.is_maximized();
        let window_is_fullscreen = window.fullscreen().is_some();
        let primary_pointer_pressed_this_frame = raw_input_has_primary_pointer_press(&egui_input);
        let mut control_actions = Vec::new();
        let mut settings_actions = Vec::new();
        let mut window_chrome_actions = Vec::new();
        let mut timeline_ui_state = std::mem::take(&mut self.timeline_ui_state);
        let pre_ui_setup_elapsed = pre_ui_setup_started_at.elapsed();
        let mut telemetry_panel_cache_elapsed = Duration::ZERO;
        let telemetry_panel_rows = if show_telemetry {
            let telemetry_panel_cache_started_at = Instant::now();
            let frame_server_diagnostics = self.frame_server_diagnostics_snapshot();
            let panel_rows = self.telemetry_panel_cache.rows_for_frame(
                Instant::now(),
                TelemetryPanelState {
                    player_snapshot,
                    telemetry: &telemetry,
                    render_diagnostics,
                    timeline_ui_state: &timeline_ui_state,
                    frame_server_diagnostics,
                    backend_name: &backend_name,
                    start_time,
                    frame_duration_estimate_ms,
                },
            );
            telemetry_panel_cache_elapsed = telemetry_panel_cache_started_at.elapsed();
            Some(panel_rows)
        } else {
            None
        };

        let mut top_bar_elapsed = Duration::ZERO;
        let mut bottom_controls_elapsed = Duration::ZERO;
        let mut telemetry_panel_elapsed = Duration::ZERO;
        let mut center_overlay_elapsed = Duration::ZERO;
        let mut video_viewport_rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.0, 0.0));
        // Exclusion-механизм остаётся в боундари рендера, но sidebar им больше
        // не пользуется: он сжимает video viewport, а не вырезает видео под собой.
        let video_exclusion_rects = Vec::new();

        let egui_run_started_at = Instant::now();
        let full_output = self.egui_ctx.run_ui(egui_input, |ui| {
            video_viewport_rect = ui.max_rect();

            let stage_started_at = Instant::now();
            let window_chrome_output = window_chrome::show(
                ui,
                WindowChromeInput {
                    title: "Rustiplayer",
                    height_points: titlebar_height_points,
                    is_maximized: window_is_maximized,
                    style: WindowChromeStyle::from_controls_style(selected_skin.controls_style()),
                },
            );
            window_chrome_actions = window_chrome_output.window_actions;
            settings_actions.extend(
                window_chrome_output
                    .titlebar_icon_actions
                    .into_iter()
                    .map(settings_action_from_titlebar_icon_action),
            );
            top_bar_elapsed = stage_started_at.elapsed();

            let stage_started_at = Instant::now();
            control_actions = player_controls::render_bottom_controls(
                ui,
                player_snapshot,
                &mut timeline_ui_state,
                timeline_inline_status,
                &selected_skin,
                window_is_fullscreen,
                self.committed_config_snapshot.live_scrub_enabled(),
            );
            bottom_controls_elapsed = stage_started_at.elapsed();

            let sidebar_rect = sidebar::show(
                ui,
                AppSidebarContent::Settings {
                    model: settings_ui_model,
                },
                sidebar_slide_progress,
                &mut settings_actions,
            );
            if let Some(sidebar_rect) = sidebar_rect {
                // Sidebar вытесняет видео: content viewport начинается от правого
                // края панели, letterbox/aspect ratio рендер пересчитает сам.
                video_viewport_rect.min.x = sidebar_rect
                    .right()
                    .clamp(video_viewport_rect.min.x, video_viewport_rect.max.x);
            }

            if let Some(telemetry_panel_rows) = telemetry_panel_rows.as_ref() {
                let stage_started_at = Instant::now();
                Self::render_telemetry_panel(ui, telemetry_panel_rows);
                telemetry_panel_elapsed =
                    telemetry_panel_cache_elapsed + stage_started_at.elapsed();
            }

            let stage_started_at = Instant::now();
            Self::render_center_overlay(
                ui,
                is_playing,
                error_message,
                pending_message,
                &selected_skin,
                animation_state,
            );
            center_overlay_elapsed = stage_started_at.elapsed();
        });
        let egui_run_elapsed = egui_run_started_at.elapsed();

        if self.sidebar_slide.is_animating() {
            // Пока анимация sidebar активна, просим следующий кадр явно:
            // без playback нет другого источника непрерывных redraw-ов.
            self.egui_ctx.request_repaint();
        }

        let post_ui_actions_started_at = Instant::now();
        self.timeline_ui_state = timeline_ui_state;
        if !settings_actions.is_empty() || !window_chrome_actions.is_empty() {
            self.release_pending_timeline_hover_leave_grace_for_non_timeline_action();
        }
        let primary_pointer_press_outside_timeline = primary_pointer_pressed_this_frame
            && !control_actions_include_timeline_pointer_target(&control_actions);
        self.release_expired_timeline_hover_leave_grace(Instant::now());
        self.handle_control_actions(window, player_snapshot, control_actions);
        if primary_pointer_press_outside_timeline {
            self.release_pending_timeline_hover_leave_grace_for_non_timeline_action();
        }
        self.release_expired_timeline_hover_leave_grace(Instant::now());
        let post_ui_actions_elapsed = post_ui_actions_started_at.elapsed();

        RenderedAppUi {
            full_output,
            settings_actions,
            window_chrome_actions,
            video_viewport_rect,
            video_exclusion_rects,
            timings: AppUiRenderTimings {
                total: render_ui_started_at.elapsed(),
                pre_ui_setup: pre_ui_setup_elapsed,
                egui_run: egui_run_elapsed,
                top_bar: top_bar_elapsed,
                bottom_controls: bottom_controls_elapsed,
                telemetry_panel: telemetry_panel_elapsed,
                center_overlay: center_overlay_elapsed,
                post_ui_actions: post_ui_actions_elapsed,
                telemetry_panel_visible: show_telemetry,
            },
        }
    }

    /// Применяет действия controls после завершения egui pass.
    pub(super) fn handle_control_actions(
        &mut self,
        window: &Window,
        player_snapshot: &PlayerSnapshot,
        actions: Vec<ControlAction>,
    ) {
        let mut timeline_hover_coalescer = TimelineHoverFrameCoalescer::default();

        for action in actions {
            if control_action_cancels_timeline_hover_leave_grace(&action) {
                self.release_pending_timeline_hover_leave_grace_for_non_timeline_action();
            }

            match action {
                ControlAction::TogglePlayback => self.toggle_playback(),
                ControlAction::OpenFile => self.open_file(window),
                ControlAction::SetVolume(requested_volume) => {
                    if let Err(error) = self
                        .player_worker
                        .try_send_command(PlayerCommand::SetVolume(requested_volume))
                    {
                        warn!(error = %error, "Некорректное значение громкости из UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::ToggleMute => {
                    if let Err(error) =
                        self.player_worker
                            .try_send_command(PlayerCommand::ToggleMute {
                                fallback_volume: self
                                    .committed_config_snapshot
                                    .default_volume_for_new_media(),
                            })
                    {
                        warn!(error = %error, "Не удалось переключить mute из UI");
                    } else {
                        self.mark_pending_worker_redraw();
                    }
                }
                ControlAction::ToggleFullscreen => Self::toggle_fullscreen(window),
                ControlAction::Timeline(timeline_action) => {
                    self.send_timeline_action(timeline_action);
                }
                ControlAction::TimelineHover(hover_intent) => {
                    timeline_hover_coalescer.record(hover_intent);
                }
            }
        }

        let hover_frame_outcome = timeline_hover_coalescer.finish(
            &mut self.timeline_hover_intent_state,
            self.committed_config_snapshot.hover_preview_enabled(),
        );
        let should_retry_pending_preview = hover_frame_outcome.visual_presentation_target.is_none()
            && !hover_frame_outcome.visual_presentation_cleared;
        self.apply_timeline_hover_frame_outcome(
            player_snapshot,
            hover_frame_outcome,
            Instant::now(),
        );
        if should_retry_pending_preview
            && let Some(visual_target) = self.timeline_hover_intent_state.pending_visual_target()
        {
            self.update_timeline_hover_preview(
                player_snapshot,
                visual_target,
                TimelineHoverPreviewLoadState::Idle,
            );
        }
    }

    /// Применяет coalesced S24 hover intent к единому S18/S25 prepare+preview path-у.
    fn apply_timeline_hover_frame_outcome(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        outcome: TimelineHoverFrameOutcome,
        now: Instant,
    ) {
        if outcome.invisible_prepare_cleared {
            self.timeline_hover_prepare_controller
                .cancel_active_span(TimelineHoverPrepareCancellationReason::TimelineLeft);
            self.start_timeline_hover_leave_grace(now);
        }
        if outcome.visual_presentation_cleared {
            self.timeline_hover_preview_render_state.clear();
            self.frame_server_diagnostics.record_hover_preview_cleared();
        }

        let mut preview_load_state = TimelineHoverPreviewLoadState::Idle;
        if let Some(target) = outcome.invisible_prepare_target {
            if !self.committed_config_snapshot.hover_preview_enabled() {
                self.frame_server_diagnostics
                    .record_hover_preview_disabled_by_config();
            }
            let prepare_outcome = self.prepare_timeline_hover_target(player_snapshot, target);
            preview_load_state = prepare_outcome.preview_load_state;
            if prepare_outcome.request_accepted {
                self.cancel_timeline_hover_leave_grace_for_reenter();
            }
        }

        if let Some(visual_target) = outcome.visual_presentation_target {
            self.update_timeline_hover_preview(player_snapshot, visual_target, preview_load_state);
        }
    }

    /// Запускает controller request для invisible prepare stream-а.
    fn prepare_timeline_hover_target(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        target: TimelineHoverTarget,
    ) -> TimelineHoverPrepareUiOutcome {
        let Some(prepare_target) =
            timeline_hover_prepare_target_from_snapshot(player_snapshot, target)
        else {
            self.timeline_hover_prepare_controller
                .cancel_active_span(TimelineHoverPrepareCancellationReason::SourceSwitched);
            return TimelineHoverPrepareUiOutcome {
                request_accepted: false,
                preview_load_state: TimelineHoverPreviewLoadState::Idle,
            };
        };

        let controller_outcome = self
            .timeline_hover_prepare_controller
            .prepare_hover_target(prepare_target);
        let preview_load_state =
            timeline_hover_preview_load_state(target, controller_outcome.executor_outcome());
        self.frame_server_diagnostics.record_hover_prepare_outcome(
            controller_outcome.transition(),
            controller_outcome.executor_outcome(),
            controller_outcome.completion_outcome(),
            preview_load_state,
        );
        trace!(
            outcome = ?controller_outcome,
            "Timeline hover prepare target processed"
        );
        TimelineHoverPrepareUiOutcome {
            request_accepted: true,
            preview_load_state,
        }
    }

    /// Запускает retention grace после leave; active decode span уже отменён caller-ом.
    fn start_timeline_hover_leave_grace(&mut self, now: Instant) {
        let grace_duration = self.committed_config_snapshot.hover_leave_grace_duration();
        match self
            .timeline_hover_leave_grace_state
            .note_timeline_left(now, grace_duration)
        {
            TimelineHoverLeaveGraceStartOutcome::Pending { expires_at } => {
                self.frame_server_diagnostics
                    .record_hover_leave_grace_started();
                trace!(
                    ?grace_duration,
                    ?expires_at,
                    "Timeline hover leave grace started"
                );
            }
            TimelineHoverLeaveGraceStartOutcome::ReleaseNow { reason } => {
                self.release_timeline_hover_owned_entries_for_session_end(reason);
            }
        }
    }

    /// Re-enter до expiry сохраняет prepared entries и отменяет pending release.
    fn cancel_timeline_hover_leave_grace_for_reenter(&mut self) {
        if self.timeline_hover_leave_grace_state.cancel_for_reenter() {
            self.frame_server_diagnostics
                .record_hover_leave_grace_reentered_before_expiry();
            trace!("Timeline hover leave grace cancelled by re-enter");
        }
    }

    /// Non-timeline action во время grace завершает hover session без ожидания deadline.
    fn release_pending_timeline_hover_leave_grace_for_non_timeline_action(&mut self) {
        if let Some(reason) = self
            .timeline_hover_leave_grace_state
            .cancel_for_non_timeline_action()
        {
            self.release_timeline_hover_owned_entries_for_session_end(reason);
        }
    }

    /// Проверяет expiry в обычном UI frame-е; отдельный поток или queue не нужны.
    fn release_expired_timeline_hover_leave_grace(&mut self, now: Instant) {
        if let Some(reason) = self.timeline_hover_leave_grace_state.expire_due(now) {
            self.release_timeline_hover_owned_entries_for_session_end(reason);
        }
    }

    /// Освобождает hover-owned entries через handoff, не читая storage напрямую.
    fn release_timeline_hover_owned_entries_for_session_end(
        &mut self,
        reason: TimelineHoverLeaveGraceReleaseReason,
    ) {
        let release_reason = timeline_hover_prepare_session_release_reason(reason);
        let release_outcome = self
            .timeline_hover_prepare_controller
            .release_hover_owned_entries_for_session_end(release_reason);
        trace!(
            ?reason,
            primary_entries_released = release_outcome.primary_entries_released(),
            recent_superseded_entries_released =
                release_outcome.recent_superseded_entries_released(),
            total_entries_released = release_outcome.total_entries_released(),
            "Timeline hover prepared entries released for session end"
        );
        self.frame_server_diagnostics
            .record_hover_leave_grace_release(reason, release_outcome);
    }

    /// Borrow/materialize visual preview из того же shared prepared working set-а.
    fn update_timeline_hover_preview(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        visual_target: TimelineHoverVisualTarget,
        load_state: TimelineHoverPreviewLoadState,
    ) {
        let Some(prepare_target) =
            timeline_hover_prepare_target_from_snapshot(player_snapshot, visual_target.target())
        else {
            self.timeline_hover_preview_render_state.clear();
            self.frame_server_diagnostics.record_hover_preview_cleared();
            return;
        };
        if !timeline_hover_prepare_allows_preview_borrow(prepare_target.playback_mode()) {
            self.timeline_hover_preview_render_state.clear();
            self.frame_server_diagnostics.record_hover_preview_cleared();
            return;
        }
        let lookup_request = prepare_target.lookup_request();
        let borrow_outcome = self
            .timeline_hover_prepare_controller
            .executor()
            .borrow_prepared_frame(lookup_request);
        let materializer = self.wgpu_frame_materializer.as_deref();
        let preview_outcome = self.timeline_hover_preview_render_state.update_from_borrow(
            visual_target,
            borrow_outcome,
            load_state,
            materializer,
        );
        trace!(
            outcome = ?preview_outcome,
            "Timeline hover preview materialization processed"
        );
        self.frame_server_diagnostics
            .record_hover_preview_update(load_state, preview_outcome);
    }

    /// Снимает live-scrub completion-gate по worker landing-сигналу.
    ///
    /// `PlayerEvent::SeekTargetFramePresented` означает, что seek/scrub landing
    /// frame уже стал presented; для active live drag это разблокирует следующий
    /// newest decode target (frame-by-frame перемотка вместо supersede-голодания).
    pub(crate) fn note_live_scrub_landing_for_dispatch(&mut self, target_position: Duration) {
        self.timeline_ui_state.note_live_scrub_landing_presented(
            media_core::MediaTime::from_duration(target_position),
        );
    }

    /// Конвертирует pointer timeline action в typed player command(s).
    ///
    /// Обычные click/disabled-drag actions остаются exact `Seek`. Live drag
    /// идёт через Begin/Preview/End scrub route и не отправляет ordinary seek.
    pub(super) fn send_timeline_action(&mut self, action: TimelineAction) {
        self.clear_timeline_inline_status_for_action();
        match action {
            TimelineAction::ClickSeek(_) | TimelineAction::CommitDragSeek(_) => {
                if let Some((command, route)) = timeline_command_from_action(action) {
                    self.send_timeline_player_command(action, route, command);
                }
            }
            TimelineAction::BeginLiveScrub(position) => {
                let now = Instant::now();
                let settings = self.live_scrub_settings_snapshot();
                self.timeline_ui_state
                    .begin_live_scrub_dispatch(settings, now, position);
                self.send_timeline_player_command(
                    action,
                    TimelineCommandRoute::LiveScrubBegin,
                    PlayerCommand::begin_live_scrub(
                        self.timeline_ui_state
                            .live_scrub_diagnostics()
                            .expect("begin_live_scrub_dispatch just initialized diagnostics"),
                    ),
                );
                self.send_timeline_player_command(
                    action,
                    TimelineCommandRoute::LiveScrubPreview,
                    PlayerCommand::preview_live_scrub(
                        SeekRequest::absolute(position),
                        self.timeline_ui_state
                            .live_scrub_diagnostics()
                            .expect("begin_live_scrub_dispatch just initialized diagnostics"),
                    ),
                );
            }
            TimelineAction::PreviewLiveScrub(position) => {
                let Some(target) = self
                    .timeline_ui_state
                    .live_scrub_preview_dispatch_target(Instant::now(), position)
                else {
                    return;
                };
                self.send_timeline_player_command(
                    action,
                    TimelineCommandRoute::LiveScrubPreview,
                    match self.timeline_ui_state.live_scrub_diagnostics() {
                        Some(live_scrub) => PlayerCommand::preview_live_scrub(
                            SeekRequest::absolute(target),
                            live_scrub,
                        ),
                        None => PlayerCommand::preview_scrub(SeekRequest::absolute(target)),
                    },
                );
            }
            TimelineAction::EndLiveScrub(position) => {
                if let Some(target) = self
                    .timeline_ui_state
                    .live_scrub_release_dispatch_target(Instant::now(), position)
                {
                    self.send_timeline_player_command(
                        action,
                        TimelineCommandRoute::LiveScrubPreview,
                        match self.timeline_ui_state.live_scrub_diagnostics() {
                            Some(live_scrub) => PlayerCommand::preview_live_scrub(
                                SeekRequest::absolute(target),
                                live_scrub,
                            ),
                            None => PlayerCommand::preview_scrub(SeekRequest::absolute(target)),
                        },
                    );
                }
                self.send_timeline_player_command(
                    action,
                    TimelineCommandRoute::LiveScrubEnd,
                    match self.timeline_ui_state.live_scrub_diagnostics() {
                        Some(live_scrub) => PlayerCommand::end_live_scrub(
                            ScrubCommitPolicy::CommitVisiblePreview,
                            live_scrub,
                        ),
                        None => PlayerCommand::end_scrub(ScrubCommitPolicy::CommitVisiblePreview),
                    },
                );
                self.timeline_ui_state.clear_live_scrub_dispatch();
            }
            TimelineAction::CancelLiveScrub => {
                self.send_timeline_player_command(
                    action,
                    TimelineCommandRoute::LiveScrubEnd,
                    match self.timeline_ui_state.live_scrub_diagnostics() {
                        Some(live_scrub) => PlayerCommand::end_live_scrub(
                            ScrubCommitPolicy::CommitLatestTarget,
                            live_scrub,
                        ),
                        None => PlayerCommand::end_scrub(ScrubCommitPolicy::CommitLatestTarget),
                    },
                );
                self.timeline_ui_state.clear_live_scrub_dispatch();
            }
        }
    }

    /// Отправляет одну timeline command с единым diagnostics/log path.
    fn send_timeline_player_command(
        &mut self,
        action: TimelineAction,
        route: TimelineCommandRoute,
        command: PlayerCommand,
    ) {
        let command_for_diagnostics = command.clone();

        if let Err(error) = self.player_worker.try_send_command(command) {
            warn!(error = %error, "Не удалось отправить timeline command");
            return;
        }

        debug!(
            action = ?action,
            command = ?command_for_diagnostics,
            route = route.as_str(),
            "Timeline action отправлен в player worker"
        );
        self.mark_pending_worker_redraw();
    }

    /// Захватывает S19 live scrub settings snapshot для нового pointer gesture-а.
    pub(super) fn live_scrub_settings_snapshot(&self) -> TimelineLiveScrubSettingsSnapshot {
        let decode_mode = match self.committed_config_snapshot.live_scrub_decode_mode() {
            FrameServerLiveScrubDecodeModeConfig::ThrottledLatest => {
                TimelineLiveScrubDecodeMode::ThrottledLatest
            }
            FrameServerLiveScrubDecodeModeConfig::EveryDragEvent => {
                TimelineLiveScrubDecodeMode::EveryDragEvent
            }
        };

        TimelineLiveScrubSettingsSnapshot {
            decode_mode,
            max_hz: self.committed_config_snapshot.live_scrub_max_hz(),
        }
    }

    /// Переключает fullscreen состояние окна.
    pub(super) fn toggle_fullscreen(window: &Window) {
        let is_fullscreen = window.fullscreen().is_some();
        if is_fullscreen {
            window.set_fullscreen(None);
            return;
        }

        if let Some(monitor) = window.current_monitor() {
            window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(Some(monitor))));
        }
    }

    /// Обрабатывает горячие клавиши shell и отправляет команды в playback worker.
    pub fn handle_hotkeys(
        &mut self,
        window: &Window,
        key: winit::keyboard::KeyCode,
        egui_wants_input: bool,
    ) -> bool {
        if egui_wants_input {
            return false;
        }

        match key {
            winit::keyboard::KeyCode::Space => {
                self.release_pending_timeline_hover_leave_grace_for_non_timeline_action();
                self.toggle_playback();
                true
            }
            winit::keyboard::KeyCode::KeyF => {
                self.release_pending_timeline_hover_leave_grace_for_non_timeline_action();
                Self::toggle_fullscreen(window);
                true
            }
            winit::keyboard::KeyCode::KeyM => {
                self.release_pending_timeline_hover_leave_grace_for_non_timeline_action();
                let player_snapshot = self.refresh_player_snapshot();
                self.publish_desktop_snapshot(&player_snapshot);
                if let Err(error) = self
                    .player_worker
                    .try_send_command(PlayerCommand::ToggleMute {
                        fallback_volume: self
                            .committed_config_snapshot
                            .default_volume_for_new_media(),
                    })
                {
                    warn!(error = %error, "Не удалось переключить mute");
                } else {
                    self.mark_pending_worker_redraw();
                }
                true
            }
            _ => false,
        }
    }

    /// Открывает локальный media-файл через file dialog.
    pub fn open_file(&mut self, window: &Window) {
        if self.has_pending_local_file_open() {
            debug!("Local file open job уже активен, повторный dialog не запускаем");
            return;
        }

        match LocalFileOpenJob::spawn(
            window,
            self.committed_config_snapshot.demux_config_for_open(),
        ) {
            Ok(job) => {
                self.local_file_open_job = Some(job);
                self.set_startup_pending("Выбор media-файла...".to_string());
            }
            Err(error) => {
                warn!(error = %error, "Не удалось запустить local file open job");
                self.set_startup_error(format!("Ошибка открытия media-файла: {error}"));
            }
        }
    }

    /// Рендерит центральный overlay состояния.
    fn render_center_overlay(
        ui: &mut egui::Ui,
        is_playing: bool,
        error_message: Option<&str>,
        pending_message: Option<&str>,
        skin: &impl PlayerSkin,
        animation_state: AnimationState,
    ) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show_inside(ui, |ui| {
                if let Some(dim_color) = skin.stale_frame_dim_color(animation_state) {
                    ui.painter().rect_filled(ui.max_rect(), 0.0, dim_color);
                }

                if let Some(error) = error_message {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.colored_label(egui::Color32::RED, error);
                    });
                } else if let Some(message) = pending_message {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.colored_label(egui::Color32::LIGHT_BLUE, message);
                    });
                } else if !is_playing {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.heading("Press Play to start");
                    });
                }
            });
    }

    /// Собирает frame counters из текущей телеметрии.
    pub(super) fn frame_counters_snapshot(&self) -> FrameCounters {
        FrameCounters {
            presented: self.telemetry.video_frames_presented(),
            dropped: self.telemetry.video_frames_dropped(),
            repeated: self.telemetry.video_frames_repeated(),
        }
    }
}
