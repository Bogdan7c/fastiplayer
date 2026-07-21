//! UI-only drag lifecycle поверх fixed-height virtualized canonical rows.

use std::collections::HashSet;
use std::sync::Arc;

use egui::{DragAndDrop, Pos2, Rect, Response};
use playlist_core::{MoveItemIntent, PlaylistEntryId, PlaylistItemId};

use super::actions::MoveItems;
use super::{PlaylistAction, PlaylistUiOutput};
use crate::playlist_runtime::{
    CompoundRuntimeViewSnapshot, PlaylistStructuralRevision, PlaylistViewModel, UpdateSelection,
};

const EDGE_ZONE_HEIGHT: f32 = 28.0;
const EDGE_SCROLL_STEP: f32 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaylistDragPayload {
    source_item_id: PlaylistItemId,
    capture_generation: u64,
}

/// Stable canonical insertion intent не содержит virtualized widget/index reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VirtualizedInsertionTarget {
    ToFront,
    ToBack,
    Before(PlaylistEntryId),
}

impl VirtualizedInsertionTarget {
    const fn into_move_intent(self) -> MoveItemIntent {
        match self {
            Self::ToFront => MoveItemIntent::ToFront,
            Self::ToBack => MoveItemIntent::ToBack,
            Self::Before(entry_id) => MoveItemIntent::Before(entry_id),
        }
    }
}

/// Geometry остаётся UI-only; authoritative order живёт в controller/domain.
#[derive(Debug, Default)]
pub(super) struct VirtualizedDragState {
    source_item_id: Option<PlaylistItemId>,
    entry_ids: Arc<[PlaylistEntryId]>,
    structural_revision: Option<PlaylistStructuralRevision>,
    drop_targets: Arc<[VirtualizedInsertionTarget]>,
    capture_generation: u64,
    pointer_position: Option<Pos2>,
    viewport: Option<Rect>,
    scroll_offset: f32,
    insertion_target: Option<VirtualizedInsertionTarget>,
    requested_scroll_offset: Option<f32>,
}

pub(super) fn begin_from_response(
    response: &Response,
    model: &PlaylistViewModel,
    entry_id: PlaylistEntryId,
    item_id: PlaylistItemId,
    row_is_selected: bool,
    state: &mut VirtualizedDragState,
    output: &mut PlaylistUiOutput,
) {
    if !response.drag_started() {
        return;
    }
    state.capture_generation = state.capture_generation.wrapping_add(1).max(1);
    state.source_item_id = Some(item_id);
    state.entry_ids = if row_is_selected {
        model.selected_entry_ids()
    } else {
        output.push_action(PlaylistAction::UpdateSelection(UpdateSelection::Replace {
            entry_id,
            structural_revision: model.structural_revision(),
        }));
        Arc::from([entry_id])
    };
    state.structural_revision = Some(model.structural_revision());
    state.drop_targets = build_drop_targets(model, &state.entry_ids);
    state.pointer_position = response.interact_pointer_pos();
    state.insertion_target = None;
    state.requested_scroll_offset = None;
    DragAndDrop::set_payload(
        &response.ctx,
        PlaylistDragPayload {
            source_item_id: item_id,
            capture_generation: state.capture_generation,
        },
    );
}

/// Применяет edge-scroll только если pointer всё ещё в capture и реально может прокрутить content.
pub(super) fn prepare_scroll_offset(
    ctx: &egui::Context,
    state: &mut VirtualizedDragState,
    item_count: usize,
    row_pitch: f32,
) -> Option<f32> {
    state.source_item_id?;
    if !capture_is_current(ctx, state) {
        clear(ctx, state);
        return None;
    }
    let Some(pointer_position) = ctx.input(|input| input.pointer.latest_pos()) else {
        clear(ctx, state);
        return None;
    };
    let Some(viewport) = state.viewport else {
        return state.requested_scroll_offset.take();
    };
    if !viewport.contains(pointer_position) {
        clear(ctx, state);
        return None;
    }

    let maximum_offset = (item_count as f32 * row_pitch - viewport.height()).max(0.0);
    if let Some(requested_offset) = edge_scroll_offset(
        pointer_position,
        viewport,
        state.scroll_offset,
        maximum_offset,
    ) {
        state.requested_scroll_offset = Some(requested_offset);
        ctx.request_repaint();
    } else {
        state.requested_scroll_offset = None;
    }
    state.requested_scroll_offset.take()
}

/// После layout вычисляет off-screen insertion target по canonical position и завершает drop.
pub(super) fn finish_frame(
    ctx: &egui::Context,
    state: &mut VirtualizedDragState,
    compound_snapshot: &CompoundRuntimeViewSnapshot,
    viewport: Rect,
    scroll_offset: f32,
    row_pitch: f32,
    output: &mut PlaylistUiOutput,
) {
    if state.source_item_id.is_none() {
        return;
    }
    if !capture_is_current(ctx, state) {
        clear(ctx, state);
        return;
    }
    let Some(pointer_position) = ctx.input(|input| input.pointer.latest_pos()) else {
        clear(ctx, state);
        return;
    };
    if !viewport.contains(pointer_position) {
        clear(ctx, state);
        return;
    }

    state.pointer_position = Some(pointer_position);
    state.viewport = Some(viewport);
    state.scroll_offset = scroll_offset;
    state.insertion_target = compound_insertion_target(
        state,
        compound_snapshot,
        pointer_position.y - viewport.top(),
        scroll_offset,
        row_pitch,
    );

    let released = ctx.input(|input| input.pointer.any_released());
    if released {
        if let (Some(target), Some(structural_revision)) =
            (state.insertion_target, state.structural_revision)
            && !state.entry_ids.is_empty()
        {
            output.push_action(PlaylistAction::MoveItems(MoveItems::new(
                Arc::clone(&state.entry_ids),
                target.into_move_intent(),
                structural_revision,
            )));
        }
        clear(ctx, state);
        return;
    }
    if !ctx.input(|input| input.pointer.any_down()) {
        clear(ctx, state);
        return;
    }

    let maximum_offset =
        (compound_snapshot.visible_row_count() as f32 * row_pitch - viewport.height()).max(0.0);
    let can_scroll_up =
        pointer_position.y <= viewport.top() + EDGE_ZONE_HEIGHT && scroll_offset > 0.0;
    let can_scroll_down = pointer_position.y >= viewport.bottom() - EDGE_ZONE_HEIGHT
        && scroll_offset < maximum_offset;
    if can_scroll_up || can_scroll_down {
        ctx.request_repaint();
    }
}

pub(super) fn marks_row(
    state: &VirtualizedDragState,
    row_index: usize,
    row_entry_id: Option<PlaylistEntryId>,
    item_count: usize,
) -> bool {
    match state.insertion_target {
        Some(VirtualizedInsertionTarget::ToFront) => row_index == 0,
        Some(VirtualizedInsertionTarget::ToBack) => row_index + 1 == item_count,
        Some(VirtualizedInsertionTarget::Before(entry_id)) => row_entry_id == Some(entry_id),
        None => false,
    }
}

/// Visible child slots схлопываются в atomic structural boundary compound snapshot-а.
fn compound_insertion_target(
    state: &VirtualizedDragState,
    compound_snapshot: &CompoundRuntimeViewSnapshot,
    pointer_y_in_viewport: f32,
    scroll_offset: f32,
    row_pitch: f32,
) -> Option<VirtualizedInsertionTarget> {
    let visible_slot = insertion_slot(
        compound_snapshot.visible_row_count(),
        pointer_y_in_viewport,
        scroll_offset,
        row_pitch,
    )?;
    let structural_slot = compound_snapshot.structural_insertion_slot(visible_slot);
    state.drop_targets.get(structural_slot).copied()
}

#[cfg(test)]
fn insertion_target(
    state: &VirtualizedDragState,
    item_count: usize,
    pointer_y_in_viewport: f32,
    scroll_offset: f32,
    row_pitch: f32,
) -> Option<VirtualizedInsertionTarget> {
    let insertion_slot =
        insertion_slot(item_count, pointer_y_in_viewport, scroll_offset, row_pitch)?;
    state.drop_targets.get(insertion_slot).copied()
}

/// Pure geometry возвращает bounded visible insertion slot без structural knowledge.
fn insertion_slot(
    item_count: usize,
    pointer_y_in_viewport: f32,
    scroll_offset: f32,
    row_pitch: f32,
) -> Option<usize> {
    if item_count == 0 || !row_pitch.is_finite() || row_pitch <= 0.0 {
        return None;
    }
    let content_y = (scroll_offset + pointer_y_in_viewport).max(0.0);
    Some((((content_y + row_pitch * 0.5) / row_pitch).floor() as usize).min(item_count))
}

/// Один O(N) drag-start precompute исключает selected anchor и per-frame queue scan.
fn build_drop_targets(
    model: &PlaylistViewModel,
    selected_entry_ids: &[PlaylistEntryId],
) -> Arc<[VirtualizedInsertionTarget]> {
    let selected_entry_ids: HashSet<_> = selected_entry_ids.iter().copied().collect();
    let canonical_entry_ids = (0..model.item_count())
        .filter_map(|row_index| model.entry_id_at(row_index))
        .collect::<Vec<_>>();
    let retained_entry_ids = canonical_entry_ids
        .iter()
        .copied()
        .filter(|entry_id| !selected_entry_ids.contains(entry_id))
        .collect::<Vec<_>>();
    let mut retained_before_slot = 0;
    let mut targets = Vec::with_capacity(canonical_entry_ids.len() + 1);
    for insertion_slot in 0..=canonical_entry_ids.len() {
        if insertion_slot > 0
            && !selected_entry_ids.contains(&canonical_entry_ids[insertion_slot - 1])
        {
            retained_before_slot += 1;
        }
        let target = if retained_before_slot == 0 {
            VirtualizedInsertionTarget::ToFront
        } else if retained_before_slot >= retained_entry_ids.len() {
            VirtualizedInsertionTarget::ToBack
        } else {
            VirtualizedInsertionTarget::Before(retained_entry_ids[retained_before_slot])
        };
        targets.push(target);
    }
    targets.into()
}

fn edge_scroll_offset(
    pointer_position: Pos2,
    viewport: Rect,
    current_offset: f32,
    maximum_offset: f32,
) -> Option<f32> {
    let requested_offset = if pointer_position.y <= viewport.top() + EDGE_ZONE_HEIGHT {
        (current_offset - EDGE_SCROLL_STEP).max(0.0)
    } else if pointer_position.y >= viewport.bottom() - EDGE_ZONE_HEIGHT {
        (current_offset + EDGE_SCROLL_STEP).min(maximum_offset)
    } else {
        current_offset
    };
    (requested_offset != current_offset).then_some(requested_offset)
}

fn capture_is_current(ctx: &egui::Context, state: &VirtualizedDragState) -> bool {
    let Some(source_item_id) = state.source_item_id else {
        return true;
    };
    DragAndDrop::payload::<PlaylistDragPayload>(ctx).is_some_and(|payload| {
        payload.source_item_id == source_item_id
            && payload.capture_generation == state.capture_generation
    })
}

/// Row keyboard handler отличает active drag от обычного Escape selection clear.
pub(super) fn is_active(state: &VirtualizedDragState) -> bool {
    state.source_item_id.is_some()
}

/// Escape отменяет только playlist-owned drag и не трогает чужой DnD payload.
pub(super) fn cancel(ctx: &egui::Context, state: &mut VirtualizedDragState) {
    clear(ctx, state);
}

fn clear(ctx: &egui::Context, state: &mut VirtualizedDragState) {
    // Потерянный capture мог означать, что другой UI owner уже заменил global DnD payload.
    // В таком случае playlist очищает только своё локальное состояние и не трогает чужой drag.
    if capture_is_current(ctx, state) {
        DragAndDrop::clear_payload(ctx);
    }
    let capture_generation = state.capture_generation;
    *state = VirtualizedDragState {
        capture_generation,
        ..VirtualizedDragState::default()
    };
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use playlist_core::{
        CachedPlaylistMetadata, LocalLocator, PlaylistItemDraft, PlaylistMediaKind, PlaylistQueue,
    };

    use super::*;

    fn model(item_count: usize) -> PlaylistViewModel {
        let mut queue = PlaylistQueue::new();
        let drafts = (0..item_count).map(|index| {
            PlaylistItemDraft::local(
                LocalLocator::Native(PathBuf::from(format!("{index}.mp3"))),
                None,
                CachedPlaylistMetadata::new(format!("{index}.mp3"), PlaylistMediaKind::Audio),
            )
        });
        queue.append_batch(drafts.collect()).unwrap();
        PlaylistViewModel::for_queue_with_revision(&queue, 1)
    }

    #[test]
    fn canonical_target_covers_first_middle_last_and_offscreen_rows() {
        let model = model(10_000);
        let selected_entry_ids = Arc::from([model.entry_id_at(500).unwrap()]);
        let state = VirtualizedDragState {
            drop_targets: build_drop_targets(&model, &selected_entry_ids),
            ..VirtualizedDragState::default()
        };
        let row_pitch = 40.0;
        assert_eq!(
            insertion_target(&state, model.item_count(), 0.0, 0.0, row_pitch),
            Some(VirtualizedInsertionTarget::ToFront)
        );
        assert_eq!(
            insertion_target(
                &state,
                model.item_count(),
                20.0,
                5_000.0 * row_pitch,
                row_pitch
            ),
            model
                .entry_id_at(5_001)
                .map(VirtualizedInsertionTarget::Before)
        );
        assert_eq!(
            insertion_target(
                &state,
                model.item_count(),
                400.0,
                9_990.0 * row_pitch,
                row_pitch
            ),
            Some(VirtualizedInsertionTarget::ToBack)
        );
    }

    #[test]
    fn discontiguous_group_precompute_never_targets_selected_anchor() {
        let model = model(5);
        let selected_entry_ids: Arc<[PlaylistEntryId]> =
            Arc::from([model.entry_id_at(1).unwrap(), model.entry_id_at(3).unwrap()]);
        let selected_set: HashSet<_> = selected_entry_ids.iter().copied().collect();
        let targets = build_drop_targets(&model, &selected_entry_ids);

        assert_eq!(targets.len(), model.item_count() + 1);
        assert!(targets.iter().all(|target| match target {
            VirtualizedInsertionTarget::Before(entry_id) => !selected_set.contains(entry_id),
            VirtualizedInsertionTarget::ToFront | VirtualizedInsertionTarget::ToBack => true,
        }));
    }

    #[test]
    fn marker_uses_stable_id_not_transient_widget_reference() {
        let model = model(3);
        let middle = model.entry_id_at(1).unwrap();
        let state = VirtualizedDragState {
            insertion_target: Some(VirtualizedInsertionTarget::Before(middle)),
            ..VirtualizedDragState::default()
        };
        assert!(marks_row(&state, 1, Some(middle), 3));
        assert!(!marks_row(
            &state,
            0,
            Some(model.entry_id_at(0).unwrap()),
            3
        ));
    }

    #[test]
    fn edge_scroll_starts_only_in_zone_and_stops_at_leave_or_content_boundary() {
        let viewport = Rect::from_min_max(Pos2::new(0.0, 100.0), Pos2::new(300.0, 300.0));
        assert_eq!(
            edge_scroll_offset(Pos2::new(20.0, 110.0), viewport, 100.0, 500.0),
            Some(86.0)
        );
        assert_eq!(
            edge_scroll_offset(Pos2::new(20.0, 290.0), viewport, 100.0, 500.0),
            Some(114.0)
        );
        assert_eq!(
            edge_scroll_offset(Pos2::new(20.0, 200.0), viewport, 100.0, 500.0),
            None
        );
        assert_eq!(
            edge_scroll_offset(Pos2::new(20.0, 110.0), viewport, 0.0, 500.0),
            None
        );
        assert_eq!(
            edge_scroll_offset(Pos2::new(20.0, 290.0), viewport, 500.0, 500.0),
            None
        );
    }

    #[test]
    fn cleanup_clears_payload_and_all_ephemeral_geometry_without_mutation() {
        let ctx = egui::Context::default();
        let item_id = model(1).item_id_at(0).unwrap();
        let payload = PlaylistDragPayload {
            source_item_id: item_id,
            capture_generation: 7,
        };
        DragAndDrop::set_payload(&ctx, payload);
        let mut state = VirtualizedDragState {
            source_item_id: Some(item_id),
            capture_generation: 7,
            pointer_position: Some(Pos2::new(10.0, 10.0)),
            viewport: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 100.0))),
            scroll_offset: 40.0,
            insertion_target: Some(VirtualizedInsertionTarget::ToBack),
            requested_scroll_offset: Some(54.0),
            ..VirtualizedDragState::default()
        };

        clear(&ctx, &mut state);

        assert!(DragAndDrop::payload::<PlaylistDragPayload>(&ctx).is_none());
        assert_eq!(state.source_item_id, None);
        assert!(state.entry_ids.is_empty());
        assert!(state.drop_targets.is_empty());
        assert_eq!(state.structural_revision, None);
        assert_eq!(state.capture_generation, 7);
        assert_eq!(state.pointer_position, None);
        assert_eq!(state.viewport, None);
        assert_eq!(state.insertion_target, None);
        assert_eq!(state.requested_scroll_offset, None);
    }

    #[test]
    fn lost_capture_preserves_payload_owned_by_another_drag() {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct OtherDragPayload(u64);

        let ctx = egui::Context::default();
        let item_id = model(1).item_id_at(0).unwrap();
        let mut state = VirtualizedDragState {
            source_item_id: Some(item_id),
            capture_generation: 11,
            ..VirtualizedDragState::default()
        };
        DragAndDrop::set_payload(&ctx, OtherDragPayload(29));

        clear(&ctx, &mut state);

        assert_eq!(
            DragAndDrop::payload::<OtherDragPayload>(&ctx).as_deref(),
            Some(&OtherDragPayload(29))
        );
        assert_eq!(state.source_item_id, None);
        assert_eq!(state.capture_generation, 11);
    }
}
