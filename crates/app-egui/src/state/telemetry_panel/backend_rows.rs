//! Строки панели телеметрии про backend декодера и активный рендер.
//!
//! Вынесено из `telemetry_panel.rs` (legacy-файл, растить нельзя). Строка `Renderer:` берётся
//! из нейтрального отчёта выбора рендера при старте (сессия 09 плана gles2-renderer), а не из
//! типа рендера: панель не знает, какой именно рендер создан.

use super::*;

/// Что показывать вместо имени backend-а, когда плеер ещё не выбрал его (синтетический тест).
const NO_ACTIVE_BACKEND_LABEL: &str = "Synthetic (test)";

impl AppState {
    /// Маппит public playback state в стабильный telemetry label.
    pub(super) fn playback_state_label_for_telemetry(
        playback_state: PlaybackState,
    ) -> &'static str {
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

    /// Добавляет `Backend: …` и, если отчёт выбора известен, `Renderer: <семейство> — <причина>`.
    pub(super) fn append_backend_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        let backend_name = panel_state
            .player_snapshot
            .active_backend
            .name
            .as_deref()
            .unwrap_or(NO_ACTIVE_BACKEND_LABEL);
        panel_rows.push(TelemetryPanelRow::normal(format!(
            "Backend: {backend_name}"
        )));
        if let Some(renderer_report) = panel_state.renderer_report {
            panel_rows.push(TelemetryPanelRow::normal(format!(
                "Renderer: {}",
                renderer_report.telemetry_text()
            )));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_backend::RendererStartupReport;
    use render_shell_api::RendererFamily;

    /// Строит строки панели для снимка плеера и (необязательного) отчёта выбора рендера.
    fn rows_for(
        player_snapshot: &PlayerSnapshot,
        renderer_report: Option<&RendererStartupReport>,
    ) -> Vec<String> {
        let telemetry = Telemetry::new();
        let render_diagnostics = RenderDiagnostics::default();
        let timeline_ui_state = TimelineUiState::default();
        let panel_rows = AppState::build_telemetry_panel_rows(TelemetryPanelState {
            player_snapshot,
            frame_counters: FrameCounters::default(),
            telemetry: &telemetry,
            render_diagnostics: &render_diagnostics,
            timeline_ui_state: &timeline_ui_state,
            renderer_report,
            start_time: Instant::now(),
            frame_duration_estimate_ms: 16.67,
        });
        panel_rows.iter().map(|row| row.text().to_owned()).collect()
    }

    fn report_without_vulkan() -> RendererStartupReport {
        RendererStartupReport::new(
            RendererFamily::Gles2,
            crate::render_backend::SelectionReason::AutoSkippedUnusable {
                skipped: vec![crate::render_backend::CandidateProbe {
                    family: RendererFamily::WgpuVulkan,
                    probe: render_shell_api::RendererProbe::Unusable {
                        reason: render_shell_api::RendererUnusableReason::NoHardwareAdapter,
                    },
                }],
            },
        )
    }

    /// Панель показывает семейство и причину выбора рендера строкой из отчёта.
    #[test]
    fn panel_shows_the_renderer_family_and_why_it_was_chosen() {
        let rows = rows_for(&PlayerSnapshot::empty(), Some(&report_without_vulkan()));

        assert!(
            rows.contains(&"Renderer: OpenGL ES 2.0 — Auto: no hardware Vulkan adapter".to_owned())
        );
    }

    /// Без отчёта (старые пути, тесты) строки рендера нет, а строка backend-а на месте.
    #[test]
    fn panel_has_no_renderer_row_without_a_report_and_keeps_the_backend_row() {
        let rows = rows_for(&PlayerSnapshot::empty(), None);

        assert!(!rows.iter().any(|row| row.starts_with("Renderer:")));
        assert!(rows.contains(&"Backend: Synthetic (test)".to_owned()));
    }

    /// Имя backend-а плеера по-прежнему идёт из снимка плеера.
    #[test]
    fn panel_backend_row_uses_the_active_player_backend_name() {
        let mut snapshot = PlayerSnapshot::empty();
        snapshot.active_backend.name = Some("VA-API".to_owned());

        let rows = rows_for(&snapshot, None);

        assert!(rows.contains(&"Backend: VA-API".to_owned()));
    }
}
