//! Разбор кейса `.ass`: ровно та часть формата, которую используют кейсы-рисунки.
//!
//! Кейс — один `Dialogue` с тегами `\an7`, `\pos`, `\bord`, `\shad`, `\1a` и рисунком
//! (`m`, `l`, `b`) в режиме `\p1`. Разбор нужен, чтобы тест строил контур из того же файла,
//! который рендерил libass, а не из копии данных.

use std::path::Path;

use crate::outline::{Outline, Point, SegmentKind};

/// Цвет ASS (прямой RGBA после преобразования из `&HAABBGGRR`, альфа — прозрачность libass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AssColor {
    pub(super) red: u8,
    pub(super) green: u8,
    pub(super) blue: u8,
    /// Прозрачность как в ASS: 0 — непрозрачный, 255 — невидимый.
    pub(super) transparency: u8,
}

/// Описание кейса, достаточное для воспроизведения рендера.
#[derive(Debug, Clone)]
pub(super) struct DrawingCase {
    pub(super) width: i32,
    pub(super) height: i32,
    pub(super) position: (f64, f64),
    pub(super) border: f64,
    pub(super) shadow: f64,
    /// Цвета: primary, secondary, outline, back.
    pub(super) colors: [AssColor; 4],
    pub(super) drawing: Outline,
}

/// Читает кейс и его размер кадра из заголовка `; reference: width=.. height=..`.
pub(super) fn load_case(path: &Path) -> DrawingCase {
    let source = std::fs::read_to_string(path).expect("case file must be readable");
    let header = source
        .lines()
        .find(|line| line.starts_with("; reference:"))
        .expect("case must have a reference header");
    let number = |key: &str| -> i32 {
        header
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))
            .and_then(|value| value.parse().ok())
            .expect("header value must be a number")
    };

    let style_line = source
        .lines()
        .find(|line| line.starts_with("Style:"))
        .expect("case must have a style");
    let style_fields: Vec<&str> = style_line["Style:".len()..]
        .split(',')
        .map(str::trim)
        .collect();
    // Поля: Name, Fontname, Fontsize, Primary, Secondary, Outline, Back, ...
    let color = |index: usize| parse_ass_color(style_fields.get(index).expect("style color field"));
    let mut colors = [color(3), color(4), color(5), color(6)];

    let dialogue = source
        .lines()
        .find(|line| line.starts_with("Dialogue:"))
        .expect("case must have a dialogue");
    let text = dialogue
        .splitn(10, ',')
        .nth(9)
        .expect("dialogue must have text");
    let (tags, drawing_text) = text
        .strip_prefix('{')
        .and_then(|rest| rest.split_once('}'))
        .expect("dialogue text must start with a tag block");

    let mut case = DrawingCase {
        width: number("width="),
        height: number("height="),
        position: (0.0, 0.0),
        border: 0.0,
        shadow: 0.0,
        colors,
        drawing: parse_drawing(drawing_text),
    };
    for tag in tags.split('\\').filter(|tag| !tag.is_empty()) {
        if let Some(arguments) = tag
            .strip_prefix("pos(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let (x, y) = arguments.split_once(',').expect("pos has two arguments");
            case.position = (x.parse().expect("pos x"), y.parse().expect("pos y"));
        } else if let Some(value) = tag.strip_prefix("bord") {
            case.border = value.parse().expect("bord value");
        } else if let Some(value) = tag.strip_prefix("shad") {
            case.shadow = value.parse().expect("shad value");
        } else if let Some(value) = tag.strip_prefix("1a&H") {
            let alpha = u8::from_str_radix(value.trim_end_matches('&'), 16).expect("1a value");
            colors[0].transparency = alpha;
            case.colors = colors;
        } else {
            assert!(
                matches!(tag, "an7" | "p1"),
                "unsupported tag in case: {tag}"
            );
        }
    }
    case
}

/// `&HAABBGGRR` -> цвет.
pub(super) fn parse_ass_color(text: &str) -> AssColor {
    let digits = text.trim_start_matches("&H").trim_end_matches('&');
    let value = u32::from_str_radix(digits, 16).expect("color must be hex");
    AssColor {
        transparency: (value >> 24) as u8,
        blue: (value >> 16) as u8,
        green: (value >> 8) as u8,
        red: value as u8,
    }
}

/// Рисунок `\p1` -> контур в 26.6 (libass: `ass_drawing_parse`, команды `m`, `l`, `b`).
fn parse_drawing(text: &str) -> Outline {
    let mut outline = Outline::new();
    let mut tokens = text.split_whitespace().peekable();
    let mut pen = Point::default();
    let mut started = false;
    let to_d6 = |value: &str| -> i32 {
        let number: f64 = value.parse().expect("drawing coordinate");
        (number * 64.0).round_ties_even() as i32
    };
    let mut command = "";
    while let Some(token) = tokens.next() {
        if matches!(token, "m" | "l" | "b") {
            command = token;
            if command == "m" {
                // Новый `m` закрывает предыдущий контур.
                if started {
                    outline.push_segment(SegmentKind::Line);
                    assert!(outline.close_contour());
                    started = false;
                }
            }
            continue;
        }
        let x = to_d6(token);
        let y = to_d6(tokens.next().expect("y coordinate"));
        match command {
            "m" => pen = Point::new(x, y),
            "l" => {
                if !started {
                    outline.push_point(pen).expect("point in range");
                }
                outline
                    .push_point(Point::new(x, y))
                    .expect("point in range");
                outline.push_segment(SegmentKind::Line);
                started = true;
            }
            "b" => {
                // Кубическая кривая: три точки, начало — текущая позиция пера.
                let second = Point::new(
                    to_d6(tokens.next().expect("x2")),
                    to_d6(tokens.next().expect("y2")),
                );
                let third = Point::new(
                    to_d6(tokens.next().expect("x3")),
                    to_d6(tokens.next().expect("y3")),
                );
                if !started {
                    outline.push_point(pen).expect("point in range");
                }
                for point in [Point::new(x, y), second, third] {
                    outline.push_point(point).expect("point in range");
                }
                outline.push_segment(SegmentKind::Cubic);
                started = true;
            }
            other => panic!("unsupported drawing command: {other}"),
        }
    }
    if started {
        outline.push_segment(SegmentKind::Line);
        assert!(outline.close_contour());
    }
    outline
}
