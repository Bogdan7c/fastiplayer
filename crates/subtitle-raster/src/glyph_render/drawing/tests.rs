use super::*;
use crate::event_style::AxisPair;
use crate::fixed_point::Vector26;
use crate::glyph_info::TextCluster;
use crate::source_text::DrawingSource;
use crate::test_support::{embedded_fonts, plain_style, plain_text, shaped_clusters};

/// Кластер-заготовка: настоящий кластер буквы, который тесты превращают в рисунок.
fn blank_cluster(scale: AxisPair) -> TextCluster {
    let mut fonts = embedded_fonts();
    let mut clusters = shaped_clusters(&mut fonts, &plain_text("x"), &plain_style());
    let mut cluster = clusters.remove(0);
    cluster.glyph_scale = scale;
    // Всё, что принесла буква, обнуляем: рисунок обязан заполнить поля сам.
    cluster.root.outline = None;
    cluster.root.advance = Vector26::default();
    cluster.root.ascent = 0;
    cluster.root.descent = 0;
    cluster.root.outline_offset_y = 0.0;
    cluster.root.bbox = GlyphBox::default();
    cluster.cluster_advance = Vector26::default();
    cluster
}

fn source(commands: &str, baseline_offset: i32) -> DrawingSource {
    DrawingSource {
        commands: commands.to_owned(),
        outline_scale: AxisPair::uniform(1.0),
        baseline_offset,
    }
}

/// Прямоугольник 200x100 пикселей рисунка = 12800x6400 единиц 26.6.
const RECTANGLE: &str = "m 0 0 l 200 0 200 100 0 100";

#[test]
fn rectangle_metrics_follow_get_outline_glyph() {
    let mut cluster = blank_cluster(AxisPair {
        horizontal: 0.5,
        vertical: 0.25,
    });
    assert_eq!(
        place_drawing(&mut cluster, &source(RECTANGLE, 0)),
        DrawingOutcome::Placed
    );
    let glyph = &cluster.root;
    // advance = lrint(12800 * 0.5); подъём = lrint(6400 * 0.25); спуска нет.
    assert_eq!(glyph.advance, Vector26 { x: 6400, y: 0 });
    assert_eq!(cluster.cluster_advance, glyph.advance);
    assert_eq!((glyph.ascent, glyph.descent), (1600, 0));
    // Рисунок стоит на базовой линии: верх на -подъёме, низ на нуле; слева сдвига нет.
    assert_eq!(glyph.outline_offset_y, -1600.0);
    assert_eq!(
        glyph.bbox,
        GlyphBox {
            x_min: 0,
            y_min: -1600,
            x_max: 6400,
            y_max: 0
        }
    );
    assert!(glyph.outline.is_some());
}

#[test]
fn baseline_offset_splits_the_height_between_ascent_and_descent() {
    let mut cluster = blank_cluster(AxisPair {
        horizontal: 0.5,
        vertical: 0.25,
    });
    place_drawing(&mut cluster, &source(RECTANGLE, 10));
    let glyph = &cluster.root;
    // desc = 64 * 10 = 640 единиц, asc = 6400 - 640 = 5760; умножаются на 0.25.
    assert_eq!((glyph.ascent, glyph.descent), (1440, 160));
    assert_eq!(glyph.outline_offset_y, -1440.0);
    // Низ контура уходит под базовую линию ровно на спуск.
    assert_eq!(glyph.bbox.y_max, 160);
    // Ширина и верх от \pbo не зависят по величине: верх выше на 160.
    assert_eq!(glyph.advance.x, 6400);
    assert_eq!(glyph.bbox.y_min, -1440);
}

#[test]
fn negative_baseline_offset_lifts_the_shape_above_the_baseline() {
    let mut cluster = blank_cluster(AxisPair::uniform(1.0));
    place_drawing(&mut cluster, &source(RECTANGLE, -10));
    let glyph = &cluster.root;
    // desc = -640, asc = 6400 + 640 = 7040: низ контура на 640 выше базовой линии.
    assert_eq!((glyph.ascent, glyph.descent), (7040, -640));
    assert_eq!(glyph.bbox.y_max, -640);
}

#[test]
fn drawing_without_an_outline_keeps_libass_metrics_of_an_empty_box() {
    let mut cluster = blank_cluster(AxisPair::uniform(1.0));
    assert_eq!(
        place_drawing(&mut cluster, &source("n 1 1", 0)),
        DrawingOutcome::NoOutline
    );
    // Перевёрнутая рамка даёт ширину и высоту 1 (заворачивание `int32`, как в C), контур пуст.
    assert_eq!(cluster.root.advance.x, 1);
    assert_eq!(cluster.root.ascent, 1);
    assert!(
        cluster
            .root
            .outline
            .as_ref()
            .is_some_and(|o| o.outline.is_empty())
    );
}

#[test]
fn rejected_drawing_leaves_the_glyph_untouched() {
    let mut cluster = blank_cluster(AxisPair::uniform(1.0));
    assert_eq!(
        place_drawing(&mut cluster, &source("m 0 0 l 99999999999 0", 0)),
        DrawingOutcome::NoOutline
    );
    assert!(cluster.root.outline.is_none());
    assert_eq!(cluster.root.advance, Vector26::default());
    assert_eq!(cluster.cluster_advance, Vector26::default());
    assert_eq!((cluster.root.ascent, cluster.root.descent), (0, 0));
}
