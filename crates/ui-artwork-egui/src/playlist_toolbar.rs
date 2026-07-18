//! Нейтральная векторная отрисовка компактных кнопок toolbar плейлиста.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, pos2, vec2};

/// Количество прямых сегментов, аппроксимирующих плавную Undo-дугу.
const UNDO_ARC_SEGMENT_COUNT: usize = 12;
/// Горизонтальное смещение центра Undo-дуги в долях icon extent.
const UNDO_ARC_CENTER_X: f32 = 0.05;
/// Вертикальное смещение центра Undo-дуги в долях icon extent.
const UNDO_ARC_CENTER_Y: f32 = 0.07;
/// Радиус Undo-дуги в долях icon extent.
const UNDO_ARC_RADIUS: f32 = 0.36;
/// Начальный угол открытой дуги: нижняя правая сторона остаётся незамкнутой.
const UNDO_ARC_START_RADIANS: f32 = -25.0_f32.to_radians();
/// Конечный угол задаёт против часовой стрелки движение к левому наконечнику.
const UNDO_ARC_END_RADIANS: f32 = 160.0_f32.to_radians();
/// Длина выраженного открытого наконечника в долях icon extent.
const UNDO_ARROWHEAD_LENGTH: f32 = 0.20;
/// Половина ширины основания наконечника в долях icon extent.
const UNDO_ARROWHEAD_HALF_WIDTH: f32 = 0.10;

/// Визуальный образ действия без зависимости от playlist domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistToolbarGlyph {
    /// Несколько документов с плюсом.
    AddFiles,
    /// Звенья URL-цепочки с плюсом.
    AddUrl,
    /// Строки разной длины и направленная вниз стрелка.
    Sort,
    /// Список с Play-маркером на текущей строке.
    CurrentItem,
    /// Открытая против часовой стрелки Undo-дуга с наконечником.
    Undo,
    /// Метла для очистки списка.
    Clear,
}

/// Уже разрешённое вызывающей стороной визуальное состояние кнопки.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaylistToolbarPaintState {
    /// Итоговый цвет glyph с учётом hover и disabled.
    pub foreground: Color32,
    /// Итоговая подложка; прозрачный цвет не создаёт лишний shape.
    pub surface_fill: Color32,
    /// Нужен ли видимый keyboard-focus outline.
    pub focus_visible: bool,
    /// Множитель прозрачности всей кнопки в диапазоне `0.0..=1.0`.
    pub opacity: f32,
    /// Масштаб glyph вокруг неизменного центра hit-area.
    pub content_scale: f32,
}

/// Геометрия одной кнопки, не содержащая app-specific цветов состояний.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaylistToolbarButtonStyle {
    /// Полный размер glyph внутри стабильной hit-area.
    pub icon_extent: f32,
    /// Единый визуальный вес всех открытых линий.
    pub glyph_stroke_width: f32,
    /// Радиус мягкой прямоугольной подложки.
    pub surface_corner_radius: f32,
    /// Контур keyboard focus.
    pub focus_outline: Stroke,
    /// Отступ focus outline внутрь hit-area.
    pub focus_inset: f32,
}

/// Рисует поверхность, focus и выбранный glyph внутри готовой hit-area.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    glyph: PlaylistToolbarGlyph,
    state: PlaylistToolbarPaintState,
    style: PlaylistToolbarButtonStyle,
) {
    // Artwork повторно защищает toolkit boundary от NaN и overshoot,
    // даже если нормальный вызывающий путь уже использовал animation-core.
    let opacity = normalized_unit_interval(state.opacity);
    // Нулевой или некорректный scale не должен отражать либо раздувать glyph.
    let content_scale = normalized_unit_interval(state.content_scale);
    // Все resolved цвета получают один opacity, чтобы fade был согласованным.
    let foreground = state.foreground.gamma_multiply(opacity);
    let surface_fill = state.surface_fill.gamma_multiply(opacity);
    let focus_outline = Stroke::new(
        style.focus_outline.width,
        style.focus_outline.color.gamma_multiply(opacity),
    );

    if surface_fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, style.surface_corner_radius.max(0.0), surface_fill);
    }

    if state.focus_visible {
        painter.rect_stroke(
            rect.shrink(style.focus_inset.max(0.0)),
            style.surface_corner_radius.max(0.0),
            focus_outline,
            StrokeKind::Inside,
        );
    }

    let maximum_extent = rect.width().min(rect.height()).max(0.0);
    // Масштаб применяется только к content rect: центр и hit-area не двигаются.
    let icon_extent = style.icon_extent.clamp(0.0, maximum_extent) * content_scale;
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(icon_extent));
    let stroke = Stroke::new(style.glyph_stroke_width.max(0.0), foreground);

    match glyph {
        PlaylistToolbarGlyph::AddFiles => paint_add_files(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::AddUrl => paint_add_url(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::Sort => paint_sort(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::CurrentItem => paint_current_item(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::Undo => paint_undo(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::Clear => paint_clear(painter, icon_rect, stroke),
    }
}

/// Нормализует внешний paint-параметр и не пропускает NaN в egui geometry.
fn normalized_unit_interval(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// Переводит нормализованные координаты `-0.5..=0.5` в icon rect.
fn icon_point(icon_rect: Rect, x: f32, y: f32) -> Pos2 {
    pos2(
        icon_rect.center().x + icon_rect.width() * x,
        icon_rect.center().y + icon_rect.height() * y,
    )
}

/// Возвращает точку Undo-окружности для угла в математической системе координат.
fn undo_arc_point(icon_rect: Rect, angle: f32) -> Pos2 {
    icon_point(
        icon_rect,
        UNDO_ARC_CENTER_X + UNDO_ARC_RADIUS * angle.cos(),
        UNDO_ARC_CENTER_Y - UNDO_ARC_RADIUS * angle.sin(),
    )
}

/// Документы остаются различимыми отдельно от плюса даже при размере 16 points.
fn paint_add_files(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    let back_page = [
        icon_point(icon_rect, -0.40, -0.27),
        icon_point(icon_rect, -0.40, 0.42),
        icon_point(icon_rect, 0.08, 0.42),
    ];
    painter.add(Shape::line(back_page.to_vec(), stroke));

    let page_left = -0.27;
    let page_top = -0.42;
    let page_right = 0.31;
    let page_bottom = 0.30;
    let fold = 0.16;
    for endpoints in [
        [
            icon_point(icon_rect, page_left, page_top),
            icon_point(icon_rect, page_right - fold, page_top),
        ],
        [
            icon_point(icon_rect, page_right - fold, page_top),
            icon_point(icon_rect, page_right, page_top + fold),
        ],
        [
            icon_point(icon_rect, page_right, page_top + fold),
            icon_point(icon_rect, page_right, page_bottom),
        ],
        [
            icon_point(icon_rect, page_right, page_bottom),
            icon_point(icon_rect, page_left, page_bottom),
        ],
        [
            icon_point(icon_rect, page_left, page_bottom),
            icon_point(icon_rect, page_left, page_top),
        ],
    ] {
        painter.line_segment(endpoints, stroke);
    }

    let plus_center = icon_point(icon_rect, 0.01, 0.06);
    let plus_half_extent = icon_rect.width() * 0.11;
    painter.line_segment(
        [
            plus_center - vec2(plus_half_extent, 0.0),
            plus_center + vec2(plus_half_extent, 0.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            plus_center - vec2(0.0, plus_half_extent),
            plus_center + vec2(0.0, plus_half_extent),
        ],
        stroke,
    );
}

/// Два пересекающихся округлённых звена читаются как URL, а отдельный плюс — как добавление.
fn paint_add_url(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    let link_size = vec2(icon_rect.width() * 0.52, icon_rect.height() * 0.25);
    let left_link = Rect::from_center_size(icon_point(icon_rect, -0.16, 0.12), link_size);
    let right_link = Rect::from_center_size(icon_point(icon_rect, 0.14, -0.06), link_size);
    let link_radius = link_size.y * 0.5;
    painter.rect_stroke(left_link, link_radius, stroke, StrokeKind::Middle);
    painter.rect_stroke(right_link, link_radius, stroke, StrokeKind::Middle);

    let plus_center = icon_point(icon_rect, 0.34, -0.34);
    let plus_half_extent = icon_rect.width() * 0.10;
    painter.line_segment(
        [
            plus_center - vec2(plus_half_extent, 0.0),
            plus_center + vec2(plus_half_extent, 0.0),
        ],
        stroke,
    );
    painter.line_segment(
        [
            plus_center - vec2(0.0, plus_half_extent),
            plus_center + vec2(0.0, plus_half_extent),
        ],
        stroke,
    );
}

/// Убывающие строки показывают порядок, а стрелка — направление сортировки.
fn paint_sort(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    for (y, right) in [(-0.31, 0.12), (0.0, 0.02), (0.31, -0.09)] {
        painter.line_segment(
            [
                icon_point(icon_rect, -0.43, y),
                icon_point(icon_rect, right, y),
            ],
            stroke,
        );
    }

    painter.line_segment(
        [
            icon_point(icon_rect, 0.34, -0.35),
            icon_point(icon_rect, 0.34, 0.34),
        ],
        stroke,
    );
    painter.line_segment(
        [
            icon_point(icon_rect, 0.20, 0.20),
            icon_point(icon_rect, 0.34, 0.34),
        ],
        stroke,
    );
    painter.line_segment(
        [
            icon_point(icon_rect, 0.48, 0.20),
            icon_point(icon_rect, 0.34, 0.34),
        ],
        stroke,
    );
}

/// Play-маркер заменяет bullet центральной строки и однозначно обозначает играющий элемент.
fn paint_current_item(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    for y in [-0.31, 0.31] {
        painter.circle_filled(
            icon_point(icon_rect, -0.37, y),
            stroke.width * 0.62,
            stroke.color,
        );
        painter.line_segment(
            [
                icon_point(icon_rect, -0.20, y),
                icon_point(icon_rect, 0.38, y),
            ],
            stroke,
        );
    }

    painter.add(Shape::convex_polygon(
        vec![
            icon_point(icon_rect, -0.42, -0.13),
            icon_point(icon_rect, -0.42, 0.13),
            icon_point(icon_rect, -0.18, 0.0),
        ],
        stroke.color,
        Stroke::NONE,
    ));
    painter.line_segment(
        [
            icon_point(icon_rect, -0.08, 0.0),
            icon_point(icon_rect, 0.28, 0.0),
        ],
        stroke,
    );
    painter.add(Shape::line(
        vec![
            icon_point(icon_rect, 0.42, -0.13),
            icon_point(icon_rect, 0.42, 0.13),
            icon_point(icon_rect, 0.31, 0.13),
        ],
        stroke,
    ));
}

/// Рисует открытую против часовой стрелки дугу и заметный наконечник.
fn paint_undo(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    // Дуга строится слева направо в математических координатах, а знак Y
    // инвертируется при переводе в экранную систему egui.
    let arc_points = (0..=UNDO_ARC_SEGMENT_COUNT)
        .map(|segment_index| {
            // Доля сегмента детерминирована и включает обе границы дуги.
            let segment_progress = segment_index as f32 / UNDO_ARC_SEGMENT_COUNT as f32;
            // Равномерный угол даёт гладкую polyline без скрытых bezier-control points.
            let angle = UNDO_ARC_START_RADIANS
                + (UNDO_ARC_END_RADIANS - UNDO_ARC_START_RADIANS) * segment_progress;
            // Нормализованная окружность масштабируется общим icon rect.
            undo_arc_point(icon_rect, angle)
        })
        .collect::<Vec<_>>();
    // Граничные точки вычисляются той же функцией и остаются доступны после move Vec.
    let arc_start = undo_arc_point(icon_rect, UNDO_ARC_START_RADIANS);
    let arrow_tip = undo_arc_point(icon_rect, UNDO_ARC_END_RADIANS);
    // Единственная открытая polyline сохраняет стабильный визуальный вес дуги.
    painter.add(Shape::line(arc_points, stroke));

    // Конечная касательная показывает именно против часовой стрелки направление.
    let arrow_direction = vec2(-UNDO_ARC_END_RADIANS.sin(), -UNDO_ARC_END_RADIANS.cos());
    // Нормаль раскрывает две симметричные стороны наконечника.
    let arrow_normal = vec2(-arrow_direction.y, arrow_direction.x);
    // Конечная точка дуги одновременно является остриём стрелки.
    // Основание смещено назад по касательной и раскрыто поперёк неё.
    let arrow_base_center =
        arrow_tip - arrow_direction * (icon_rect.width() * UNDO_ARROWHEAD_LENGTH);
    // Два конца основания определяют выраженный открытый наконечник.
    let arrow_half_width = icon_rect.width() * UNDO_ARROWHEAD_HALF_WIDTH;
    let arrow_base_first = arrow_base_center + arrow_normal * arrow_half_width;
    let arrow_base_second = arrow_base_center - arrow_normal * arrow_half_width;
    // Обе стороны используют тот же stroke, поэтому дуга и наконечник едины.
    painter.line_segment([arrow_tip, arrow_base_first], stroke);
    painter.line_segment([arrow_tip, arrow_base_second], stroke);

    // Малые круги превращают butt-caps egui polyline в визуально круглые концы.
    let cap_radius = stroke.width * 0.5;
    for cap_center in [arc_start, arrow_tip, arrow_base_first, arrow_base_second] {
        painter.circle_filled(cap_center, cap_radius, stroke.color);
    }
}

/// Наклонная ручка, муфта и цельный веер щетинок образуют силуэт метлы.
fn paint_clear(painter: &Painter, icon_rect: Rect, stroke: Stroke) {
    painter.line_segment(
        [
            icon_point(icon_rect, 0.42, -0.43),
            icon_point(icon_rect, -0.03, 0.02),
        ],
        stroke,
    );
    painter.line_segment(
        [
            icon_point(icon_rect, -0.13, -0.07),
            icon_point(icon_rect, 0.07, 0.13),
        ],
        Stroke::new(stroke.width * 1.6, stroke.color),
    );
    let bristle_origin = icon_point(icon_rect, -0.04, 0.05);
    let bristle_tips = [
        icon_point(icon_rect, -0.43, 0.31),
        icon_point(icon_rect, -0.31, 0.43),
        icon_point(icon_rect, -0.14, 0.46),
        icon_point(icon_rect, 0.06, 0.37),
    ];
    for bristle_tip in bristle_tips {
        painter.line_segment([bristle_origin, bristle_tip], stroke);
    }
    for adjacent_tips in bristle_tips.windows(2) {
        painter.line_segment([adjacent_tips[0], adjacent_tips[1]], stroke);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use egui::{Context, RawInput};

    use super::*;

    fn hit_rect() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), Vec2::splat(28.0))
    }

    fn style() -> PlaylistToolbarButtonStyle {
        PlaylistToolbarButtonStyle {
            icon_extent: 16.0,
            glyph_stroke_width: 1.5,
            surface_corner_radius: 4.0,
            focus_outline: Stroke::new(1.5, Color32::WHITE),
            focus_inset: 1.5,
        }
    }

    fn state(surface_fill: Color32, focus_visible: bool) -> PlaylistToolbarPaintState {
        PlaylistToolbarPaintState {
            foreground: Color32::from_gray(230),
            surface_fill,
            focus_visible,
            opacity: 1.0,
            content_scale: 1.0,
        }
    }

    fn glyph_output(glyph: PlaylistToolbarGlyph) -> egui::FullOutput {
        Context::default().run_ui(RawInput::default(), |ui| {
            paint(
                ui.painter(),
                hit_rect(),
                glyph,
                state(Color32::TRANSPARENT, false),
                style(),
            );
        })
    }

    #[test]
    fn glyphs_have_stable_shape_counts() {
        let actual_counts = [
            PlaylistToolbarGlyph::AddFiles,
            PlaylistToolbarGlyph::AddUrl,
            PlaylistToolbarGlyph::Sort,
            PlaylistToolbarGlyph::CurrentItem,
            PlaylistToolbarGlyph::Undo,
            PlaylistToolbarGlyph::Clear,
        ]
        .map(|glyph| glyph_output(glyph).shapes.len());

        assert_eq!(actual_counts, [8, 4, 6, 7, 7, 9]);
    }

    #[test]
    fn every_glyph_stays_inside_its_hit_area() {
        let bounds = hit_rect().expand(style().glyph_stroke_width);
        for glyph in [
            PlaylistToolbarGlyph::AddFiles,
            PlaylistToolbarGlyph::AddUrl,
            PlaylistToolbarGlyph::Sort,
            PlaylistToolbarGlyph::CurrentItem,
            PlaylistToolbarGlyph::Undo,
            PlaylistToolbarGlyph::Clear,
        ] {
            let output = glyph_output(glyph);
            assert!(
                output
                    .shapes
                    .iter()
                    .all(|shape| bounds.contains_rect(shape.shape.visual_bounding_rect())),
                "{glyph:?} вышел за hit-area"
            );
        }
    }

    #[test]
    fn every_semantic_glyph_has_distinct_geometry() {
        let fingerprints: HashSet<_> = [
            PlaylistToolbarGlyph::AddFiles,
            PlaylistToolbarGlyph::AddUrl,
            PlaylistToolbarGlyph::Sort,
            PlaylistToolbarGlyph::CurrentItem,
            PlaylistToolbarGlyph::Undo,
            PlaylistToolbarGlyph::Clear,
        ]
        .map(|glyph| format!("{:?}", glyph_output(glyph).shapes))
        .into_iter()
        .collect();

        assert_eq!(fingerprints.len(), 6);
    }

    #[test]
    fn hover_and_focus_add_surfaces_without_moving_glyph() {
        let idle_output = glyph_output(PlaylistToolbarGlyph::CurrentItem);
        let decorated_output = Context::default().run_ui(RawInput::default(), |ui| {
            paint(
                ui.painter(),
                hit_rect(),
                PlaylistToolbarGlyph::CurrentItem,
                state(Color32::from_white_alpha(28), true),
                style(),
            );
        });

        assert_eq!(decorated_output.shapes.len(), idle_output.shapes.len() + 2);
        for (idle_shape, decorated_shape) in idle_output
            .shapes
            .iter()
            .zip(decorated_output.shapes.iter().skip(2))
        {
            assert_eq!(
                idle_shape.shape.visual_bounding_rect(),
                decorated_shape.shape.visual_bounding_rect()
            );
        }
    }

    #[test]
    fn undo_opacity_and_scale_preserve_the_hit_area_center() {
        let full_output = glyph_output(PlaylistToolbarGlyph::Undo);
        let animated_output = Context::default().run_ui(RawInput::default(), |ui| {
            let mut animated_state = state(Color32::TRANSPARENT, false);
            animated_state.opacity = 0.5;
            animated_state.content_scale = 0.8;
            paint(
                ui.painter(),
                hit_rect(),
                PlaylistToolbarGlyph::Undo,
                animated_state,
                style(),
            );
        });

        assert_eq!(animated_output.shapes.len(), full_output.shapes.len());
        let full_bounds = full_output
            .shapes
            .iter()
            .fold(Rect::NOTHING, |bounds, shape| {
                bounds.union(shape.shape.visual_bounding_rect())
            });
        let animated_bounds = animated_output
            .shapes
            .iter()
            .fold(Rect::NOTHING, |bounds, shape| {
                bounds.union(shape.shape.visual_bounding_rect())
            });
        // Несимметричный glyph сам имеет смещённый visual center, поэтому при
        // масштабировании вокруг hit-area его смещение тоже уменьшается на 0.8.
        let hit_center = hit_rect().center();
        let expected_animated_center = hit_center + (full_bounds.center() - hit_center) * 0.8;
        assert!((animated_bounds.center().x - expected_animated_center.x).abs() < 0.1);
        assert!((animated_bounds.center().y - expected_animated_center.y).abs() < 0.1);
        assert!(animated_bounds.width() < full_bounds.width());
        assert!(animated_bounds.height() < full_bounds.height());
        assert!(animated_output.shapes.iter().all(|shape| {
            let color = match &shape.shape {
                Shape::Path(path) => match &path.stroke.color {
                    egui::epaint::ColorMode::Solid(color) => *color,
                    unexpected => panic!("неожиданный цвет Undo path: {unexpected:?}"),
                },
                Shape::LineSegment { stroke, .. } => stroke.color,
                Shape::Circle(circle) => circle.fill,
                unexpected => panic!("неожиданный Undo shape: {unexpected:?}"),
            };
            color.a() < Color32::from_gray(230).a()
        }));
    }

    #[test]
    fn undo_bounds_remain_stable_on_fractional_hidpi_coordinates() {
        let fractional_rect = Rect::from_min_size(pos2(10.25, 20.75), Vec2::splat(28.0));
        let bounds = fractional_rect.expand(style().glyph_stroke_width);
        let output = Context::default().run_ui(RawInput::default(), |ui| {
            paint(
                ui.painter(),
                fractional_rect,
                PlaylistToolbarGlyph::Undo,
                state(Color32::TRANSPARENT, false),
                style(),
            );
        });

        assert!(
            output
                .shapes
                .iter()
                .all(|shape| bounds.contains_rect(shape.shape.visual_bounding_rect()))
        );
    }
}
