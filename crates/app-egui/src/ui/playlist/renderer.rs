//! Fixed-height `show_rows` renderer и stable viewport anchor.

use egui::layers::ShapeIdx;
use egui::{Align, Layout, Sense, TextWrapMode, WidgetInfo, WidgetType};
use playlist_core::{PlaylistItemId, PlaylistMediaKind};
use ui_artwork_egui::{ArtworkPainter, MediaKindGlyph};

use super::{
    PlaylistAction, PlaylistUiOutput, PlaylistUiState, ViewportAnchor,
    active_accent::{
        AuthoritativeViewport, AuthoritativeViewportInput, BeginFrameInput, FinishFrameInput,
        ViewportControl,
    },
    row_interactions, virtualized_drag,
};
use crate::playlist_runtime::{
    ClearSelectionCursor, PlaylistViewModel, PlaylistVisibleRow, UpdateSelection,
};
use crate::ui::animation::UiMotion;
use crate::ui::skin::PlaylistRowStyle;

pub(super) const ROW_HEIGHT: f32 = 34.0;
pub(super) const TOOLTIP_MAX_WIDTH: f32 = 320.0;
pub(super) const INDEX_WIDTH: f32 = 38.0;
pub(super) const MEDIA_KIND_WIDTH: f32 = 16.0;
const DURATION_WIDTH: f32 = 50.0;
const BADGES_WIDTH: f32 = 58.0;
/// Зарезервированная badge-ячейка moving `Play` glyph-а.
const ACTIVE_GLYPH_CELL_WIDTH: f32 = 13.0;
/// Большой frame stall не должен мгновенно проматывать UI transition.
const MAX_ANIMATION_DELTA_SECONDS: f32 = 0.1;
/// Минимальная скорость, считающаяся пользовательской kinetic-прокруткой.
const MANUAL_SCROLL_VELOCITY_EPSILON: f32 = 0.01;

/// Read-only row dependencies группируются, чтобы boundary не превращался в список параметров.
#[derive(Clone, Copy)]
struct PlaylistRowRenderContext<'a> {
    /// Authoritative immutable snapshot для selection, drag targets и stable IDs.
    model: &'a PlaylistViewModel,
    /// Skin-owned токены не читаются из глобальных egui visuals.
    row_style: PlaylistRowStyle,
    /// Одноразовый focus intent разрешён до начала virtualized row pass.
    focus_item: Option<PlaylistItemId>,
}

/// Первый visible pass резервирует geometry и background slots без content paint.
struct PreparedPlaylistRow {
    /// Canonical индекс нужен interaction/accessibility и drag target.
    row_index: usize,
    /// Bounded visible read model переносится без повторного snapshot lookup.
    row: PlaylistVisibleRow,
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
    if model.is_empty() {
        state.observed_structural_revision = Some(model.structural_revision());
        state.viewport_anchor = None;
        state
            .active_accent
            .observe_empty(model.structural_revision().get());
        return;
    }

    let row_pitch = ROW_HEIGHT + ui.spacing().item_spacing.y;
    let go_current_item = match state.take_go_current() {
        Some(crate::playlist_runtime::PlaylistGoCurrentTarget::Row(item_id)) => Some(item_id),
        Some(crate::playlist_runtime::PlaylistGoCurrentTarget::Tombstone) | None => None,
    };
    let focus_item = state.take_row_focus().or(go_current_item);
    let drag_was_active = virtualized_drag::is_active(&state.drag);
    let drag_offset = virtualized_drag::prepare_scroll_offset(
        ui.ctx(),
        &mut state.drag,
        model.item_count(),
        row_pitch,
    );
    let manual_scroll_input = state.active_accent.has_manual_scroll_input(ui.ctx());
    let explicit_viewport_intent = focus_item.is_some();
    let manual_viewport_override =
        manual_scroll_input || drag_was_active || explicit_viewport_intent;
    let delta_seconds = ui
        .input(|input| input.stable_dt)
        .clamp(0.0, MAX_ANIMATION_DELTA_SECONDS);
    let active_item_id = model.active_item_id();
    let accent_scroll_offset = state.active_accent.begin_frame(BeginFrameInput {
        active_item_id,
        structural_revision: model.structural_revision().get(),
        target_row_index: active_item_id.and_then(|item_id| model.row_index(item_id)),
        item_count: model.item_count(),
        row_pitch,
        motion,
        delta_seconds,
        manual_viewport_override,
    });
    let anchored_offset = focus_item
        .and_then(|item_id| model.row_index(item_id))
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
        model.item_count(),
        |rows_ui, visible_range| {
            rows_ui.set_min_width(0.0);
            let visible_rows = model.visible_rows(visible_range.clone());
            let row_context = PlaylistRowRenderContext {
                model,
                row_style,
                focus_item,
            };
            render_visible_rows_in_two_passes(
                rows_ui,
                row_context,
                visible_range.start,
                visible_rows,
                active_item_id,
                state,
                output,
            )
        },
    );
    clear_selection_from_empty_area(
        ui,
        model,
        scroll_output.inner_rect,
        scroll_output.state.offset.y,
        row_pitch,
        output,
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
                    item_count: model.item_count(),
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
    update_viewport_anchor(model, state, row_pitch, scroll_output.state.offset.y);
}

/// Выполняет два прохода только по bounded visible rows.
fn render_visible_rows_in_two_passes(
    ui: &mut egui::Ui,
    context: PlaylistRowRenderContext<'_>,
    first_visible_row_index: usize,
    visible_rows: Vec<PlaylistVisibleRow>,
    active_item_id: Option<PlaylistItemId>,
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
        // Stable Item ID изолирует auto IDs duplicate locator rows.
        let item_id_value = row.item_id().expose_value_for_persistence();
        // Scope нужен только для стабильного row interaction ID первого pass.
        let prepared_row = ui
            .push_id(("playlist_row", item_id_value), |row_ui| {
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
    let active_target_rect = active_item_id.and_then(|target_item_id| {
        prepared_rows
            .iter()
            .find(|prepared| prepared.row.item_id() == target_item_id)
            .map(|prepared| prepared.row_rect)
    });
    // Overlay plan имеет ту же bounded ёмкость.
    let mut row_overlays = Vec::with_capacity(prepared_rows.len());

    // Второй pass рисует content и регистрирует interaction по готовой геометрии.
    for prepared_row in &prepared_rows {
        // Demand hint публикуется один раз на реально отрисованную строку.
        output.record_visible(prepared_row.row.item_id());
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
        // Active outline располагается поверх content, но ниже focus/insertion.
        artwork.playlist_row_outline(active_rect, row_style.active_stroke);
        // Glyph движется тем же rect sample и не создаёт AccessKit node.
        artwork.active_track_glyph(
            active_glyph_cell(active_rect),
            row_style.active_stroke.color,
        );
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
    model: &PlaylistViewModel,
    viewport: egui::Rect,
    scroll_offset: f32,
    row_pitch: f32,
    output: &mut PlaylistUiOutput,
) {
    let content_bottom = viewport.top() + model.item_count() as f32 * row_pitch - scroll_offset;
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

fn render_prepared_row(
    ui: &mut egui::Ui,
    context: PlaylistRowRenderContext<'_>,
    prepared: &PreparedPlaylistRow,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) -> PlaylistRowOverlay {
    // Второй pass использует готовую geometry и не продвигает parent layout повторно.
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt((
                "playlist_row_content",
                prepared.row.item_id().expose_value_for_persistence(),
            ))
            .max_rect(prepared.row_rect)
            .layout(Layout::left_to_right(Align::Center)),
    );
    // Content клипуется точным row rect и текущим ScrollArea clip.
    row_ui.set_clip_rect(prepared.row_rect.intersect(ui.clip_rect()));
    // Text/pending/error layout остаётся статичным во время decorative transition.
    render_row_content(&mut row_ui, prepared.row_index, &prepared.row);

    // Единственный full-width response регистрируется после layout всего content.
    let response = ui.interact(prepared.row_rect, prepared.row_id, Sense::click_and_drag());
    // Accessibility сразу читает authoritative active flag, а не moving paint rect.
    let accessibility_text = accessibility_text(prepared.row_index, &prepared.row);
    // Selectable node остаётся единственным AccessKit элементом строки.
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::SelectableLabel,
            ui.is_enabled(),
            prepared.row.is_selected(),
            &accessibility_text,
        )
    });
    // Explicit focus/Go Current использует существующий egui center contract.
    if context.focus_item == Some(prepared.row.item_id()) {
        response.scroll_to_me(Some(Align::Center));
        response.request_focus();
    }

    // Hover/selection fill расположен над active fill, но под content.
    let row_fill = row_fill(context.row_style, &prepared.row, response.hovered());
    // Background slot не содержит outline: priority strokes рисуются последним pass.
    ArtworkPainter::new(ui.painter()).playlist_row_background(
        prepared.background_shape,
        prepared.row_rect,
        row_fill,
        egui::Stroke::NONE,
    );

    // Interaction mapping по-прежнему публикует только typed post-render actions.
    row_interactions::handle_row_response(
        ui,
        &response,
        context.model,
        prepared.row_index,
        prepared.row.item_id(),
        state,
        output,
    );
    // Tooltip остаётся привязанным к единственному full-width response.
    egui::Tooltip::for_enabled(&response)
        .width(tooltip_width(response.rect.width()))
        .show(|ui| show_safe_tooltip(ui, &prepared.row));

    // Insertion/focus вычисляются после interaction и откладываются до overlay pass.
    let priority_stroke = priority_row_stroke(
        context.row_style,
        response.has_focus(),
        virtualized_drag::marks_row(
            &state.drag,
            prepared.row_index,
            prepared.row.item_id(),
            context.model.item_count(),
        ),
    );
    // Overlay record не хранит widget/Response между кадрами.
    PlaylistRowOverlay {
        row_rect: prepared.row_rect,
        priority_stroke,
    }
}

fn row_fill(style: PlaylistRowStyle, row: &PlaylistVisibleRow, hovered: bool) -> egui::Color32 {
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

fn render_row_content(ui: &mut egui::Ui, row_index: usize, row: &PlaylistVisibleRow) {
    ui.add_sized(
        [INDEX_WIDTH, ROW_HEIGHT],
        egui::Label::new(egui::RichText::new(format!("{}.", row_index + 1)).weak())
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
    render_media_kind_icon(ui, row.media_kind());

    let trailing_width = DURATION_WIDTH + BADGES_WIDTH;
    let spacing_width = ui.spacing().item_spacing.x * 2.0;
    let title_width = (ui.available_width() - trailing_width - spacing_width).max(24.0);
    ui.add_sized(
        [title_width, ROW_HEIGHT],
        egui::Label::new(egui::RichText::new(row.display_title()).strong())
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
    ui.add_sized(
        [DURATION_WIDTH, ROW_HEIGHT],
        egui::Label::new(format_duration(row.duration()))
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
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
            // Active glyph рисуется отдельным decorative overlay, поэтому layout всегда стабилен.
            ui.add_space(ACTIVE_GLYPH_CELL_WIDTH);
            if row.is_pending() {
                ui.add_sized([14.0, 14.0], egui::Spinner::new());
            } else {
                ui.add_space(14.0);
            }
            if row.runtime_error().is_some() {
                ui.add(
                    egui::Label::new(egui::RichText::new("!").color(ui.visuals().error_fg_color))
                        .selectable(false),
                );
            } else {
                ui.add_space(8.0);
            }
        },
    );
}

/// Возвращает badge-ячейку moving glyph-а внутри full-width row rect.
fn active_glyph_cell(row_rect: egui::Rect) -> egui::Rect {
    // Badge group прижимается к правому краю после duration column.
    let badge_left = (row_rect.right() - BADGES_WIDTH).max(row_rect.left());
    // Узкая очередь безопасно уменьшает ячейку вместо выхода за row bounds.
    let glyph_width = ACTIVE_GLYPH_CELL_WIDTH.min(row_rect.right() - badge_left);
    // Полная высота строки даёт artwork owner-у стабильный центр.
    egui::Rect::from_min_size(
        egui::pos2(badge_left, row_rect.top()),
        egui::vec2(glyph_width.max(0.0), row_rect.height()),
    )
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
