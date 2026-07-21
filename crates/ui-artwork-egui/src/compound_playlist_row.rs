//! Нейтральная геометрия disclosure и соединяющего accent-а составной строки.

use egui::{Color32, Painter, Pos2, Rect, Stroke, pos2};

/// Положение child определяет, продолжается ли вертикальный connector ниже строки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundPlaylistPartPosition {
    /// Единственная часть завершает connector на своей оси.
    Only,
    /// Первая часть сохраняет connector к следующим children.
    First,
    /// Средняя часть соединяет соседние children.
    Middle,
    /// Последняя часть завершает connector на своей оси.
    Last,
}

/// Тип строки задаёт только paint geometry, но не domain interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundPlaylistRowKind {
    /// Header рисует disclosure и начало общей вертикальной линии.
    Header {
        /// Disclosure orientation отражает authoritative snapshot state.
        expanded: bool,
        /// Активная группа усиливает rail, не копируя exact playback marker.
        active: bool,
    },
    /// Part рисует connector и короткое ответвление к content.
    Part {
        /// Положение части ограничивает вертикальную линию границами группы.
        position: CompoundPlaylistPartPosition,
        /// Exact active part использует согласованный усиленный rail.
        active: bool,
    },
}

/// Skin-owned токены не зависят от playlist domain types.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompoundPlaylistRowStyle {
    /// Горизонтальное положение rail относительно левого края строки.
    pub rail_x_offset: f32,
    /// Отступ начала header rail от верхней границы.
    pub header_top_inset: f32,
    /// Длина горизонтального connector-а child.
    pub child_connector_length: f32,
    /// Центр disclosure по X относительно левого края.
    pub disclosure_center_x: f32,
    /// Полуразмер disclosure-chevron.
    pub disclosure_half_extent: f32,
    /// Толщина rail и child connector.
    pub rail_stroke_width: f32,
    /// Толщина disclosure-chevron.
    pub disclosure_stroke_width: f32,
    /// Постоянный compound accent обычной группы.
    pub rail_color: Color32,
    /// Усиленный accent группы с active part.
    pub active_rail_color: Color32,
    /// Цвет disclosure-chevron.
    pub disclosure_color: Color32,
}

/// Вычисленная geometry отделяет validation от Painter side effects.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CompoundPlaylistGeometry {
    vertical_start: Pos2,
    vertical_end: Pos2,
    child_connector_end: Option<Pos2>,
    disclosure_points: Option<[Pos2; 3]>,
}

/// Рисует accent поверх background, но под текстом и interaction outline.
pub(crate) fn paint(
    painter: &Painter,
    row_rect: Rect,
    kind: CompoundPlaylistRowKind,
    style: CompoundPlaylistRowStyle,
) {
    // Невалидная geometry не должна попадать в paint list.
    let Some(geometry) = geometry(row_rect, kind, style) else {
        return;
    };
    // Active меняет только тон rail, сохраняя форму group identity.
    let active = match kind {
        CompoundPlaylistRowKind::Header { active, .. }
        | CompoundPlaylistRowKind::Part { active, .. } => active,
    };
    // Stroke width ограничивается неотрицательным finite значением в geometry().
    let rail_stroke = Stroke::new(
        style.rail_stroke_width,
        if active {
            style.active_rail_color
        } else {
            style.rail_color
        },
    );
    // Вертикальная линия визуально связывает header и раскрытые children.
    painter.line_segment(
        [geometry.vertical_start, geometry.vertical_end],
        rail_stroke,
    );
    // Только child получает горизонтальное ответвление к indented content.
    if let Some(connector_end) = geometry.child_connector_end {
        painter.line_segment(
            [
                pos2(geometry.vertical_start.x, row_rect.center().y),
                connector_end,
            ],
            rail_stroke,
        );
    }
    // Chevron принадлежит artwork, а hit-area и actions остаются в app-egui.
    if let Some([start, corner, end]) = geometry.disclosure_points {
        let disclosure_stroke = Stroke::new(style.disclosure_stroke_width, style.disclosure_color);
        painter.line_segment([start, corner], disclosure_stroke);
        painter.line_segment([corner, end], disclosure_stroke);
    }
}

/// Возвращает bounded geometry либо отклоняет NaN/вырожденный rect.
fn geometry(
    row_rect: Rect,
    kind: CompoundPlaylistRowKind,
    style: CompoundPlaylistRowStyle,
) -> Option<CompoundPlaylistGeometry> {
    // Все размеры должны быть finite и неотрицательны до арифметики координат.
    let dimensions = [
        style.rail_x_offset,
        style.header_top_inset,
        style.child_connector_length,
        style.disclosure_center_x,
        style.disclosure_half_extent,
        style.rail_stroke_width,
        style.disclosure_stroke_width,
    ];
    if !row_rect.is_positive()
        || dimensions
            .iter()
            .any(|dimension| !dimension.is_finite() || *dimension < 0.0)
    {
        return None;
    }
    // Rail и disclosure clamp-ятся внутрь даже очень узкой строки.
    let rail_x = (row_rect.left() + style.rail_x_offset).clamp(row_rect.left(), row_rect.right());
    let center_y = row_rect.center().y;
    match kind {
        CompoundPlaylistRowKind::Header { expanded, .. } => {
            // Header начинает rail с inset и доводит его до нижней границы.
            let vertical_start = pos2(
                rail_x,
                (row_rect.top() + style.header_top_inset).min(row_rect.bottom()),
            );
            let vertical_end = pos2(rail_x, row_rect.bottom());
            let disclosure_center_x = (row_rect.left() + style.disclosure_center_x)
                .clamp(row_rect.left(), row_rect.right());
            let half_extent = style
                .disclosure_half_extent
                .min(row_rect.height() * 0.25)
                .min(row_rect.width() * 0.25);
            let disclosure_points = if expanded {
                // Expanded `v` читается независимо от rail color.
                Some([
                    pos2(
                        disclosure_center_x - half_extent,
                        center_y - half_extent * 0.5,
                    ),
                    pos2(disclosure_center_x, center_y + half_extent * 0.5),
                    pos2(
                        disclosure_center_x + half_extent,
                        center_y - half_extent * 0.5,
                    ),
                ])
            } else {
                // Collapsed `>` использует ту же bounding box без layout shift.
                Some([
                    pos2(
                        disclosure_center_x - half_extent * 0.5,
                        center_y - half_extent,
                    ),
                    pos2(disclosure_center_x + half_extent * 0.5, center_y),
                    pos2(
                        disclosure_center_x - half_extent * 0.5,
                        center_y + half_extent,
                    ),
                ])
            };
            Some(CompoundPlaylistGeometry {
                vertical_start,
                vertical_end,
                child_connector_end: None,
                disclosure_points,
            })
        }
        CompoundPlaylistRowKind::Part { position, .. } => {
            // Last/Only завершают rail на центральной оси; остальные продолжают вниз.
            let vertical_end_y = match position {
                CompoundPlaylistPartPosition::Only | CompoundPlaylistPartPosition::Last => center_y,
                CompoundPlaylistPartPosition::First | CompoundPlaylistPartPosition::Middle => {
                    row_rect.bottom()
                }
            };
            let connector_end_x = (rail_x + style.child_connector_length).min(row_rect.right());
            Some(CompoundPlaylistGeometry {
                vertical_start: pos2(rail_x, row_rect.top()),
                vertical_end: pos2(rail_x, vertical_end_y),
                child_connector_end: Some(pos2(connector_end_x, center_y)),
                disclosure_points: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic test style отделяет geometry от конкретной app skin.
    fn style() -> CompoundPlaylistRowStyle {
        CompoundPlaylistRowStyle {
            rail_x_offset: 5.0,
            header_top_inset: 4.0,
            child_connector_length: 14.0,
            disclosure_center_x: 17.0,
            disclosure_half_extent: 4.0,
            rail_stroke_width: 2.0,
            disclosure_stroke_width: 1.5,
            rail_color: Color32::from_gray(120),
            active_rail_color: Color32::from_gray(240),
            disclosure_color: Color32::from_gray(220),
        }
    }

    #[test]
    fn collapsed_and_expanded_chevrons_share_bounds_but_not_orientation() {
        let row_rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(350.0, 34.0));
        let collapsed = geometry(
            row_rect,
            CompoundPlaylistRowKind::Header {
                expanded: false,
                active: false,
            },
            style(),
        )
        .expect("valid collapsed geometry");
        let expanded = geometry(
            row_rect,
            CompoundPlaylistRowKind::Header {
                expanded: true,
                active: false,
            },
            style(),
        )
        .expect("valid expanded geometry");
        assert_ne!(collapsed.disclosure_points, expanded.disclosure_points);
        assert_eq!(collapsed.vertical_start, expanded.vertical_start);
        assert_eq!(collapsed.vertical_end, expanded.vertical_end);
    }

    #[test]
    fn only_and_last_parts_end_at_center_while_middle_continues() {
        let row_rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(420.0, 34.0));
        let geometry_for = |position| {
            geometry(
                row_rect,
                CompoundPlaylistRowKind::Part {
                    position,
                    active: false,
                },
                style(),
            )
            .expect("valid part geometry")
        };
        assert_eq!(
            geometry_for(CompoundPlaylistPartPosition::Only)
                .vertical_end
                .y,
            row_rect.center().y
        );
        assert_eq!(
            geometry_for(CompoundPlaylistPartPosition::Last)
                .vertical_end
                .y,
            row_rect.center().y
        );
        assert_eq!(
            geometry_for(CompoundPlaylistPartPosition::Middle)
                .vertical_end
                .y,
            row_rect.bottom()
        );
    }

    #[test]
    fn invalid_geometry_produces_no_characterization_shape() {
        let invalid_rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(0.0, 34.0));
        assert!(
            geometry(
                invalid_rect,
                CompoundPlaylistRowKind::Header {
                    expanded: false,
                    active: false,
                },
                style(),
            )
            .is_none()
        );
    }
}
