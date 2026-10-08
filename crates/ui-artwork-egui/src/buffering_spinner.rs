//! Нейтральная векторная отрисовка индикатора ожидания данных («буферизация»).
//!
//! Модуль рисует только круглую полупрозрачную подложку и дугу под заданным углом.
//! Он не знает о playback-состоянии, задержке появления и reduced motion: угол поворота
//! вычисляет `app-egui` (из времени кадра или фиксированный), здесь — только геометрия.

use std::f32::consts::TAU;

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, vec2};

/// Доля окружности, которую занимает дуга: разрыв в четверть круга делает вращение
/// заметным, а неподвижную дугу (reduced motion) — узнаваемым знаком ожидания.
const ARC_SWEEP_FRACTION: f32 = 0.75;
/// Число прямых сегментов, аппроксимирующих дугу; на типичном размере 44 pt ломаная
/// неотличима от окружности.
const ARC_SEGMENT_COUNT: usize = 32;

/// Уже разрешённое вызывающей стороной состояние кадра.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BufferingSpinnerPaintState {
    /// Угол начала дуги в радианах (по часовой стрелке от направления «вправо»).
    pub rotation_radians: f32,
}

/// Нейтральные визуальные токены спиннера.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BufferingSpinnerStyle {
    /// Заливка круглой подложки; отделяет дугу от светлого или пёстрого кадра.
    pub backdrop_fill: Color32,
    /// Отступ дуги от края подложки.
    pub arc_inset: f32,
    /// Толщина и цвет дуги.
    pub arc_stroke: Stroke,
}

/// Рисует подложку и дугу, вписанные в квадрат по центру `rect`.
///
/// Вырожденный (нулевой, отрицательный, NaN) размер ничего не рисует: лучше пропустить
/// кадр индикатора, чем нарисовать фигуру в бесконечности.
pub(crate) fn paint(
    painter: &Painter,
    rect: Rect,
    state: BufferingSpinnerPaintState,
    style: BufferingSpinnerStyle,
) {
    // Проверяется сам rect: `f32::min` молча пропускает NaN одной из сторон.
    if !(rect.is_finite() && rect.is_positive()) {
        return;
    }
    let backdrop_radius = rect.width().min(rect.height()) * 0.5;
    let center = rect.center();
    painter.circle_filled(center, backdrop_radius, style.backdrop_fill);

    let arc_radius = backdrop_radius - style.arc_inset.max(0.0) - style.arc_stroke.width * 0.5;
    if arc_radius <= 0.0 {
        return;
    }
    painter.add(Shape::line(
        arc_points(
            center,
            arc_radius,
            normalized_rotation(state.rotation_radians),
        ),
        style.arc_stroke,
    ));
}

/// Точки дуги от `rotation` на [`ARC_SWEEP_FRACTION`] окружности, включая оба конца.
fn arc_points(center: Pos2, radius: f32, rotation: f32) -> Vec<Pos2> {
    let sweep = TAU * ARC_SWEEP_FRACTION;
    (0..=ARC_SEGMENT_COUNT)
        .map(|segment| {
            let angle = rotation + sweep * segment as f32 / ARC_SEGMENT_COUNT as f32;
            center + radius * vec2(angle.cos(), angle.sin())
        })
        .collect()
}

/// Приводит угол к `0..TAU`; NaN/бесконечность (сломанный источник времени) дают 0.
fn normalized_rotation(rotation_radians: f32) -> f32 {
    if rotation_radians.is_finite() {
        rotation_radians.rem_euclid(TAU)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use egui::{Context, RawInput, pos2};

    use super::*;

    fn style() -> BufferingSpinnerStyle {
        BufferingSpinnerStyle {
            backdrop_fill: Color32::from_black_alpha(140),
            arc_inset: 6.0,
            arc_stroke: Stroke::new(3.0, Color32::WHITE),
        }
    }

    fn painted_shapes(rect: Rect, rotation_radians: f32) -> Vec<Shape> {
        let context = Context::default();
        let output = crate::test_frame::run_ui_frame(&context, RawInput::default(), |ui| {
            paint(
                ui.painter(),
                rect,
                BufferingSpinnerPaintState { rotation_radians },
                style(),
            );
        });
        output
            .shapes
            .into_iter()
            .map(|clipped| clipped.shape)
            .collect()
    }

    fn arc_of(shapes: &[Shape]) -> Vec<Pos2> {
        match shapes {
            [Shape::Circle(_), Shape::Path(path)] => path.points.clone(),
            other => panic!("ожидались подложка и дуга, нарисовано: {other:?}"),
        }
    }

    fn square() -> Rect {
        Rect::from_center_size(pos2(100.0, 80.0), vec2(44.0, 44.0))
    }

    #[test]
    fn paints_backdrop_and_open_arc_inside_rect() {
        let shapes = painted_shapes(square(), 0.0);
        let Shape::Circle(backdrop) = &shapes[0] else {
            panic!("первой рисуется подложка: {shapes:?}");
        };
        assert_eq!(backdrop.center, square().center());
        assert!((backdrop.radius - 22.0).abs() < 1e-4);

        let arc = arc_of(&shapes);
        assert_eq!(arc.len(), ARC_SEGMENT_COUNT + 1);
        // Радиус дуги: 22 − отступ 6 − половина толщины 1.5.
        for point in &arc {
            assert!((point.distance(square().center()) - 14.5).abs() < 1e-3);
            assert!(square().contains(*point));
        }
        // Дуга открыта: концы разнесены, четверть круга пустая.
        let gap = arc[0].distance(arc[arc.len() - 1]);
        assert!(gap > 14.5, "концы дуги слишком близко: {gap}");
    }

    #[test]
    fn rotation_moves_arc_start_and_full_turn_is_identity() {
        let at_zero = arc_of(&painted_shapes(square(), 0.0));
        let quarter = arc_of(&painted_shapes(square(), TAU / 4.0));
        let full_turn = arc_of(&painted_shapes(square(), TAU));

        // Начало дуги при нуле — справа от центра, при четверти оборота — снизу.
        assert!((at_zero[0] - pos2(114.5, 80.0)).length() < 1e-3);
        assert!((quarter[0] - pos2(100.0, 94.5)).length() < 1e-3);
        for (expected, actual) in at_zero.iter().zip(&full_turn) {
            assert!((*expected - *actual).length() < 1e-3);
        }
    }

    #[test]
    fn broken_rotation_falls_back_to_zero_angle() {
        let at_zero = arc_of(&painted_shapes(square(), 0.0));
        for broken in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(arc_of(&painted_shapes(square(), broken)), at_zero);
        }
    }

    #[test]
    fn degenerate_rect_paints_nothing() {
        for rect in [
            Rect::from_min_size(pos2(10.0, 10.0), vec2(0.0, 44.0)),
            Rect::from_min_size(pos2(10.0, 10.0), vec2(f32::NAN, 44.0)),
            Rect::NOTHING,
        ] {
            assert!(painted_shapes(rect, 0.0).is_empty(), "{rect:?}");
        }
    }

    #[test]
    fn too_small_rect_keeps_backdrop_without_inverted_arc() {
        let tiny = Rect::from_center_size(pos2(50.0, 50.0), vec2(8.0, 8.0));
        let shapes = painted_shapes(tiny, 0.0);
        assert!(
            matches!(shapes.as_slice(), [Shape::Circle(_)]),
            "{shapes:?}"
        );
    }
}
