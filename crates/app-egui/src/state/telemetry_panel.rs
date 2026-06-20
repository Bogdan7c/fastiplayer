use super::*;

/// Частота обновления тяжёлого текста telemetry panel.
///
/// Видео всё равно перерисовывает egui каждый кадр, но diagnostics-тексту достаточно
/// 4 Hz, чтобы не конкурировать с 60 fps video pacing.
pub(super) const TELEMETRY_PANEL_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

/// Начальная вместимость строк telemetry panel, чтобы refresh не делал лишних realloc.
pub(super) const TELEMETRY_PANEL_ROW_CAPACITY: usize = 96;

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

        panel_rows.into()
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
