//! Компактный icon-only toolbar: layout, interaction, accessibility и actions.

use egui::{Color32, Key, Popup, Rect, Response, Sense, Ui, WidgetInfo, WidgetType, pos2, vec2};
use playlist_core::{SortCanonicalQueue, SortDirection};
use ui_artwork_egui::{
    ArtworkPainter, PlaylistToolbarButtonStyle, PlaylistToolbarGlyph, PlaylistToolbarPaintState,
};

use crate::playlist_runtime::{PlaylistGoCurrentTarget, PlaylistInteractionModel};
use crate::ui::skin::PlaylistToolbarStyle;

use super::super::PlaylistUiOutput;
use super::super::actions::PlaylistAction;
use super::SORT_KEYS;

/// Стабильный порядок четырёх обычных действий слева.
const LEFT_CONTROLS: [ToolbarControl; 4] = [
    ToolbarControl::AddFiles,
    ToolbarControl::AddUrl,
    ToolbarControl::Sort,
    ToolbarControl::CurrentItem,
];

/// Intent одного toolbar control без знания его прямоугольника.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolbarControl {
    /// Открывает multi-file picker.
    AddFiles,
    /// Открывает встроенную URL-форму.
    AddUrl,
    /// Открывает меню canonical sort.
    Sort,
    /// Фокусирует текущий играющий элемент.
    CurrentItem,
    /// Очищает очередь, не останавливая detached playback.
    Clear,
}

impl ToolbarControl {
    /// Стабильная часть egui Id не зависит от визуального порядка.
    const fn id_suffix(self) -> &'static str {
        match self {
            Self::AddFiles => "playlist_toolbar_add_files",
            Self::AddUrl => "playlist_toolbar_add_url",
            Self::Sort => "playlist_toolbar_sort",
            Self::CurrentItem => "playlist_toolbar_current_item",
            Self::Clear => "playlist_toolbar_clear",
        }
    }

    /// Нейтральный artwork glyph выбирается только по intent.
    const fn glyph(self) -> PlaylistToolbarGlyph {
        match self {
            Self::AddFiles => PlaylistToolbarGlyph::AddFiles,
            Self::AddUrl => PlaylistToolbarGlyph::AddUrl,
            Self::Sort => PlaylistToolbarGlyph::Sort,
            Self::CurrentItem => PlaylistToolbarGlyph::CurrentItem,
            Self::Clear => PlaylistToolbarGlyph::Clear,
        }
    }

    /// Короткое имя используется AccessKit и не зависит от tooltip-подробностей.
    const fn accessible_label(self) -> &'static str {
        match self {
            Self::AddFiles => "Добавить файлы",
            Self::AddUrl => "Добавить URL",
            Self::Sort => "Сортировать плейлист",
            Self::CurrentItem => "Перейти к текущему медиа",
            Self::Clear => "Очистить очередь",
        }
    }

    /// Разрешает interaction и формулирует точную причину disabled-состояния.
    fn presentation(self, model: &PlaylistInteractionModel) -> ToolbarPresentation {
        match self {
            Self::AddFiles => ToolbarPresentation {
                enabled: model.structural_actions_enabled && !model.file_dialog_open,
                tooltip: "Добавить несколько файлов в конец плейлиста",
                disabled_tooltip: if model.file_dialog_open {
                    "Диалог выбора файлов уже открыт"
                } else {
                    "Сейчас нельзя изменять состав плейлиста"
                },
            },
            Self::AddUrl => ToolbarPresentation {
                enabled: model.structural_actions_enabled,
                tooltip: "Добавить медиа по URL в конец плейлиста",
                disabled_tooltip: "Сейчас нельзя изменять состав плейлиста",
            },
            Self::Sort => ToolbarPresentation {
                enabled: model.structural_actions_enabled
                    && model.item_count > 1
                    && model.progress.is_none(),
                tooltip: "Однократно изменить порядок плейлиста",
                disabled_tooltip: sort_disabled_tooltip(model),
            },
            Self::CurrentItem => ToolbarPresentation {
                enabled: model.go_current_target.is_some(),
                tooltip: current_item_tooltip(model.go_current_target),
                disabled_tooltip: "Сейчас нет активного медиа",
            },
            Self::Clear => ToolbarPresentation {
                enabled: model.structural_actions_enabled && model.item_count > 0,
                tooltip: "Очистить очередь; текущее воспроизведение продолжится",
                disabled_tooltip: if model.item_count == 0 {
                    "Очередь уже пуста"
                } else {
                    "Сейчас нельзя изменять состав плейлиста"
                },
            },
        }
    }

    /// Обычная кнопка создаёт только существующий typed action.
    fn action(self, model: &PlaylistInteractionModel) -> Option<PlaylistAction> {
        match self {
            Self::AddFiles => Some(PlaylistAction::AddFiles),
            Self::AddUrl => Some(PlaylistAction::OpenUrlEditor),
            Self::CurrentItem => model.go_current_target.map(PlaylistAction::GoCurrent),
            Self::Clear => Some(PlaylistAction::Clear),
            Self::Sort => None,
        }
    }
}

/// Тексты и availability вычисляются вместе, чтобы tooltip не расходился с boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolbarPresentation {
    /// Может ли один click создать intent в текущем frame.
    enabled: bool,
    /// Полное объяснение доступного действия.
    tooltip: &'static str,
    /// Полное объяснение причины недоступности.
    disabled_tooltip: &'static str,
}

/// Точные rect-ы всех кнопок внутри одной строки.
#[derive(Debug, Clone, Copy, PartialEq)]
struct IconBarLayout {
    /// Добавление локальных файлов.
    add_files: Rect,
    /// Добавление URL.
    add_url: Rect,
    /// Меню сортировки.
    sort: Rect,
    /// Переход к текущему элементу.
    current_item: Rect,
    /// Очистка у правого края.
    clear: Rect,
}

impl IconBarLayout {
    /// Возвращает rect по intent без позиционных индексов у callsite.
    const fn rect(self, control: ToolbarControl) -> Rect {
        match control {
            ToolbarControl::AddFiles => self.add_files,
            ToolbarControl::AddUrl => self.add_url,
            ToolbarControl::Sort => self.sort,
            ToolbarControl::CurrentItem => self.current_item,
            ToolbarControl::Clear => self.clear,
        }
    }
}

/// Рисует toolbar и публикует только typed post-render actions.
pub(super) fn show(
    ui: &mut Ui,
    model: &PlaylistInteractionModel,
    style: PlaylistToolbarStyle,
    output: &mut PlaylistUiOutput,
) {
    let row_width = ui.available_width().max(0.0);
    // Flow-height остаётся равной hit-area: дополнительная высота сдвинула бы весь summary вниз.
    let row_height = style.button_size.max(0.0);
    let (row_rect, _) = ui.allocate_exact_size(vec2(row_width, row_height), Sense::hover());
    let layout = icon_bar_layout(row_rect, style);

    for control in LEFT_CONTROLS {
        let presentation = control.presentation(model);
        let response = render_control(ui, layout.rect(control), control, presentation, style);
        if control == ToolbarControl::Sort {
            show_sort_menu(ui, &response, presentation.enabled, output);
        } else if response.clicked()
            && let Some(action) = control.action(model)
        {
            output.push_action(action);
        }
    }

    let clear_control = ToolbarControl::Clear;
    let clear_presentation = clear_control.presentation(model);
    let clear_response = render_control(
        ui,
        layout.rect(clear_control),
        clear_control,
        clear_presentation,
        style,
    );
    if clear_response.clicked()
        && let Some(action) = clear_control.action(model)
    {
        output.push_action(action);
    }
}

/// Левая группа следует общей window-control сетке, а Clear хранит независимый правый отступ.
fn icon_bar_layout(row_rect: Rect, style: PlaylistToolbarStyle) -> IconBarLayout {
    let left_group_padding = style
        .left_group_padding
        .max(0.0)
        .min(row_rect.width().max(0.0) * 0.5);
    let clear_right_padding = style
        .clear_right_padding
        .max(0.0)
        .min(row_rect.width().max(0.0) * 0.5);
    let content_rect = Rect::from_min_max(
        pos2(row_rect.left() + left_group_padding, row_rect.top()),
        pos2(row_rect.right() - clear_right_padding, row_rect.bottom()),
    );
    let requested_gap = style.button_gap.max(0.0);
    let gap = requested_gap.min(content_rect.width().max(0.0) / 3.0);
    let maximum_non_overlapping_size = ((content_rect.width() - gap * 3.0).max(0.0) / 5.0).max(0.0);
    let button_size = style.button_size.max(0.0).min(maximum_non_overlapping_size);
    let button_extent = vec2(button_size, button_size);
    // Внешний egui spacing уже оставляет место перед summary, поэтому rect можно
    // оптически опустить в этот промежуток, не меняя положение следующего блока.
    let button_center_y = content_rect.center().y + style.button_center_y_offset;
    let first_center = pos2(content_rect.left() + button_size * 0.5, button_center_y);
    let center_step = button_size + gap;
    let left_rect = |index: usize| {
        Rect::from_center_size(
            first_center + vec2(center_step * index as f32, 0.0),
            button_extent,
        )
    };
    let clear = Rect::from_center_size(
        pos2(content_rect.right() - button_size * 0.5, button_center_y),
        button_extent,
    );

    IconBarLayout {
        add_files: left_rect(0),
        add_url: left_rect(1),
        sort: left_rect(2),
        current_item: left_rect(3),
        clear,
    }
}

/// Создаёт focusable custom button и передаёт artwork только resolved paint-state.
fn render_control(
    ui: &mut Ui,
    rect: Rect,
    control: ToolbarControl,
    presentation: ToolbarPresentation,
    style: PlaylistToolbarStyle,
) -> Response {
    let effective_enabled = ui.is_enabled() && presentation.enabled;
    let widget_id = ui.make_persistent_id(control.id_suffix());
    let response = ui
        .add_enabled_ui(presentation.enabled, |ui| {
            let response = ui.interact(rect, widget_id, Sense::click());
            response.widget_info(|| {
                WidgetInfo::labeled(
                    WidgetType::Button,
                    ui.is_enabled(),
                    control.accessible_label(),
                )
            });
            if response.clicked() && response.interact_pointer_pos().is_some() {
                response.surrender_focus();
            }
            paint_control(ui, rect, control, style, &response);
            response
        })
        .inner;

    if effective_enabled {
        response.on_hover_text(presentation.tooltip)
    } else {
        response.on_disabled_hover_text(presentation.disabled_tooltip)
    }
}

/// Отделяет app-owned interaction state от domain-neutral geometry.
fn paint_control(
    ui: &Ui,
    rect: Rect,
    control: ToolbarControl,
    style: PlaylistToolbarStyle,
    response: &Response,
) {
    let keyboard_pressed = response.has_focus()
        && ui.input(|input| input.key_down(Key::Space) || input.key_down(Key::Enter));
    let pressed = response.is_pointer_button_down_on() || keyboard_pressed;
    let enabled = ui.is_enabled();
    let foreground = if !enabled {
        style.foreground_disabled
    } else if response.hovered() || pressed {
        style.foreground_hover
    } else {
        style.foreground_idle
    };
    let surface_fill = if !enabled {
        Color32::TRANSPARENT
    } else if pressed {
        style.surface_pressed
    } else if response.hovered() {
        style.surface_hover
    } else {
        Color32::TRANSPARENT
    };

    ArtworkPainter::new(ui.painter()).playlist_toolbar_button(
        rect,
        control.glyph(),
        PlaylistToolbarPaintState {
            foreground,
            surface_fill,
            focus_visible: enabled && response.has_focus(),
        },
        PlaylistToolbarButtonStyle {
            icon_extent: style.icon_extent,
            glyph_stroke_width: style.glyph_stroke_width,
            surface_corner_radius: style.surface_corner_radius,
            focus_outline: style.focus_outline,
            focus_inset: style.focus_inset,
        },
    );
}

/// Custom icon response остаётся полноценным anchor-ом штатного egui menu popup.
fn show_sort_menu(
    ui: &Ui,
    response: &Response,
    model_enabled: bool,
    output: &mut PlaylistUiOutput,
) {
    let effective_enabled = ui.is_enabled() && model_enabled;
    let popup_id = Popup::default_response_id(response);
    if !effective_enabled {
        Popup::close_id(ui.ctx(), popup_id);
        return;
    }

    Popup::menu(response).show(|ui| {
        for (key, label) in SORT_KEYS {
            ui.menu_button(label, |ui| {
                if ui.button("По возрастанию ↑").clicked() {
                    output.push_action(PlaylistAction::Sort(SortCanonicalQueue::new(
                        key,
                        SortDirection::Ascending,
                    )));
                    ui.close();
                }
                if ui.button("По убыванию ↓").clicked() {
                    output.push_action(PlaylistAction::Sort(SortCanonicalQueue::new(
                        key,
                        SortDirection::Descending,
                    )));
                    ui.close();
                }
            });
        }
    });
}

/// Tooltip сохраняет различие между неподходящей очередью, mutation gate и активной работой.
fn sort_disabled_tooltip(model: &PlaylistInteractionModel) -> &'static str {
    if !model.structural_actions_enabled {
        "Сейчас нельзя изменять порядок плейлиста"
    } else if model.item_count <= 1 {
        "Для сортировки нужны хотя бы два элемента"
    } else {
        "Дождитесь завершения текущей операции"
    }
}

/// Текущий row и detached tombstone имеют разные пользовательские последствия.
const fn current_item_tooltip(target: Option<PlaylistGoCurrentTarget>) -> &'static str {
    match target {
        Some(PlaylistGoCurrentTarget::Row(_)) => "Сфокусировать и показать текущую строку",
        Some(PlaylistGoCurrentTarget::Tombstone) => {
            "Показать продолжающее воспроизводиться удалённое медиа"
        }
        None => "Сейчас нет активного медиа",
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, Event, Key, Modifiers, PointerButton, RawInput, Rect, pos2, vec2};

    use super::*;
    use crate::ui::skin::{MinimalSkin, PlayerSkin};

    /// Создаёт deterministic viewport для настоящего egui interaction pass.
    fn raw_input(events: Vec<Event>, time: f64) -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 80.0))),
            time: Some(time),
            events,
            ..RawInput::default()
        }
    }

    /// Строит один pointer press/release event.
    fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }

    /// Создаёт keyboard press/release в одном deterministic frame.
    fn keyboard_input(key: Key) -> RawInput {
        raw_input(
            vec![
                Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
                Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                },
            ],
            0.0,
        )
    }

    /// Рендерит production icon bar и возвращает typed actions кадра.
    fn render_input(
        context: &Context,
        model: &PlaylistInteractionModel,
        input: RawInput,
    ) -> Vec<PlaylistAction> {
        let mut output = PlaylistUiOutput::default();
        let _ = context.run_ui(input, |ui| {
            ui.set_width(420.0);
            show(ui, model, MinimalSkin.playlist_toolbar_style(), &mut output);
        });
        output.take_actions()
    }

    /// Изолирует один production custom button для focus/keyboard regression.
    fn control_frame(
        context: &Context,
        model: &PlaylistInteractionModel,
        control: ToolbarControl,
        input: RawInput,
    ) -> (Vec<PlaylistAction>, bool) {
        let mut output = PlaylistUiOutput::default();
        let mut has_focus = false;
        let _ = context.run_ui(input, |ui| {
            let response = render_control(
                ui,
                Rect::from_min_size(pos2(20.0, 20.0), vec2(28.0, 28.0)),
                control,
                control.presentation(model),
                MinimalSkin.playlist_toolbar_style(),
            );
            if response.clicked()
                && let Some(action) = control.action(model)
            {
                output.push_action(action);
            }
            has_focus = response.has_focus();
        });
        (output.take_actions(), has_focus)
    }

    /// Выполняет полный hover/press/release цикл по центру control.
    fn click_control(
        context: &Context,
        model: &PlaylistInteractionModel,
        control: ToolbarControl,
    ) -> Vec<PlaylistAction> {
        let style = MinimalSkin.playlist_toolbar_style();
        let layout = icon_bar_layout(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, style.button_size)),
            style,
        );
        let position = layout.rect(control).center();

        assert!(
            render_input(
                context,
                model,
                raw_input(vec![Event::PointerMoved(position)], 0.0)
            )
            .is_empty()
        );
        assert!(
            render_input(
                context,
                model,
                raw_input(vec![pointer_button(position, true)], 0.01)
            )
            .is_empty()
        );
        render_input(
            context,
            model,
            raw_input(vec![pointer_button(position, false)], 0.02),
        )
    }

    #[test]
    fn layout_aligns_left_group_to_titlebar_axes_and_keeps_clear_at_right_edge() {
        let style = MinimalSkin.playlist_toolbar_style();
        let controls_style = MinimalSkin.controls_style();
        let expected_first_center_inset =
            controls_style.left_edge_control_first_center_inset_points();
        let expected_center_step = controls_style.left_edge_control_center_step;

        for width in [350.0, 420.0, 600.0] {
            let row = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, style.button_size));
            let layout = icon_bar_layout(row, style);

            assert_eq!(layout.add_files.size(), vec2(32.0, 32.0));
            assert_eq!(
                layout.add_files.center().x - row.left(),
                expected_first_center_inset
            );
            assert_eq!(
                layout.add_url.center().x - layout.add_files.center().x,
                expected_center_step
            );
            assert_eq!(
                layout.sort.center().x - layout.add_url.center().x,
                expected_center_step
            );
            assert_eq!(
                layout.current_item.center().x - layout.sort.center().x,
                expected_center_step
            );
            assert_eq!(
                row.right() - layout.clear.right(),
                style.clear_right_padding
            );
            assert_eq!(
                layout.add_files.center().y - row.center().y,
                style.button_center_y_offset
            );
            assert_eq!(
                layout.clear.center().y - row.center().y,
                style.button_center_y_offset
            );
            assert!(layout.current_item.right() < layout.clear.left());
        }
    }

    #[test]
    fn icon_clicks_publish_the_existing_typed_actions() {
        let model = PlaylistInteractionModel {
            item_count: 3,
            go_current_target: Some(PlaylistGoCurrentTarget::Tombstone),
            ..PlaylistInteractionModel::default()
        };
        for (control, expected_action) in [
            (ToolbarControl::AddFiles, PlaylistAction::AddFiles),
            (ToolbarControl::AddUrl, PlaylistAction::OpenUrlEditor),
            (
                ToolbarControl::CurrentItem,
                PlaylistAction::GoCurrent(PlaylistGoCurrentTarget::Tombstone),
            ),
            (ToolbarControl::Clear, PlaylistAction::Clear),
        ] {
            assert_eq!(
                click_control(&Context::default(), &model, control),
                vec![expected_action]
            );
        }
    }

    #[test]
    fn sort_icon_opens_popup_without_publishing_a_sort_intent() {
        let context = Context::default();
        let model = PlaylistInteractionModel {
            item_count: 3,
            ..PlaylistInteractionModel::default()
        };

        assert!(click_control(&context, &model, ToolbarControl::Sort).is_empty());
        assert!(Popup::is_any_open(&context));
    }

    #[test]
    fn disabled_controls_do_not_publish_actions_or_open_sort_popup() {
        let disabled_cases = [
            (
                ToolbarControl::AddFiles,
                PlaylistInteractionModel {
                    file_dialog_open: true,
                    ..PlaylistInteractionModel::default()
                },
            ),
            (
                ToolbarControl::AddUrl,
                PlaylistInteractionModel {
                    structural_actions_enabled: false,
                    ..PlaylistInteractionModel::default()
                },
            ),
            (ToolbarControl::Sort, PlaylistInteractionModel::default()),
            (
                ToolbarControl::CurrentItem,
                PlaylistInteractionModel::default(),
            ),
            (ToolbarControl::Clear, PlaylistInteractionModel::default()),
        ];

        for (control, model) in disabled_cases {
            let context = Context::default();
            assert!(click_control(&context, &model, control).is_empty());
            assert!(!Popup::is_any_open(&context));
        }
    }

    #[test]
    fn tab_focus_allows_space_and_enter_activation() {
        for activation_key in [Key::Space, Key::Enter] {
            let context = Context::default();
            let model = PlaylistInteractionModel::default();
            let _ = control_frame(
                &context,
                &model,
                ToolbarControl::AddUrl,
                RawInput::default(),
            );
            let (_, tab_focused) = control_frame(
                &context,
                &model,
                ToolbarControl::AddUrl,
                keyboard_input(Key::Tab),
            );
            let (actions, still_focused) = control_frame(
                &context,
                &model,
                ToolbarControl::AddUrl,
                keyboard_input(activation_key),
            );

            assert!(tab_focused);
            assert!(still_focused);
            assert_eq!(actions, vec![PlaylistAction::OpenUrlEditor]);
        }
    }

    #[test]
    fn icon_only_controls_keep_explicit_russian_accessibility_names() {
        assert_eq!(
            ToolbarControl::AddFiles.accessible_label(),
            "Добавить файлы"
        );
        assert_eq!(ToolbarControl::AddUrl.accessible_label(), "Добавить URL");
        assert_eq!(
            ToolbarControl::Sort.accessible_label(),
            "Сортировать плейлист"
        );
        assert_eq!(
            ToolbarControl::CurrentItem.accessible_label(),
            "Перейти к текущему медиа"
        );
        assert_eq!(ToolbarControl::Clear.accessible_label(), "Очистить очередь");
    }
}
