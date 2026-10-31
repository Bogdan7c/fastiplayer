use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, Painter, Rect};

/// Символ, которым обрезается слишком длинный заголовок.
const OVERFLOW_CHARACTER: char = '…';

/// Рисует заголовок по центру `r` в одну строку. Слишком длинный заголовок
/// обрезается многоточием по ширине `r`, а не уходит под кнопки окна.
pub(crate) fn paint(p: &Painter, r: Rect, title: &str, color: Color32) {
    let mut job = LayoutJob::simple_singleline(title.to_owned(), FontId::proportional(15.0), color);
    job.wrap = TextWrapping {
        max_width: r.width().max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some(OVERFLOW_CHARACTER),
    };
    let galley = p.layout_job(job);
    // Центрируем по прямоугольнику; clip страхует от лишних пикселей.
    let top_left = r.center() - galley.size() / 2.0;
    p.with_clip_rect(r).galley(top_left, galley, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Рисует заголовок в прямоугольник заданной ширины, возвращает показанный текст
    /// первой строки и ширину нарисованного текста.
    fn painted_row_text_and_width(title: &str, width: f32) -> (String, f32) {
        let context = egui::Context::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            let rect = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, 30.0));
            paint(&ui.painter().clone(), rect, title, Color32::WHITE);
        });
        // Тест не применяет текстуры на GPU: их обновления сбрасываем явно.
        output.textures_delta.clear();
        let mut painted = (String::new(), 0.0);
        for clipped in output.shapes {
            if let egui::epaint::Shape::Text(text) = clipped.shape {
                painted = (text.galley.rows[0].text(), text.galley.size().x);
            }
        }
        painted
    }

    /// Длинный заголовок режется многоточием и не шире отведённого места.
    #[test]
    fn a_long_title_is_ellipsized_to_the_available_width() {
        let long_title = "A very long page title that will never fit in the title bar ".repeat(5);
        let (text, width) = painted_row_text_and_width(&long_title, 120.0);
        assert!(width <= 120.5, "width {width}");
        assert!(text.ends_with(OVERFLOW_CHARACTER), "{text}");
    }

    /// Короткий заголовок остаётся целым.
    #[test]
    fn a_short_title_stays_intact() {
        let (text, _) = painted_row_text_and_width("Fastiplayer", 400.0);
        assert_eq!(text, "Fastiplayer");
    }
}
