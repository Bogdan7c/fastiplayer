//! Fixed-height `show_rows` renderer и stable viewport anchor.

use egui::layers::ShapeIdx;
use egui::{Align, Layout, Sense, WidgetInfo, WidgetType};
use ui_artwork_egui::ArtworkPainter;

use super::{
    PlaylistAction, PlaylistUiOutput, PlaylistUiState, ViewportAnchor,
    active_accent::{
        AuthoritativeViewport, AuthoritativeViewportInput, BeginFrameInput, FinishFrameInput,
        ViewportControl,
    },
    compound_rows,
    row_content::{
        ROW_HEIGHT, accessibility_text, render_row_content, show_safe_tooltip, tooltip_width,
    },
    row_interactions, virtualized_drag,
};
use crate::playlist_runtime::{
    ClearSelectionCursor, CompoundCurrentItemScrollTarget, CompoundRuntimeRow,
    CompoundRuntimeRowId, CompoundRuntimeViewSnapshot, CompoundRuntimeVisibleRow,
    PlaylistViewModel, PlaylistVisibleRow, UpdateSelection,
};
use crate::ui::animation::UiMotion;
use crate::ui::skin::PlaylistRowStyle;

/// Большой frame stall не должен мгновенно проматывать UI transition.
const MAX_ANIMATION_DELTA_SECONDS: f32 = 0.1;
/// Минимальная скорость, считающаяся пользовательской kinetic-прокруткой.
const MANUAL_SCROLL_VELOCITY_EPSILON: f32 = 0.01;

/// Read-only row dependencies группируются, чтобы boundary не превращался в список параметров.
#[derive(Clone, Copy)]
struct PlaylistRowRenderContext<'a> {
    /// Authoritative immutable snapshot для selection, drag targets и stable IDs.
    model: &'a PlaylistViewModel,
    /// Compound snapshot является единственным visible-row layout owner-ом.
    compound_snapshot: &'a CompoundRuntimeViewSnapshot,
    /// Skin-owned токены не читаются из глобальных egui visuals.
    row_style: PlaylistRowStyle,
    /// Одноразовый focus intent разрешён до начала virtualized row pass.
    focus_row: Option<CompoundRuntimeRowId>,
}

/// Первый visible pass резервирует geometry и background slots без content paint.
struct PreparedPlaylistRow {
    /// Canonical индекс нужен interaction/accessibility и drag target.
    row_index: usize,
    /// Bounded visible read model переносится без повторного snapshot lookup.
    row: CompoundRuntimeVisibleRow,
    /// Один stable full-width interaction ID принадлежит всей строке.
    row_id: egui::Id,
    /// Экранный rect строки не меняется во втором pass.
    row_rect: egui::Rect,
    /// Hover/selection fill slot располагается над active fill и под content.
    background_shape: ShapeIdx,
}

/// Overlay каждой видимой строки рисуется после moving active outline.
#[derive(Clone, Copy)]
struct PlaylistRowOverlay {
    /// Full-width row rect нужен stroke и physical-pixel separator.
    row_rect: egui::Rect,
    /// Только insertion/focus stroke; active stroke рисуется отдельным слоем.
    priority_stroke: egui::Stroke,
}

/// Bounded paint plan возвращается из ScrollArea closure без полного queue traversal.
struct VisibleRowsPaintPlan {
    /// Самый ранний shape slot гарантирует active fill под hover/selection.
    active_fill_shape: ShapeIdx,
    /// Первый visible rect восстанавливает content-coordinate mapping.
    reference_row: Option<(usize, egui::Rect)>,
    /// Authoritative target rect присутствует только когда строка видима.
    active_target_rect: Option<egui::Rect>,
    /// Finite overlays содержат только фактически отрисованные строки.
    row_overlays: Vec<PlaylistRowOverlay>,
}

pub(super) fn show_rows(
    ui: &mut egui::Ui,
    model: &PlaylistViewModel,
    row_style: PlaylistRowStyle,
    motion: UiMotion,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    // Один immutable S17V snapshot задаёт visible identities и disclosure layout.
    let compound_snapshot = model.compound_snapshot();
    // Tombstone больше не имеет status-row; intent всё равно потребляется ровно один раз.
    let go_current_item = match state.take_go_current() {
        Some(crate::playlist_runtime::PlaylistGoCurrentTarget::Row(item_id)) => Some(item_id),
        Some(crate::playlist_runtime::PlaylistGoCurrentTarget::Tombstone) | None => None,
    };
    if model.is_empty() {
        state.observed_playlist_layout_identity = Some(model.layout_identity());
        state.viewport_anchor = None;
        state.active_accent.observe_empty(model.layout_identity());
        return;
    }

    let row_pitch = ROW_HEIGHT + ui.spacing().item_spacing.y;
    let focus_row = state.take_row_focus().or_else(|| {
        go_current_item.and_then(|item_id| {
            match compound_snapshot.current_item_scroll_target(item_id)? {
                CompoundCurrentItemScrollTarget::Header(entry_id) => {
                    Some(CompoundRuntimeRowId::Entry(entry_id))
                }
                CompoundCurrentItemScrollTarget::Part(part_item_id) => {
                    let entry_id = model.entry_id_at(model.row_index(part_item_id)?)?;
                    Some(CompoundRuntimeRowId::Part {
                        compound_entry_id: entry_id,
                        part_item_id,
                    })
                }
            }
        })
    });
    let drag_was_active = virtualized_drag::is_active(&state.drag);
    let drag_offset = virtualized_drag::prepare_scroll_offset(
        ui.ctx(),
        &mut state.drag,
        compound_snapshot.visible_row_count(),
        row_pitch,
    );
    let manual_scroll_input = state.active_accent.has_manual_scroll_input(ui.ctx());
    let explicit_viewport_intent = focus_row.is_some();
    let manual_viewport_override =
        manual_scroll_input || drag_was_active || explicit_viewport_intent;
    let delta_seconds = ui
        .input(|input| input.stable_dt)
        .clamp(0.0, MAX_ANIMATION_DELTA_SECONDS);
    let active_item_id = model.active_item_id();
    let accent_scroll_offset = state.active_accent.begin_frame(BeginFrameInput {
        active_item_id,
        layout_identity: model.layout_identity(),
        target_row_index: active_item_id
            .and_then(|item_id| compound_snapshot.active_row_index(item_id)),
        item_count: compound_snapshot.visible_row_count(),
        row_pitch,
        motion,
        delta_seconds,
        manual_viewport_override,
    });
    let anchored_offset = focus_row
        .and_then(|row_id| compound_snapshot.row_index(row_id))
        .map(|index| index as f32 * row_pitch)
        .or(drag_offset)
        .or(accent_scroll_offset)
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
        compound_snapshot.visible_row_count(),
        |rows_ui, visible_range| {
            rows_ui.set_min_width(0.0);
            let visible_rows = compound_snapshot.visible_presented_rows(visible_range.clone());
            let row_context = PlaylistRowRenderContext {
                model,
                compound_snapshot,
                row_style,
                focus_row,
            };
            render_visible_rows_in_two_passes(
                rows_ui,
                row_context,
                visible_range.start,
                visible_rows,
                active_item_id.and_then(|item_id| compound_snapshot.active_row_index(item_id)),
                state,
                output,
            )
        },
    );
    clear_selection_from_empty_area(
        ui,
        compound_snapshot.visible_row_count(),
        scroll_output.inner_rect,
        scroll_output.state.offset.y,
        row_pitch,
        output,
    );
    virtualized_drag::finish_frame(
        ui.ctx(),
        &mut state.drag,
        compound_snapshot,
        scroll_output.inner_rect,
        scroll_output.state.offset.y,
        row_pitch,
        output,
    );
    let scroll_area_dragged = ui.ctx().is_being_dragged(scroll_output.id.with(1usize))
        || ui.ctx().is_being_dragged(scroll_output.id.with("area"));
    let kinetic_scroll_active =
        scroll_output.state.velocity().y.abs() > MANUAL_SCROLL_VELOCITY_EPSILON;
    let manual_override_after_render =
        scroll_area_dragged || kinetic_scroll_active || virtualized_drag::is_active(&state.drag);
    let viewport =
        scroll_output
            .inner
            .reference_row
            .and_then(|(reference_row_index, reference_row_rect)| {
                AuthoritativeViewport::from_rendered_row(AuthoritativeViewportInput {
                    screen_rect: scroll_output.inner_rect,
                    scroll_offset: scroll_output.state.offset.y,
                    row_pitch,
                    row_height: ROW_HEIGHT,
                    item_count: compound_snapshot.visible_row_count(),
                    reference_row_index,
                    reference_row_rect,
                    control: if manual_override_after_render {
                        ViewportControl::Manual
                    } else {
                        ViewportControl::Automatic
                    },
                })
            });
    let active_rect = state.active_accent.finish_frame(FinishFrameInput {
        viewport,
        target_row_rect: scroll_output.inner.active_target_rect,
        manual_viewport_override: manual_override_after_render,
    });
    paint_row_overlays(
        ui,
        scroll_output.inner_rect,
        row_style,
        active_rect,
        &scroll_output.inner,
    );
    if state.active_accent.needs_repaint() {
        ui.ctx().request_repaint();
    }
    update_viewport_anchor(
        compound_snapshot,
        state,
        row_pitch,
        scroll_output.state.offset.y,
    );
}

/// Выполняет два прохода только по bounded visible rows.
fn render_visible_rows_in_two_passes(
    ui: &mut egui::Ui,
    context: PlaylistRowRenderContext<'_>,
    first_visible_row_index: usize,
    visible_rows: Vec<CompoundRuntimeVisibleRow>,
    active_row_index: Option<usize>,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) -> VisibleRowsPaintPlan {
    // Active fill slot резервируется раньше всех row surfaces.
    let active_fill_shape = ArtworkPainter::new(ui.painter()).reserve_playlist_row_background();
    // Vector capacity равна уже bounded visible read model, а не полной очереди.
    let mut prepared_rows = Vec::with_capacity(visible_rows.len());

    // Первый pass только выделяет fixed-height rect-ы и paint slots.
    for (visible_offset, row) in visible_rows.into_iter().enumerate() {
        // Canonical index восстанавливается из начала egui visible range.
        let row_index = first_visible_row_index + visible_offset;
        // Stable top-level identity изолирует Single и Compound header rows.
        let stable_row_id = row.row().row_id();
        // Scope нужен только для стабильного row interaction ID первого pass.
        let prepared_row = ui
            .push_id(("playlist_row", stable_row_id), |row_ui| {
                // Каждая surface занимает всю доступную ширину ScrollArea content.
                let available_width = row_ui.available_width().max(1.0);
                // Allocation продвигает layout ровно один раз.
                let (row_id, row_rect) =
                    row_ui.allocate_space(egui::vec2(available_width, ROW_HEIGHT));
                // Row fill slot располагается после active fill, но до content.
                let background_shape =
                    ArtworkPainter::new(row_ui.painter()).reserve_playlist_row_background();
                // Prepared record не содержит ссылок на временный widget.
                PreparedPlaylistRow {
                    row_index,
                    row,
                    row_id,
                    row_rect,
                    background_shape,
                }
            })
            .inner;
        // Bounded plan сохраняет только видимые строки.
        prepared_rows.push(prepared_row);
    }

    // Reference geometry существует для любого непустого visible pass.
    let reference_row = prepared_rows
        .first()
        .map(|prepared| (prepared.row_index, prepared.row_rect));
    // Active target rect ищется только среди уже bounded visible records.
    let active_target_rect = active_row_index.and_then(|target_row_index| {
        prepared_rows
            .iter()
            .find(|prepared| prepared.row_index == target_row_index)
            .map(|prepared| prepared.row_rect)
    });
    // Overlay plan имеет ту же bounded ёмкость.
    let mut row_overlays = Vec::with_capacity(prepared_rows.len());

    // Второй pass рисует content и регистрирует interaction по готовой геометрии.
    for prepared_row in &prepared_rows {
        // Demand hint публикуется один раз на реально отрисованную строку.
        output.record_visible(prepared_row.row.presentation().item_id());
        // Overlay stroke откладывается до moving active outline.
        row_overlays.push(render_prepared_row(
            ui,
            context,
            prepared_row,
            state,
            output,
        ));
    }

    // Paint plan не содержит domain actions и не переживает текущий кадр.
    VisibleRowsPaintPlan {
        active_fill_shape,
        reference_row,
        active_target_rect,
        row_overlays,
    }
}

/// Рисует moving accent, затем priority strokes и separators в явном порядке.
fn paint_row_overlays(
    ui: &egui::Ui,
    viewport_rect: egui::Rect,
    row_style: PlaylistRowStyle,
    active_rect: Option<egui::Rect>,
    plan: &VisibleRowsPaintPlan,
) {
    // Декоративные shapes не имеют права выходить за ScrollArea viewport.
    let clip_rect = viewport_rect.intersect(ui.clip_rect());
    // Clipped painter остаётся на том же layer, где были зарезервированы slots.
    let painter = ui.painter().with_clip_rect(clip_rect);
    // Facade сохраняет запрет прямых Painter primitives внутри app-egui.
    let artwork = ArtworkPainter::new(&painter);

    // Active fill занимает самый ранний slot и остаётся под hover/selection.
    if let Some(active_rect) = active_rect {
        artwork.playlist_row_background(
            plan.active_fill_shape,
            active_rect,
            row_style.active_fill,
            egui::Stroke::NONE,
        );
        // Левый marker располагается поверх row surfaces, но ниже focus/insertion.
        artwork.playlist_row_marker(active_rect, row_style.active_marker);
    }

    // Insertion/focus strokes имеют окончательный визуальный приоритет.
    for overlay in &plan.row_overlays {
        artwork.playlist_row_outline(overlay.row_rect, overlay.priority_stroke);
    }
    // Separator остаётся последней physical-pixel линией каждой строки.
    for overlay in &plan.row_overlays {
        artwork.playlist_row_separator(
            overlay.row_rect,
            row_style.separator_color,
            ui.ctx().pixels_per_point(),
        );
    }
}

/// Клик ниже последней строки снимает selection, не создавая fake row action.
fn clear_selection_from_empty_area(
    ui: &mut egui::Ui,
    visible_row_count: usize,
    viewport: egui::Rect,
    scroll_offset: f32,
    row_pitch: f32,
    output: &mut PlaylistUiOutput,
) {
    let content_bottom = viewport.top() + visible_row_count as f32 * row_pitch - scroll_offset;
    let empty_top = content_bottom.clamp(viewport.top(), viewport.bottom());
    if empty_top >= viewport.bottom() {
        return;
    }
    let empty_rect = egui::Rect::from_min_max(
        egui::pos2(viewport.left(), empty_top),
        viewport.right_bottom(),
    );
    let response = ui.interact(
        empty_rect,
        ui.make_persistent_id("playlist_empty_area"),
        Sense::click(),
    );
    if response.clicked_by(egui::PointerButton::Primary) {
        output.push_action(PlaylistAction::UpdateSelection(UpdateSelection::Clear {
            cursor: ClearSelectionCursor::Clear,
        }));
    }
}

pub(super) fn anchored_scroll_offset(
    model: &PlaylistViewModel,
    state: &mut PlaylistUiState,
    row_pitch: f32,
) -> Option<f32> {
    let layout_identity = model.layout_identity();
    let previous_identity = state
        .observed_playlist_layout_identity
        .replace(layout_identity)?;
    if previous_identity == layout_identity {
        return None;
    }
    let anchor = state.viewport_anchor?;
    model
        .compound_snapshot()
        .active_row_index(anchor.item_id)
        .map(|row_index| {
            row_index as f32 * row_pitch + anchor.intra_row_offset.clamp(0.0, row_pitch)
        })
}

fn update_viewport_anchor(
    compound_snapshot: &CompoundRuntimeViewSnapshot,
    state: &mut PlaylistUiState,
    row_pitch: f32,
    scroll_offset: f32,
) {
    let top_row_index = ((scroll_offset / row_pitch).floor() as usize)
        .min(compound_snapshot.visible_row_count().saturating_sub(1));
    let Some(item_id) = compound_snapshot
        .visible_presented_rows(top_row_index..top_row_index.saturating_add(1))
        .first()
        .map(|row| row.presentation().item_id())
    else {
        state.viewport_anchor = None;
        return;
    };
    state.viewport_anchor = Some(ViewportAnchor {
        item_id,
        intra_row_offset: (scroll_offset - top_row_index as f32 * row_pitch).clamp(0.0, row_pitch),
    });
}

fn render_prepared_row(
    ui: &mut egui::Ui,
    context: PlaylistRowRenderContext<'_>,
    prepared: &PreparedPlaylistRow,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) -> PlaylistRowOverlay {
    // Projection identity задаёт interaction policy, presentation — только display metadata.
    let projection = prepared.row.row();
    let presentation = prepared.row.presentation();
    // Top-level numbering не меняется при раскрытии либо сворачивании children.
    let structural_entry_id = projection.structural_entry_id().or(match projection {
        CompoundRuntimeRow::CompoundPart {
            compound_entry_id, ..
        } => Some(compound_entry_id),
        CompoundRuntimeRow::Single { .. } | CompoundRuntimeRow::CompoundHeader { .. } => None,
    });
    let top_level_index = structural_entry_id
        .and_then(|entry_id| context.model.entry_row_index(entry_id))
        .unwrap_or(prepared.row_index);
    // Второй pass использует готовую geometry и не продвигает parent layout повторно.
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(("playlist_row_content", projection.row_id()))
            .max_rect(prepared.row_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    // Content клипуется точным row rect и текущим ScrollArea clip.
    row_ui.set_clip_rect(prepared.row_rect.intersect(ui.clip_rect()));
    // Compound rail/disclosure не создаёт собственный widget либо AccessKit node.
    compound_rows::paint_compound_artwork(
        &row_ui,
        prepared.row_rect,
        &prepared.row,
        context.row_style,
    );
    // Text/pending/error layout остаётся статичным во время decorative transition.
    match projection {
        CompoundRuntimeRow::Single { .. } => {
            render_row_content(
                &mut row_ui,
                top_level_index,
                presentation,
                context.row_style,
            );
        }
        CompoundRuntimeRow::CompoundHeader { .. } | CompoundRuntimeRow::CompoundPart { .. } => {
            compound_rows::render_content(
                &mut row_ui,
                top_level_index,
                &prepared.row,
                context.row_style,
            );
        }
    }

    // Единственный full-width response регистрируется после layout всего content.
    let response = ui.interact(
        prepared.row_rect,
        prepared.row_id,
        compound_rows::interaction_sense(projection),
    );
    match projection {
        CompoundRuntimeRow::Single { .. } => {
            // Accessibility сразу читает authoritative active flag, а не moving paint rect.
            let accessibility_text = accessibility_text(top_level_index, presentation);
            response.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::SelectableLabel,
                    ui.is_enabled(),
                    presentation.is_selected(),
                    &accessibility_text,
                )
            });
        }
        CompoundRuntimeRow::CompoundHeader { .. } | CompoundRuntimeRow::CompoundPart { .. } => {
            compound_rows::configure_accessibility(ui, &response, top_level_index, &prepared.row);
        }
    }
    // Explicit focus/Go Current использует существующий egui center contract.
    if context.focus_row == Some(projection.row_id()) {
        response.scroll_to_me(Some(Align::Center));
        response.request_focus();
    }

    // Interaction публикует typed actions; child path не получает structural mutation APIs.
    let mut row_interaction_context = row_interactions::RowInteractionContext {
        model: context.model,
        compound_snapshot: context.compound_snapshot,
        visible_row_index: prepared.row_index,
        state,
        output,
    };
    let interaction = match projection {
        CompoundRuntimeRow::Single {
            entry_id, item_id, ..
        } => {
            row_interactions::handle_row_response(
                ui,
                &response,
                entry_id,
                row_interactions::StructuralRowActivation::Single { item_id },
                0.0,
                &mut row_interaction_context,
            );
            compound_rows::CompoundRowInteractionResult {
                fill: row_fill(context.row_style, presentation, response.hovered()),
                focused: response.has_focus(),
            }
        }
        CompoundRuntimeRow::CompoundHeader { .. } | CompoundRuntimeRow::CompoundPart { .. } => {
            compound_rows::handle_interaction(
                ui,
                &response,
                &prepared.row,
                context.row_style,
                &mut row_interaction_context,
            )
        }
    };
    // Background slot не содержит outline: priority strokes рисуются последним pass.
    ArtworkPainter::new(ui.painter()).playlist_row_background(
        prepared.background_shape,
        prepared.row_rect,
        interaction.fill,
        egui::Stroke::NONE,
    );
    // Tooltip остаётся привязанным к единственному full-width response.
    match projection {
        CompoundRuntimeRow::Single { .. } => {
            egui::Tooltip::for_enabled(&response)
                .width(tooltip_width(response.rect.width()))
                .show(|ui| show_safe_tooltip(ui, presentation));
        }
        CompoundRuntimeRow::CompoundHeader { .. } | CompoundRuntimeRow::CompoundPart { .. } => {
            compound_rows::show_tooltip(&response, &prepared.row);
        }
    }

    // Insertion/focus вычисляются после interaction и откладываются до overlay pass.
    let priority_stroke = priority_row_stroke(
        context.row_style,
        interaction.focused,
        virtualized_drag::marks_row(
            &row_interaction_context.state.drag,
            prepared.row_index,
            projection.structural_entry_id(),
            context.compound_snapshot.visible_row_count(),
        ),
    );
    // Overlay record не хранит widget/Response между кадрами.
    PlaylistRowOverlay {
        row_rect: prepared.row_rect,
        priority_stroke,
    }
}

pub(super) fn row_fill(
    style: PlaylistRowStyle,
    row: &PlaylistVisibleRow,
    hovered: bool,
) -> egui::Color32 {
    // Active fill принадлежит отдельному moving layer под row surfaces.
    match (row.is_selected(), hovered) {
        (true, true) => style.selected_hover_fill,
        (true, false) => style.selected_fill,
        (false, true) => style.hover_fill,
        (false, false) => egui::Color32::TRANSPARENT,
    }
}

/// Stroke priority сохраняет insertion и focus поверх active playback.
fn priority_row_stroke(
    style: PlaylistRowStyle,
    focused: bool,
    insertion_target: bool,
) -> egui::Stroke {
    // Drag insertion является самым важным точным positional intent.
    if insertion_target {
        style.insertion_stroke
    // Keyboard focus остаётся видим поверх moving active outline.
    } else if focused {
        style.focus_stroke
    // Обычная строка не добавляет overlay stroke.
    } else {
        egui::Stroke::NONE
    }
}
