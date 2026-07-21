//! Compound-specific content, interaction и accessibility поверх общего virtualized owner-а.

use egui::{Key, Modifiers, PointerButton, Response, Sense, TextWrapMode, WidgetInfo, WidgetType};
use playlist_core::{PlaylistEntryId, PlaylistItemId};
use ui_artwork_egui::{ArtworkPainter, CompoundPlaylistPartPosition, CompoundPlaylistRowKind};

use super::PlaylistAction;
use super::row_content::{
    BADGES_WIDTH, DURATION_WIDTH, INDEX_WIDTH, ROW_HEIGHT, format_duration, media_kind_text,
    render_badges, render_media_kind_icon, show_safe_tooltip, tooltip_width,
};
use super::row_interactions::{self, RowInteractionContext, StructuralRowActivation};
use crate::playlist_runtime::{
    CompoundHeaderPlayAction, CompoundPartPlayAction, CompoundPartPosition, CompoundRuntimeRow,
    CompoundRuntimeVisibleRow,
};
use crate::ui::skin::PlaylistRowStyle;

/// Типизированный render-result возвращает только значения, нужные общему overlay pass.
pub(super) struct CompoundRowInteractionResult {
    /// Hover/selection/group fill заполняет заранее зарезервированный background slot.
    pub(super) fill: egui::Color32,
    /// Focus участвует в общем priority outline.
    pub(super) focused: bool,
}

/// Child не получает drag sense и поэтому не может создать structural payload.
pub(super) fn interaction_sense(row: CompoundRuntimeRow) -> Sense {
    match row {
        CompoundRuntimeRow::Single { .. } | CompoundRuntimeRow::CompoundHeader { .. } => {
            Sense::click_and_drag()
        }
        CompoundRuntimeRow::CompoundPart { .. } => Sense::click(),
    }
}

/// Рисует rail/disclosure в neutral artwork crate до текстового content.
pub(super) fn paint_compound_artwork(
    ui: &egui::Ui,
    row_rect: egui::Rect,
    row: &CompoundRuntimeVisibleRow,
    style: PlaylistRowStyle,
) {
    let kind = match row.row() {
        CompoundRuntimeRow::Single { .. } => return,
        CompoundRuntimeRow::CompoundHeader {
            expanded,
            active_part_item_id,
            ..
        } => CompoundPlaylistRowKind::Header {
            expanded,
            active: active_part_item_id.is_some(),
        },
        CompoundRuntimeRow::CompoundPart { active, .. } => {
            let Some(position) = row.part_position().map(artwork_part_position) else {
                return;
            };
            CompoundPlaylistRowKind::Part { position, active }
        }
    };
    ArtworkPainter::new(ui.painter()).compound_playlist_row(row_rect, kind, style.compound_artwork);
}

/// Header и child используют собственные leading columns, сохраняя trailing contract.
pub(super) fn render_content(
    ui: &mut egui::Ui,
    top_level_index: usize,
    row: &CompoundRuntimeVisibleRow,
    style: PlaylistRowStyle,
) {
    match row.row() {
        CompoundRuntimeRow::Single { .. } => {
            unreachable!("обычная строка рендерится существующим renderer path")
        }
        CompoundRuntimeRow::CompoundHeader { .. } => {
            // Leading cell резервирует постоянную доступную hit-area disclosure.
            ui.add_space(style.compound_disclosure_hit_width.max(0.0));
            // Index остаётся top-level и не зависит от числа раскрытых children.
            render_index(ui, top_level_index + 1);
        }
        CompoundRuntimeRow::CompoundPart { ordinal, .. } => {
            // Child title получает дополнительный отступ и не имитирует top-level index.
            ui.add_space(
                (style.compound_disclosure_hit_width + style.compound_child_indent).max(0.0),
            );
            // One-based ordinal помогает visual и screen-reader navigation совпадать.
            render_index(ui, ordinal.one_based() as usize);
        }
    }
    let presentation = row.presentation();
    render_media_kind_icon(ui, presentation.media_kind());
    let trailing_width = DURATION_WIDTH + BADGES_WIDTH;
    let spacing_width = ui.spacing().item_spacing.x * 2.0;
    let title_width = (ui.available_width() - trailing_width - spacing_width).max(24.0);
    let mut title = egui::RichText::new(presentation.display_title());
    if presentation.is_active() {
        title = title.color(style.active_title_color);
    }
    ui.add_sized(
        [title_width, ROW_HEIGHT],
        egui::Label::new(title)
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
    ui.add_sized(
        [DURATION_WIDTH, ROW_HEIGHT],
        egui::Label::new(format_duration(presentation.duration()))
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
    render_badges(ui, presentation);
}

/// Регистрирует один full-row node и дополняет header настоящим expanded property.
pub(super) fn configure_accessibility(
    ui: &egui::Ui,
    response: &Response,
    top_level_index: usize,
    row: &CompoundRuntimeVisibleRow,
) {
    let label = accessibility_text(top_level_index, row);
    match row.row() {
        CompoundRuntimeRow::Single { .. } => {
            unreachable!("обычная строка использует существующий selectable node")
        }
        CompoundRuntimeRow::CompoundHeader {
            expanded, selected, ..
        } => {
            response.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::CollapsingHeader,
                    ui.is_enabled(),
                    selected,
                    &label,
                )
            });
            // egui 0.34.2 не переносит disclosure state из WidgetInfo автоматически.
            response
                .ctx
                .accesskit_node_builder(response.id, |node| node.set_expanded(expanded));
        }
        CompoundRuntimeRow::CompoundPart { .. } => {
            response
                .widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), &label));
        }
    }
}

/// Обрабатывает compound interaction, не создавая renderer-owned authority.
pub(super) fn handle_interaction(
    ui: &mut egui::Ui,
    response: &Response,
    row: &CompoundRuntimeVisibleRow,
    style: PlaylistRowStyle,
    context: &mut RowInteractionContext<'_>,
) -> CompoundRowInteractionResult {
    match row.row() {
        CompoundRuntimeRow::Single { .. } => {
            unreachable!("обычная строка использует существующий interaction path")
        }
        CompoundRuntimeRow::CompoundHeader {
            entry_id,
            header_play_item_id,
            expanded,
            active_part_item_id,
            selected,
            ..
        } => {
            row_interactions::handle_row_response(
                ui,
                response,
                entry_id,
                StructuralRowActivation::CompoundHeader {
                    action: CompoundHeaderPlayAction {
                        compound_entry_id: entry_id,
                        structural_revision: context.compound_snapshot.structural_revision(),
                    },
                    header_play_item_id,
                },
                style.compound_disclosure_hit_width,
                context,
            );
            CompoundRowInteractionResult {
                fill: header_fill(
                    style,
                    selected,
                    response.hovered(),
                    expanded && active_part_item_id.is_some(),
                ),
                focused: response.has_focus(),
            }
        }
        CompoundRuntimeRow::CompoundPart {
            compound_entry_id,
            part_item_id,
            ..
        } => {
            handle_part_response(ui, response, compound_entry_id, part_item_id, context);
            CompoundRowInteractionResult {
                fill: if response.hovered() {
                    style.compound_child_hover_fill
                } else {
                    style.compound_child_fill
                },
                focused: response.has_focus(),
            }
        }
    }
}

/// Compound tooltip переиспользует privacy-safe ordinary row presentation.
pub(super) fn show_tooltip(response: &Response, row: &CompoundRuntimeVisibleRow) {
    egui::Tooltip::for_enabled(response)
        .width(tooltip_width(response.rect.width()))
        .show(|ui| show_safe_tooltip(ui, row.presentation()));
}

/// Structural selection сохраняет прежний приоритет над compound hover/group surfaces.
fn header_fill(
    style: PlaylistRowStyle,
    selected: bool,
    hovered: bool,
    active_expanded_group: bool,
) -> egui::Color32 {
    match (selected, hovered, active_expanded_group) {
        (true, true, _) => style.selected_hover_fill,
        (true, false, _) => style.selected_fill,
        (false, true, _) => style.compound_header_hover_fill,
        (false, false, true) => style.compound_active_header_fill,
        (false, false, false) => style.compound_header_fill,
    }
}

/// Exact child activation не публикует selection/remove/reorder intents.
fn handle_part_response(
    ui: &mut egui::Ui,
    response: &Response,
    compound_entry_id: PlaylistEntryId,
    part_item_id: PlaylistItemId,
    context: &mut RowInteractionContext<'_>,
) {
    let row_was_focused = response.has_focus();
    let play_action = PlaylistAction::PlayCompoundPart(CompoundPartPlayAction {
        compound_entry_id,
        part_item_id,
        structural_revision: context.compound_snapshot.structural_revision(),
    });
    if response.clicked_by(PointerButton::Primary) {
        context.output.push_action(play_action.clone());
        response.request_focus();
    }
    // Screen reader Default action обязан работать и до отдельного focus notification.
    if response.clicked() && !response.clicked_by(PointerButton::Primary) && !row_was_focused {
        context.output.push_action(play_action.clone());
        response.request_focus();
    }
    if row_was_focused {
        row_interactions::handle_visible_navigation(ui, compound_entry_id, context);
        let space_pressed = ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Space));
        let enter_pressed = ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
        if space_pressed || enter_pressed {
            context.output.push_action(play_action.clone());
        }
        let assistive_activation = response.clicked()
            && !response.clicked_by(PointerButton::Primary)
            && !space_pressed
            && !enter_pressed;
        if assistive_activation {
            context.output.push_action(play_action.clone());
        }
    }
    response.context_menu(|menu_ui| {
        if menu_ui.button("Воспроизвести часть").clicked() {
            context.output.push_action(play_action);
            menu_ui.close();
        }
    });
}

/// Russian screen-reader text явно различает group и subordinate part.
fn accessibility_text(top_level_index: usize, row: &CompoundRuntimeVisibleRow) -> String {
    let presentation = row.presentation();
    let runtime_details = runtime_accessibility_details(presentation);
    match row.row() {
        CompoundRuntimeRow::Single { .. } => String::new(),
        CompoundRuntimeRow::CompoundHeader {
            retained_part_count,
            expanded,
            active_part_item_id,
            selected,
            ..
        } => {
            let disclosure = if expanded {
                "Развёрнуто"
            } else {
                "Свёрнуто"
            };
            let mut text = format!(
                "Составная запись {}. {}. Частей: {}. {}. Тип: {}. Длительность: {}.",
                top_level_index + 1,
                presentation.display_title(),
                retained_part_count,
                disclosure,
                media_kind_text(presentation.media_kind()),
                format_duration(presentation.duration()),
            );
            if active_part_item_id.is_some() {
                text.push_str(" Группа содержит текущую воспроизводимую часть.");
            }
            if selected {
                text.push_str(" Выбрано.");
            }
            text.push_str(&runtime_details);
            text.push_str(" Пробел раскрывает или сворачивает; Enter воспроизводит.");
            text
        }
        CompoundRuntimeRow::CompoundPart {
            ordinal,
            retained_part_count,
            ..
        } => {
            let mut text = format!(
                "Вложенная часть {} из {}. {}. Тип: {}. Длительность: {}.",
                ordinal.one_based(),
                retained_part_count,
                presentation.display_title(),
                media_kind_text(presentation.media_kind()),
                format_duration(presentation.duration()),
            );
            if presentation.is_active() {
                text.push_str(" Сейчас играет эта часть.");
            }
            text.push_str(&runtime_details);
            text.push_str(" Удаление и перемещение этой части недоступны.");
            text
        }
    }
}

/// Pending/error формулировки сохраняют различимые состояния ordinary renderer-а.
fn runtime_accessibility_details(row: &crate::playlist_runtime::PlaylistVisibleRow) -> String {
    match (row.runtime_error(), row.is_pending()) {
        (Some(error), true) => format!(
            " Предыдущая попытка завершилась ошибкой; выполняется повторная попытка. Ошибка: {}.",
            error.safe_summary()
        ),
        (Some(error), false) => format!(" Ошибка: {}.", error.safe_summary()),
        (None, true) => " Выполняется открытие.".to_owned(),
        (None, false) => String::new(),
    }
}

/// Fixed-width index/ordinal label не принимает pointer interaction.
fn render_index(ui: &mut egui::Ui, one_based_index: usize) {
    ui.add_sized(
        [INDEX_WIDTH, ROW_HEIGHT],
        egui::Label::new(egui::RichText::new(format!("{one_based_index}.")).weak())
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
}

/// Runtime position переводится в domain-neutral artwork vocabulary полным match-ом.
const fn artwork_part_position(position: CompoundPartPosition) -> CompoundPlaylistPartPosition {
    match position {
        CompoundPartPosition::Only => CompoundPlaylistPartPosition::Only,
        CompoundPartPosition::First => CompoundPlaylistPartPosition::First,
        CompoundPartPosition::Middle => CompoundPlaylistPartPosition::Middle,
        CompoundPartPosition::Last => CompoundPlaylistPartPosition::Last,
    }
}

#[cfg(test)]
mod tests {
    use super::header_fill;
    use crate::ui::skin::{MinimalSkin, PlayerSkin};

    #[test]
    fn active_group_surface_and_structural_selection_keep_distinct_priority() {
        let style = MinimalSkin.playlist_row_style();
        assert_eq!(
            header_fill(style, false, false, true),
            style.compound_active_header_fill
        );
        assert_eq!(
            header_fill(style, true, false, true),
            style.selected_fill,
            "selection не дублируется active child outline-ом на header"
        );
        assert_ne!(style.compound_active_header_fill, style.selected_fill);
    }
}
