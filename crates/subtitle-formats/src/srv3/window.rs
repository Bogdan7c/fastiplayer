//! Окно srv3: стиль (`<ws>`) и позиция (`<wp>`) -> размещение реплики модели.

use subtitle_core::{
    CueLine, CueLineAlign, CuePlacement, CuePosition, CuePositionAlign, CueTextAlign,
    PercentOfVideo,
};

/// Сколько строк окна показывать, если `rc` не задан: «без ограничения» ограничено,
/// чтобы враждебный файл не раздул одну реплику.
pub(super) const UNLIMITED_ROWS_CAP: usize = 16;

/// Горизонтальное выравнивание текста окна (`ju`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Justification {
    Left,
    Right,
    Center,
}

/// Стиль окна `<ws>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct WindowStyle {
    /// `ju`: 0 — слева, 1 — справа, 2 — по центру, 3 (по ширине) — как по центру.
    pub(super) justification: Option<Justification>,
}

/// Результат разбора `<ws>`: стиль и признак неподдерживаемых атрибутов.
pub(super) struct ParsedWindowStyle {
    pub(super) style: WindowStyle,
    pub(super) has_ignored_attributes: bool,
}

impl WindowStyle {
    /// Собирает стиль из атрибутов `<ws>`; `mh`, `pd`, `sd` и т. п. не поддерживаются.
    pub(super) fn from_attributes<'attr>(
        attributes: impl IntoIterator<Item = (&'attr str, &'attr str)>,
    ) -> ParsedWindowStyle {
        let mut style = Self::default();
        let mut has_ignored_attributes = false;
        for (name, value) in attributes {
            match name {
                "id" => {}
                "ju" => match value {
                    "0" => style.justification = Some(Justification::Left),
                    "1" => style.justification = Some(Justification::Right),
                    "2" | "3" => style.justification = Some(Justification::Center),
                    _ => has_ignored_attributes = true,
                },
                _ => has_ignored_attributes = true,
            }
        }
        ParsedWindowStyle {
            style,
            has_ignored_attributes,
        }
    }
}

/// Позиция окна `<wp>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct WindowPosition {
    /// `ap` 0..=8: якорная точка окна по сетке 3x3 (строки сверху вниз, столбцы слева направо).
    anchor_point: Option<u8>,
    /// `ah`: горизонталь якоря, % ширины видео.
    horizontal_percent: Option<u8>,
    /// `av`: вертикаль якоря, % высоты видео.
    vertical_percent: Option<u8>,
    /// `rc`: сколько строк окна показывается одновременно.
    pub(super) rows: Option<usize>,
}

/// Результат разбора `<wp>`.
pub(super) struct ParsedWindowPosition {
    pub(super) position: WindowPosition,
    pub(super) has_ignored_attributes: bool,
}

/// Столбец якоря.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorColumn {
    Left,
    Center,
    Right,
}

/// Строка якоря.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorRow {
    Top,
    Middle,
    Bottom,
}

impl WindowPosition {
    /// Собирает позицию из атрибутов `<wp>`; `cc` (число колонок) не поддерживается.
    pub(super) fn from_attributes<'attr>(
        attributes: impl IntoIterator<Item = (&'attr str, &'attr str)>,
    ) -> ParsedWindowPosition {
        let mut position = Self::default();
        let mut has_ignored_attributes = false;
        for (name, value) in attributes {
            match name {
                "id" => {}
                "ap" => match value.parse::<u8>() {
                    Ok(point) if point <= 8 => position.anchor_point = Some(point),
                    _ => has_ignored_attributes = true,
                },
                "ah" => match value.parse::<u8>() {
                    Ok(percent) if percent <= 100 => position.horizontal_percent = Some(percent),
                    _ => has_ignored_attributes = true,
                },
                "av" => match value.parse::<u8>() {
                    Ok(percent) if percent <= 100 => position.vertical_percent = Some(percent),
                    _ => has_ignored_attributes = true,
                },
                "rc" => match value.parse::<usize>() {
                    Ok(rows) if rows > 0 => position.rows = Some(rows),
                    _ => has_ignored_attributes = true,
                },
                _ => has_ignored_attributes = true,
            }
        }
        ParsedWindowPosition {
            position,
            has_ignored_attributes,
        }
    }

    /// Якорь (столбец, строка) по `ap`; без `ap` — нижний центр (умолчание YouTube).
    fn anchor(&self) -> (AnchorColumn, AnchorRow) {
        let point = self.anchor_point.unwrap_or(DEFAULT_ANCHOR_POINT);
        let column = match point % 3 {
            0 => AnchorColumn::Left,
            1 => AnchorColumn::Center,
            _ => AnchorColumn::Right,
        };
        let row = match point / 3 {
            0 => AnchorRow::Top,
            1 => AnchorRow::Middle,
            _ => AnchorRow::Bottom,
        };
        (column, row)
    }

    /// Позиция совпадает со стандартной подписью YouTube: низ по центру (`ap=7 ah=50 av=100`).
    /// Такую позицию отдаём стилю пользователя (его нижний отступ), а не фиксируем в процентах.
    fn is_standard_bottom_center(&self) -> bool {
        self.anchor_point
            .is_none_or(|point| point == DEFAULT_ANCHOR_POINT)
            && self.horizontal_percent.is_none_or(|percent| percent == 50)
            && self.vertical_percent.is_none_or(|percent| percent == 100)
    }
}

/// Нижний центр по сетке 3x3.
const DEFAULT_ANCHOR_POINT: u8 = 7;

/// Размещение реплики по позиции и стилю окна. `None` у обоих — умолчание YouTube.
pub(super) fn placement_for(
    position: Option<&WindowPosition>,
    style: Option<&WindowStyle>,
) -> CuePlacement {
    let default_position = WindowPosition::default();
    let position = position.unwrap_or(&default_position);
    let (column, row) = position.anchor();
    let mut placement = CuePlacement {
        text_align: text_align_for(style.and_then(|style| style.justification), column),
        ..CuePlacement::default()
    };
    if position.is_standard_bottom_center() {
        return placement;
    }
    placement.line =
        percent(position.vertical_percent.unwrap_or(100)).map_or(CueLine::Auto, CueLine::Percent);
    placement.line_align = match row {
        AnchorRow::Top => CueLineAlign::Start,
        AnchorRow::Middle => CueLineAlign::Center,
        AnchorRow::Bottom => CueLineAlign::End,
    };
    placement.position = percent(position.horizontal_percent.unwrap_or(50))
        .map_or(CuePosition::Auto, CuePosition::Percent);
    placement.position_align = match column {
        AnchorColumn::Left => CuePositionAlign::LineLeft,
        AnchorColumn::Center => CuePositionAlign::Center,
        AnchorColumn::Right => CuePositionAlign::LineRight,
    };
    placement
}

/// Выравнивание текста: явный `ju`, иначе по столбцу якоря.
fn text_align_for(justification: Option<Justification>, column: AnchorColumn) -> CueTextAlign {
    match justification {
        Some(Justification::Left) => CueTextAlign::Left,
        Some(Justification::Right) => CueTextAlign::Right,
        Some(Justification::Center) => CueTextAlign::Center,
        None => match column {
            AnchorColumn::Left => CueTextAlign::Left,
            AnchorColumn::Center => CueTextAlign::Center,
            AnchorColumn::Right => CueTextAlign::Right,
        },
    }
}

fn percent(value: u8) -> Option<PercentOfVideo> {
    PercentOfVideo::new(f32::from(value)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(attributes: &[(&str, &str)]) -> WindowPosition {
        WindowPosition::from_attributes(attributes.iter().copied()).position
    }

    #[test]
    fn standard_bottom_center_keeps_user_bottom_margin() {
        let placement = placement_for(
            Some(&position(&[("ap", "7"), ("ah", "50"), ("av", "100")])),
            None,
        );
        assert_eq!(placement.line, CueLine::Auto);
        assert_eq!(placement.position, CuePosition::Auto);
        assert_eq!(placement.text_align, CueTextAlign::Center);
        assert_eq!(placement, placement_for(None, None));
    }

    #[test]
    fn top_right_anchor_maps_to_percent_line_and_position() {
        let placement = placement_for(
            Some(&position(&[("ap", "2"), ("ah", "90"), ("av", "5")])),
            None,
        );
        assert_eq!(
            placement.line.clone(),
            CueLine::Percent(PercentOfVideo::new(5.0).unwrap_or(PercentOfVideo::ZERO))
        );
        assert_eq!(placement.line_align, CueLineAlign::Start);
        assert_eq!(placement.position_align, CuePositionAlign::LineRight);
        assert_eq!(placement.text_align, CueTextAlign::Right);
    }

    #[test]
    fn justification_overrides_anchor_column() {
        let style = WindowStyle::from_attributes([("ju", "2")]).style;
        let placement = placement_for(Some(&position(&[("ap", "6"), ("ah", "20")])), Some(&style));
        assert_eq!(placement.text_align, CueTextAlign::Center);
        assert_eq!(placement.position_align, CuePositionAlign::LineLeft);
    }

    #[test]
    fn out_of_range_values_are_flagged_and_not_applied() {
        let parsed = WindowPosition::from_attributes([("ap", "9"), ("av", "300"), ("rc", "0")]);
        assert!(parsed.has_ignored_attributes);
        assert_eq!(parsed.position, WindowPosition::default());
    }
}
