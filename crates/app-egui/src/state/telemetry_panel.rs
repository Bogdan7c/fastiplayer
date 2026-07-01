use super::frame_server_diagnostics::AppFrameServerDiagnosticsSnapshot;
use super::timeline_hover_leave_grace::TimelineHoverLeaveGraceReleaseReason;
use super::*;
use crate::frame_prepare::{TimelineHoverPreviewLoadState, TimelineHoverPreviewUpdateOutcome};
use frame_server_core::{
    CountSummary, DurationSummary, LiveScrubDecodeMode, ScrubDiagnosticsSnapshot,
    ScrubHoverNetworkState,
};

/// Частота обновления тяжёлого текста telemetry panel.
///
/// Видео всё равно перерисовывает egui каждый кадр, но diagnostics-тексту достаточно
/// 4 Hz, чтобы не конкурировать с 60 fps video pacing.
pub(super) const TELEMETRY_PANEL_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// Начальная вместимость строк telemetry panel, чтобы refresh не делал лишних realloc.
pub(super) const TELEMETRY_PANEL_ROW_CAPACITY: usize = 160;

/// Данные правой diagnostic panel, сгруппированные отдельно от UI output.
pub(super) struct TelemetryPanelState<'panel> {
    /// Snapshot player-а на начало egui frame.
    pub(super) player_snapshot: &'panel PlayerSnapshot,

    /// Shared telemetry counters.
    pub(super) telemetry: &'panel Telemetry,

    /// Последняя renderer-neutral диагностика.
    pub(super) render_diagnostics: &'panel RenderDiagnostics,

    /// Transient состояние timeline UI.
    pub(super) timeline_ui_state: &'panel TimelineUiState,

    /// App-owned frame-server diagnostics, которых нет в player snapshot.
    pub(super) frame_server_diagnostics: AppFrameServerDiagnosticsSnapshot,

    /// Имя активного backend-а для diagnostics.
    pub(super) backend_name: &'panel str,

    /// Время запуска приложения.
    pub(super) start_time: std::time::Instant,

    /// Оценка длительности video frame в миллисекундах.
    pub(super) frame_duration_estimate_ms: f64,
}

/// Кэш уже отформатированных строк telemetry panel.
pub(super) struct TelemetryPanelCache {
    /// Строки, которые egui будет раскладывать в текущих frame-ах.
    rows: Arc<[TelemetryPanelRow]>,

    /// Момент, после которого diagnostics нужно перечитать и переформатировать.
    next_refresh_at: Option<Instant>,
}

impl Default for TelemetryPanelCache {
    /// Создаёт пустой cache, который обновится при первом видимом кадре панели.
    fn default() -> Self {
        let rows: Arc<[TelemetryPanelRow]> = Vec::new().into();

        Self {
            rows,
            next_refresh_at: None,
        }
    }
}

impl TelemetryPanelCache {
    /// Возвращает строки для текущего кадра, обновляя тяжёлую diagnostics только по таймеру.
    pub(super) fn rows_for_frame(
        &mut self,
        now: Instant,
        panel_state: TelemetryPanelState<'_>,
    ) -> Arc<[TelemetryPanelRow]> {
        if self.needs_refresh(now) {
            self.rows = AppState::build_telemetry_panel_rows(panel_state);
            self.next_refresh_at = Some(now + TELEMETRY_PANEL_REFRESH_INTERVAL);
        }

        Arc::clone(&self.rows)
    }

    /// Проверяет, пора ли перечитать counters/snapshot и собрать новый текст.
    fn needs_refresh(&self, now: Instant) -> bool {
        if self.rows.is_empty() {
            return true;
        }

        match self.next_refresh_at {
            Some(next_refresh_at) => now >= next_refresh_at,
            None => true,
        }
    }
}

/// Визуальный тон строки telemetry panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TelemetryPanelRowTone {
    /// Обычная diagnostics-строка.
    Normal,

    /// Заголовок секции внутри virtualized списка.
    Heading,

    /// Успешное/здоровое состояние.
    Good,

    /// Пограничное состояние, которое стоит заметить.
    Warning,

    /// Ошибка или явно плохое состояние.
    Error,

    /// Пустая строка-разделитель.
    Spacer,
}

impl TelemetryPanelRowTone {
    /// Возвращает цвет текста, не меняя глобальный egui style.
    const fn color(self) -> egui::Color32 {
        match self {
            Self::Normal => egui::Color32::LIGHT_GRAY,
            Self::Heading => egui::Color32::LIGHT_BLUE,
            Self::Good => egui::Color32::GREEN,
            Self::Warning => egui::Color32::YELLOW,
            Self::Error => egui::Color32::RED,
            Self::Spacer => egui::Color32::TRANSPARENT,
        }
    }
}

/// Одна fixed-height строка virtualized telemetry panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TelemetryPanelRow {
    /// Уже отформатированный текст, чтобы не собирать строки каждый video frame.
    text: String,

    /// Цветовой смысл строки.
    tone: TelemetryPanelRowTone,
}

impl TelemetryPanelRow {
    /// Создаёт обычную строку diagnostics.
    fn normal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: TelemetryPanelRowTone::Normal,
        }
    }

    /// Создаёт строку-заголовок секции.
    fn heading(text: impl Into<String>) -> Self {
        Self {
            text: format!("[{}]", text.into()),
            tone: TelemetryPanelRowTone::Heading,
        }
    }

    /// Создаёт status-строку с явным цветовым тоном.
    fn status(text: impl Into<String>, tone: TelemetryPanelRowTone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }

    /// Создаёт пустую строку без отдельного egui separator widget.
    fn spacer() -> Self {
        Self {
            text: String::new(),
            tone: TelemetryPanelRowTone::Spacer,
        }
    }

    /// Дописывает строку (как отдельную text-line) в общий `LayoutJob` панели.
    ///
    /// Вся панель рисуется ОДНИМ виджетом-галеёй, а не Label-ом на строку: при живых
    /// telemetry-данных набор/позиции строк меняются каждый кадр, и сотни per-row
    /// виджетов роняли egui debug-warning «changed id between passes» (id одних и тех
    /// же позиций «съезжали»). Один widget id это полностью устраняет и дешевле по перфу.
    fn append_to_layout_job(&self, job: &mut egui::text::LayoutJob, font_id: egui::FontId) {
        job.append(
            &self.text,
            0.0,
            egui::TextFormat {
                font_id,
                color: self.tone.color(),
                ..Default::default()
            },
        );
        job.append("\n", 0.0, egui::TextFormat::default());
    }

    /// Возвращает текст строки для focused unit tests cache-а.
    #[cfg(test)]
    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

impl AppState {
    /// Рендерит правую диагностическую панель из заранее отформатированных строк.
    pub(super) fn render_telemetry_panel(ui: &mut egui::Ui, panel_rows: &[TelemetryPanelRow]) {
        let telemetry_frame =
            egui::Frame::NONE.fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160));
        egui::Panel::right("telemetry")
            .resizable(true)
            .default_size(280.0)
            .size_range(220.0..=520.0)
            .frame(telemetry_frame)
            .show_inside(ui, |ui| {
                ui.heading("Telemetry");
                ui.separator();

                let font_id = egui::TextStyle::Monospace.resolve(ui.style());
                let mut job = egui::text::LayoutJob::default();
                // Без переноса: длинные строки уходят за край и отсекаются clip rect-ом
                // панели (как прежний per-row Truncate).
                job.wrap.max_width = f32::INFINITY;
                for row in panel_rows {
                    row.append_to_layout_job(&mut job, font_id.clone());
                }

                egui::ScrollArea::vertical()
                    .id_salt("telemetry_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Вся диагностика — ОДИН widget (галея): стабильный id независимо
                        // от того, как меняется содержимое строк между кадрами.
                        ui.label(job);
                    });
            });
    }

    /// Собирает cached модель telemetry panel без доступа к renderer/player internals.
    pub(super) fn build_telemetry_panel_rows(
        panel_state: TelemetryPanelState<'_>,
    ) -> Arc<[TelemetryPanelRow]> {
        let mut panel_rows = Vec::with_capacity(TELEMETRY_PANEL_ROW_CAPACITY);

        Self::append_telemetry_summary_rows(&mut panel_rows, &panel_state);
        Self::append_media_info_rows(&mut panel_rows, &panel_state);
        Self::append_frame_server_rows(&mut panel_rows, &panel_state);

        panel_rows.into()
    }

    /// Маппит public playback state в стабильный telemetry label.
    fn playback_state_label_for_telemetry(playback_state: PlaybackState) -> &'static str {
        match playback_state {
            PlaybackState::Idle => "Idle",
            PlaybackState::Opening => "Opening",
            PlaybackState::Paused => "Paused",
            PlaybackState::Playing => "Playing",
            PlaybackState::Buffering => "Buffering",
            PlaybackState::Seeking => "Seeking",
            PlaybackState::Scrubbing => "Scrubbing",
            PlaybackState::Draining => "Draining",
            PlaybackState::Ended => "Ended",
            PlaybackState::Stopped => "Stopped",
            PlaybackState::Failed => "Failed",
        }
    }

    /// Добавляет верхнюю summary-секцию telemetry panel.
    fn append_telemetry_summary_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let telemetry = panel_state.telemetry;
        let fps = telemetry.current_fps();
        let frame_time = telemetry.last_frame_time_ms();
        let frames_presented_to_surface = telemetry.frames_presented_to_surface();
        let surface_dropped_frames = telemetry.surface_dropped_frames();
        let surface_drop_rate = telemetry.surface_drop_rate_percent();
        let frame_pacing_tone = if fps >= 55 {
            TelemetryPanelRowTone::Good
        } else if fps >= 30 {
            TelemetryPanelRowTone::Warning
        } else {
            TelemetryPanelRowTone::Error
        };
        let frame_pacing_text = if fps >= 55 {
            "Frame pacing: OK"
        } else if fps >= 30 {
            "Frame pacing: warning"
        } else {
            "Frame pacing: bad"
        };

        panel_rows.push(TelemetryPanelRow::normal(format!("FPS: {fps}")));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Frame time: {:.2} ms",
            frame_time as f64
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Swapchain"));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "frames_presented_to_surface: {frames_presented_to_surface}"
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "surface_dropped_frames: {surface_dropped_frames}"
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "surface_drop_rate: {surface_drop_rate:.2}%"
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Smoothness"));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback visible drops: {}",
            telemetry.playback_visible_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "repeated_frames: {}",
            telemetry.repeated_frames()
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Seek"));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Seek discard, expected: {}",
            telemetry.seek_discarded_frames()
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::status(
            frame_pacing_text,
            frame_pacing_tone,
        ));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback state: {}",
            Self::playback_state_label_for_telemetry(panel_state.player_snapshot.playback_state)
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Backend: {}",
            panel_state.backend_name
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Elapsed: {:.1}s",
            panel_state.start_time.elapsed().as_secs_f64()
        )));
    }

    /// Добавляет media/player diagnostics в cached строки telemetry panel.
    fn append_media_info_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let player_snapshot = panel_state.player_snapshot;
        let telemetry = panel_state.telemetry;

        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Media Info"));

        if player_snapshot.source_label.is_none() {
            panel_rows.push(TelemetryPanelRow::normal("No file loaded"));
            return;
        }

        for track in &player_snapshot.tracks {
            match track.kind {
                TrackKind::Video => {
                    panel_rows.push(TelemetryPanelRow::normal(format!(
                        "Video: {}",
                        track.codec_id
                    )));
                    if let Some(color_summary) = &track.video_color_summary {
                        panel_rows.push(TelemetryPanelRow::normal(format!(
                            "  Color: {color_summary}"
                        )));
                    }
                }
                TrackKind::Audio => {
                    panel_rows.push(TelemetryPanelRow::normal(format!(
                        "Audio: {}",
                        track.codec_id
                    )));
                }
            };
        }

        if let Some(duration) = player_snapshot.duration {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Duration: {}",
                timeline::format_seconds(Some(duration.as_secs_f64()))
            )));
        }

        if let Some(source_label) = &player_snapshot.media_title {
            panel_rows.push(TelemetryPanelRow::normal(format!("File: {source_label}")));
        }

        panel_rows.push(TelemetryPanelRow::spacer());
        Self::append_packet_rows(panel_rows, telemetry);
        Self::append_timeline_rows(panel_rows, panel_state);
        Self::append_audio_rows(panel_rows, player_snapshot);
        Self::append_video_rows(panel_rows, panel_state);
        Self::append_capability_rows(panel_rows, player_snapshot);
    }

    /// Добавляет packet counters shell telemetry.
    fn append_packet_rows(panel_rows: &mut Vec<TelemetryPanelRow>, telemetry: &Telemetry) {
        let total = telemetry.packets_read();
        let video_packets = telemetry.video_packets();
        let audio_packets = telemetry.audio_packets();

        panel_rows.push(TelemetryPanelRow::normal(format!("Packets: {total}")));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  Video: {video_packets}"
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  Audio: {audio_packets}"
        )));
    }

    /// Добавляет timeline строки, которые берутся из UI-state/snapshot boundary.
    fn append_timeline_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let playback_position_secs = panel_state
            .timeline_ui_state
            .display_position(&panel_state.player_snapshot.timeline)
            .as_duration()
            .as_secs_f64();

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback PTS: {}",
            timeline::format_seconds(Some(playback_position_secs))
        )));
        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Timeline: {}",
            timeline::format_seconds(Some(playback_position_secs))
        )));
    }

    /// Добавляет S29B frame-server diagnostics из player snapshot и app-owned boundary.
    fn append_frame_server_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let player_scrub = &panel_state.player_snapshot.diagnostics.frame_server_scrub;
        let app_frame_server = &panel_state.frame_server_diagnostics;
        let app_scrub = &app_frame_server.scrub;
        let player_prepared = player_scrub.prepared_frames;
        let app_hover_preview = app_frame_server.hover_preview;

        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Frame Server"));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS shared prepared branch: hover_preview_ready_borrow={} s17_promoted_branch={}",
            app_hover_preview.ready_count, player_prepared.ownership.promoted_to_seek_ownership
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS preview vs S17: preview_updates={} preview_ready={} promotion_hits={}",
            app_hover_preview.update_count,
            app_hover_preview.ready_count,
            player_scrub.working_set.promotion_hits
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS superseded prepared: demoted_recent_superseded={} demote_rejected={} released_without_demote={}",
            player_prepared.ownership.demoted_to_recent_superseded,
            player_prepared.ownership.demote_rejected,
            player_prepared.ownership.released_without_demote
        )));

        Self::append_frame_server_scrub_rows(panel_rows, "FS player", player_scrub);
        Self::append_frame_server_scrub_rows(panel_rows, "FS app", app_scrub);
        Self::append_frame_server_hover_preview_rows(panel_rows, panel_state);
        Self::append_frame_server_hover_leave_grace_rows(panel_rows, panel_state);
        Self::append_frame_server_network_rows(panel_rows, panel_state);
        Self::append_frame_server_live_scrub_rows(panel_rows, player_scrub);
    }

    /// Добавляет compact snapshot строки без Debug-dump-а всего diagnostics объекта.
    fn append_frame_server_scrub_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        prefix: &str,
        scrub: &ScrubDiagnosticsSnapshot,
    ) {
        let requests = scrub.requests;
        let outcomes = scrub.outcomes;
        let prepared = scrub.prepared_frames;
        let runway = prepared.video_runway;
        let ownership = prepared.ownership;
        let demote_rejections = ownership.demote_rejection_reasons;
        let working_set = scrub.working_set;
        let hover_admission = scrub.hover_prepare.admission;
        let hover_span = scrub.hover_prepare.dependency_span;
        let hover_incomplete = hover_span.incomplete_reasons;
        let hover_network = scrub.hover_prepare.network;

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} requests: seek={} live={} hover_preview={} hover_prepare={}",
            requests.accepted.seek_landing,
            requests.accepted.live_scrub,
            requests.accepted.hover_preview,
            requests.accepted.timeline_hover_prepare_window
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} outcomes: cold_progress={} exact_ready={} resume_pending={} audio_timeout={} audio_error={} cancelled={} stale={} timeout={} fatal={}",
            outcomes.cold_decode_in_progress,
            outcomes.exact_frame_ready,
            outcomes.audio_resume_pending,
            outcomes.audio_resume_timed_out,
            outcomes.audio_resume_failed,
            outcomes.cancelled,
            outcomes.stale_generation,
            outcomes.timed_out,
            outcomes.fatal
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} backpressure: demux_unsupported={} demux_unavailable={} decoder={} host_upload={} resource_busy={}",
            outcomes.demux_unsupported,
            outcomes.demux_unavailable,
            outcomes.decoder_backpressure,
            outcomes.host_upload_backpressure,
            outcomes.resource_busy
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} latency: demux_seek={} exact_decode={} packets_to_target={}",
            Self::duration_summary_for_telemetry(scrub.demux_seek_latency),
            Self::duration_summary_for_telemetry(scrub.decode_latency),
            Self::count_summary_for_telemetry(scrub.packets_from_decode_point_to_target)
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} prepared: hits={} resume_ready={} resume_runway_pending={} commit_gate_pending={} audio_gate_pending={} cold_exact_pending={}",
            prepared.prepared_frame_hits,
            prepared.resume_ready_prepared_hits,
            prepared.prepared_frame_resume_runway_pending,
            prepared.prepared_frame_commit_gate_pending,
            prepared.prepared_frame_audio_gate_pending,
            prepared.cold_exact_decode_pending
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} prepared pending reasons: frame_only={} continuation_missing={} runway={} commit_gate={} audio_gate={}",
            prepared.resume_pending_reasons.frame_only,
            prepared.resume_pending_reasons.continuation_missing,
            prepared.resume_pending_reasons.runway_pending,
            prepared.resume_pending_reasons.commit_gate_pending,
            prepared.resume_pending_reasons.audio_gate_pending
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} runway: pending={} repositioned={} post_target_packet={} displayable_queued={} next_almost_ready={} progress_only={} commit_ready={}",
            runway.pending,
            runway.repositioned,
            runway.post_target_packet_accepted,
            runway.displayable_frame_queued,
            runway.next_frame_almost_ready,
            runway.progress_only,
            runway.commit_ready
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} prepared ownership: promoted={} resume_ready_branch={} visual_override_resume_pending={} demoted_recent_superseded={} demote_rejected={} released_without_demote={} no_promoted_on_release={}",
            ownership.promoted_to_seek_ownership,
            ownership.promoted_resume_ready_branch,
            ownership.promoted_visual_override_resume_pending,
            ownership.demoted_to_recent_superseded,
            ownership.demote_rejected,
            ownership.released_without_demote,
            ownership.no_promoted_frame_on_release
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} demote rejections: cancel_reason={} promoted_key_not_current={} timing={} recent_disabled={}",
            demote_rejections.cancel_reason_does_not_allow_demote,
            demote_rejections.promoted_key_not_current,
            demote_rejections.timing_rejected,
            demote_rejections.recent_superseded_retention_disabled
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} working set: hit={} miss={} timing_reject={} evict={} promotion_hit={} promotion_miss={} pressure_recent={} pressure_primary={} pressure_miss={}",
            working_set.hits,
            working_set.misses,
            working_set.timing_rejections,
            working_set.evictions,
            working_set.promotion_hits,
            working_set.promotion_misses,
            working_set.released_recent_superseded,
            working_set.released_primary_byproduct,
            working_set.pressure_release_misses
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} hover admission: admitted={} no_op={} spare_slot={} replace_primary={} evict_byproduct={} provider_spare={} provider_pressure={} no_spare_slot={} live_suspended={}",
            hover_admission.admitted,
            hover_admission.no_op,
            hover_admission.use_spare_primary_slot,
            hover_admission.replace_existing_primary,
            hover_admission.evict_oldest_primary_byproduct,
            hover_admission.provider_spare_slot_available,
            hover_admission.provider_resource_pressure,
            hover_admission.no_spare_hover_slot,
            hover_admission.active_live_scrub_suspends_hover_prepare
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} hover span: resolved={} incomplete={} retarget={} extend={} restart={} superseded={}",
            hover_span.resolved,
            hover_span.incomplete,
            hover_span.same_span_retarget,
            hover_span.span_tail_extension,
            hover_span.span_restart,
            hover_span.span_superseded
        )));
        if let Some(progress) = hover_span.latest_progress {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "{prefix} hover span latest: packets={} frames={} post_target_drain={} prepared_targets={}",
                progress.packets_decoded_to_target,
                progress.frames_decoded_to_target,
                progress.post_target_reorder_drain_frames,
                progress.prepared_targets_produced
            )));
        }
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} hover incomplete: decode_not_wired={} resolver_not_wired={} seek_unsupported={} seek_unavailable={} resolve_failed={} network_opening={} network_throttled={} network_failed_no_retry={} source_unavailable={} stale_generation={} resource_pressure={} fatal={}",
            hover_incomplete.decode_execution_not_wired,
            hover_incomplete.resolver_not_wired,
            hover_incomplete.seek_unsupported,
            hover_incomplete.seek_unavailable,
            hover_incomplete.resolve_failed,
            hover_incomplete.network_opening,
            hover_incomplete.network_throttled,
            hover_incomplete.network_failed_no_retry,
            hover_incomplete.source_unavailable,
            hover_incomplete.stale_generation,
            hover_incomplete.resource_pressure,
            hover_incomplete.fatal
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "{prefix} hover network counters: opening={} opened={} throttled={} failed_held={} zero_throttle_no_delay={} latest_only_replaced={} stale_late_ignored={} throttle_delay={} latest_state={}",
            hover_network.opening,
            hover_network.opened,
            hover_network.throttled,
            hover_network.failed_target_held,
            hover_network.zero_throttle_no_delay,
            hover_network.latest_only_replaced_in_flight,
            hover_network.stale_late_result_ignored,
            Self::duration_summary_for_telemetry(hover_network.throttle_delay),
            Self::hover_network_state_label(hover_network.latest_state)
        )));
    }

    /// Добавляет app-owned visual HoverPreview строки: visual suppression != invisible prepare.
    fn append_frame_server_hover_preview_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let hover_preview = panel_state.frame_server_diagnostics.hover_preview;
        let invisible_prepare_requests = panel_state
            .frame_server_diagnostics
            .scrub
            .requests
            .accepted
            .timeline_hover_prepare_window;

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS hover preview visual: enabled={} disabled_visual_only={} invisible_prepare_requests={}",
            hover_preview.visual_preview_enabled,
            hover_preview.disabled_by_config_count,
            invisible_prepare_requests
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS hover preview borrow: updates={} ready={} loading={} busy_kept={} unavailable={} clear={} latest_update={} latest_load={}",
            hover_preview.update_count,
            hover_preview.ready_count,
            hover_preview.loading_count,
            hover_preview.busy_kept_last_ready_count,
            hover_preview.unavailable_count,
            hover_preview.clear_count,
            Self::hover_preview_update_label(hover_preview.latest_update_outcome),
            Self::hover_preview_load_state_label(hover_preview.latest_load_state)
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS hover preview render: ready={} loading_preview_only={}",
            hover_preview.render_state.ready, hover_preview.render_state.loading_preview_only
        )));
    }

    /// Добавляет UX lifetime grace diagnostics, не смешивая их с decode span coverage.
    fn append_frame_server_hover_leave_grace_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let leave_grace = panel_state.frame_server_diagnostics.hover_leave_grace;
        let latest_release_outcome = leave_grace.latest_release_outcome;
        let latest_primary_release = latest_release_outcome
            .map(|outcome| outcome.primary_entries_released())
            .unwrap_or(0);
        let latest_recent_release = latest_release_outcome
            .map(|outcome| outcome.recent_superseded_entries_released())
            .unwrap_or(0);

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS hover leave grace: configured={} pending={} started={} reentered={} zero_grace_immediate={} expired={} non_timeline_cancel={} latest_reason={}",
            Self::duration_for_telemetry(leave_grace.configured_grace),
            leave_grace.pending,
            leave_grace.started_count,
            leave_grace.reentered_before_expiry_count,
            leave_grace.zero_grace_immediate_release_count,
            leave_grace.expired_release_count,
            leave_grace.non_timeline_cancel_release_count,
            Self::hover_leave_grace_reason_label(leave_grace.latest_release_reason)
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS recent-superseded lifetime: latest_release_primary={} latest_release_recent={} pressure_recent_releases={} demoted_recent_superseded={}",
            latest_primary_release,
            latest_recent_release,
            panel_state
                .player_snapshot
                .diagnostics
                .frame_server_scrub
                .working_set
                .released_recent_superseded,
            panel_state
                .player_snapshot
                .diagnostics
                .frame_server_scrub
                .prepared_frames
                .ownership
                .demoted_to_recent_superseded
        )));
    }

    /// Добавляет network hover open diagnostics из app-owned controller snapshot.
    fn append_frame_server_network_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let network_open = panel_state.frame_server_diagnostics.network_open;

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS network open: throttle={} generation={} in_flight={} failed_target_held={} zero_throttle_no_delay={} latest_only_replaced={} stale_late_ignored={} throttle_delay_count={} latest_delay={}",
            Self::duration_for_telemetry(network_open.inter_start_throttle),
            network_open.source_generation,
            network_open.in_flight_count,
            network_open.failed_target_held,
            network_open.zero_throttle_no_delay_count,
            network_open.latest_only_replaced_in_flight_count,
            network_open.stale_late_result_ignored_count,
            network_open.throttle_delay_count,
            Self::optional_duration_for_telemetry(network_open.latest_throttle_delay)
        )));
    }

    /// Добавляет latest-only live scrub settings diagnostics без user-facing hint/status.
    fn append_frame_server_live_scrub_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_scrub: &ScrubDiagnosticsSnapshot,
    ) {
        let Some(live_scrub) = player_scrub.latest_live_scrub else {
            panel_rows.push(TelemetryPanelRow::normal(
                "FS live scrub: inactive deferred_changes=0",
            ));
            return;
        };

        let settings = live_scrub.settings_snapshot;
        let latest_change = live_scrub
            .latest_deferred_live_scrub_settings_change
            .map(|change| {
                format!(
                    "{} / {}hz -> {} / {}hz",
                    Self::live_scrub_decode_mode_label(change.old_snapshot.decode_mode),
                    change.old_snapshot.max_hz,
                    Self::live_scrub_decode_mode_label(change.new_snapshot.decode_mode),
                    change.new_snapshot.max_hz
                )
            })
            .unwrap_or_else(|| "none".to_string());

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "FS live scrub: mode={} max_hz={} deferred_changes={} latest_change={} throttle_skips={}",
            Self::live_scrub_decode_mode_label(settings.decode_mode),
            settings.max_hz,
            live_scrub.deferred_live_scrub_settings_change_count,
            latest_change,
            live_scrub.throttled_latest_skip_count
        )));
    }

    /// Форматирует duration в миллисекундах для cached telemetry row.
    fn duration_for_telemetry(duration: Duration) -> String {
        format!("{:.2}ms", duration.as_secs_f64() * 1000.0)
    }

    /// Форматирует optional duration без временных event histories.
    fn optional_duration_for_telemetry(duration: Option<Duration>) -> String {
        duration
            .map(Self::duration_for_telemetry)
            .unwrap_or_else(|| "none".to_string())
    }

    /// Форматирует bounded duration summary в одну compact строку.
    fn duration_summary_for_telemetry(summary: DurationSummary) -> String {
        if summary.is_empty() {
            return "samples=0".to_string();
        }

        format!(
            "samples={} min={} max={}",
            summary.samples,
            Self::optional_duration_for_telemetry(summary.min),
            Self::optional_duration_for_telemetry(summary.max)
        )
    }

    /// Форматирует bounded numeric summary в одну compact строку.
    fn count_summary_for_telemetry(summary: CountSummary) -> String {
        if summary.is_empty() {
            return "samples=0".to_string();
        }

        format!(
            "samples={} min={} max={} total={}",
            summary.samples,
            summary.min.unwrap_or(0),
            summary.max.unwrap_or(0),
            summary.total
        )
    }

    /// Стабильный label для live-scrub decode mode без Debug formatting.
    const fn live_scrub_decode_mode_label(mode: LiveScrubDecodeMode) -> &'static str {
        match mode {
            LiveScrubDecodeMode::ThrottledLatest => "throttled_latest",
            LiveScrubDecodeMode::EveryDragEvent => "every_drag_event",
        }
    }

    /// Стабильный label для latest network hover state.
    const fn hover_network_state_label(state: Option<ScrubHoverNetworkState>) -> &'static str {
        match state {
            Some(ScrubHoverNetworkState::NonNetworkSource) => "non_network_source",
            Some(ScrubHoverNetworkState::Opening) => "opening",
            Some(ScrubHoverNetworkState::Opened) => "opened",
            Some(ScrubHoverNetworkState::Throttled) => "throttled",
            Some(ScrubHoverNetworkState::MissingActiveSource) => "missing_active_source",
            Some(ScrubHoverNetworkState::Unsupported) => "unsupported",
            Some(ScrubHoverNetworkState::OpenFailed) => "open_failed",
            Some(ScrubHoverNetworkState::Disconnected) => "disconnected",
            Some(ScrubHoverNetworkState::FailedTargetHeld) => "failed_target_held",
            None => "none",
        }
    }

    /// Стабильный label для visual preview materialization outcome.
    const fn hover_preview_update_label(
        outcome: Option<TimelineHoverPreviewUpdateOutcome>,
    ) -> &'static str {
        match outcome {
            Some(TimelineHoverPreviewUpdateOutcome::Loading) => "loading",
            Some(TimelineHoverPreviewUpdateOutcome::Ready) => "ready",
            Some(TimelineHoverPreviewUpdateOutcome::BusyKeptLastReady) => "busy_kept_last_ready",
            Some(TimelineHoverPreviewUpdateOutcome::BusyEmpty) => "busy_empty",
            Some(TimelineHoverPreviewUpdateOutcome::MissingMaterializer) => "missing_materializer",
            Some(TimelineHoverPreviewUpdateOutcome::WorkingSetMiss) => "working_set_miss",
            Some(TimelineHoverPreviewUpdateOutcome::TimingRejected) => "timing_rejected",
            Some(TimelineHoverPreviewUpdateOutcome::Missing) => "missing",
            Some(TimelineHoverPreviewUpdateOutcome::Unsupported) => "unsupported",
            Some(TimelineHoverPreviewUpdateOutcome::Error) => "error",
            None => "none",
        }
    }

    /// Стабильный label для preview-only loading state.
    const fn hover_preview_load_state_label(state: TimelineHoverPreviewLoadState) -> &'static str {
        match state {
            TimelineHoverPreviewLoadState::Idle => "idle",
            TimelineHoverPreviewLoadState::NetworkOpening { .. } => "network_opening",
        }
    }

    /// Стабильный label для причины release-а hover leave grace.
    const fn hover_leave_grace_reason_label(
        reason: Option<TimelineHoverLeaveGraceReleaseReason>,
    ) -> &'static str {
        match reason {
            Some(TimelineHoverLeaveGraceReleaseReason::ImmediateTimelineLeave) => {
                "immediate_timeline_leave"
            }
            Some(TimelineHoverLeaveGraceReleaseReason::LeaveGraceExpired) => "leave_grace_expired",
            Some(TimelineHoverLeaveGraceReleaseReason::NonTimelineAction) => "non_timeline_action",
            None => "none",
        }
    }

    /// Добавляет audio diagnostics из `PlayerSnapshot`.
    fn append_audio_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(buffer_level) = player_snapshot.audio_buffer.level {
            let buffer_ms = buffer_level.as_secs_f64() * 1000.0;
            let buffer_tone = if buffer_ms > 10.0 {
                TelemetryPanelRowTone::Good
            } else if buffer_ms > 0.0 {
                TelemetryPanelRowTone::Warning
            } else {
                TelemetryPanelRowTone::Error
            };

            panel_rows.push(TelemetryPanelRow::status(
                format!("Audio buf: {buffer_ms:.1}ms"),
                buffer_tone,
            ));
        }

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Audio underruns: {}",
            player_snapshot.audio_buffer.underruns
        )));
    }

    /// Добавляет video diagnostics без чтения внутренних очередей player-core напрямую.
    fn append_video_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let player_snapshot = panel_state.player_snapshot;
        let telemetry = panel_state.telemetry;
        let diagnostics = &player_snapshot.diagnostics;
        let publish_pressure = diagnostics.decoder_frame_publish_pressure;

        panel_rows.push(TelemetryPanelRow::spacer());
        panel_rows.push(TelemetryPanelRow::heading("Video"));

        if let Some(backend_name) = &player_snapshot.active_backend.name {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Backend: {backend_name}"
            )));
        }
        if let Some(active_color_path_text) =
            Self::active_color_path_text_for_ui(panel_state.render_diagnostics)
        {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Color: {active_color_path_text}"
            )));
        }
        if let Some(reference_defaults_text) =
            Self::hdr_reference_defaults_text_for_ui(panel_state.render_diagnostics)
        {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "HDR metadata: {reference_defaults_text}"
            )));
        }
        if panel_state.render_diagnostics.video_draw_rect_count > 0 {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "video_draw_rect_count: {}",
                panel_state.render_diagnostics.video_draw_rect_count
            )));
        }

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Decoded: {}",
            telemetry.video_frames_decoded()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "video_frames_presented: {}",
            player_snapshot.frame_counters.presented
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback visible drops: {}",
            telemetry.playback_visible_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Playback drops incl pause: {}",
            player_snapshot.frame_counters.dropped
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_late_drops: {}",
            telemetry.video_late_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_queue_drops: {}",
            telemetry.video_queue_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_pause_drops: {}",
            telemetry.video_pause_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_decoder_starvation: {}",
            telemetry.video_decoder_starvation()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  video_other_drops: {}",
            telemetry.video_other_drops()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Seek discard, expected: {}",
            telemetry.seek_discarded_frames()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  seek_preroll_discarded: {}",
            telemetry.seek_preroll_discarded()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  stale_generation_discarded: {}",
            telemetry.stale_generation_discarded()
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core seek/pre-roll diagnostics: {}",
            diagnostics.drops.seek_preroll
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core stale-generation diagnostics: {}",
            diagnostics.drops.stale_generation
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core render acquisition timeout: {}",
            diagnostics.drops.render_acquisition_timeout
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "  core decoder starvation diagnostics: {}",
            diagnostics.drops.decoder_starvation
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "repeated_frames: {}",
            player_snapshot.frame_counters.repeated
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Worker repeats: {}",
            diagnostics.repeated_video_frames
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Decoder publish pressure: {}",
            publish_pressure.frame_publish_channel_full_count
        )));

        Self::append_decoder_control_rows(panel_rows, player_snapshot);

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Publish max: {:.3}ms",
            publish_pressure
                .max_decoded_frame_publish_latency
                .as_secs_f64()
                * 1000.0
        )));
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Frame dur: {:.2}ms",
            panel_state.frame_duration_estimate_ms
        )));

        Self::append_worker_wakeup_rows(panel_rows, player_snapshot);

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Queue: {}",
            player_snapshot.queues.decoded_video_frames
        )));

        Self::append_texture_pool_rows(panel_rows, player_snapshot);
        Self::append_latency_rows(panel_rows, player_snapshot);

        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Pipeline pauses: {}",
            diagnostics.pauses.total
        )));
    }

    /// Добавляет decoder control-channel строки, если backend их опубликовал.
    fn append_decoder_control_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(control_pressure) = player_snapshot.diagnostics.queues.decoder_control_channel {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Decoder control queue: {}/{}",
                control_pressure.control_channel_len, control_pressure.control_channel_capacity
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Control full/fail: full={} release={} flush={}",
                control_pressure.control_channel_full_count,
                control_pressure.release_control_send_fail_count,
                control_pressure.flush_control_send_fail_count
            )));
        }
    }

    /// Добавляет wakeup/scheduler строки из player diagnostics snapshot.
    fn append_worker_wakeup_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        let worker_wakeup = player_snapshot.diagnostics.worker_wakeup;

        if let Some(reason) = worker_wakeup.reason {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Wake: {}",
                reason.metric_name()
            )));
        }
        if let Some(planned_delay) = worker_wakeup.planned_delay {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Wake delay: {:.2}ms",
                planned_delay.as_secs_f64() * 1000.0
            )));
        }
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Wake late: {:.2}ms",
            worker_wakeup.tick_late_by.as_secs_f64() * 1000.0
        )));
        if let Some(frame_timing) = worker_wakeup.frame_timing {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "PTS-target: {:.2}ms",
                frame_timing.front_frame_delta_from_target_us as f64 / 1000.0
            )));
        }
    }

    /// Добавляет texture-pool строки из backend-neutral diagnostics snapshot.
    fn append_texture_pool_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(texture_pool) = player_snapshot.active_backend.texture_pool {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Textures: {}/{}",
                texture_pool.in_use, texture_pool.capacity
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Texture slots: {}",
                texture_pool.slots
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Texture free: {}",
                texture_pool.free_surfaces
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Texture wait: gpu={} decoder={}",
                texture_pool.waiting_gpu_completion, texture_pool.waiting_decoder_reuse
            )));
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Imports: create={} reuse={} replace={} fail={}",
                texture_pool.imports_created,
                texture_pool.imports_reused,
                texture_pool.imports_replaced,
                texture_pool.import_failures
            )));
        }
        if let Some(memory_path) = player_snapshot.diagnostics.zero_copy_memory_path {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Memory path: {memory_path}"
            )));
        }
    }

    /// Добавляет worst-latency строки, сохраняя прежний набор diagnostics.
    fn append_latency_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        let worst_import = player_snapshot
            .diagnostics
            .worst_latencies
            .dma_buf_import
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_import_ms) = worst_import {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Worst import: {worst_import_ms:.2}ms"
            )));
        }

        let worst_sync = player_snapshot
            .diagnostics
            .worst_latencies
            .hardware_sync
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_sync_ms) = worst_sync {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Worst sync: {worst_sync_ms:.2}ms"
            )));
        }

        let worst_render_acquire = player_snapshot
            .diagnostics
            .worst_latencies
            .render_acquire
            .worst
            .map(|sample| sample.duration.as_secs_f64() * 1000.0);
        if let Some(worst_render_acquire_ms) = worst_render_acquire {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Worst acquire: {worst_render_acquire_ms:.3}ms"
            )));
        }

        let render_resource_lock_wait = player_snapshot
            .diagnostics
            .worst_latencies
            .render_resource_lock_wait;
        if render_resource_lock_wait.samples > 0 {
            let average_lock_wait_ms = render_resource_lock_wait.average.as_secs_f64() * 1000.0;
            let worst_lock_wait_ms = render_resource_lock_wait
                .worst
                .map(|sample| sample.duration.as_secs_f64() * 1000.0)
                .unwrap_or(0.0);
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Render resource lock: count={} avg={average_lock_wait_ms:.3}ms max={worst_lock_wait_ms:.3}ms",
                render_resource_lock_wait.samples
            )));
        }
        if player_snapshot.diagnostics.render_resource_lock_busy_count > 0 {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Render resource busy: count={} reuse={}",
                player_snapshot.diagnostics.render_resource_lock_busy_count,
                player_snapshot
                    .diagnostics
                    .render_resource_previous_frame_reuse_count
            )));
        }
    }

    /// Добавляет capability summary построчно, чтобы virtualization могла скрыть offscreen строки.
    fn append_capability_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        player_snapshot: &PlayerSnapshot,
    ) {
        if let Some(capability_summary) = &player_snapshot.capability_summary {
            panel_rows.push(TelemetryPanelRow::spacer());
            panel_rows.push(TelemetryPanelRow::heading("Capabilities"));
            for line in capability_summary.lines() {
                panel_rows.push(TelemetryPanelRow::normal(line));
            }
        }
    }

    /// Формирует UI-строку active color path из renderer-neutral diagnostics.
    pub(super) fn active_color_path_text_for_ui(
        render_diagnostics: &RenderDiagnostics,
    ) -> Option<String> {
        render_diagnostics.active_color_path_text()
    }

    /// Формирует UI-строку source markers optional HDR metadata.
    pub(super) fn hdr_reference_defaults_text_for_ui(
        render_diagnostics: &RenderDiagnostics,
    ) -> Option<String> {
        render_diagnostics.hdr_reference_defaults_text()
    }
}
