//! Раскладка TTML: `<layout><region>` с `tts:origin` / `tts:extent`.
//!
//! Поддержаны проценты и пиксели; пиксели переводятся в проценты через размер холста
//! документа (`tts:extent` на `<tt>`, например `1920px 1080px`). Без него пиксельные
//! значения не поддержаны (считаются в отчёте), регион остаётся на весь экран.

use std::collections::HashMap;

use super::model::{ElementKind, RawElement, RawTree};
use super::style::{StyleBook, StyleSet};

/// Размер длины в одной оси.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Length {
    Percent(f32),
    Pixels(f32),
}

/// Размер холста документа в пикселях (из `tts:extent` корня), если задан.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Canvas {
    width: f32,
    height: f32,
}

/// Прямоугольник региона в процентах кадра: `left + width <= 100`, `top + height <= 100`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct RegionRect {
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

impl RegionRect {
    /// Регион по умолчанию: весь кадр.
    const FULL: Self = Self {
        left: 0.0,
        top: 0.0,
        width: 100.0,
        height: 100.0,
    };
}

/// Регион: прямоугольник и стиль, который он задаёт попавшему в него тексту.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Region {
    pub(super) rect: RegionRect,
    pub(super) style: StyleSet,
}

/// Все регионы документа по `xml:id`.
#[derive(Debug, Default)]
pub(super) struct RegionBook {
    regions: HashMap<String, Region>,
    /// Сколько атрибутов регионов не поддержано или некорректно.
    ignored_count: usize,
}

impl RegionBook {
    pub(super) fn from_tree(tree: &RawTree, styles: &StyleBook) -> Self {
        let canvas = tree
            .element(tree.root)
            .and_then(|root| root.style_attribute("extent"))
            .and_then(parse_canvas);
        let mut book = Self::default();
        for element in &tree.elements {
            if element.kind != ElementKind::Region {
                continue;
            }
            // Регион без `xml:id` не может быть выбран: ссылаться нечем.
            let Some(id) = element.id.as_deref() else {
                continue;
            };
            let region = book.read_region(element, canvas, styles);
            // При повторе id действует первое определение.
            book.regions.entry(id.to_owned()).or_insert(region);
        }
        book
    }

    /// Регион по идентификатору.
    pub(super) fn get(&self, id: &str) -> Option<&Region> {
        self.regions.get(id)
    }

    pub(super) fn ignored_count(&self) -> usize {
        self.ignored_count
    }

    fn read_region(
        &mut self,
        element: &RawElement,
        canvas: Option<Canvas>,
        styles: &StyleBook,
    ) -> Region {
        let defaults = RegionRect::FULL;
        let origin = self.read_pair(element.style_attribute("origin"), canvas);
        let extent = self.read_pair(element.style_attribute("extent"), canvas);
        let (left, top) = origin.unwrap_or((defaults.left, defaults.top));
        let (width, height) = extent.unwrap_or((defaults.width, defaults.height));
        // Регион не выходит за кадр: лишнее обрезается (а не прячет текст за краем).
        let left = left.clamp(0.0, 100.0);
        let top = top.clamp(0.0, 100.0);
        let rect = RegionRect {
            left,
            top,
            width: width.clamp(0.0, 100.0 - left),
            height: height.clamp(0.0, 100.0 - top),
        };
        let (specified, ignored) = styles.specified_for(element);
        self.ignored_count += ignored;
        Region {
            rect,
            // Фон региона закрашивает его площадь; у нас такой плашки нет.
            style: specified.without_background(),
        }
    }

    /// Пара `x y` в процентах. `None` — атрибута нет или он не поддержан (тогда счётчик растёт
    /// только для присутствующего атрибута).
    fn read_pair(&mut self, text: Option<&str>, canvas: Option<Canvas>) -> Option<(f32, f32)> {
        let text = text?;
        let pair = parse_length_pair(text).and_then(|(horizontal, vertical)| {
            Some((
                to_percent(horizontal, canvas.map(|c| c.width))?,
                to_percent(vertical, canvas.map(|c| c.height))?,
            ))
        });
        if pair.is_none() {
            self.ignored_count += 1;
        }
        pair
    }
}

/// `1920px 1080px` -> размер холста.
fn parse_canvas(text: &str) -> Option<Canvas> {
    match parse_length_pair(text)? {
        (Length::Pixels(width), Length::Pixels(height)) if width > 0.0 && height > 0.0 => {
            Some(Canvas { width, height })
        }
        _ => None,
    }
}

fn parse_length_pair(text: &str) -> Option<(Length, Length)> {
    let mut parts = text.split_whitespace();
    let horizontal = parse_length(parts.next()?)?;
    let vertical = parse_length(parts.next()?)?;
    parts.next().is_none().then_some((horizontal, vertical))
}

fn parse_length(text: &str) -> Option<Length> {
    if let Some(number) = text.strip_suffix('%') {
        return parse_finite(number).map(Length::Percent);
    }
    text.strip_suffix("px")
        .and_then(parse_finite)
        .map(Length::Pixels)
}

/// Неотрицательное конечное число (`inf`/`NaN` из `f32::from_str` отвергаются).
fn parse_finite(text: &str) -> Option<f32> {
    text.parse::<f32>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn to_percent(length: Length, canvas_side: Option<f32>) -> Option<f32> {
    match length {
        Length::Percent(percent) => Some(percent),
        Length::Pixels(pixels) => canvas_side.map(|side| pixels / side * 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_and_pixel_pairs_are_parsed() {
        assert_eq!(
            parse_length_pair("10% 20.5%"),
            Some((Length::Percent(10.0), Length::Percent(20.5)))
        );
        assert_eq!(
            parse_length_pair("100px 50px"),
            Some((Length::Pixels(100.0), Length::Pixels(50.0)))
        );
        assert_eq!(parse_length_pair("10%"), None);
        assert_eq!(parse_length_pair("auto"), None);
        assert_eq!(parse_length_pair("1em 2em"), None);
        assert_eq!(parse_length_pair("-5% 1%"), None);
        assert_eq!(parse_length_pair("NaN% 1%"), None);
    }

    #[test]
    fn pixels_convert_through_canvas() {
        let canvas = parse_canvas("1920px 1080px").expect("canvas");
        assert_eq!(
            to_percent(Length::Pixels(960.0), Some(canvas.width)),
            Some(50.0)
        );
        assert_eq!(to_percent(Length::Pixels(960.0), None), None);
    }
}
