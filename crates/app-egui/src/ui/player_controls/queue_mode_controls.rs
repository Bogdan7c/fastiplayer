//! Persistent Shuffle/Repeat controls: authoritative state, layout, input и анимации.
//!
//! `PlaylistRuntime` подтверждает режимы, этот модуль владеет только UI intent,
//! hit-testing, accessibility и преобразованием style tokens в нейтральный paint-state.

use egui::{
    Color32, Id, Key, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType, pos2,
};
use playlist_core::RepeatMode;
use ui_artwork_egui::{ArtworkPainter, QueueModeControlStyle, QueueModeGlyph, QueueModePaintState};

use crate::playlist_runtime::PlaylistTransportUiModel;
use crate::ui::skin::{ControlsStyle, PersistentControlStyle};

use super::{ControlAction, TransportControlAction};

/// Hover плавно меняет цвет за 120 миллисекунд.
const HOVER_TRANSITION_SECONDS: f32 = 0.120;
/// Authoritative active snapshot плавно меняет цвет за 160 миллисекунд.
const ACTIVE_TRANSITION_SECONDS: f32 = 0.160;
/// Pressed surface и content scale реагируют за 80 миллисекунд.
const PRESSED_TRANSITION_SECONDS: f32 = 0.080;
/// Обычный press уменьшает только содержимое, не затрагивая layout/hit-area.
const PRESSED_CONTENT_SCALE: f32 = 0.97;

/// Стабильная геометрия внешних mode-controls для одного кадра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct QueueModeControlLayout {
    /// Shuffle зеркально расположен слева от Play/Pause.
    pub(super) shuffle_rect: Rect,
    /// Repeat является первичным внешним якорем справа.
    pub(super) repeat_rect: Rect,
}

/// Конкретная постоянная кнопка определяет glyph, label и точный следующий intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueModeControl {
    /// Toggle перемешивания без изменения порядка строк.
    Shuffle,
    /// Циклическое переключение Off → All → One → Off.
    Repeat,
}

impl QueueModeControl {
    /// Stable suffix сохраняет анимацию и focus между immediate-mode кадрами.
    const fn id_suffix(self) -> &'static str {
        match self {
            Self::Shuffle => "queue_mode_shuffle",
            Self::Repeat => "queue_mode_repeat",
        }
    }

    /// Подтверждённое selected-состояние приходит только из runtime snapshot.
    fn selected(self, model: &PlaylistTransportUiModel) -> bool {
        match self {
            Self::Shuffle => model.shuffle_enabled,
            Self::Repeat => model.repeat_mode != RepeatMode::StopAtEnd,
        }
    }

    /// Нейтральный artwork glyph не раскрывает crate-у playlist типы.
    const fn glyph(self, model: &PlaylistTransportUiModel) -> QueueModeGlyph {
        match self {
            Self::Shuffle => QueueModeGlyph::Shuffle,
            Self::Repeat => match model.repeat_mode {
                RepeatMode::StopAtEnd | RepeatMode::RepeatQueue => QueueModeGlyph::Repeat,
                RepeatMode::RepeatOne => QueueModeGlyph::RepeatOne,
            },
        }
    }

    /// Русская accessibility-метка сообщает текущее состояние и следующее действие.
    const fn accessible_label(self, model: &PlaylistTransportUiModel) -> &'static str {
        match self {
            Self::Shuffle if model.shuffle_enabled => {
                "Перемешивание включено. Выключить перемешивание"
            }
            Self::Shuffle => "Перемешивание выключено. Включить перемешивание",
            Self::Repeat => match model.repeat_mode {
                RepeatMode::StopAtEnd => "Повтор выключен. Включить повтор очереди",
                RepeatMode::RepeatQueue => "Повтор очереди включён. Включить повтор одного трека",
                RepeatMode::RepeatOne => "Повтор одного трека включён. Выключить повтор",
            },
        }
    }

    /// Клик несёт точное следующее значение, а не неоднозначную команду toggle/cycle.
    const fn action(self, model: &PlaylistTransportUiModel) -> TransportControlAction {
        match self {
            Self::Shuffle => TransportControlAction::SetShuffleEnabled {
                enabled: !model.shuffle_enabled,
            },
            Self::Repeat => TransportControlAction::SetRepeatMode {
                mode: next_repeat_mode(model.repeat_mode),
            },
        }
    }
}

/// Возвращает точный следующий Repeat mode для согласованного трёхшагового цикла.
const fn next_repeat_mode(current_mode: RepeatMode) -> RepeatMode {
    match current_mode {
        RepeatMode::StopAtEnd => RepeatMode::RepeatQueue,
        RepeatMode::RepeatQueue => RepeatMode::RepeatOne,
        RepeatMode::RepeatOne => RepeatMode::StopAtEnd,
    }
}

/// Ставит Repeat от центра, сжимает расстояние по статическим краям и зеркалит Shuffle.
pub(super) fn control_layout(
    playback_button_rect: Rect,
    open_file_button_rect: Rect,
    fullscreen_button_rect: Rect,
    controls_style: ControlsStyle,
    item_spacing: f32,
) -> QueueModeControlLayout {
    // Обе кнопки используют ту же 32-point hit-area, что Previous/Next.
    let button_size = Vec2::splat(controls_style.transport_button_size);
    // Половина hit-area нужна для ограничения внешней границы по соседним controls.
    let button_half_width = button_size.x * 0.5;
    // Необходимый внешний зазор не меньше общего item spacing и skin-owned 12 points.
    let external_gap = item_spacing.max(controls_style.queue_mode_neighbor_gap);
    // Правый предел зависит только от статического Fullscreen, а не от rate progress.
    let right_distance_limit = fullscreen_button_rect.left()
        - external_gap
        - button_half_width
        - playback_button_rect.center().x;
    // Левый предел зависит только от статического Open и является симметричным constraint.
    let left_distance_limit = playback_button_rect.center().x
        - (open_file_button_rect.right() + external_gap + button_half_width);
    // На широком окне сохраняются 156 points; на узком расстояние уменьшается симметрично.
    let resolved_center_distance = controls_style
        .queue_mode_button_center_distance
        .min(right_distance_limit)
        .min(left_distance_limit)
        .max(0.0);
    // Repeat вычисляется первым согласно архитектурному layout-контракту.
    let repeat_center = pos2(
        playback_button_rect.center().x + resolved_center_distance,
        playback_button_rect.center().y,
    );
    let repeat_rect = Rect::from_center_size(repeat_center, button_size);
    // Shuffle зеркалит уже разрешённый Repeat относительно неизменного центра Play/Pause.
    let shuffle_center = pos2(
        playback_button_rect.center().x - resolved_center_distance,
        playback_button_rect.center().y,
    );
    let shuffle_rect = Rect::from_center_size(shuffle_center, button_size);

    QueueModeControlLayout {
        shuffle_rect,
        repeat_rect,
    }
}

/// Рисует обе кнопки и добавляет только точные typed intents после interaction.
pub(super) fn render(
    ui: &mut Ui,
    layout: QueueModeControlLayout,
    model: &PlaylistTransportUiModel,
    controls_style: ControlsStyle,
    reduced_motion: bool,
    actions: &mut Vec<ControlAction>,
) {
    let _ = render_control(
        ui,
        layout.shuffle_rect,
        QueueModeControl::Shuffle,
        model,
        controls_style,
        reduced_motion,
        actions,
    );
    let _ = render_control(
        ui,
        layout.repeat_rect,
        QueueModeControl::Repeat,
        model,
        controls_style,
        reduced_motion,
        actions,
    );
}

/// Создаёт focusable custom button с selected accessibility semantics.
fn render_control(
    ui: &mut Ui,
    rect: Rect,
    control: QueueModeControl,
    model: &PlaylistTransportUiModel,
    controls_style: ControlsStyle,
    reduced_motion: bool,
    actions: &mut Vec<ControlAction>,
) -> Response {
    // Stable Id отделяет focus/animation двух кнопок и не зависит от порядка соседних widgets.
    let widget_id = ui.make_persistent_id(control.id_suffix());
    // Disabled UI блокирует pointer/keyboard activation, сохраняя selected metadata.
    let response = ui
        .add_enabled_ui(model.queue_modes_enabled, |ui| {
            // Sense::click является focusable и даёт Space/Enter semantics через egui.
            let response = ui.interact(rect, widget_id, Sense::click());
            // AccessKit получает Button + selected/toggled, включая disabled active state.
            response.widget_info(|| {
                WidgetInfo::selected(
                    WidgetType::Button,
                    ui.is_enabled(),
                    control.selected(model),
                    control.accessible_label(model),
                )
            });
            // Pointer click не оставляет persistent keyboard outline после завершения действия.
            if response.clicked() && response.interact_pointer_pos().is_some() {
                response.surrender_focus();
            }
            // Interaction owner преобразует runtime snapshot и response в нейтральный paint-state.
            paint_control(
                ui,
                rect,
                control,
                model,
                controls_style,
                reduced_motion,
                &response,
            );
            response
        })
        .inner
        // Glyph-only кнопка повторяет полную текущую/следующую accessibility формулировку.
        .on_hover_text(control.accessible_label(model))
        // Disabled tooltip объясняет именно временную недоступность mutation boundary.
        .on_disabled_hover_text("Режим очереди сейчас нельзя изменить");

    // Один authoritative snapshot порождает один exact intent; optimistic state не записывается.
    if response.clicked() {
        actions.push(ControlAction::Transport(control.action(model)));
    }
    // Response нужен focused tests и остаётся полезным будущему composition-owner-у.
    response
}

/// Анимирует только paint-параметры; layout и hit-area остаются неизменными.
fn paint_control(
    ui: &Ui,
    rect: Rect,
    control: QueueModeControl,
    model: &PlaylistTransportUiModel,
    controls_style: ControlsStyle,
    reduced_motion: bool,
    response: &Response,
) {
    // Stable производные Id позволяют каждому переходу независимо реверсироваться.
    let hover_progress = animated_bool(
        ui,
        response.id.with("hover"),
        response.hovered(),
        HOVER_TRANSITION_SECONDS,
    );
    let active_progress = animated_bool(
        ui,
        response.id.with("active"),
        control.selected(model),
        ACTIVE_TRANSITION_SECONDS,
    );
    // Pointer и keyboard press используют одну surface/scale анимацию.
    let keyboard_pressed = response.has_focus()
        && ui.input(|input| input.key_down(Key::Space) || input.key_down(Key::Enter));
    let pressed_progress = animated_bool(
        ui,
        response.id.with("pressed"),
        response.is_pointer_button_down_on() || keyboard_pressed,
        PRESSED_TRANSITION_SECONDS,
    );
    // Все конкретные цвета приходят из общего persistent-control style.
    let paint_state = resolve_paint_state(
        controls_style.persistent_control,
        ui.is_enabled(),
        response.has_focus(),
        reduced_motion,
        hover_progress,
        active_progress,
        pressed_progress,
    );
    // Artwork boundary получает только нейтральные glyph/state/style.
    ArtworkPainter::new(ui.painter()).queue_mode_control(
        rect,
        control.glyph(model),
        paint_state,
        QueueModeControlStyle {
            icon_extent: controls_style.transport_button_icon_extent,
            glyph_stroke_width: controls_style.queue_mode_glyph_stroke_width,
            focus_outline: Stroke::new(
                controls_style.queue_mode_focus_outline_width,
                controls_style.persistent_control.focus_outline,
            ),
            focus_inset: controls_style.queue_mode_focus_inset,
        },
    );
}

/// Делегирует timing/reversal egui и не просит repaint после достижения target.
fn animated_bool(ui: &Ui, animation_id: Id, target: bool, duration_seconds: f32) -> f32 {
    ui.ctx().animate_bool_with_time_and_easing(
        animation_id,
        target,
        duration_seconds,
        egui::emath::easing::cubic_in_out,
    )
}

/// Преобразует animation progress в один paint-state без знания конкретной кнопки.
fn resolve_paint_state(
    style: PersistentControlStyle,
    enabled: bool,
    focus_visible: bool,
    reduced_motion: bool,
    hover_progress: f32,
    active_progress: f32,
    pressed_progress: f32,
) -> QueueModePaintState {
    // Disabled foreground применяется сразу, но active surface продолжает показывать selected state.
    let foreground = if enabled {
        // Hover сначала осветляет idle glyph.
        let hovered_foreground = lerp_color(
            style.foreground_idle,
            style.foreground_hover,
            hover_progress,
        );
        // Active transition приводит оба hover-варианта к общему яркому foreground.
        lerp_color(hovered_foreground, style.foreground_active, active_progress)
    } else {
        style.foreground_disabled
    };
    // Неактивный hover и активная поверхность интерполируются независимо.
    let inactive_surface = lerp_color(style.surface_idle, style.surface_hover, hover_progress);
    let active_surface = lerp_color(
        style.surface_active,
        style.surface_active_hover,
        hover_progress,
    );
    // Authoritative active progress допускает быстрый реверс без визуального скачка.
    let persistent_surface = lerp_color(inactive_surface, active_surface, active_progress);
    // Pressed token усиливает поверхность, но не меняет её layout.
    let surface_fill = lerp_color(persistent_surface, style.surface_pressed, pressed_progress);
    // Reduced motion сохраняет color/opacity transitions, отключая только scale.
    let content_scale = if reduced_motion {
        1.0
    } else {
        1.0 - (1.0 - PRESSED_CONTENT_SCALE) * pressed_progress.clamp(0.0, 1.0)
    };

    QueueModePaintState {
        foreground,
        surface_fill,
        focus_visible,
        content_scale,
    }
}

/// Линейно интерполирует unmultiplied RGBA токены с явным clamp progress.
fn lerp_color(start: Color32, end: Color32, progress: f32) -> Color32 {
    let progress = progress.clamp(0.0, 1.0);
    // Точные endpoints не проходят через округление и сохраняют исходные style tokens.
    if progress <= 0.0 {
        return start;
    }
    if progress >= 1.0 {
        return end;
    }
    // Интерполяция идёт в unmultiplied каналах, затем Color32 сам применяет alpha.
    let [start_red, start_green, start_blue, start_alpha] = start.to_srgba_unmultiplied();
    let [end_red, end_green, end_blue, end_alpha] = end.to_srgba_unmultiplied();
    let interpolate_channel = |start_channel: u8, end_channel: u8| {
        (f32::from(start_channel) + (f32::from(end_channel) - f32::from(start_channel)) * progress)
            .round() as u8
    };

    Color32::from_rgba_unmultiplied(
        interpolate_channel(start_red, end_red),
        interpolate_channel(start_green, end_green),
        interpolate_channel(start_blue, end_blue),
        interpolate_channel(start_alpha, end_alpha),
    )
}

#[cfg(test)]
mod tests;
