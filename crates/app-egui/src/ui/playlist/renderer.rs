//! Fixed-height `show_rows` renderer и stable viewport anchor.

use egui::{Align, Layout, Sense, TextWrapMode, WidgetInfo, WidgetType};
use playlist_core::PlaylistMediaKind;
use ui_artwork_egui::{ArtworkPainter, MediaKindGlyph};

use super::{
    PlaylistUiOutput, PlaylistUiState, ViewportAnchor, row_interactions, virtualized_drag,
};
use crate::playlist_runtime::{PlaylistViewModel, PlaylistVisibleRow};

pub(super) const ROW_HEIGHT: f32 = 34.0;
pub(super) const TOOLTIP_MAX_WIDTH: f32 = 320.0;
pub(super) const INDEX_WIDTH: f32 = 38.0;
pub(super) const MEDIA_KIND_WIDTH: f32 = 16.0;
const DURATION_WIDTH: f32 = 50.0;
const BADGES_WIDTH: f32 = 58.0;

pub(super) fn show_rows(
    ui: &mut egui::Ui,
    model: &PlaylistViewModel,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    if model.is_empty() {
        state.observed_structural_revision = Some(model.structural_revision());
        state.viewport_anchor = None;
        return;
    }

    let row_pitch = ROW_HEIGHT + ui.spacing().item_spacing.y;
    let go_current_item = match state.take_go_current() {
        Some(crate::playlist_runtime::PlaylistGoCurrentTarget::Row(item_id)) => Some(item_id),
        Some(crate::playlist_runtime::PlaylistGoCurrentTarget::Tombstone) | None => None,
    };
    let focus_item = state.take_row_focus().or(go_current_item);
    let drag_offset = virtualized_drag::prepare_scroll_offset(
        ui.ctx(),
        &mut state.drag,
        model.item_count(),
        row_pitch,
    );
    let anchored_offset = focus_item
        .and_then(|item_id| model.row_index(item_id))
        .map(|index| index as f32 * row_pitch)
        .or(drag_offset)
        .or_else(|| anchored_scroll_offset(model, state, row_pitch));
    let mut scroll_area = egui::ScrollArea::vertical()
        .id_salt("playlist_rows_scroll")
        .auto_shrink([false, false]);
    if let Some(anchored_offset) = anchored_offset {
        scroll_area = scroll_area.vertical_scroll_offset(anchored_offset);
    }

    let scroll_output = scroll_area.show_rows(
        ui,
        ROW_HEIGHT,
        model.item_count(),
        |rows_ui, visible_range| {
            rows_ui.set_min_width(0.0);
            let visible_rows = model.visible_rows(visible_range.clone());
            for (visible_offset, row) in visible_rows.iter().enumerate() {
                output.record_visible(row.item_id());
                render_row(
                    rows_ui,
                    model,
                    visible_range.start + visible_offset,
                    row,
                    focus_item == Some(row.item_id()),
                    state,
                    output,
                );
            }
        },
    );
    virtualized_drag::finish_frame(
        ui.ctx(),
        &mut state.drag,
        model,
        scroll_output.inner_rect,
        scroll_output.state.offset.y,
        row_pitch,
        output,
    );
    update_viewport_anchor(model, state, row_pitch, scroll_output.state.offset.y);
}

pub(super) fn anchored_scroll_offset(
    model: &PlaylistViewModel,
    state: &mut PlaylistUiState,
    row_pitch: f32,
) -> Option<f32> {
    let revision = model.structural_revision();
    let previous_revision = state.observed_structural_revision.replace(revision)?;
    if previous_revision == revision {
        return None;
    }
    let anchor = state.viewport_anchor?;
    model.row_index(anchor.item_id).map(|row_index| {
        row_index as f32 * row_pitch + anchor.intra_row_offset.clamp(0.0, row_pitch)
    })
}

fn update_viewport_anchor(
    model: &PlaylistViewModel,
    state: &mut PlaylistUiState,
    row_pitch: f32,
    scroll_offset: f32,
) {
    let top_row_index =
        ((scroll_offset / row_pitch).floor() as usize).min(model.item_count().saturating_sub(1));
    let Some(item_id) = model.item_id_at(top_row_index) else {
        state.viewport_anchor = None;
        return;
    };
    state.viewport_anchor = Some(ViewportAnchor {
        item_id,
        intra_row_offset: (scroll_offset - top_row_index as f32 * row_pitch).clamp(0.0, row_pitch),
    });
}

fn render_row(
    ui: &mut egui::Ui,
    model: &PlaylistViewModel,
    row_index: usize,
    row: &PlaylistVisibleRow,
    focus_requested: bool,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    let item_id_value = row.item_id().expose_value_for_persistence();
    ui.push_id(("playlist_row", item_id_value), |ui| {
        let available_width = ui.available_width().max(1.0);
        let (row_rect, response) = ui.allocate_exact_size(
            egui::vec2(available_width, ROW_HEIGHT),
            Sense::click_and_drag(),
        );
        let row_fill = row_fill(ui, row);
        let row_stroke = if virtualized_drag::marks_row(
            &state.drag,
            row_index,
            row.item_id(),
            model.item_count(),
        ) {
            ui.visuals().selection.stroke
        } else if row.is_active() {
            ui.visuals().widgets.active.fg_stroke
        } else {
            egui::Stroke::NONE
        };
        let mut row_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("content")
                .max_rect(row_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        row_ui.set_clip_rect(row_rect.intersect(ui.clip_rect()));
        egui::Frame::new()
            .fill(row_fill)
            .stroke(row_stroke)
            .show(&mut row_ui, |ui| render_row_content(ui, row_index, row));

        let accessibility_text = accessibility_text(row_index, row);
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::SelectableLabel,
                ui.is_enabled(),
                row.is_selected(),
                &accessibility_text,
            )
        });
        if focus_requested {
            response.scroll_to_me(Some(Align::Center));
            response.request_focus();
        }
        row_interactions::handle_row_response(
            ui,
            &response,
            model,
            row_index,
            row.item_id(),
            state,
            output,
        );
        egui::Tooltip::for_enabled(&response)
            .width(tooltip_width(response.rect.width()))
            .show(|ui| show_safe_tooltip(ui, row));
    });
}

fn row_fill(ui: &egui::Ui, row: &PlaylistVisibleRow) -> egui::Color32 {
    if row.is_selected() {
        ui.visuals().selection.bg_fill
    } else if row.is_active() {
        ui.visuals().widgets.active.weak_bg_fill
    } else {
        egui::Color32::TRANSPARENT
    }
}

fn render_row_content(ui: &mut egui::Ui, row_index: usize, row: &PlaylistVisibleRow) {
    ui.add_sized(
        [INDEX_WIDTH, ROW_HEIGHT],
        egui::Label::new(egui::RichText::new(format!("{}.", row_index + 1)).weak())
            .wrap_mode(TextWrapMode::Truncate),
    );
    render_media_kind_icon(ui, row.media_kind());

    let trailing_width = DURATION_WIDTH + BADGES_WIDTH;
    let spacing_width = ui.spacing().item_spacing.x * 2.0;
    let title_width = (ui.available_width() - trailing_width - spacing_width).max(24.0);
    ui.add_sized(
        [title_width, ROW_HEIGHT],
        egui::Label::new(egui::RichText::new(row.display_title()).strong())
            .wrap_mode(TextWrapMode::Truncate),
    );
    ui.add_sized(
        [DURATION_WIDTH, ROW_HEIGHT],
        egui::Label::new(format_duration(row.duration())).wrap_mode(TextWrapMode::Truncate),
    );
    render_badges(ui, row);
}

/// Резервирует компактную неинтерактивную ячейку и передаёт рисование artwork-crate.
fn render_media_kind_icon(ui: &mut egui::Ui, media_kind: PlaylistMediaKind) {
    // `Sense::hover` не создаёт click/drag-владельца и сохраняет взаимодействие с целой строкой.
    let (response, painter) =
        ui.allocate_painter(egui::vec2(MEDIA_KIND_WIDTH, ROW_HEIGHT), Sense::hover());
    // Цвет и толщина берутся из текущей темы, поэтому glyph наследует состояние интерфейса.
    let stroke = ui.visuals().widgets.noninteractive.fg_stroke;
    // App-level выполняет только типизированное отображение domain-вида в визуальный glyph.
    ArtworkPainter::new(&painter).media_kind_icon(
        response.rect,
        media_kind_glyph(media_kind),
        stroke,
    );
}

/// Переводит playlist-domain тип в нейтральный artwork-контракт.
pub(super) const fn media_kind_glyph(media_kind: PlaylistMediaKind) -> MediaKindGlyph {
    // Полный match не позволит молча забыть новый domain-вариант.
    match media_kind {
        PlaylistMediaKind::Unknown => MediaKindGlyph::Unknown,
        PlaylistMediaKind::Audio => MediaKindGlyph::Audio,
        PlaylistMediaKind::Video => MediaKindGlyph::Video,
    }
}

fn render_badges(ui: &mut egui::Ui, row: &PlaylistVisibleRow) {
    ui.allocate_ui_with_layout(
        egui::vec2(BADGES_WIDTH, ROW_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            if row.is_active() {
                ui.strong("▶");
            } else {
                ui.add_space(13.0);
            }
            if row.is_pending() {
                ui.add_sized([14.0, 14.0], egui::Spinner::new());
            } else {
                ui.add_space(14.0);
            }
            if row.runtime_error().is_some() {
                ui.colored_label(ui.visuals().error_fg_color, "!");
            } else {
                ui.add_space(8.0);
            }
        },
    );
}

fn media_kind_text(media_kind: PlaylistMediaKind) -> &'static str {
    match media_kind {
        PlaylistMediaKind::Unknown => "Медиа",
        PlaylistMediaKind::Audio => "Аудио",
        PlaylistMediaKind::Video => "Видео",
    }
}

fn format_duration(duration: Option<media_core::MediaDuration>) -> String {
    let Some(duration) = duration else {
        return "—".to_owned();
    };
    let total_seconds = duration.as_duration().as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

pub(super) fn accessibility_text(row_index: usize, row: &PlaylistVisibleRow) -> String {
    let mut text = format!(
        "Элемент {}. {}. Тип: {}. Длительность: {}.",
        row_index + 1,
        row.display_title(),
        media_kind_text(row.media_kind()),
        format_duration(row.duration())
    );
    if row.display_title() != row.fallback_display_name() {
        text.push_str(" Имя файла: ");
        text.push_str(row.fallback_display_name());
        text.push('.');
    }
    if row.is_active() {
        text.push_str(" Сейчас играет.");
    }
    if row.is_selected() {
        text.push_str(" Выбрано.");
    }
    match (row.runtime_error(), row.is_pending()) {
        (Some(error), true) => {
            text.push_str(
                " Предыдущая попытка завершилась ошибкой; выполняется повторная попытка. Ошибка: ",
            );
            text.push_str(error.safe_summary());
            text.push('.');
        }
        (Some(error), false) => {
            text.push_str(" Ошибка: ");
            text.push_str(error.safe_summary());
            text.push('.');
        }
        (None, true) => text.push_str(" Выполняется открытие."),
        (None, false) => {}
    }
    text
}

fn show_safe_tooltip(ui: &mut egui::Ui, row: &PlaylistVisibleRow) {
    ui.add(egui::Label::new(row.display_title()).wrap_mode(TextWrapMode::Wrap));
    if row.display_title() != row.fallback_display_name() {
        ui.add(
            egui::Label::new(egui::RichText::new(row.fallback_display_name()).weak())
                .wrap_mode(TextWrapMode::Wrap),
        );
    }
    if let Some(error) = row.runtime_error() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(error.safe_summary()).color(ui.visuals().error_fg_color),
            )
            .wrap_mode(TextWrapMode::Wrap),
        );
    }
}

/// Ограничивает tooltip шириной строки и не выпускает длинное имя поверх всего видео.
pub(super) fn tooltip_width(row_width: f32) -> f32 {
    row_width.clamp(1.0, TOOLTIP_MAX_WIDTH)
}

#[cfg(test)]
pub(super) fn stable_row_id(
    parent_id: egui::Id,
    item_id: playlist_core::PlaylistItemId,
) -> egui::Id {
    parent_id.with(("playlist_row", item_id.expose_value_for_persistence()))
}
