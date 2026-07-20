//! Общий 32-point header всех сменяемых секций sidebar.
//!
//! Подмодуль остаётся частью sidebar owner-а: он резервирует vertical geometry,
//! строит Undo hit-area по window-chrome grid и не владеет Panel/resize state.

use egui::{Button, RichText, Ui};

use crate::playlist_runtime::PlaylistViewModel;
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
    /// Фактический rect Playlist position либо отсутствие индикатора.
    playlist_position_rect: Option<egui::Rect>,
    /// Фактический rect крестика.
    close_rect: egui::Rect,
}

/// Read-only состояние компактного Playlist position внутри общего header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaylistHeaderPosition {
    /// Подтверждённо играющий Item ID всё ещё находится в canonical queue.
    Active {
        /// Однобазовая позиция текущего трека, понятная пользователю.
        one_based_position: usize,
        /// Полное число элементов в canonical queue.
        total_items: usize,
    },
    /// Очередь непуста, но подтверждённой позиции внутри неё сейчас нет.
    Inactive {
        /// Полное число элементов в canonical queue.
        total_items: usize,
    },
}

impl PlaylistHeaderPosition {
    /// Форматирует только короткий видимый текст без UI geometry.
    fn visible_text(self) -> String {
        match self {
            Self::Active {
                one_based_position,
                total_items,
            } => format!("{one_based_position}/{total_items}"),
            Self::Inactive { total_items } => format!("—/{total_items}"),
        }
    }
}

/// Рисует заголовок конкретной секции, сохраняя её close semantics.
pub(super) fn show(ui: &mut Ui, section: SidebarSection, context: &mut SidebarRenderContext<'_>) {
    let header_rect = allocate_sidebar_header_rect(ui);
    // Только Playlist получает trailing position; остальные секции сохраняют пустой trailing slot.
    let playlist_position = if section == SidebarSection::Playlist {
        playlist_header_position(context.playlist_model)
    } else {
        None
    };
    let chrome_output = render_header_chrome(
        ui,
        header_rect,
        sidebar_section_title(section),
        sidebar_close_tooltip(section),
        playlist_position,
    );
    // Явные assertions закрепляют вертикальное центрирование реального egui layout.
    debug_assert!((chrome_output.title_rect.center().y - header_rect.center().y).abs() < 0.1);
    debug_assert!((chrome_output.close_rect.center().y - header_rect.center().y).abs() < 0.1);
    debug_assert!(
        chrome_output
            .playlist_position_rect
            .is_none_or(|position_rect| {
                (position_rect.center().y - header_rect.center().y).abs() < 0.1
            })
    );

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
            context.ui_motion,
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
    playlist_position: Option<PlaylistHeaderPosition>,
) -> SidebarHeaderChromeOutput {
    let mut header_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("sidebar_header_chrome")
            .max_rect(header_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    header_ui.set_clip_rect(header_rect.intersect(ui.clip_rect()));
    // Strong theme token делает все section headings яркими без цветового hardcode.
    let strong_text_color = header_ui.visuals().strong_text_color();
    let title_response = header_ui.label(RichText::new(title).heading().color(strong_text_color));
    let (close_response, playlist_position_response) = header_ui
        .with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |header_ui| {
                // Крестик остаётся крайним правым элементом общего header.
                let close_response = header_ui
                    .add(Button::new(RichText::new("×").strong()))
                    .on_hover_text(close_tooltip);
                // Position добавляется после крестика в right-to-left layout и оказывается слева от него.
                let playlist_position_response = playlist_position.map(|position| {
                    header_ui.label(RichText::new(position.visible_text()).color(strong_text_color))
                });
                (close_response, playlist_position_response)
            },
        )
        .inner;

    SidebarHeaderChromeOutput {
        close_clicked: close_response.clicked(),
        title_rect: title_response.rect,
        playlist_position_rect: playlist_position_response.map(|response| response.rect),
        close_rect: close_response.rect,
    }
}

/// Преобразует authoritative Playlist read model в компактное состояние header.
fn playlist_header_position(model: Option<&PlaylistViewModel>) -> Option<PlaylistHeaderPosition> {
    // Пока модель не подключена, header не изображает выдуманное состояние очереди.
    let model = model?;
    // Пустая очередь не показывает индикатор вообще по выбранному UX-контракту.
    let total_items = model.item_count();
    if total_items == 0 {
        return None;
    }
    // Только подтверждённый active Item ID имеет право задавать текущую позицию.
    let active_row_index = model
        .active_item_id()
        .and_then(|active_item_id| model.row_index(active_item_id));
    // UI показывает однобазовую позицию; отсутствующая строка остаётся отдельным состоянием.
    Some(active_row_index.map_or(
        PlaylistHeaderPosition::Inactive { total_items },
        |zero_based_position| PlaylistHeaderPosition::Active {
            one_based_position: zero_based_position + 1,
            total_items,
        },
    ))
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
    use std::path::PathBuf;

    use egui::RawInput;
    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
        RemoveItemOutcome,
    };

    use super::*;
    use crate::ui::skin::{MinimalSkin, PlayerSkin};

    /// Строит canonical Playlist queue с устойчивыми Item ID для header tests.
    fn playlist_queue(item_count: usize) -> PlaylistQueue {
        // Queue остаётся единственным owner allocator-а Item ID даже в UI fixture.
        let mut queue = PlaylistQueue::new();
        // Пустой batch не нужен: empty queue должен остаться отдельным test state.
        if item_count == 0 {
            return queue;
        }
        // Каждый draft получает различимый privacy-safe локальный locator.
        let drafts = (0..item_count)
            .map(|index| {
                PlaylistItemDraft::local(
                    LocalLocator::Native(PathBuf::from(format!("header-{index}.mp3"))),
                    None,
                    CachedPlaylistMetadata::new(
                        format!("header-{index}.mp3"),
                        PlaylistMediaKind::Audio,
                    ),
                )
            })
            .collect();
        // Test fixture обязан явно проверить admission вместо игнорирования ошибки.
        queue
            .append_batch(drafts)
            .expect("header test queue must fit the playlist hard cap");
        queue
    }

    /// Строит read model только с подтверждённым active identity.
    fn playlist_model(
        queue: &PlaylistQueue,
        active_item_id: Option<playlist_core::PlaylistItemId>,
    ) -> PlaylistViewModel {
        // Test-only constructor сохраняет production row/index construction.
        PlaylistViewModel::for_queue_with_active_item_for_test(queue, 1, active_item_id)
    }

    /// Находит фактический цвет конкретного текста в headless egui output.
    fn painted_text_color(output: &egui::FullOutput, text: &str) -> egui::Color32 {
        // Ищем именно Text shape с полным совпадением, чтобы не спутать title и tooltip.
        output
            .shapes
            .iter()
            .find_map(|clipped_shape| match &clipped_shape.shape {
                egui::epaint::Shape::Text(text_shape)
                    if text_shape.galley.job.text.as_str() == text =>
                {
                    text_shape
                        .galley
                        .job
                        .sections
                        .first()
                        .map(|section| section.format.color)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("text shape `{text}` must be painted"))
    }

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
                    None,
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

    /// Position использует только confirmed active ID и однобазовый canonical index.
    #[test]
    fn playlist_position_covers_active_inactive_removed_and_empty_states() {
        // Неподключённая и пустая модели не занимают header.
        assert_eq!(playlist_header_position(None), None);
        let empty_queue = playlist_queue(0);
        let empty_model = playlist_model(&empty_queue, None);
        assert_eq!(playlist_header_position(Some(&empty_model)), None);

        // Непустая очередь без confirmed active row показывает явное отсутствие позиции.
        let mut populated_queue = playlist_queue(30);
        let inactive_model = playlist_model(&populated_queue, None);
        let inactive_position = playlist_header_position(Some(&inactive_model))
            .expect("non-empty queue must keep a position indicator");
        assert_eq!(
            inactive_position,
            PlaylistHeaderPosition::Inactive { total_items: 30 }
        );
        assert_eq!(inactive_position.visible_text(), "—/30");

        // Первый, средний и последний active Item ID разрешаются через production index.
        for (zero_based_position, expected_text) in [(0, "1/30"), (5, "6/30"), (29, "30/30")] {
            let active_item_id = populated_queue
                .iter_playable_ids()
                .nth(zero_based_position)
                .expect("fixture должен содержать playable строку по заданной позиции");
            let active_model = playlist_model(&populated_queue, Some(active_item_id));
            let active_position = playlist_header_position(Some(&active_model))
                .expect("active row in non-empty queue must be visible");
            assert_eq!(active_position.visible_text(), expected_text);
        }

        // Удалённый active ID больше не имеет canonical position, но total остаётся точным.
        let removed_active_item_id = populated_queue
            .iter_playable_ids()
            .nth(5)
            .expect("fixture должен содержать удаляемую playable строку");
        assert!(matches!(
            populated_queue.remove(removed_active_item_id),
            RemoveItemOutcome::Removed { .. }
        ));
        let removed_active_model = playlist_model(&populated_queue, Some(removed_active_item_id));
        let removed_position = playlist_header_position(Some(&removed_active_model))
            .expect("non-empty queue must keep an inactive position indicator");
        assert_eq!(
            removed_position,
            PlaylistHeaderPosition::Inactive { total_items: 29 }
        );
        assert_eq!(removed_position.visible_text(), "—/29");
    }

    /// Bright title и compact position делят один fixed header без geometry jumps.
    #[test]
    fn playlist_position_is_bright_centered_and_stable_across_widths_and_transition() {
        // Production skin задаёт общие axes Undo и нижних window controls.
        let controls_style = MinimalSkin.controls_style();
        let edge_alignment = WindowChromeEdgeAlignment::from_controls_style(controls_style);
        let undo_style = MinimalSkin.playlist_header_undo_style();
        let position = PlaylistHeaderPosition::Active {
            one_based_position: 6,
            total_items: 30,
        };
        let mut expected_position_right_inset: Option<f32> = None;

        // Supported sidebar widths обязаны сохранять одну и ту же trailing alignment.
        for (width, horizontal_offset) in [(350.0, 0.0), (420.0, -147.0), (600.0, 273.0)] {
            let context = egui::Context::default();
            let input = RawInput {
                screen_rect: Some(egui::Rect::from_min_max(
                    egui::pos2(-400.0, 0.0),
                    egui::pos2(1_200.0, 180.0),
                )),
                ..RawInput::default()
            };
            let header_rect = egui::Rect::from_min_size(
                egui::pos2(horizontal_offset, 12.0),
                egui::vec2(width, SIDEBAR_HEADER_HEIGHT_POINTS),
            );
            let undo_rect = playlist_header_undo_rect(header_rect, edge_alignment, &undo_style);
            let mut measured = None;
            let output = context.run_ui(input, |ui| {
                let chrome = render_header_chrome(
                    ui,
                    header_rect,
                    sidebar_section_title(SidebarSection::Playlist),
                    sidebar_close_tooltip(SidebarSection::Playlist),
                    Some(position),
                );
                measured = Some(chrome);
            });
            let chrome = measured.expect("playlist header chrome must be measured");
            let position_rect = chrome
                .playlist_position_rect
                .expect("active Playlist must paint its position");

            // Все header элементы имеют одну vertical axis.
            assert!((chrome.title_rect.center().y - header_rect.center().y).abs() < 0.1);
            assert!((position_rect.center().y - header_rect.center().y).abs() < 0.1);
            assert!((chrome.close_rect.center().y - header_rect.center().y).abs() < 0.1);
            // Right-to-left order и typed Undo axis не пересекаются.
            assert!(position_rect.right() <= chrome.close_rect.left());
            assert!(undo_rect.right() <= position_rect.left());
            assert!(chrome.title_rect.right() <= undo_rect.left());
            // Horizontal slide меняет absolute X, но не relative trailing inset.
            let position_right_inset = header_rect.right() - position_rect.right();
            if let Some(expected_inset) = expected_position_right_inset {
                assert!((position_right_inset - expected_inset).abs() < 0.1);
            } else {
                expected_position_right_inset = Some(position_right_inset);
            }
            // И heading, и счётчик используют яркий theme token.
            let strong_text_color = egui::Visuals::dark().strong_text_color();
            assert_eq!(painted_text_color(&output, "Плейлист"), strong_text_color);
            assert_eq!(painted_text_color(&output, "6/30"), strong_text_color);
        }
    }

    /// Все shared sidebar headings используют один bright theme token.
    #[test]
    fn every_sidebar_heading_uses_bright_theme_color() {
        for section in [
            SidebarSection::Playlist,
            SidebarSection::Settings,
            SidebarSection::Url,
            SidebarSection::Info,
        ] {
            let context = egui::Context::default();
            let input = RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(420.0, 180.0),
                )),
                ..RawInput::default()
            };
            let output = context.run_ui(input, |ui| {
                let header_rect = egui::Rect::from_min_size(
                    egui::pos2(0.0, 12.0),
                    egui::vec2(420.0, SIDEBAR_HEADER_HEIGHT_POINTS),
                );
                let _chrome = render_header_chrome(
                    ui,
                    header_rect,
                    sidebar_section_title(section),
                    sidebar_close_tooltip(section),
                    None,
                );
            });

            assert_eq!(
                painted_text_color(&output, sidebar_section_title(section)),
                egui::Visuals::dark().strong_text_color()
            );
        }
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
