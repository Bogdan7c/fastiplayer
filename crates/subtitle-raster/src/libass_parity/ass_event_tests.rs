//! Паритет ASS-событий с libass 0.17.5: файл `cases/ass_*.ass` -> `load_text_subtitle` ->
//! публичный `SubtitleRasterizer::render` -> композит против эталонов libass.
//!
//! В отличие от `text_tests` (внутренний `render_text_event`), здесь проверяется ВЕСЬ путь
//! ASS: разбор файла, стиль, PlayRes, позиционирование, слои, столкновения, обводка и тень.
//!
//! ## Правило допуска (решение владельца)
//! Число, порядок, типы и цвета картинок должны совпасть с libass; позиции картинок — в пределах
//! 1 px, размеры — 2 px; число пикселей композита с разницей канала больше 2 не должно
//! превышать 1 % площади, которую libass закрасил (альфа > 0). Измеренные значения каждого
//! кейса записаны в [`ASS_CASES`]: допуск кейса — измерение с небольшим запасом, а не общий 1 %.
//!
//! ## Кейсы в нескольких моментах
//! Заголовок `time_ms=0,1000,2500` задаёт несколько моментов одного файла. Каждый момент —
//! отдельная «опорная точка» ([`ReferencePoint`]) со своими эталонами `<case>@t<мс:05>.png/.json`
//! и своей строкой допуска; кейс с одним моментом называется просто `<case>`.

use subtitle_core::{SubtitleContent, SubtitleTimeline, TimelineInsertOutcome};
use subtitle_formats::{
    AssParseLimits, LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};

use super::ass_tolerances::{ASS_CASES, AssTolerance};
use super::ass_tolerances_animation::LAYER2_ANIMATION_CASES;
use super::ass_tolerances_layer2::LAYER2_STATIC_CASES;
use super::ass_tolerances_layer3::{LAYER3_BLUR_CASES, LAYER3_CLIP_CASES, LAYER3_DRAWING_CASES};
use super::tests::{fixtures_dir, load_expected_png};
use super::text_tests::dump_comparison;
use crate::appearance::SubtitleAppearance;
use crate::ass::{AssRenderOptions, AssStorageSize};
use crate::fonts::{FontLookupCacheLimits, SubtitleFontLibrary, TextLanguagePreference};
use crate::overlay::{CanvasSize, SubtitleBitmapKind, SubtitleOverlay};
use crate::renderer::{
    AssEventTiming, CueKey, CueSourceId, RasterCacheLimits, RasterInputLimits, RasterLimits,
    RenderRequest, RenderableCue, SubtitleRasterizer,
};

/// Входные данные кейса из файла.
struct AssCaseInput {
    width: u32,
    height: u32,
    /// Моменты, в которых снят эталон (из заголовка `time_ms=a,b,c`), в порядке записи.
    times_ms: Vec<i64>,
    storage: AssStorageSize,
    file_text: String,
}

/// Один эталон: кейс в одном из его моментов.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReferencePoint {
    /// Имя файла кейса без расширения.
    case: String,
    /// Имя эталона и строки допуска: `<case>` для одного момента, `<case>@t<мс:05>` для нескольких.
    stem: String,
    /// Момент показа, мс.
    time_ms: i64,
}

impl ReferencePoint {
    /// Опорные точки кейса: по одной на момент заголовка.
    fn of_case(case: &str, times_ms: &[i64]) -> Vec<Self> {
        times_ms
            .iter()
            .map(|time_ms| Self {
                case: case.to_owned(),
                stem: if times_ms.len() == 1 {
                    case.to_owned()
                } else {
                    format!("{case}@t{time_ms:05}")
                },
                time_ms: *time_ms,
            })
            .collect()
    }
}

fn load_case(name: &str) -> AssCaseInput {
    let path = fixtures_dir().join("cases").join(format!("{name}.ass"));
    let file_text = std::fs::read_to_string(path).expect("case must be readable");
    let header = file_text
        .lines()
        .find(|line| line.starts_with("; reference:"))
        .expect("reference header");
    let value_of = |key: &str| -> Option<&str> {
        header
            .split_whitespace()
            .find_map(|part| part.strip_prefix(key))
    };
    let number = |key: &str| -> Option<u32> {
        value_of(key).map(|value| value.parse().expect("header number"))
    };
    let width = number("width=").expect("width");
    let height = number("height=").expect("height");
    // Без явного размера хранения генератор задаёт его равным кадру (render_reference.c).
    let storage = AssStorageSize::from_pixels(
        number("storage_width=").unwrap_or(width),
        number("storage_height=").unwrap_or(height),
    );
    let times_ms: Vec<i64> = value_of("time_ms=")
        .expect("time_ms")
        .split(',')
        .map(|moment| moment.parse().expect("time_ms moment"))
        .collect();
    AssCaseInput {
        width,
        height,
        times_ms,
        storage,
        file_text,
    }
}

/// Растеризатор со встроенными шрифтами.
fn rasterizer() -> SubtitleRasterizer {
    let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: 64 * 1024 * 1024,
                shaper_faces: 16,
                blur_scratch_retained_bytes: 32 * 1024 * 1024,
            },
            input: RasterInputLimits {
                max_chars_per_cue: 10_000,
                max_cues_per_frame: 256,
                max_ass_events_per_frame: 256,
                max_canvas_side: 16_384,
            },
        },
    )
}

/// Рисует кейс публичным API в момент `time_ms` и возвращает overlay.
fn render_case(input: &AssCaseInput, time_ms: i64) -> SubtitleOverlay {
    let limits = SubtitleParseLimits::new(1 << 22, 100_000, 100).with_ass_limits(AssParseLimits {
        max_header_bytes: 1 << 20,
        max_styles: 64,
        max_override_tags_per_event: 256,
        max_tracked_unknown_tag_names: 16,
    });
    let loaded = load_text_subtitle(
        input.file_text.as_bytes(),
        TextSubtitleFormat::Ass,
        LegacyEncodingHint::Generic,
        &limits,
    )
    .expect("case must parse");
    let mut timeline = SubtitleTimeline::new();
    for event in loaded.document().events() {
        assert!(matches!(
            timeline.insert(event.clone()),
            TimelineInsertOutcome::Inserted(_)
        ));
    }
    let time = subtitle_core::SubtitleTime::from_millis(time_ms);
    let mut active = Vec::new();
    timeline.collect_active_at(time, &mut active);
    let mut cues = Vec::new();
    for id in active {
        let event = timeline.event(id).expect("active event");
        let SubtitleContent::Ass(cue) = event.content() else {
            panic!("ASS case must produce ASS events");
        };
        cues.push((
            id,
            event.layer(),
            cue.clone(),
            AssEventTiming::new(event.start(), event.end()),
        ));
    }
    let renderable: Vec<RenderableCue<'_>> = cues
        .iter()
        .map(|(id, layer, cue, timing)| {
            RenderableCue::ass(CueKey::new(CueSourceId::new(1), *id), *layer, cue, *timing)
        })
        .collect();
    let canvas = CanvasSize::new(input.width, input.height).expect("canvas");
    let mut rasterizer = rasterizer();
    let appearance = SubtitleAppearance::youtube_like();
    rasterizer
        .render(&RenderRequest {
            moment: time,
            cues: &renderable,
            canvas,
            appearance: &appearance,
            text_language: TextLanguagePreference::unknown(),
            ass: AssRenderOptions {
                storage_size: input.storage,
                ..AssRenderOptions::default()
            },
        })
        .clone()
}

/// Тип картинки в libass; у подложки `BorderStyle = 4` поле неинициализировано.
fn libass_image_type(kind: SubtitleBitmapKind) -> Option<i64> {
    match kind {
        SubtitleBitmapKind::Glyph => Some(0),
        SubtitleBitmapKind::Outline => Some(1),
        SubtitleBitmapKind::Shadow => Some(2),
        SubtitleBitmapKind::Background => None,
    }
}

/// Результат сравнения кейса.
#[derive(Debug, Default)]
struct AssReport {
    /// Нарушения геометрии (число, порядок, позиции, размеры, цвета, типы).
    geometry_mismatches: Vec<String>,
    /// Пикселей композита с разницей канала больше 2.
    differing_pixels: usize,
    /// Наибольшая разница канала среди таких пикселей.
    max_channel_difference: u8,
    /// Пикселей, которые закрасил libass (альфа эталона > 0).
    reference_area: usize,
    /// Наибольшее смещение позиции картинки, px.
    max_position_delta: i64,
    /// Наибольшее расхождение размера картинки, px.
    max_size_delta: i64,
}

fn compare(point: &ReferencePoint) -> AssReport {
    let name = point.stem.as_str();
    let input = load_case(&point.case);
    let overlay = render_case(&input, point.time_ms);
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixtures_dir().join("expected").join(format!("{name}.json")))
            .expect("expected json"),
    )
    .expect("json");
    assert_eq!(json["libass_version"], "0.17.5");
    assert_eq!(
        json["time_ms"].as_i64(),
        Some(point.time_ms),
        "{name}: the reference was rendered at another moment"
    );
    // libass оставляет в списке картинки нулевого размера (w = h = 0): так векторный клип
    // «удаляет» картинку, не разрывая список. У нашего выхода пустых картинок нет по
    // инварианту `SubtitleBitmap`, поэтому в сравнении такие записи пропускаются.
    let is_visible = |reference: &serde_json::Value| {
        reference["w"].as_i64().is_some_and(|width| width > 0)
            && reference["h"].as_i64().is_some_and(|height| height > 0)
    };
    let expected: Vec<&serde_json::Value> = json["images"]
        .as_array()
        .expect("images")
        .iter()
        .filter(|reference| is_visible(reference))
        .collect();
    let mut report = AssReport::default();
    if overlay.bitmaps().len() != expected.len() {
        report.geometry_mismatches.push(format!(
            "image count: ours {} vs libass {}",
            overlay.bitmaps().len(),
            expected.len()
        ));
    }
    for (index, (image, reference)) in overlay.bitmaps().iter().zip(expected).enumerate() {
        let get = |key: &str| reference[key].as_i64().expect("number");
        let deltas = [
            (i64::from(image.origin().x) - get("dst_x")).abs(),
            (i64::from(image.origin().y) - get("dst_y")).abs(),
        ];
        let size_deltas = [
            (i64::from(image.width()) - get("w")).abs(),
            (i64::from(image.height()) - get("h")).abs(),
        ];
        report.max_position_delta = report
            .max_position_delta
            .max(deltas.into_iter().max().unwrap_or(0));
        report.max_size_delta = report
            .max_size_delta
            .max(size_deltas.into_iter().max().unwrap_or(0));
        if deltas.into_iter().any(|delta| delta > 1) {
            report.geometry_mismatches.push(format!(
                "image {index}: position ({}, {}) vs libass ({}, {})",
                image.origin().x,
                image.origin().y,
                get("dst_x"),
                get("dst_y")
            ));
        }
        if size_deltas.into_iter().any(|delta| delta > 2) {
            report.geometry_mismatches.push(format!(
                "image {index}: size {}x{} vs libass {}x{}",
                image.width(),
                image.height(),
                get("w"),
                get("h")
            ));
        }
        if let Some(kind) = libass_image_type(image.kind())
            && kind != get("type")
        {
            report
                .geometry_mismatches
                .push(format!("image {index}: type {kind} vs {}", get("type")));
        }
        let color: Vec<i64> = reference["color"]
            .as_array()
            .expect("color")
            .iter()
            .map(|value| value.as_i64().expect("channel"))
            .collect();
        let c = image.color();
        let ours = [c.red, c.green, c.blue, c.alpha].map(i64::from);
        if ours.as_slice() != color.as_slice() {
            report
                .geometry_mismatches
                .push(format!("image {index}: color {ours:?} vs {color:?}"));
        }
    }

    let actual = overlay.composite_rgba_over_transparent();
    let (width, height, reference_pixels) = load_expected_png(name);
    assert_eq!((width, height), (input.width, input.height), "{name}: size");
    for (ours, theirs) in actual
        .as_chunks::<4>()
        .0
        .iter()
        .zip(reference_pixels.as_chunks::<4>().0)
    {
        if theirs[3] > 0 {
            report.reference_area += 1;
        }
        let delta = ours
            .iter()
            .zip(theirs)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        if delta > 2 {
            report.differing_pixels += 1;
            report.max_channel_difference = report.max_channel_difference.max(delta);
        }
    }
    dump_comparison(name, width, height, &actual, &reference_pixels);
    report
}

/// Строки допусков всех таблиц (слои 1-3).
fn all_tolerance_rows() -> impl Iterator<Item = &'static AssTolerance> {
    ASS_CASES
        .iter()
        .chain(LAYER2_STATIC_CASES)
        .chain(LAYER2_ANIMATION_CASES)
        .chain(LAYER3_DRAWING_CASES)
        .chain(LAYER3_BLUR_CASES)
        .chain(LAYER3_CLIP_CASES)
}

/// Все опорные точки кейсов `ass_*` (по одной на момент каждого кейса).
fn reference_points() -> Vec<ReferencePoint> {
    case_names()
        .iter()
        .flat_map(|case| ReferencePoint::of_case(case, &load_case(case).times_ms))
        .collect()
}

/// Все кейсы `ass_*` из каталога фикстур.
fn case_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixtures_dir().join("cases"))
        .expect("cases directory")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let stem = path.file_stem()?.to_str()?.to_owned();
            (path.extension()? == "ass" && stem.starts_with("ass_")).then_some(stem)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn every_ass_case_has_a_measured_tolerance() {
    let names: Vec<String> = reference_points()
        .into_iter()
        .map(|point| point.stem)
        .collect();
    assert!(
        names.len() >= 40,
        "expected the ASS reference set: {names:?}"
    );
    let known: Vec<&str> = all_tolerance_rows().map(|case| case.name).collect();
    for name in &names {
        assert!(
            known.contains(&name.as_str()),
            "{name} has no tolerance row"
        );
    }
    assert_eq!(known.len(), names.len(), "stale tolerance rows");
}

#[test]
fn multi_moment_headers_name_references_with_a_time_suffix() {
    let single = ReferencePoint::of_case("ass_x", &[1000]);
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].stem, "ass_x", "один момент — имя без суффикса");
    let several = ReferencePoint::of_case("ass_x", &[0, 1000, 25_000]);
    let stems: Vec<&str> = several.iter().map(|point| point.stem.as_str()).collect();
    assert_eq!(
        stems,
        ["ass_x@t00000", "ass_x@t01000", "ass_x@t25000"],
        "несколько моментов — суффикс @t<мс:05>"
    );
    assert_eq!(several[1].time_ms, 1000);
    assert!(several.iter().all(|point| point.case == "ass_x"));
}

#[test]
fn ass_events_match_libass_within_measured_tolerance() {
    let points = reference_points();
    for case in all_tolerance_rows() {
        let point = points
            .iter()
            .find(|point| point.stem == case.name)
            .expect("tolerance row without a reference point");
        let report = compare(point);
        let percent = if report.reference_area == 0 {
            0.0
        } else {
            100.0 * report.differing_pixels as f64 / report.reference_area as f64
        };
        eprintln!(
            "{}: differing {} / area {} ({:.2} %), max diff {}, position delta {}, size delta {}",
            case.name,
            report.differing_pixels,
            report.reference_area,
            percent,
            report.max_channel_difference,
            report.max_position_delta,
            report.max_size_delta
        );
        assert!(
            report.geometry_mismatches.is_empty(),
            "{}: geometry differs from libass: {:?}",
            case.name,
            report.geometry_mismatches
        );
        assert!(
            report.differing_pixels <= case.differing_pixels
                && report.max_channel_difference <= case.max_channel_difference,
            "{}: {} pixels / max diff {} exceed measured tolerance {} / {}",
            case.name,
            report.differing_pixels,
            report.max_channel_difference,
            case.differing_pixels,
            case.max_channel_difference
        );
        assert!(
            percent <= case.area_percent_limit,
            "{}: {percent:.2} % of the libass area differs (limit {} %)",
            case.name,
            case.area_percent_limit
        );
    }
}

/// Печатает измерения всех кейсов (для заполнения таблицы допусков при добавлении кейса):
/// `cargo test -p subtitle-raster --lib ass_event_tests::measure -- --ignored --nocapture`.
#[test]
#[ignore = "prints measurements for the tolerance table"]
fn measure_all_cases() {
    for point in reference_points() {
        let name = &point.stem;
        let report = compare(&point);
        eprintln!(
            "{name}: differing={} area={} max_diff={} pos_delta={} size_delta={} geometry={:?}",
            report.differing_pixels,
            report.reference_area,
            report.max_channel_difference,
            report.max_position_delta,
            report.max_size_delta,
            report.geometry_mismatches
        );
    }
}
