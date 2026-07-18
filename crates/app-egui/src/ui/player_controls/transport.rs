//! Custom-painted transport/status UI поверх typed runtime model.
//!
//! Модуль владеет interaction/accessibility и anchored layout, а `ui-artwork-egui` —
//! только векторной геометрией. Traversal и wait semantics остаются у `PlaylistRuntime`.

use egui::{Rect, Sense, Ui, Vec2, WidgetInfo, WidgetType, pos2};
use ui_artwork_egui::{ArtworkPainter, ButtonVisualState, TransportButtonStyle, TransportGlyph};

use crate::playlist_runtime::{NavigationControlAvailability, PlaylistTransportUiModel};
use crate::ui::skin::ControlsStyle;

use super::{ControlAction, button_visual_state};

/// Один transport intent; origin фиксируется adapter-ом как `Ui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportControlAction {
    Previous,
    TogglePlayback,
    Next,
    SetShuffleEnabled { enabled: bool },
    SetRepeatMode { mode: playlist_core::RepeatMode },
    CancelNavigation,
    UndoRemoval,
}

/// Направление боковой transport-кнопки связывает layout, glyph, label и typed action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NavigationDirection {
    /// Кнопка слева от центральной play/pause.
    Previous,

    /// Кнопка справа от центральной play/pause.
    Next,
}

impl NavigationDirection {
    /// Возвращает знак горизонтального смещения относительно play/pause.
    const fn horizontal_sign(self) -> f32 {
        match self {
            Self::Previous => -1.0,
            Self::Next => 1.0,
        }
    }

    /// Возвращает domain-neutral glyph для artwork boundary.
    const fn glyph(self) -> TransportGlyph {
        match self {
            Self::Previous => TransportGlyph::Previous,
            Self::Next => TransportGlyph::Next,
        }
    }

    /// Возвращает русское имя custom widget для tooltip и accessibility.
    const fn accessible_label(self) -> &'static str {
        match self {
            Self::Previous => "Предыдущий",
            Self::Next => "Следующий",
        }
    }

    /// Возвращает typed transport action без зависимости artwork от playlist domain.
    const fn action(self) -> TransportControlAction {
        match self {
            Self::Previous => TransportControlAction::Previous,
            Self::Next => TransportControlAction::Next,
        }
    }
}

/// Рисует Previous в заранее рассчитанном anchored rect.
pub(super) fn render_previous_button(
    ui: &mut Ui,
    button_rect: Rect,
    availability: NavigationControlAvailability,
    controls_style: ControlsStyle,
    actions: &mut Vec<ControlAction>,
) {
    render_navigation_button(
        ui,
        button_rect,
        NavigationDirection::Previous,
        availability,
        controls_style,
        actions,
    );
}

/// Возвращает Previous rect, симметрично привязанный к центру play/pause.
pub(super) fn previous_button_rect(
    playback_button_rect: Rect,
    controls_style: ControlsStyle,
) -> Rect {
    navigation_button_rect(
        playback_button_rect,
        NavigationDirection::Previous,
        controls_style,
    )
}

/// Рисует Next в заранее рассчитанном anchored rect.
pub(super) fn render_next_button(
    ui: &mut Ui,
    button_rect: Rect,
    availability: NavigationControlAvailability,
    controls_style: ControlsStyle,
    actions: &mut Vec<ControlAction>,
) {
    render_navigation_button(
        ui,
        button_rect,
        NavigationDirection::Next,
        availability,
        controls_style,
        actions,
    );
}

/// Возвращает Next rect, симметрично привязанный к центру play/pause.
pub(super) fn next_button_rect(playback_button_rect: Rect, controls_style: ControlsStyle) -> Rect {
    navigation_button_rect(
        playback_button_rect,
        NavigationDirection::Next,
        controls_style,
    )
}

/// Вычисляет квадратную hit-area по typed направлению и skin-owned расстоянию.
fn navigation_button_rect(
    playback_button_rect: Rect,
    direction: NavigationDirection,
    controls_style: ControlsStyle,
) -> Rect {
    // Центральная play/pause остаётся единственным горизонтальным и вертикальным якорем.
    let playback_center = playback_button_rect.center();
    // Знак направления размещает обе кнопки на одинаковом расстоянии от якоря.
    let center_x = playback_center.x
        + direction.horizontal_sign() * controls_style.transport_button_center_distance;
    // Квадратная hit-area остаётся больше glyph и удобна для клика.
    let button_size = Vec2::splat(controls_style.transport_button_size);
    // Оба варианта используют тот же Y, включая вертикальный подъём центральной кнопки.
    Rect::from_center_size(pos2(center_x, playback_center.y), button_size)
}

/// Создаёт настоящий custom widget и возвращает только typed UI intent.
fn render_navigation_button(
    ui: &mut Ui,
    button_rect: Rect,
    direction: NavigationDirection,
    availability: NavigationControlAvailability,
    controls_style: ControlsStyle,
    actions: &mut Vec<ControlAction>,
) {
    // Disabled sub-UI блокирует pointer interaction на уровне egui.
    let response = ui
        .add_enabled_ui(availability.is_enabled(), |ui| {
            // Sense::click сохраняет click, keyboard focus и accessibility-семантику кнопки.
            let response = ui.allocate_rect(button_rect, Sense::click());
            // Клик переносит keyboard focus на transport widget, как у центральной кнопки.
            if response.clicked() {
                response.request_focus();
            }
            // Custom painting требует явного AccessKit-описания виджета.
            response.widget_info(|| {
                WidgetInfo::labeled(
                    WidgetType::Button,
                    ui.is_enabled(),
                    direction.accessible_label(),
                )
            });
            // Painter получает только нейтральные визуальные параметры.
            paint_navigation_button(ui, button_rect, direction, controls_style, &response);
            // Response возвращается владельцу typed action после завершения sub-UI.
            response
        })
        .inner
        // У glyph-кнопки нет видимого текста, поэтому enabled hover показывает её имя.
        .on_hover_text(direction.accessible_label())
        // Disabled hover дополнительно сохраняет точную runtime-причину недоступности.
        .on_disabled_hover_text(availability.explanation());

    // Один release-click порождает ровно один существующий transport intent.
    if response.clicked() {
        actions.push(ControlAction::Transport(direction.action()));
    }
}

/// Передаёт artwork-фасаду уже разрешённые visual state и skin colors.
fn paint_navigation_button(
    ui: &Ui,
    button_rect: Rect,
    direction: NavigationDirection,
    controls_style: ControlsStyle,
    response: &egui::Response,
) {
    // Disabled-цвет выбирает interaction-owner, не раскрывая artwork playlist-состояния.
    let color = if ui.is_enabled() {
        controls_style.text_color
    } else {
        controls_style.transport_button_disabled_color
    };
    // Disabled Response никогда не hovered, поэтому hover-подложка для него не появится.
    let visual_state: ButtonVisualState = button_visual_state(response.hovered());
    // Facade сохраняет запрет на прямые Painter-примитивы внутри app-egui.
    ArtworkPainter::new(ui.painter()).transport_button(
        button_rect,
        direction.glyph(),
        visual_state,
        TransportButtonStyle {
            icon_extent: controls_style.transport_button_icon_extent,
            bar_width: controls_style.transport_button_bar_width,
            color,
            hover_fill: controls_style.transport_button_hover_fill,
        },
    );
}

/// D80 status и recovery actions не зависят от sidebar visibility.
pub(super) fn render_global_status(
    ui: &mut Ui,
    model: &PlaylistTransportUiModel,
    actions: &mut Vec<ControlAction>,
) {
    if model.global_status.is_none() && model.undo.is_none() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if let Some(status) = model.global_status {
            ui.label(status.label());
            if status.can_cancel() && ui.button("Отменить ожидание").clicked() {
                actions.push(ControlAction::Transport(
                    TransportControlAction::CancelNavigation,
                ));
            }
        }
        if let Some(undo) = model.undo {
            let label = format!(
                "Отменить {} ({} с)",
                undo.kind_label, undo.seconds_remaining
            );
            if ui.button(label).clicked() {
                actions.push(ControlAction::Transport(
                    TransportControlAction::UndoRemoval,
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use egui::{Event, Modifiers, PointerButton, RawInput, pos2, vec2};

    use super::*;
    use crate::ui::skin::{MinimalSkin, PlayerSkin};

    #[test]
    fn navigation_directions_preserve_labels_glyphs_and_actions() {
        assert_eq!(
            NavigationDirection::Previous.accessible_label(),
            "Предыдущий"
        );
        assert_eq!(
            NavigationDirection::Previous.glyph(),
            TransportGlyph::Previous
        );
        assert_eq!(
            NavigationDirection::Previous.action(),
            TransportControlAction::Previous
        );
        assert_eq!(NavigationDirection::Next.accessible_label(), "Следующий");
        assert_eq!(NavigationDirection::Next.glyph(), TransportGlyph::Next);
        assert_eq!(
            NavigationDirection::Next.action(),
            TransportControlAction::Next
        );
    }

    #[test]
    fn availability_preserves_disabled_wait_and_pending_explanations() {
        assert!(!NavigationControlAvailability::Disabled.is_enabled());
        assert!(NavigationControlAvailability::PotentialWait.is_enabled());
        assert!(NavigationControlAvailability::Pending.is_enabled());
        assert_ne!(
            NavigationControlAvailability::Disabled.explanation(),
            NavigationControlAvailability::PotentialWait.explanation()
        );
        assert_ne!(
            NavigationControlAvailability::PotentialWait.explanation(),
            NavigationControlAvailability::Pending.explanation()
        );
    }

    #[test]
    fn navigation_rects_are_symmetric_skin_owned_and_center_anchored() {
        // Minimal skin содержит согласованные пользователем размеры transport-группы.
        let controls_style = MinimalSkin.controls_style();
        // Центральная кнопка использует реальный production-диаметр 48 points.
        let playback_rect = Rect::from_center_size(
            pos2(320.0, 24.0),
            Vec2::splat(controls_style.playback_button_diameter),
        );
        // Previous вычисляется только от центрального rect и typed style.
        let previous_rect = previous_button_rect(playback_rect, controls_style);
        // Next использует тот же anchor и противоположный знак направления.
        let next_rect = next_button_rect(playback_rect, controls_style);

        // Обе hit-area обязаны иметь skin-owned размер 32x32.
        assert_eq!(
            previous_rect.size(),
            Vec2::splat(controls_style.transport_button_size)
        );
        assert_eq!(next_rect.size(), previous_rect.size());
        // Центры располагаются ровно в 64 points от play/pause.
        assert_eq!(
            playback_rect.center().x - previous_rect.center().x,
            controls_style.transport_button_center_distance
        );
        assert_eq!(
            next_rect.center().x - playback_rect.center().x,
            controls_style.transport_button_center_distance
        );
        // Вертикальная привязка не должна повторно применять playback raise.
        assert_eq!(previous_rect.center().y, playback_rect.center().y);
        assert_eq!(next_rect.center().y, playback_rect.center().y);
        // Между каждой боковой hit-area и центральной кнопкой остаётся явный зазор.
        assert!(previous_rect.right() < playback_rect.left());
        assert!(playback_rect.right() < next_rect.left());
    }

    #[test]
    fn next_and_playback_rate_buttons_do_not_overlap() {
        let controls_style = MinimalSkin.controls_style();
        let row_rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 48.0));
        let playback_rect = Rect::from_center_size(
            row_rect.center(),
            Vec2::splat(controls_style.playback_button_diameter),
        );
        let base_next_rect = next_button_rect(playback_rect, controls_style);
        let repeat_rect = Rect::from_center_size(
            pos2(
                playback_rect.center().x + controls_style.queue_mode_button_center_distance,
                row_rect.center().y,
            ),
            vec2(32.0, 32.0),
        );
        let rate_layout = super::super::playback_rate::control_layout(
            playback_rect,
            base_next_rect,
            repeat_rect,
            controls_style,
            8.0,
            1.0,
        );

        assert!(rate_layout.button_rect.right() <= rate_layout.next_button_rect.left());
        assert!(
            rate_layout.next_button_rect.right() + controls_style.queue_mode_neighbor_gap
                <= repeat_rect.left()
        );
    }

    #[test]
    fn transport_and_adjacent_controls_do_not_overlap_in_narrow_row() {
        // Узкая строка проверяет реальную композицию без запаса desktop-ширины.
        let controls_style = MinimalSkin.controls_style();
        // 400 points — минимальная поддерживаемая ширина окна.
        let row_rect = Rect::from_min_size(
            pos2(10.0, 0.0),
            vec2(380.0, controls_style.playback_button_diameter),
        );
        // Production helpers сохраняют одинаковый вертикальный raise всех anchored buttons.
        let playback_rect = super::super::playback_button_anchor_rect(row_rect, controls_style);
        // Левая внешняя кнопка задаёт начало доступной volume-зоны.
        let open_file_rect = super::super::open_file_button_anchor_rect(row_rect, controls_style);
        // Правая внешняя кнопка ограничивает playback-rate control.
        let fullscreen_rect = super::super::fullscreen_button_anchor_rect(row_rect, controls_style);
        // Queue-mode controls сжимаются только по статическим внешним границам.
        let queue_mode_layout = super::super::queue_mode_controls::control_layout(
            playback_rect,
            open_file_rect,
            fullscreen_rect,
            controls_style,
            8.0,
        );
        // Next остаётся левой границей playback-rate control.
        let base_next_rect = next_button_rect(playback_rect, controls_style);
        // Egui spacing совпадает с production значением проверяемой раскладки.
        let item_spacing = 8.0;
        // Volume layout получает тот же exact Shuffle rect, что render path.
        let volume_zone = super::super::volume_controls_zone_rect(
            row_rect,
            open_file_rect,
            queue_mode_layout.shuffle_rect,
            item_spacing,
        );
        // Rate layout получает тот же exact Next rect, что render path.
        let rate_layout = super::super::playback_rate::control_layout(
            playback_rect,
            base_next_rect,
            queue_mode_layout.repeat_rect,
            controls_style,
            item_spacing,
            1.0,
        );

        // Левая группа не пересекает внешний open-file control.
        assert!(open_file_rect.right() <= volume_zone.left());
        // Volume-зона сохраняет production gap перед Shuffle.
        assert!(volume_zone.right() <= queue_mode_layout.shuffle_rect.left() - item_spacing);
        // Rate control раскрывается перед сдвинутым Next без пересечения.
        assert!(rate_layout.button_rect.right() <= rate_layout.next_button_rect.left());
        // Узкий row действительно уменьшает preferred rate width, а не перекрывает Fullscreen.
        assert!(rate_layout.button_rect.width() < controls_style.playback_rate_button_width);
        // Next сдвигается ровно на фактически доступную, уже уменьшенную ширину.
        assert!(
            (rate_layout.next_button_rect.left()
                - base_next_rect.left()
                - rate_layout.button_rect.width())
            .abs()
                < 0.0001
        );
        // Сдвинутый Next сохраняет обязательные 12 points перед Repeat.
        assert!(
            rate_layout.next_button_rect.right() + controls_style.queue_mode_neighbor_gap
                <= queue_mode_layout.repeat_rect.left()
        );
        // Repeat остаётся перед статическим Fullscreen.
        assert!(
            queue_mode_layout.repeat_rect.right() + controls_style.queue_mode_neighbor_gap
                <= fullscreen_rect.left()
        );
    }

    /// Создаёт deterministic egui input для custom transport widget.
    fn raw_input(events: Vec<Event>) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(240.0, 200.0))),
            events,
            ..RawInput::default()
        }
    }

    /// Создаёт pointer press/release event в центре transport hit-area.
    fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    /// Рендерит одну transport-кнопку и возвращает собранные typed actions.
    fn render_actions_for_input(
        context: &egui::Context,
        input: RawInput,
        direction: NavigationDirection,
        availability: NavigationControlAvailability,
    ) -> Vec<ControlAction> {
        // Один и тот же rect сохраняет стабильный widget id между input frames.
        let button_rect = Rect::from_center_size(pos2(120.0, 100.0), Vec2::splat(32.0));
        // Production skin предоставляет visual style без test-only констант.
        let controls_style = MinimalSkin.controls_style();
        // Вектор накапливает только post-render intents проверяемого frame.
        let mut actions = Vec::new();
        // Egui обрабатывает pointer state внутри обычного immediate-mode pass.
        let _ = context.run_ui(input, |ui| {
            render_navigation_button(
                ui,
                button_rect,
                direction,
                availability,
                controls_style,
                &mut actions,
            );
        });
        // Typed actions возвращаются без выполнения playlist side effects.
        actions
    }

    /// Выполняет полный hover -> press -> release цикл одной кнопки.
    fn click_navigation_button(
        direction: NavigationDirection,
        availability: NavigationControlAvailability,
    ) -> Vec<ControlAction> {
        // Отдельный Context изолирует pointer capture каждого сценария.
        let context = egui::Context::default();
        // Центр совпадает с button rect из render helper.
        let pointer_position = pos2(120.0, 100.0);
        // Warmup frame сообщает egui, какой widget находится под указателем.
        let warmup_actions = render_actions_for_input(
            &context,
            raw_input(vec![Event::PointerMoved(pointer_position)]),
            direction,
            availability,
        );
        // Hover не должен сам создавать transport intent.
        assert!(warmup_actions.is_empty());
        // Press захватывает кнопку, но click фиксируется только на release.
        let press_actions = render_actions_for_input(
            &context,
            raw_input(vec![pointer_button(pointer_position, true)]),
            direction,
            availability,
        );
        // Down-event не считается завершённым кликом.
        assert!(press_actions.is_empty());
        // Release frame возвращает единственный итоговый action либо ничего для disabled.
        render_actions_for_input(
            &context,
            raw_input(vec![pointer_button(pointer_position, false)]),
            direction,
            availability,
        )
    }

    #[test]
    fn every_enabled_navigation_state_emits_exact_typed_action() {
        for availability in [
            NavigationControlAvailability::Ready,
            NavigationControlAvailability::PotentialWait,
            NavigationControlAvailability::Pending,
        ] {
            assert_eq!(
                click_navigation_button(NavigationDirection::Previous, availability),
                vec![ControlAction::Transport(TransportControlAction::Previous)]
            );
            assert_eq!(
                click_navigation_button(NavigationDirection::Next, availability),
                vec![ControlAction::Transport(TransportControlAction::Next)]
            );
        }
    }

    #[test]
    fn disabled_navigation_buttons_never_emit_actions() {
        assert!(
            click_navigation_button(
                NavigationDirection::Previous,
                NavigationControlAvailability::Disabled,
            )
            .is_empty()
        );
        assert!(
            click_navigation_button(
                NavigationDirection::Next,
                NavigationControlAvailability::Disabled,
            )
            .is_empty()
        );
    }
}
