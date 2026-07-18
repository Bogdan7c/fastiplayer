//! Общий 32-point header всех сменяемых секций sidebar.
//!
//! Подмодуль остаётся частью sidebar owner-а: он резервирует vertical geometry,
//! строит Undo hit-area по window-chrome grid и не владеет Panel/resize state.

use egui::{Button, RichText, Ui};

use crate::settings_ui::SettingsUiAction;
use crate::state::SidebarSection;
use crate::ui::playlist;
use crate::ui::skin::PlaylistHeaderUndoStyle;
use crate::ui::window_chrome::WindowChromeEdgeAlignment;

use super::SidebarRenderContext;

/// Единая высота Playlist/Settings/URL/Info header в логических points.
const SIDEBAR_HEADER_HEIGHT_POINTS: f32 = 32.0;

/// Результат headless-testable chrome layout внутри точного header rect.
struct SidebarHeaderChromeOutput {
    /// Создал ли крестик intent закрытия.
    close_clicked: bool,
    /// Фактический rect heading-текста.
    title_rect: egui::Rect,
    /// Фактический rect крестика.
    close_rect: egui::Rect,
}

/// Рисует заголовок конкретной секции, сохраняя её close semantics.
pub(super) fn show(ui: &mut Ui, section: SidebarSection, context: &mut SidebarRenderContext<'_>) {
    let header_rect = allocate_sidebar_header_rect(ui);
    let chrome_output = render_header_chrome(
        ui,
        header_rect,
        sidebar_section_title(section),
        sidebar_close_tooltip(section),
    );
    // Явные assertions закрепляют вертикальное центрирование реального egui layout.
    debug_assert!((chrome_output.title_rect.center().y - header_rect.center().y).abs() < 0.1);
    debug_assert!((chrome_output.close_rect.center().y - header_rect.center().y).abs() < 0.1);

    if section == SidebarSection::Playlist {
        let undo_rect = playlist_header_undo_rect(
            header_rect,
            context.window_chrome_edge_alignment,
            &context.playlist_header_undo_style,
        );
        playlist::show_header_undo(
            ui,
            undo_rect,
            context.playlist_undo,
            &context.playlist_header_undo_style,
            context.visibility_motion,
            context.playlist_output,
        );
    }

    if chrome_output.close_clicked {
        // Settings X остаётся explicit Cancel/rollback.
        if section == SidebarSection::Settings {
            context
                .settings_actions
                .push(settings_sidebar_close_action());
        }
        // Остальные X только скрывают общий host.
        *context.close_requested = true;
    }
}

/// Резервирует единственный точный header rect в вертикальном flow sidebar.
fn allocate_sidebar_header_rect(ui: &mut Ui) -> egui::Rect {
    let header_size = egui::vec2(ui.available_width().max(0.0), SIDEBAR_HEADER_HEIGHT_POINTS);
    ui.allocate_exact_size(header_size, egui::Sense::hover()).0
}

/// Рисует heading и крестик внутри уже зарезервированного rect.
fn render_header_chrome(
    ui: &mut Ui,
    header_rect: egui::Rect,
    title: &str,
    close_tooltip: &str,
) -> SidebarHeaderChromeOutput {
    let mut header_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("sidebar_header_chrome")
            .max_rect(header_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    header_ui.set_clip_rect(header_rect.intersect(ui.clip_rect()));
    let title_response = header_ui.label(RichText::new(title).heading());
    let close_response = header_ui
        .with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |header_ui| {
                header_ui
                    .add(Button::new(RichText::new("×").strong()))
                    .on_hover_text(close_tooltip)
            },
        )
        .inner;

    SidebarHeaderChromeOutput {
        close_clicked: close_response.clicked(),
        title_rect: title_response.rect,
        close_rect: close_response.rect,
    }
}

/// Возвращает локализованный heading без знания о render branch.
const fn sidebar_section_title(section: SidebarSection) -> &'static str {
    match section {
        SidebarSection::Playlist => "Плейлист",
        SidebarSection::Settings => "Настройки",
        SidebarSection::Url => "URL",
        SidebarSection::Info => "Информация",
    }
}

/// Settings сохраняет rollback-обещание, остальные секции только скрываются.
const fn sidebar_close_tooltip(section: SidebarSection) -> &'static str {
    match section {
        SidebarSection::Settings => "Отменить изменения и закрыть настройки",
        SidebarSection::Playlist | SidebarSection::Url | SidebarSection::Info => "Закрыть панель",
    }
}

/// Строит app-owned Undo hit-area по typed URL rect общей window-chrome сетки.
fn playlist_header_undo_rect(
    header_rect: egui::Rect,
    edge_alignment: WindowChromeEdgeAlignment,
    style: &PlaylistHeaderUndoStyle,
) -> egui::Rect {
    let url_button_rect =
        edge_alignment.sidebar_section_button_rect(header_rect, SidebarSection::Url);
    let hit_area_size = style
        .hit_area_size
        .max(0.0)
        .min(header_rect.width().min(header_rect.height()).max(0.0));
    egui::Rect::from_center_size(
        egui::pos2(url_button_rect.center().x, header_rect.center().y),
        egui::Vec2::splat(hit_area_size),
    )
}

/// Типизированно сохраняет прежнюю Settings Cancel semantics.
#[must_use]
const fn settings_sidebar_close_action() -> SettingsUiAction {
    SettingsUiAction::Cancel
}

#[cfg(test)]
mod tests {
    use egui::RawInput;

    use super::*;
    use crate::ui::skin::{MinimalSkin, PlayerSkin};

    #[test]
    fn settings_sidebar_close_maps_to_cancel() {
        assert_eq!(settings_sidebar_close_action(), SettingsUiAction::Cancel);
    }

    /// Все секции проходят через один 32-point header и одинаковый vertical flow.
    #[test]
    fn all_section_headers_have_equal_height_centering_and_content_offset() {
        let sections = [
            SidebarSection::Playlist,
            SidebarSection::Settings,
            SidebarSection::Url,
            SidebarSection::Info,
        ];
        let mut content_offsets = Vec::new();

        for section in sections {
            let context = egui::Context::default();
            let input = RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 180.0),
                )),
                ..RawInput::default()
            };
            let mut measured = None;
            let _ = context.run_ui(input, |ui| {
                ui.set_width(420.0);
                let header_rect = allocate_sidebar_header_rect(ui);
                let chrome = render_header_chrome(
                    ui,
                    header_rect,
                    sidebar_section_title(section),
                    sidebar_close_tooltip(section),
                );
                ui.separator();
                measured = Some((
                    header_rect,
                    chrome.title_rect,
                    chrome.close_rect,
                    ui.available_rect_before_wrap().top(),
                ));
            });
            let (header_rect, title_rect, close_rect, content_top) =
                measured.expect("headless header should be measured");

            assert_eq!(header_rect.height(), SIDEBAR_HEADER_HEIGHT_POINTS);
            assert!((title_rect.center().y - header_rect.center().y).abs() < 0.1);
            assert!((close_rect.center().y - header_rect.center().y).abs() < 0.1);
            content_offsets.push(content_top - header_rect.top());
        }

        assert!(
            content_offsets
                .windows(2)
                .all(|pair| (pair[0] - pair[1]).abs() < f32::EPSILON)
        );
    }

    /// Undo совпадает с typed URL axis на всех ширинах и движется вместе с header.
    #[test]
    fn playlist_header_undo_tracks_url_axis_across_widths_and_transitions() {
        let controls_style = MinimalSkin.controls_style();
        let edge_alignment = WindowChromeEdgeAlignment::from_controls_style(controls_style);
        let undo_style = MinimalSkin.playlist_header_undo_style();

        for width in [350.0, 420.0, 600.0] {
            let header_rect = egui::Rect::from_min_size(
                egui::pos2(0.0, 12.0),
                egui::vec2(width, SIDEBAR_HEADER_HEIGHT_POINTS),
            );
            let url_rect =
                edge_alignment.sidebar_section_button_rect(header_rect, SidebarSection::Url);
            let undo_rect = playlist_header_undo_rect(header_rect, edge_alignment, &undo_style);

            assert_eq!(undo_rect.size(), egui::Vec2::splat(32.0));
            assert_eq!(undo_rect.center().x, url_rect.center().x);
            assert_eq!(undo_rect.center().y, header_rect.center().y);
        }

        let base_header = egui::Rect::from_min_size(
            egui::pos2(0.0, 12.0),
            egui::vec2(420.0, SIDEBAR_HEADER_HEIGHT_POINTS),
        );
        let expected_relative_axis = edge_alignment
            .sidebar_section_button_rect(base_header, SidebarSection::Url)
            .center()
            .x
            - base_header.left();
        for horizontal_offset in [-147.0, 273.0] {
            let moving_header = base_header.translate(egui::vec2(horizontal_offset, 0.0));
            let moving_undo = playlist_header_undo_rect(moving_header, edge_alignment, &undo_style);

            assert!(
                (moving_undo.center().x - moving_header.left() - expected_relative_axis).abs()
                    < f32::EPSILON
            );
        }
    }
}
