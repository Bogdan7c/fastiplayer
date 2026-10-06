//! Индикатор получения данных по ссылке: настоящий egui-кадр, текст и клик «Отменить».

use std::sync::Arc;

use egui::{Context, Event, Modifiers, PointerButton, RawInput, Rect, pos2, vec2};

use super::{CANCEL_BUTTON_LABEL, show};
use crate::playlist_runtime::PlaylistUrlImportProgress;
use crate::ui::playlist::PlaylistUiOutput;
use crate::ui::playlist::actions::PlaylistAction;

/// Итог одного кадра: действия, весь нарисованный текст и прямоугольник кнопки отмены.
struct RenderedFrame {
    actions: Vec<PlaylistAction>,
    painted_texts: Vec<String>,
    cancel_button_rect: Option<Rect>,
}

/// Рисует индикатор в headless egui с заданными событиями ввода.
fn render(
    context: &Context,
    progress: &PlaylistUrlImportProgress,
    events: Vec<Event>,
) -> RenderedFrame {
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(420.0, 120.0))),
        events,
        ..RawInput::default()
    };
    let mut output = PlaylistUiOutput::default();
    let full_output = crate::ui::test_frame::run_ui_frame(context, input, |ui| {
        ui.set_width(420.0);
        show(ui, progress, &mut output);
    });
    let painted_texts = full_output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text_shape) => Some(text_shape.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    // Прямоугольник кнопки берём из дерева доступности — так же её «видит» диктор.
    let cancel_button_rect = full_output
        .platform_output
        .accesskit_update
        .and_then(|update| {
            update.nodes.into_iter().find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node.label() == Some(CANCEL_BUTTON_LABEL))
                .then(|| node.bounds())
                .flatten()
            })
        })
        .map(|bounds| {
            Rect::from_min_max(
                pos2(bounds.x0 as f32, bounds.y0 as f32),
                pos2(bounds.x1 as f32, bounds.y1 as f32),
            )
        });
    RenderedFrame {
        actions: output.take_actions(),
        painted_texts,
        cancel_button_rect,
    }
}

/// Нажатие левой кнопкой мыши в точке.
fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos: position,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn indicator_names_domain_and_cancel_click_publishes_cancel_action() {
    let context = Context::default();
    context.enable_accesskit();
    let progress = PlaylistUrlImportProgress {
        display_host: Some(Arc::from("youtube.com")),
    };

    let first_frame = render(&context, &progress, Vec::new());
    assert!(
        first_frame
            .painted_texts
            .iter()
            .any(|text| text == "Получаем данные по ссылке (youtube.com)…"),
        "индикатор показывает домен: {:?}",
        first_frame.painted_texts
    );
    // Сам по себе индикатор ничего не отменяет.
    assert!(first_frame.actions.is_empty());

    let button_center = first_frame
        .cancel_button_rect
        .expect("кнопка «Отменить» есть в дереве доступности")
        .center();
    let _hover = render(
        &context,
        &progress,
        vec![Event::PointerMoved(button_center)],
    );
    let _press = render(
        &context,
        &progress,
        vec![pointer_button(button_center, true)],
    );
    let release = render(
        &context,
        &progress,
        vec![pointer_button(button_center, false)],
    );

    assert_eq!(release.actions, vec![PlaylistAction::CancelUrlImport]);
}

#[test]
fn indicator_without_domain_does_not_invent_one() {
    let context = Context::default();
    let progress = PlaylistUrlImportProgress { display_host: None };

    let frame = render(&context, &progress, Vec::new());

    assert!(
        frame
            .painted_texts
            .iter()
            .any(|text| text == "Получаем данные по ссылке…"),
        "{:?}",
        frame.painted_texts
    );
    assert!(!frame.painted_texts.iter().any(|text| text.contains('(')));
}
