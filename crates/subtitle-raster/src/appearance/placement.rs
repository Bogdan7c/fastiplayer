//! Размещение блока реплики: `CuePlacement` + стиль -> якорь и поля в пикселях.
//!
//! Текстовый конвейер (как libass без `\pos`) умеет одно: блок текста занимает полосу между
//! левым и правым полем, а по вертикали прижат к верху, низу или центру холста с полем.
//! Все варианты `CuePlacement` приводятся к этому виду.
//!
//! ## Что поддержано
//! - `anchor` (`\an1`..`\an9`) — выравнивание по строке/столбцу numpad; ширина блока
//!   `max_line_width`, поля из стиля.
//! - WebVTT `size` — ширина блока (доля `max_line_width`), `position` и `position_align` —
//!   горизонтальное положение блока, `text_align` — выравнивание текста в блоке
//!   (`start`/`end` считаются как `left`/`right`: письмо слева направо).
//! - WebVTT `line`: процент — вертикальная координата, число — номер строки (от верха для
//!   положительных, от низа для отрицательных; высота строки = ячейка шрифта).
//!
//! ## Что НЕ поддержано (осознанно, детали в `docs`/отчёте сессии 02)
//! - `line_align: Center` приравнен к `Start` (верх блока на заданной линии);
//! - `region_id` (регионы WebVTT и прокрутка) — игнорируется;
//! - вертикальное письмо — реплика рисуется горизонтально.

use subtitle_core::{
    CueLine, CueLineAlign, CuePlacement, CuePosition, CuePositionAlign, CueTextAlign,
    CueWritingDirection, ScreenAnchor,
};

use super::SubtitleAppearance;
use crate::event_style::TextMargins;
use crate::overlay::CanvasSize;

/// Горизонтальная привязка текста внутри блока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Left,
    Center,
    Right,
}

/// Вертикальная привязка блока к холсту.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Top,
    Middle,
    Bottom,
}

/// Отступы плашки вокруг текста в пикселях: поля блока увеличиваются на них, чтобы
/// ВНЕШНИЙ край плашки, а не текста, стоял на заданном расстоянии от краёв холста.
#[derive(Debug, Clone, Copy)]
pub(super) struct PlateInset {
    pub(super) horizontal: f64,
    pub(super) vertical: f64,
}

/// Итог размещения: якорь и поля для `EventStyle`.
#[derive(Debug, Clone, Copy)]
pub(super) struct BlockPlacement {
    pub(super) alignment: ScreenAnchor,
    pub(super) margins: TextMargins,
}

/// Вычисляет якорь и поля блока (см. модульную документацию про поддержанное).
pub(super) fn place_block(
    appearance: &SubtitleAppearance,
    placement: &CuePlacement,
    canvas: CanvasSize,
    cell_height: f64,
    inset: PlateInset,
) -> BlockPlacement {
    if placement.writing_direction != CueWritingDirection::Horizontal {
        tracing::debug!("vertical writing is not supported, cue is drawn horizontally");
    }
    if placement.region_id.is_some() {
        tracing::debug!("WebVTT regions are not supported, region id is ignored");
    }

    let width = f64::from(canvas.width());
    let height = f64::from(canvas.height());
    let (column, row) = match placement.anchor {
        Some(anchor) => split_anchor(anchor),
        None => (text_column(placement.text_align), Row::Bottom),
    };

    // Горизонталь: блок [left, left + block_width] внутри холста.
    let side_margin = f64::from(appearance.side_margin.get()) * width;
    let size_fraction = if placement.anchor.is_some() {
        1.0
    } else {
        f64::from(placement.size.get()) / 100.0
    };
    let block_width =
        (width * f64::from(appearance.max_line_width.get()) * size_fraction).clamp(1.0, width);
    let block_left = block_left(placement, column, width, block_width, side_margin);
    let margin_left = block_left + inset.horizontal;
    let margin_right = width - (block_left + block_width) + inset.horizontal;

    // Вертикаль: якорь и поле.
    let edge_gap = f64::from(appearance.bottom_margin.get()) * height + inset.vertical;
    let (row, margin_vertical) = match placement.anchor {
        Some(_) => (row, if row == Row::Middle { 0.0 } else { edge_gap }),
        None => webvtt_line(placement, height, cell_height, edge_gap, inset.vertical),
    };

    BlockPlacement {
        alignment: join_anchor(column, row),
        margins: TextMargins {
            left: round_margin(margin_left, width),
            right: round_margin(margin_right, width),
            vertical: round_margin(margin_vertical, height),
        },
    }
}

/// Левый край блока в пикселях холста.
fn block_left(
    placement: &CuePlacement,
    column: Column,
    width: f64,
    block_width: f64,
    side_margin: f64,
) -> f64 {
    let left = match (placement.anchor, placement.position) {
        // Явная позиция WebVTT: процент ширины + выравнивание относительно неё.
        (None, CuePosition::Percent(percent)) => {
            let anchor_x = width * f64::from(percent.get()) / 100.0;
            match resolve_position_align(placement.position_align, column) {
                CuePositionAlign::LineRight => anchor_x - block_width,
                CuePositionAlign::Center => anchor_x - block_width / 2.0,
                CuePositionAlign::LineLeft | CuePositionAlign::Auto => anchor_x,
            }
        }
        // Позиции нет: блок прижат по столбцу якоря.
        _ => match column {
            Column::Left => side_margin,
            Column::Center => (width - block_width) / 2.0,
            Column::Right => width - side_margin - block_width,
        },
    };
    left.clamp(0.0, (width - block_width).max(0.0))
}

/// `position_align: auto` в WebVTT определяется выравниванием текста.
fn resolve_position_align(align: CuePositionAlign, column: Column) -> CuePositionAlign {
    match align {
        CuePositionAlign::Auto => match column {
            Column::Left => CuePositionAlign::LineLeft,
            Column::Center => CuePositionAlign::Center,
            Column::Right => CuePositionAlign::LineRight,
        },
        explicit => explicit,
    }
}

/// Якорь и поле по `line` WebVTT.
fn webvtt_line(
    placement: &CuePlacement,
    height: f64,
    cell_height: f64,
    edge_gap: f64,
    vertical_inset: f64,
) -> (Row, f64) {
    match placement.line {
        CueLine::Auto => (Row::Bottom, edge_gap),
        CueLine::Percent(percent) => {
            let line_y = height * f64::from(percent.get()) / 100.0;
            match placement.line_align {
                // `End`: низ блока на линии. `Center` приравнен к `Start` (не поддержан).
                CueLineAlign::End => (Row::Bottom, height - line_y + vertical_inset),
                CueLineAlign::Start | CueLineAlign::Center => (Row::Top, line_y + vertical_inset),
            }
        }
        CueLine::LineNumber(number) if number >= 0 => {
            (Row::Top, f64::from(number) * cell_height + edge_gap)
        }
        CueLine::LineNumber(number) => {
            // -1 — нижняя строка (как Auto), -2 — на строку выше и так далее.
            let lines_above = f64::from(number.unsigned_abs().saturating_sub(1));
            (Row::Bottom, edge_gap + lines_above * cell_height)
        }
    }
}

/// Выравнивание текста WebVTT -> столбец (`start`/`end` для письма слева направо).
fn text_column(align: CueTextAlign) -> Column {
    match align {
        CueTextAlign::Start | CueTextAlign::Left => Column::Left,
        CueTextAlign::Center => Column::Center,
        CueTextAlign::End | CueTextAlign::Right => Column::Right,
    }
}

/// Раскладывает якорь numpad на столбец и строку.
fn split_anchor(anchor: ScreenAnchor) -> (Column, Row) {
    match anchor {
        ScreenAnchor::BottomLeft => (Column::Left, Row::Bottom),
        ScreenAnchor::BottomCenter => (Column::Center, Row::Bottom),
        ScreenAnchor::BottomRight => (Column::Right, Row::Bottom),
        ScreenAnchor::MiddleLeft => (Column::Left, Row::Middle),
        ScreenAnchor::MiddleCenter => (Column::Center, Row::Middle),
        ScreenAnchor::MiddleRight => (Column::Right, Row::Middle),
        ScreenAnchor::TopLeft => (Column::Left, Row::Top),
        ScreenAnchor::TopCenter => (Column::Center, Row::Top),
        ScreenAnchor::TopRight => (Column::Right, Row::Top),
    }
}

/// Собирает якорь из столбца и строки.
fn join_anchor(column: Column, row: Row) -> ScreenAnchor {
    match (row, column) {
        (Row::Bottom, Column::Left) => ScreenAnchor::BottomLeft,
        (Row::Bottom, Column::Center) => ScreenAnchor::BottomCenter,
        (Row::Bottom, Column::Right) => ScreenAnchor::BottomRight,
        (Row::Middle, Column::Left) => ScreenAnchor::MiddleLeft,
        (Row::Middle, Column::Center) => ScreenAnchor::MiddleCenter,
        (Row::Middle, Column::Right) => ScreenAnchor::MiddleRight,
        (Row::Top, Column::Left) => ScreenAnchor::TopLeft,
        (Row::Top, Column::Center) => ScreenAnchor::TopCenter,
        (Row::Top, Column::Right) => ScreenAnchor::TopRight,
    }
}

/// Поле в целых пикселях: не меньше нуля и не больше стороны холста.
fn round_margin(value: f64, side: f64) -> i32 {
    // `as i32` насыщает; значение уже ограничено стороной холста.
    value.clamp(0.0, side).round() as i32
}
