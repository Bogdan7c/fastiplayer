//! Поверхность жестов поверх свободной области видео: перетаскивание слоя, угловые маркеры,
//! колесо и проглатывание кликов.
//!
//! Поверхность - `egui::Area` порядка `Middle`: она выше панелей (фон) и ниже панели настроек,
//! меню и уведомлений (`Foreground`). Курсор над панелью поэтому не доходит до поверхности, а
//! колесо над панелью не меняет размер текста.

use egui::{CursorIcon, Id, Order, Response, Sense, Ui};
use ui_artwork_egui::{
    ArtworkPainter, SubtitleEditFrameHandle, SubtitleEditFramePaintState,
    subtitle_edit_frame_handle_rects,
};

use super::gesture;
use super::{SubtitleAppearanceEditAction, SubtitleAppearanceEditInput};
use crate::state::AppearanceEdit;

/// Стабильный id области жестов.
const SURFACE_AREA_ID: &str = "subtitle_appearance_edit_surface";

/// Рисует рамку и собирает намерения, которые дали жесты этого кадра.
pub(super) fn show(
    ui: &mut Ui,
    input: SubtitleAppearanceEditInput<'_>,
) -> Vec<SubtitleAppearanceEditAction> {
    if !input.video_rect.is_positive() {
        return Vec::new();
    }
    egui::Area::new(Id::new(SURFACE_AREA_ID))
        .order(Order::Middle)
        .fixed_pos(input.video_rect.min)
        // Сама область не ловит клики: жесты принимают виджеты внутри неё.
        .sense(Sense::hover())
        .show(ui.ctx(), |ui| collect_gestures(ui, input))
        .inner
}

/// Регистрирует виджеты жестов, рисует рамку и возвращает правки.
fn collect_gestures(
    ui: &mut Ui,
    input: SubtitleAppearanceEditInput<'_>,
) -> Vec<SubtitleAppearanceEditAction> {
    let SubtitleAppearanceEditInput {
        view,
        video_rect,
        style,
    } = input;
    let pixels_per_point = ui.ctx().pixels_per_point();
    let base_id = ui.id();
    let mut edits = Vec::new();

    // Область занимает всю свободную часть видео: так она получает hover колеса.
    ui.allocate_rect(video_rect, Sense::hover());
    // Клики по видео гасим здесь (двойной клик не должен переключать полноэкранный режим).
    // Без `FOCUSABLE`: клик по видео не забирает клавиатурный фокус и не мешает Esc.
    let _swallowed_clicks = ui.interact(video_rect, base_id.with("clicks"), Sense::CLICK);

    let frame = view
        .layer_rect
        .and_then(|layer| gesture::frame_rect_points(layer, pixels_per_point, style.frame_padding));
    let mut highlighted_handle = None;
    if let Some(frame) = frame {
        // Перетаскивание внутри рамки - сдвиг слоя. Регистрируется раньше маркеров, поэтому
        // маркеры (позже = выше) перехватывают углы.
        let move_response = ui.interact(frame, base_id.with("move"), Sense::DRAG);
        if move_response.drag_started() {
            // Отсчёт переноса - от места, где слой виден, иначе рамка «залипает» (см. владельца).
            edits.push(SubtitleAppearanceEditAction::StartMove);
        }
        if move_response.dragged()
            && let Some(picture) = view.picture_rect
            && let Some(edit) =
                gesture::move_edit(move_response.drag_delta(), pixels_per_point, picture)
        {
            edits.push(SubtitleAppearanceEditAction::Edit(edit));
        }
        set_cursor(ui, &move_response, CursorIcon::Grab, CursorIcon::Grabbing);

        for (handle, hit_rect) in subtitle_edit_frame_handle_rects(frame, style.handle_hit_extent) {
            let response = ui.interact(hit_rect, base_id.with(handle), Sense::DRAG);
            if response.dragged()
                && let Some(edit) = corner_drag_edit(ui, &response, frame.center())
            {
                edits.push(SubtitleAppearanceEditAction::Edit(edit));
            }
            let resize_icon = resize_cursor(handle);
            set_cursor(ui, &response, resize_icon, resize_icon);
            if highlighted_handle.is_none() && (response.dragged() || response.hovered()) {
                highlighted_handle = Some(handle);
            }
        }
        ArtworkPainter::new(ui.painter()).subtitle_edit_frame(
            frame,
            SubtitleEditFramePaintState { highlighted_handle },
            style.frame,
        );
    }

    // Колесо над любой частью видео (поверх панели настроек - нет): размер текста.
    if ui.rect_contains_pointer(video_rect) {
        edits.extend(
            gesture::wheel_scale_edit(take_scroll_points(ui.ctx()))
                .map(SubtitleAppearanceEditAction::Edit),
        );
    }
    edits
}

/// Угловой жест: множитель по смещению указателя относительно центра рамки.
fn corner_drag_edit(
    ui: &Ui,
    response: &Response,
    frame_center: egui::Pos2,
) -> Option<AppearanceEdit> {
    let current_pointer = ui.ctx().pointer_interact_pos()?;
    let previous_pointer = current_pointer - response.drag_delta();
    gesture::corner_scale_edit(frame_center, previous_pointer, current_pointer)
}

/// Курсор: `drag_icon` во время перетаскивания, `hover_icon` над виджетом.
fn set_cursor(ui: &Ui, response: &Response, hover_icon: CursorIcon, drag_icon: CursorIcon) {
    if response.dragged() {
        ui.ctx().set_cursor_icon(drag_icon);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(hover_icon);
    }
}

/// Курсор-стрелка по диагонали, на которой лежит угол.
fn resize_cursor(handle: SubtitleEditFrameHandle) -> CursorIcon {
    match handle {
        SubtitleEditFrameHandle::TopLeft | SubtitleEditFrameHandle::BottomRight => {
            CursorIcon::ResizeNwSe
        }
        SubtitleEditFrameHandle::TopRight | SubtitleEditFrameHandle::BottomLeft => {
            CursorIcon::ResizeNeSw
        }
    }
}

/// Забирает вертикальную прокрутку кадра и обнуляет её, чтобы никто ниже по кадру не
/// прокрутил то же самое (как в `video_surface_input/wheel.rs`).
fn take_scroll_points(egui_ctx: &egui::Context) -> f32 {
    egui_ctx.input_mut(|input| {
        let delta = input.smooth_scroll_delta().y;
        if delta != 0.0 {
            input.smooth_scroll_delta.y = 0.0;
        }
        delta
    })
}
