//! Единый privacy-safe status owner под Playlist toolbar.

mod presentation;

use std::time::Duration;

use animation_core::{Easing, SlideTransition};
use egui::{Rect, Response, Sense, Ui, UiBuilder, pos2, vec2};

use crate::playlist_runtime::{PlaylistInteractionModel, PlaylistViewModel};
use crate::ui::animation::UiMotion;

use self::presentation::{
    PlaylistStatusAction, PlaylistStatusPresentation, PlaylistStatusRow, StatusRowKind, StatusTone,
};
use super::{PlaylistUiOutput, PlaylistUiState};

#[cfg(test)]
pub(super) use self::presentation::{navigation_message, save_message};

/// Весь путь открытия или закрытия занимает ровно 180 мс.
const STATUS_TRANSITION_DURATION: Duration = Duration::from_millis(180);
/// Bounded status не может приблизиться к этой высоте; запас нужен только sizing pass.
const STATUS_MEASUREMENT_MAX_HEIGHT_POINTS: f32 = 4_096.0;

/// UI-only transition и последний безопасный snapshot для остаточной отрисовки.
#[derive(Debug)]
pub(super) struct PlaylistStatusAnimationState {
    /// Линейная позиция сохраняет непрерывность при mid-flight reverse.
    transition: SlideTransition,
    /// Snapshot живёт только до полного завершения закрытия.
    retained_presentation: Option<PlaylistStatusPresentation>,
}

impl Default for PlaylistStatusAnimationState {
    fn default() -> Self {
        Self {
            transition: SlideTransition::closed(),
            retained_presentation: None,
        }
    }
}

impl PlaylistStatusAnimationState {
    /// Обновляет target и возвращает owned paint snapshot текущего кадра.
    fn advance(
        &mut self,
        current_presentation: Option<PlaylistStatusPresentation>,
        motion: UiMotion,
        delta_seconds: f32,
    ) -> PlaylistStatusAnimationFrame {
        let authoritative = current_presentation.is_some();
        if let Some(presentation) = current_presentation.as_ref() {
            // Retained copy содержит только уже отформатированные privacy-safe строки.
            self.retained_presentation = Some(presentation.clone());
        }

        self.transition.set_target_open(authoritative);
        let duration_seconds = match motion {
            UiMotion::Standard => STATUS_TRANSITION_DURATION.as_secs_f32(),
            UiMotion::Reduced => 0.0,
        };
        self.transition.advance(delta_seconds, duration_seconds);

        let progress = self.transition.eased_progress(Easing::EaseOutCubic);
        if self.transition.is_fully_closed() {
            // После последнего residual pixel старый snapshot больше не нужен.
            self.retained_presentation = None;
        }
        let presentation = current_presentation.or_else(|| self.retained_presentation.clone());

        PlaylistStatusAnimationFrame {
            presentation,
            progress,
            authoritative,
            needs_repaint: self.transition.is_animating(),
        }
    }
}

/// Все данные одного кадра отделены от persistent UI state.
#[derive(Debug)]
struct PlaylistStatusAnimationFrame {
    /// Current либо residual safe snapshot.
    presentation: Option<PlaylistStatusPresentation>,
    /// Cubic progress задаёт только геометрию, без opacity.
    progress: f32,
    /// Только current snapshot имеет право публиковать action и менять focus.
    authoritative: bool,
    /// Repaint нужен лишь пока finite transition не достиг target.
    needs_repaint: bool,
}

/// Результат invisible sizing pass разделяет content и стандартный separator.
#[derive(Debug, Clone, Copy)]
struct PlaylistStatusLayoutMetrics {
    /// Полная динамическая высота строк с учётом wrapping.
    content_height: f32,
    /// Поточная высота второго стандартного separator-а.
    separator_height: f32,
}

impl PlaylistStatusLayoutMetrics {
    /// Полная раскрытая высота между верхней линией и virtualized rows.
    fn full_height(self) -> f32 {
        self.content_height + self.separator_height
    }

    /// Видимая высота движется монотонно от нуля до полного layout.
    fn visible_height(self, progress: f32) -> f32 {
        self.full_height() * progress.clamp(0.0, 1.0)
    }
}

/// Интерактивен только единственный authoritative render pass.
enum PlaylistStatusRenderAccess<'a> {
    Authoritative {
        state: &'a mut PlaylistUiState,
        output: &'a mut PlaylistUiOutput,
    },
    Actionless,
}

pub(super) fn show_unavailable(ui: &mut Ui) {
    ui.label("Плейлист ещё подключается…");
}

/// Рисует один верхний separator и анимируемый status owner под ним.
pub(super) fn show_status(
    ui: &mut Ui,
    model: &PlaylistViewModel,
    interaction: &PlaylistInteractionModel,
    motion: UiMotion,
    state: &mut PlaylistUiState,
    output: &mut PlaylistUiOutput,
) {
    // Верхняя граница не двигается при появлении или исчезновении сообщений.
    ui.separator();

    let current_presentation = PlaylistStatusPresentation::from_models(model, interaction);
    let delta_seconds = ui.input(|input| input.stable_dt).max(0.0);
    let frame = state
        .status
        .advance(current_presentation, motion, delta_seconds);
    if frame.needs_repaint {
        ui.ctx().request_repaint();
    }

    let Some(presentation) = frame.presentation else {
        return;
    };
    let metrics = measure_status_layout(ui, &presentation);
    let visible_height = metrics.visible_height(frame.progress);
    if visible_height <= f32::EPSILON {
        return;
    }

    let visible_rect = allocate_reveal_rect(ui, visible_height);
    let access = if frame.authoritative {
        PlaylistStatusRenderAccess::Authoritative { state, output }
    } else {
        PlaylistStatusRenderAccess::Actionless
    };
    render_revealed_status(ui, visible_rect, metrics, &presentation, access);
}

/// Резервирует ровно анимируемую высоту без второго неанимируемого item-spacing.
fn allocate_reveal_rect(ui: &mut Ui, visible_height: f32) -> Rect {
    let row_width = ui.available_width().max(0.0);
    let original_vertical_spacing = ui.spacing().item_spacing.y;
    // Верхний separator уже зарезервировал свой стандартный trailing spacing.
    ui.spacing_mut().item_spacing.y = 0.0;
    let (visible_rect, _) = ui.allocate_exact_size(vec2(row_width, visible_height), Sense::hover());
    // Следующие Playlist rows снова используют обычный theme spacing.
    ui.spacing_mut().item_spacing.y = original_vertical_spacing;
    visible_rect
}

/// Disabled sidebar animation copy показывает current status полностью и без side effects.
pub(super) fn show_disabled_copy(
    ui: &mut Ui,
    model: &PlaylistViewModel,
    interaction: &PlaylistInteractionModel,
) {
    // Disabled copy сохраняет ту же постоянную верхнюю границу.
    ui.separator();
    let Some(presentation) = PlaylistStatusPresentation::from_models(model, interaction) else {
        return;
    };

    // Прямой flow render уже полностью раскрыт и не трогает authoritative transition.
    render_presentation(ui, &presentation, PlaylistStatusRenderAccess::Actionless);
    ui.separator();
}

/// Измеряет wrapping и второй separator в отдельном невидимом actionless child UI.
fn measure_status_layout(
    ui: &mut Ui,
    presentation: &PlaylistStatusPresentation,
) -> PlaylistStatusLayoutMetrics {
    let measurement_rect = Rect::from_min_size(
        ui.cursor().left_top(),
        vec2(
            ui.available_width().max(0.0),
            STATUS_MEASUREMENT_MAX_HEIGHT_POINTS,
        ),
    );
    let mut measurement_ui = ui.new_child(
        UiBuilder::new()
            .id_salt("playlist_status_measurement")
            .max_rect(measurement_rect)
            .sizing_pass()
            .invisible(),
    );

    let content_top = measurement_ui.cursor().top();
    render_presentation(
        &mut measurement_ui,
        presentation,
        PlaylistStatusRenderAccess::Actionless,
    );
    let separator_top = measurement_ui.cursor().top();
    measurement_ui.separator();
    let bottom = measurement_ui.cursor().top();

    PlaylistStatusLayoutMetrics {
        content_height: (separator_top - content_top).max(0.0),
        separator_height: (bottom - separator_top).max(0.0),
    }
}

/// Рисует content ровно один раз и ограничивает его текущей раскрытой высотой.
fn render_revealed_status(
    ui: &mut Ui,
    visible_rect: Rect,
    metrics: PlaylistStatusLayoutMetrics,
    presentation: &PlaylistStatusPresentation,
    access: PlaylistStatusRenderAccess<'_>,
) {
    let parent_clip_rect = ui.clip_rect();
    let content_rect = Rect::from_min_size(
        visible_rect.min,
        vec2(visible_rect.width(), metrics.content_height),
    );
    let mut content_ui = ui.new_child(
        UiBuilder::new()
            .id_salt("playlist_status_content")
            .max_rect(content_rect),
    );
    content_ui.set_clip_rect(visible_rect.intersect(parent_clip_rect));
    render_presentation(&mut content_ui, presentation, access);

    // Separator живёт на движущейся нижней границе и не создаёт второй flow allocation.
    let separator_rect = Rect::from_min_size(
        pos2(
            visible_rect.left(),
            visible_rect.bottom() - metrics.separator_height,
        ),
        vec2(visible_rect.width(), metrics.separator_height),
    );
    let mut separator_ui = ui.new_child(
        UiBuilder::new()
            .id_salt("playlist_status_lower_separator")
            .max_rect(separator_rect),
    );
    separator_ui.set_clip_rect(visible_rect.intersect(parent_clip_rect));
    separator_ui.separator();
}

/// Рисует упорядоченные строки без повторного принятия suppression-решений.
fn render_presentation(
    ui: &mut Ui,
    presentation: &PlaylistStatusPresentation,
    mut access: PlaylistStatusRenderAccess<'_>,
) {
    for row in presentation.rows() {
        render_status_row(ui, row, &mut access);
    }
}

/// Рисует одну строку и связывает action только с её authoritative кнопкой.
fn render_status_row(
    ui: &mut Ui,
    row: &PlaylistStatusRow,
    access: &mut PlaylistStatusRenderAccess<'_>,
) {
    if let Some(action) = row.action() {
        ui.horizontal_wrapped(|ui| {
            if let Some(message) = row.text() {
                let _ = render_status_message(ui, message, row.tone(), row.kind());
            }
            let _ = render_status_action(ui, action, access);
        });
        return;
    }

    let Some(message) = row.text() else {
        return;
    };
    let response = render_status_message(ui, message, row.tone(), row.kind());
    if matches!(row.kind(), StatusRowKind::Tombstone)
        && let PlaylistStatusRenderAccess::Authoritative { state, .. } = access
        && state.take_tombstone_request()
    {
        response.scroll_to_me(Some(egui::Align::Center));
        response.request_focus();
    }
}

/// Рисует themed label; Loading добавляет spinner, но не меняет текст.
fn render_status_message(
    ui: &mut Ui,
    message: &str,
    tone: StatusTone,
    kind: StatusRowKind,
) -> Response {
    if matches!(kind, StatusRowKind::Loading) {
        return ui
            .horizontal_wrapped(|ui| {
                ui.spinner();
                render_toned_label(ui, message, tone, StatusRowKind::Normal)
            })
            .inner;
    }
    render_toned_label(ui, message, tone, kind)
}

/// Применяет semantic RichText и theme-owned warning/error foreground.
fn render_toned_label(
    ui: &mut Ui,
    message: &str,
    tone: StatusTone,
    kind: StatusRowKind,
) -> Response {
    let mut text = egui::RichText::new(message);
    text = match kind {
        StatusRowKind::Weak => text.weak(),
        StatusRowKind::Small => text.small(),
        StatusRowKind::Tombstone => text.strong(),
        StatusRowKind::Normal | StatusRowKind::Loading => text,
    };
    text = match tone {
        StatusTone::Normal => text,
        StatusTone::Warning => text.color(ui.visuals().warn_fg_color),
        StatusTone::Error => text.color(ui.visuals().error_fg_color),
    };

    let label = egui::Label::new(text).wrap();
    if matches!(kind, StatusRowKind::Tombstone) {
        ui.add(label.sense(Sense::focusable_noninteractive()))
    } else {
        ui.add(label)
    }
}

/// Публикует action только из current authoritative presentation.
fn render_status_action(
    ui: &mut Ui,
    action: PlaylistStatusAction,
    access: &mut PlaylistStatusRenderAccess<'_>,
) -> Response {
    match access {
        PlaylistStatusRenderAccess::Authoritative { output, .. } => {
            let mut response = ui.button(action.button_label());
            if let Some(tooltip) = action.tooltip() {
                response = response.on_hover_text(tooltip);
            }
            if response.clicked() {
                output.push_action(action.into_playlist_action());
            }
            response
        }
        PlaylistStatusRenderAccess::Actionless => {
            // Residual/measurement/sidebar copies сохраняют paint, но не action/accessibility action.
            ui.add_enabled(false, egui::Button::new(action.button_label()))
        }
    }
}

#[cfg(test)]
mod tests;
