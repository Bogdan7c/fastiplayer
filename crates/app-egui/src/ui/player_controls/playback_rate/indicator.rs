//! Анимированная кнопка сброса скорости и синхронная раскладка Next.

use egui::{Rect, Sense, Stroke, TextStyle, Ui, WidgetInfo, WidgetType, pos2, vec2};
use player_core::{PlaybackRate, PlayerSnapshot};
use ui_artwork_egui::{ArtworkPainter, PlaybackRateButtonGeometry, PlaybackRateButtonStyle};

use super::super::{ControlAction, button_visual_state};
use super::label;
use crate::ui::skin::ControlsStyle;

/// Полный переход скрыто↔раскрыто занимает ровно 250 миллисекунд.
const PLAYBACK_RATE_TRANSITION_SECONDS: f32 = 0.250;
/// Stable suffix изолирует animation manager state от focus/input accumulator.
const PLAYBACK_RATE_TRANSITION_ID_SUFFIX: &str = "playback_rate_reset_reveal";

/// Полная раскладка индикатора и сдвинутой Next для одного animation progress.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(in crate::ui::player_controls) struct PlaybackRateControlLayout {
    /// Полный rect кнопки в текущей translated позиции.
    pub(in crate::ui::player_controls) button_rect: Rect,
    /// Фактически раскрытая прямоугольная hit-area справа от Play/Pause.
    pub(in crate::ui::player_controls) visible_rect: Rect,
    /// Next с тем же точным горизонтальным смещением.
    pub(in crate::ui::player_controls) next_button_rect: Rect,
    /// Нормализованный eased progress `0.0..=1.0`.
    reveal_progress: f32,
}

/// Возвращает единый layout кнопки скорости и Next для текущего progress.
pub(in crate::ui::player_controls) fn control_layout(
    playback_button_rect: Rect,
    base_next_button_rect: Rect,
    repeat_button_rect: Rect,
    controls_style: ControlsStyle,
    item_spacing: f32,
    reveal_progress: f32,
) -> PlaybackRateControlLayout {
    // Progress от внешнего animation manager защищаем от некорректного custom input.
    let reveal_progress = reveal_progress.clamp(0.0, 1.0);
    // Next может сдвигаться только до обязательных 12 points перед неподвижным Repeat.
    let next_to_repeat_gap = controls_style.queue_mode_neighbor_gap.max(item_spacing);
    let available_shift =
        (repeat_button_rect.left() - next_to_repeat_gap - base_next_button_rect.right()).max(0.0);
    // На обычном окне используется skin-owned 48 points; на узком — безопасный остаток.
    let resolved_width = controls_style
        .playback_rate_button_width
        .min(available_shift);
    // По два skin-owned points сверху и снизу делают кнопку ниже Next, сохраняя общий центр.
    let button_height = (controls_style.transport_button_size
        - controls_style.playback_rate_button_vertical_inset * 2.0)
        .max(0.0);
    // Горизонтальный reveal не зависит от уменьшенной визуальной высоты.
    let button_size = vec2(resolved_width, button_height);
    // В раскрытом состоянии bounding rect начинается через 5 points после Play/Pause.
    let expanded_min = pos2(
        playback_button_rect.right() + controls_style.playback_rate_button_gap,
        playback_button_rect.center().y - button_size.y * 0.5,
    );
    // Expanded rect остаётся стабильной конечной геометрией для обоих направлений.
    let expanded_rect = Rect::from_min_size(expanded_min, button_size);
    // Скрытое состояние переносит весь rect влево ровно на его resolved width.
    let hidden_offset = -resolved_width * (1.0 - reveal_progress);
    // Moving rect обеспечивает настоящий slide текста и контура под фиксированным clip.
    let button_rect = expanded_rect.translate(vec2(hidden_offset, 0.0));
    // Visible rect растёт только вправо от неизменной границы рядом с Play/Pause.
    let visible_rect = Rect::from_min_max(
        expanded_rect.left_top(),
        pos2(
            button_rect.right().max(expanded_rect.left()),
            expanded_rect.bottom(),
        ),
    );
    // Next использует ту же величину раскрытия без отдельной анимации и рассинхронизации.
    let next_button_rect =
        base_next_button_rect.translate(vec2(resolved_width * reveal_progress, 0.0));

    // Typed layout не раскрывает вызывающему коду внутреннюю формулу.
    PlaybackRateControlLayout {
        button_rect,
        visible_rect,
        next_button_rect,
        reveal_progress,
    }
}

/// Читает widget-local animation state из egui и возвращает eased progress.
pub(in crate::ui::player_controls) fn reveal_progress(
    ui: &Ui,
    playback_rate: PlaybackRate,
    reduced_motion: bool,
) -> f32 {
    // Stable Id нужен даже при полностью скрытой кнопке, чтобы первый non-1x кадр анимировался.
    let transition_id = ui.make_persistent_id(PLAYBACK_RATE_TRANSITION_ID_SUFFIX);
    // Только реальный snapshot определяет цель: optimistic reset не скрывает кнопку.
    let target_visible = playback_rate != PlaybackRate::NORMAL;
    // Reduced motion делает layout-переход мгновенным и не запускает animation repaint.
    if reduced_motion {
        return if target_visible { 1.0 } else { 0.0 };
    }
    // egui сам хранит переход, поддерживает реверс и запрашивает repaint до достижения цели.
    ui.ctx().animate_bool_with_time_and_easing(
        transition_id,
        target_visible,
        PLAYBACK_RATE_TRANSITION_SECONDS,
        egui::emath::easing::cubic_in_out,
    )
}

/// Рисует custom-кнопку скорости и добавляет только typed reset intent.
pub(in crate::ui::player_controls) fn render_reset_button_at(
    ui: &mut Ui,
    layout: PlaybackRateControlLayout,
    playback_button_rect: Rect,
    player_snapshot: &PlayerSnapshot,
    controls_style: ControlsStyle,
    actions: &mut Vec<ControlAction>,
) {
    // При progress=0 кнопка не создаёт ни paint shapes, ни невидимую hit-area.
    if layout.reveal_progress <= 0.0 {
        return;
    }

    // Подпись существует только для реальной non-1x скорости; при closing контур пустой.
    let rate_label = (player_snapshot.playback_rate != PlaybackRate::NORMAL)
        .then(|| label(player_snapshot.playback_rate.as_f32()));
    // Reset interaction разрешён только пока snapshot действительно не равен 1x.
    let button_response = rate_label.as_ref().and_then(|rate_label| {
        // На первом почти скрытом кадре нулевая hit-area ещё не является виджетом.
        (layout.visible_rect.width() > 0.0).then(|| {
            // Sense::click сохраняет pointer, keyboard focus и accessibility-семантику.
            let response = ui.allocate_rect(layout.visible_rect, Sense::click());
            // Как и у transport-кнопок, успешный click переносит focus на сам widget.
            if response.clicked() {
                response.request_focus();
            }
            // Custom artwork требует явного описания для AccessKit.
            response.widget_info(|| {
                WidgetInfo::labeled(
                    WidgetType::Button,
                    ui.is_enabled(),
                    format!("Скорость воспроизведения {rate_label}"),
                )
            });
            // Glyphless control получает понятную подсказку при наведении.
            response.on_hover_text("Сбросить скорость воспроизведения")
        })
    });

    // Клик создаёт существующий intent; преобразование в PlayerCommand остаётся у AppState.
    if button_response
        .as_ref()
        .is_some_and(egui::Response::clicked)
    {
        actions.push(ControlAction::ResetPlaybackRate);
    }

    // Closing и hidden части не должны сохранять hover-подложку.
    let visual_state = button_visual_state(
        button_response
            .as_ref()
            .is_some_and(egui::Response::hovered),
    );
    // Stroke padding сохраняет внешнюю половину контура сверху, справа и снизу.
    let stroke_padding = controls_style.playback_rate_button_stroke_width;
    // Левая граница clip не расширяется: скрытая часть должна оставаться под Play/Pause.
    let paint_clip_rect = Rect::from_min_max(
        pos2(
            layout.visible_rect.left(),
            layout.button_rect.top() - stroke_padding,
        ),
        pos2(
            layout.button_rect.right() + stroke_padding,
            layout.button_rect.bottom() + stroke_padding,
        ),
    );
    // Artwork получает только визуальную geometry, label и skin-owned стиль.
    ArtworkPainter::new(ui.painter()).playback_rate_button(
        PlaybackRateButtonGeometry {
            button_rect: layout.button_rect,
            visible_clip_rect: paint_clip_rect,
            concave_radius: playback_button_rect.width() * 0.5,
        },
        rate_label.as_deref(),
        visual_state,
        PlaybackRateButtonStyle {
            outline: Stroke::new(
                controls_style.playback_rate_button_stroke_width,
                controls_style.text_color,
            ),
            hover_fill: controls_style.transport_button_hover_fill,
            text_color: controls_style.text_color,
            font_id: TextStyle::Button.resolve(ui.style()),
        },
    );
}
