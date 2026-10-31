//! Блок `Page` в начале панели Info окна браузерного режима (04c).
//!
//! Вынесен из `telemetry_panel.rs` (legacy-файл, растить нельзя). Домен и заголовок
//! страницы-источника живут только в памяти окна (инвариант 7) и сюда приходят готовой
//! подписью; обычное окно подписи не имеет и блока не показывает.

use super::*;
use crate::page_label::PageLabel;

/// Что показывать вместо заголовка у страницы без `<title>`.
const NO_TITLE_LABEL: &str = "(none)";

/// Строки блока `Page`: заголовок секции, `Site: …`, `Title: …` и пустая строка после.
/// Без подписи страницы — пусто.
pub(super) fn page_rows(page: Option<&PageLabel>) -> Vec<TelemetryPanelRow> {
    let Some(page) = page else {
        return Vec::new();
    };
    let title = if page.title().trim().is_empty() {
        NO_TITLE_LABEL
    } else {
        page.title()
    };
    vec![
        TelemetryPanelRow::heading("Page"),
        TelemetryPanelRow::normal(format!("Site: {}", page.domain())),
        TelemetryPanelRow::normal(format!("Title: {title}")),
        TelemetryPanelRow::spacer(),
    ]
}

impl AppState {
    /// Добавляет блок `Page` в начало панели (только если окну назвали страницу).
    pub(super) fn append_page_rows(
        panel_rows: &mut Vec<TelemetryPanelRow>,
        panel_state: &TelemetryPanelState<'_>,
    ) {
        panel_rows.extend(page_rows(panel_state.page));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(rows: &[TelemetryPanelRow]) -> Vec<String> {
        rows.iter().map(|row| row.text().to_owned()).collect()
    }

    /// Блок есть у браузерного окна и называет домен и заголовок.
    #[test]
    fn the_block_names_the_site_and_the_title() {
        let rows = page_rows(Some(&PageLabel::new("Cats", "video.example")));
        assert_eq!(
            texts(&rows),
            vec!["[Page]", "Site: video.example", "Title: Cats", ""]
        );
    }

    /// Страница без заголовка — понятная пометка, а не пустое поле.
    #[test]
    fn a_page_without_a_title_says_so() {
        let rows = page_rows(Some(&PageLabel::new("", "video.example")));
        assert!(texts(&rows).contains(&"Title: (none)".to_owned()));
    }

    /// Обычное окно подписи не имеет: блока нет вовсе.
    #[test]
    fn the_standard_window_has_no_page_block() {
        assert!(page_rows(None).is_empty());
    }

    /// Блок стоит ПЕРВЫМ в собранной панели, перед сводкой телеметрии.
    #[test]
    fn the_block_leads_the_whole_panel() {
        let player_snapshot = PlayerSnapshot::empty();
        let telemetry = Telemetry::new();
        let render_diagnostics = RenderDiagnostics::default();
        let timeline_ui_state = TimelineUiState::default();
        let page = PageLabel::new("Cats", "video.example");
        let panel_rows = AppState::build_telemetry_panel_rows(TelemetryPanelState {
            player_snapshot: &player_snapshot,
            frame_counters: FrameCounters::default(),
            telemetry: &telemetry,
            render_diagnostics: &render_diagnostics,
            timeline_ui_state: &timeline_ui_state,
            renderer_report: None,
            page: Some(&page),
            start_time: Instant::now(),
            frame_duration_estimate_ms: 16.67,
        });
        let first_four: Vec<String> = panel_rows
            .iter()
            .take(4)
            .map(|row| row.text().to_owned())
            .collect();
        assert_eq!(
            first_four,
            vec!["[Page]", "Site: video.example", "Title: Cats", ""]
        );
        assert!(panel_rows.len() > 4);
    }
}
