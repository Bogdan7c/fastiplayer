//! Нейтральная векторная отрисовка компактных кнопок toolbar плейлиста.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, pos2, vec2};

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
    /// Метла для очистки списка.
    Clear,
}

/// Уже разрешённое вызывающей стороной визуальное состояние кнопки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaylistToolbarPaintState {
    /// Итоговый цвет glyph с учётом hover и disabled.
    pub foreground: Color32,
    /// Итоговая подложка; прозрачный цвет не создаёт лишний shape.
    pub surface_fill: Color32,
    /// Нужен ли видимый keyboard-focus outline.
    pub focus_visible: bool,
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
    if state.surface_fill != Color32::TRANSPARENT {
        painter.rect_filled(
            rect,
            style.surface_corner_radius.max(0.0),
            state.surface_fill,
        );
    }

    if state.focus_visible {
        painter.rect_stroke(
            rect.shrink(style.focus_inset.max(0.0)),
            style.surface_corner_radius.max(0.0),
            style.focus_outline,
            StrokeKind::Inside,
        );
    }

    let maximum_extent = rect.width().min(rect.height()).max(0.0);
    let icon_extent = style.icon_extent.clamp(0.0, maximum_extent);
    let icon_rect = Rect::from_center_size(rect.center(), Vec2::splat(icon_extent));
    let stroke = Stroke::new(style.glyph_stroke_width.max(0.0), state.foreground);

    match glyph {
        PlaylistToolbarGlyph::AddFiles => paint_add_files(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::AddUrl => paint_add_url(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::Sort => paint_sort(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::CurrentItem => paint_current_item(painter, icon_rect, stroke),
        PlaylistToolbarGlyph::Clear => paint_clear(painter, icon_rect, stroke),
    }
}

/// Переводит нормализованные координаты `-0.5..=0.5` в icon rect.
fn icon_point(icon_rect: Rect, x: f32, y: f32) -> Pos2 {
    pos2(
        icon_rect.center().x + icon_rect.width() * x,
        icon_rect.center().y + icon_rect.height() * y,
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
            PlaylistToolbarGlyph::Clear,
        ]
        .map(|glyph| glyph_output(glyph).shapes.len());

        assert_eq!(actual_counts, [8, 4, 6, 7, 9]);
    }

    #[test]
    fn every_glyph_stays_inside_its_hit_area() {
        let bounds = hit_rect().expand(style().glyph_stroke_width);
        for glyph in [
            PlaylistToolbarGlyph::AddFiles,
            PlaylistToolbarGlyph::AddUrl,
            PlaylistToolbarGlyph::Sort,
            PlaylistToolbarGlyph::CurrentItem,
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
            PlaylistToolbarGlyph::Clear,
        ]
        .map(|glyph| format!("{:?}", glyph_output(glyph).shapes))
        .into_iter()
        .collect();

        assert_eq!(fingerprints.len(), 5);
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
}
