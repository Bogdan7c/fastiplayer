use super::*;

const SCREEN_SIZE: egui::Vec2 = egui::vec2(1280.0, 720.0);

/// Нарисованный текст и его прямоугольник.
fn collect_texts(shape: &egui::Shape, texts: &mut Vec<(String, egui::Rect)>) {
    match shape {
        egui::Shape::Text(text_shape) => {
            texts.push((
                text_shape.galley.text().to_owned(),
                text_shape.visual_bounding_rect(),
            ));
        }
        egui::Shape::Vec(nested) => nested.iter().for_each(|shape| collect_texts(shape, texts)),
        _ => {}
    }
}

/// Рисует кадры с подсветкой и возвращает тексты последнего.
///
/// Первый кадр `Area` у egui — «измерительный» (содержимое скрыто), поэтому, как и в
/// приложении, где egui сам просит следующий кадр, прогоняем два.
fn painted_texts(overlay: Option<DropOverlay>) -> Vec<(String, egui::Rect)> {
    let ctx = crate::ui::test_frame::app_behavior_context();
    let mut texts = Vec::new();
    for _frame in 0..2 {
        let output = crate::ui::test_frame::run_ui_frame(
            &ctx,
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN_SIZE)),
                ..Default::default()
            },
            |ui| show_external_drop_overlay(ui.ctx(), overlay),
        );
        texts.clear();
        for clipped in &output.shapes {
            collect_texts(&clipped.shape, &mut texts);
        }
    }
    texts
}

#[test]
fn playlist_target_draws_add_hint_inside_panel_rect() {
    let panel = egui::Rect::from_min_max(egui::pos2(900.0, 40.0), egui::pos2(1280.0, 680.0));
    let texts = painted_texts(Some(DropOverlay {
        target: DropTarget::Playlist,
        rect: panel,
    }));

    let hint = texts
        .iter()
        .find(|(text, _)| text == "Отпустите, чтобы добавить в плейлист")
        .expect("подсказка панели нарисована");
    assert!(
        panel.contains_rect(hint.1),
        "подсказка внутри панели: {:?} vs {panel:?}",
        hint.1
    );
}

#[test]
fn video_target_draws_open_hint_and_no_overlay_draws_nothing() {
    let video = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(900.0, 720.0));
    let texts = painted_texts(Some(DropOverlay {
        target: DropTarget::Video,
        rect: video,
    }));
    assert!(
        texts
            .iter()
            .any(|(text, _)| text == "Отпустите, чтобы открыть")
    );
    assert!(!texts.iter().any(|(text, _)| text.contains("плейлист")));

    assert!(painted_texts(None).is_empty(), "без жеста подсветки нет");
}
