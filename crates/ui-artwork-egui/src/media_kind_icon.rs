//! Ручная отрисовка компактных иконок типа медиа для строк плейлиста.

use egui::{Painter, Rect, Shape, Stroke, StrokeKind, pos2, vec2};

/// Нейтральный визуальный тип медиа без зависимости от playlist-domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKindGlyph {
    /// Тип медиа ещё не подтверждён.
    Unknown,
    /// Медиа содержит только аудиодорожку.
    Audio,
    /// Медиа содержит как минимум одну видеодорожку.
    Video,
}

/// Отступ защищает край штриха от обрезания clip-прямоугольником ячейки.
const ICON_EDGE_INSET: f32 = 1.0;

/// Рисует выбранную иконку в центре переданной ячейки.
pub(crate) fn paint(painter: &Painter, cell_rect: Rect, glyph: MediaKindGlyph, stroke: Stroke) {
    // Квадрат строится по меньшей стороне, поэтому широкая или высокая ячейка не искажает glyph.
    let icon_side = (cell_rect.width().min(cell_rect.height()) - ICON_EDGE_INSET * 2.0).max(0.0);
    // Все варианты получают одинаковую визуальную область и остаются выровненными по центру.
    let icon_rect = Rect::from_center_size(cell_rect.center(), egui::Vec2::splat(icon_side));

    // Каждый вариант делегируется маленькой функции со своей геометрией.
    match glyph {
        MediaKindGlyph::Unknown => paint_unknown_file(painter, icon_rect, stroke),
        MediaKindGlyph::Audio => paint_audio_note(painter, icon_rect, stroke),
        MediaKindGlyph::Video => paint_video_frame(painter, icon_rect, stroke),
    }
}

/// Рисует ноту: заполненную головку, ножку и короткий флажок.
fn paint_audio_note(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    // Головка располагается в нижней левой части квадратной области.
    let note_head = pos2(
        icon_rect.left() + icon_rect.width() * 0.30,
        icon_rect.bottom() - icon_rect.height() * 0.22,
    );
    // Радиус масштабируется вместе с ячейкой и не зависит от DPI.
    let head_radius = icon_rect.width() * 0.14;
    // Ножка начинается у правого края головки.
    let stem_x = note_head.x + head_radius * 0.72;
    // Верх ножки оставляет небольшой визуальный отступ от края.
    let stem_top_y = icon_rect.top() + icon_rect.height() * 0.14;
    // Конец флажка направлен вправо и немного вниз.
    let flag_end = pos2(
        icon_rect.right() - icon_rect.width() * 0.10,
        icon_rect.top() + icon_rect.height() * 0.30,
    );
    // Нижняя точка делает флажок узнаваемым даже в размере около 14 пикселей.
    let flag_tip = pos2(
        icon_rect.right() - icon_rect.width() * 0.18,
        icon_rect.top() + icon_rect.height() * 0.43,
    );

    // Заполненная головка остаётся читаемой на тёмном фоне плейлиста.
    painter.circle_filled(note_head, head_radius, stroke.color);
    // Вертикальный штрих формирует ножку ноты.
    painter.line_segment(
        [pos2(stem_x, note_head.y), pos2(stem_x, stem_top_y)],
        stroke,
    );
    // Первый сегмент флажка идёт от ножки к правому краю.
    painter.line_segment([pos2(stem_x, stem_top_y), flag_end], stroke);
    // Второй сегмент слегка загибает флажок вниз.
    painter.line_segment([flag_end, flag_tip], stroke);
}

/// Рисует видеокадр с заполненным треугольником Play.
fn paint_video_frame(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    // Видеокадр шире своей высоты, как экран или миниатюра ролика.
    let frame_rect = Rect::from_center_size(
        icon_rect.center(),
        vec2(icon_rect.width(), icon_rect.height() * 0.72),
    );
    // Малое скругление согласуется с остальными контурными иконками приложения.
    let corner_radius = frame_rect.height() * 0.10;
    // Контур размещается по центру линии и не расширяет заявленную геометрию.
    painter.rect_stroke(frame_rect, corner_radius, stroke, StrokeKind::Middle);

    // Центр треугольника слегка сдвинут вправо для оптического выравнивания.
    let play_center = frame_rect.center() + vec2(frame_rect.width() * 0.025, 0.0);
    // Половина высоты задаёт компактный, но различимый знак Play.
    let play_half_height = frame_rect.height() * 0.24;
    // Половина ширины учитывает вытянутую форму треугольника.
    let play_half_width = frame_rect.width() * 0.17;
    // Заполненный треугольник лучше читается, чем ещё один тонкий контур.
    painter.add(Shape::convex_polygon(
        vec![
            pos2(
                play_center.x - play_half_width,
                play_center.y - play_half_height,
            ),
            pos2(
                play_center.x - play_half_width,
                play_center.y + play_half_height,
            ),
            pos2(play_center.x + play_half_width, play_center.y),
        ],
        stroke.color,
        Stroke::NONE,
    ));
}

/// Рисует нейтральный файл со знаком вопроса для ещё не определённого типа.
fn paint_unknown_file(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    // Узкая вертикальная форма отличает файл от видеокадра.
    let file_rect = Rect::from_center_size(
        icon_rect.center(),
        vec2(icon_rect.width() * 0.74, icon_rect.height()),
    );
    // Сгиб занимает небольшую долю ширины и остаётся читаемым в 14 пикселях.
    let fold_extent = file_rect.width() * 0.25;
    // Верхняя горизонталь заканчивается перед сгибом.
    let fold_start = pos2(file_rect.right() - fold_extent, file_rect.top());
    // Диагональ сгиба приходит на правую грань файла.
    let fold_end = pos2(file_rect.right(), file_rect.top() + fold_extent);

    // Пять сегментов образуют контур файла без лишней заливки.
    for segment in [
        [file_rect.left_top(), fold_start],
        [fold_start, fold_end],
        [fold_end, file_rect.right_bottom()],
        [file_rect.right_bottom(), file_rect.left_bottom()],
        [file_rect.left_bottom(), file_rect.left_top()],
    ] {
        // Каждый сегмент использует общий stroke текущей темы.
        painter.line_segment(segment, stroke);
    }

    // Знак вопроса центрируется в свободной нижней части файла.
    let question_center_x = file_rect.center().x;
    // Верх вопроса начинается ниже сгиба.
    let question_top_y = file_rect.top() + file_rect.height() * 0.36;
    // Ломаная имитирует дугу вопроса без размытия на малом размере.
    painter.add(Shape::line(
        vec![
            pos2(question_center_x - file_rect.width() * 0.14, question_top_y),
            pos2(
                question_center_x,
                question_top_y - file_rect.height() * 0.06,
            ),
            pos2(question_center_x + file_rect.width() * 0.13, question_top_y),
            pos2(
                question_center_x + file_rect.width() * 0.10,
                question_top_y + file_rect.height() * 0.13,
            ),
            pos2(
                question_center_x,
                question_top_y + file_rect.height() * 0.20,
            ),
            pos2(
                question_center_x,
                question_top_y + file_rect.height() * 0.29,
            ),
        ],
        stroke,
    ));
    // Отдельная точка завершает знак вопроса и сохраняет чёткость.
    painter.circle_filled(
        pos2(
            question_center_x,
            file_rect.bottom() - file_rect.height() * 0.16,
        ),
        (stroke.width * 0.65).max(0.7),
        stroke.color,
    );
}
