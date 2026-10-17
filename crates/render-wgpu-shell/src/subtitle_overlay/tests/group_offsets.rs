//! Независимые сдвиги групп размещения: настоящий ASS -> растеризатор -> GPU pass -> пиксели.
//!
//! Диалог снизу (группа Main) и надпись с `\pos` (группа Fixed) должны ехать каждая своим
//! сдвигом, а смена сдвигов не должна перезагружать маски в атлас.

use subtitle_core::{
    AssCue, SubtitleContent, SubtitleEventId, SubtitleLayer, SubtitleTimeline,
    TimelineInsertOutcome,
};
use subtitle_formats::{
    AssParseLimits, LegacyEncodingHint, SubtitleParseLimits, TextSubtitleFormat, load_text_subtitle,
};
use subtitle_raster::{AssEventTiming, CanvasRect, OverlayPlacementGroup};

use super::*;

/// Файл: диалог внизу (по умолчанию) и красная надпись у верхнего левого угла.
const TWO_GROUP_SCRIPT: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 256\nPlayResY: 144\nScaledBorderAndShadow: yes\nWrapStyle: 0\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Noto Sans,16,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,Bottom dialogue\nDialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,{\\an7\\pos(10,10)\\1c&H0000FF&}SIGN\n";

/// Реплики файла `TWO_GROUP_SCRIPT`, прошедшие настоящий разбор.
fn two_group_cues() -> Vec<(CueKey, SubtitleLayer, AssCue, AssEventTiming)> {
    let limits = SubtitleParseLimits::new(1 << 20, 1000, 100).with_ass_limits(AssParseLimits {
        max_header_bytes: 1 << 16,
        max_styles: 16,
        max_override_tags_per_event: 64,
        max_tracked_unknown_tag_names: 8,
    });
    let loaded = load_text_subtitle(
        TWO_GROUP_SCRIPT.as_bytes(),
        TextSubtitleFormat::Ass,
        LegacyEncodingHint::Generic,
        &limits,
    )
    .expect("two-group ASS script must load");
    let mut timeline = SubtitleTimeline::new();
    let ids: Vec<(SubtitleEventId, SubtitleLayer)> = loaded
        .document()
        .events()
        .iter()
        .map(|event| match timeline.insert(event.clone()) {
            TimelineInsertOutcome::Inserted(id) => (id, event.layer()),
            other => panic!("events must be distinct: {other:?}"),
        })
        .collect();
    ids.into_iter()
        .map(|(id, layer)| {
            let event = timeline.event(id).expect("inserted event");
            let SubtitleContent::Ass(cue) = event.content() else {
                panic!("ASS file must give ASS events");
            };
            (
                CueKey::new(CueSourceId::new(7), id),
                layer,
                cue.clone(),
                AssEventTiming::new(event.start(), event.end()),
            )
        })
        .collect()
}

/// Растеризует обе реплики на холсте 256x144.
fn two_group_overlay() -> SubtitleOverlay {
    let cues = two_group_cues();
    let renderables: Vec<RenderableCue<'_>> = cues
        .iter()
        .map(|(key, layer, cue, timing)| RenderableCue::ass(*key, *layer, cue, *timing))
        .collect();
    let mut rasterizer = rasterizer();
    rasterizer
        .render(&RenderRequest {
            moment: subtitle_core::SubtitleTime::ZERO,
            cues: &renderables,
            canvas: CanvasSize::new(TARGET_WIDTH, TARGET_HEIGHT).expect("canvas"),
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}

fn full_picture() -> render_core::PictureRect {
    render_core::PictureRect {
        x: 0.0,
        y: 0.0,
        width: TARGET_WIDTH as f32,
        height: TARGET_HEIGHT as f32,
    }
}

fn region_pixels(pixels: &[u8], area: CanvasRect, shift_x: i32, shift_y: i32) -> Vec<[u8; 4]> {
    let mut collected = Vec::new();
    for y in area.y..area.y + area.height as i32 {
        for x in area.x..area.x + area.width as i32 {
            collected.push(pixel(pixels, (x + shift_x) as u32, (y + shift_y) as u32));
        }
    }
    collected
}

#[test]
fn main_and_fixed_groups_move_independently_without_reuploading_masks() {
    let mut harness = gpu_or_skip!();
    let overlay = two_group_overlay();
    let main = overlay
        .group_bounds(OverlayPlacementGroup::Main)
        .expect("bottom dialogue is the main group");
    let fixed = overlay
        .group_bounds(OverlayPlacementGroup::Fixed)
        .expect("\\pos sign is the fixed group");
    let frame_with = |offsets| SubtitleOverlayFrame {
        overlay: &overlay,
        picture_rect: full_picture(),
        layer_offsets: offsets,
    };
    let (_, video_only) = harness.frame(None, full_view());
    let (_, original) = harness.frame(Some(&frame_with(SubtitleLayerOffsets::NONE)), full_view());
    let uploads_before = harness.renderer.upload_stats();

    // Поднимаем только основную группу на 20 px; надпись остаётся.
    let lift = -20_i32;
    let (outcome, lifted) = harness.frame(
        Some(&frame_with(SubtitleLayerOffsets {
            main: SubtitleLayerOffset {
                x: 0.0,
                y: lift as f32,
            },
            fixed: SubtitleLayerOffset::NONE,
        })),
        full_view(),
    );
    assert!(matches!(outcome, SubtitleOverlayOutcome::Drawn { .. }));

    // Надпись: все пиксели области совпадают с исходным кадром и отличаются от видео.
    assert_eq!(
        region_pixels(&lifted, fixed, 0, 0),
        region_pixels(&original, fixed, 0, 0)
    );
    assert_ne!(
        region_pixels(&lifted, fixed, 0, 0),
        region_pixels(&video_only, fixed, 0, 0),
        "надпись действительно нарисована"
    );
    // Диалог: область, сдвинутая на `lift`, совпадает с исходной областью.
    assert_eq!(
        region_pixels(&lifted, main, 0, lift),
        region_pixels(&original, main, 0, 0)
    );
    assert_ne!(
        region_pixels(&original, main, 0, 0),
        region_pixels(&video_only, main, 0, 0),
        "диалог действительно нарисован"
    );

    // Фиксированная группа едет отдельно: теперь надпись на 6 px вниз, диалог на месте.
    let drop_y = 6_i32;
    let (_, dropped) = harness.frame(
        Some(&frame_with(SubtitleLayerOffsets {
            main: SubtitleLayerOffset::NONE,
            fixed: SubtitleLayerOffset {
                x: 0.0,
                y: drop_y as f32,
            },
        })),
        full_view(),
    );
    assert_eq!(
        region_pixels(&dropped, fixed, 0, drop_y),
        region_pixels(&original, fixed, 0, 0)
    );
    assert_eq!(
        region_pixels(&dropped, main, 0, 0),
        region_pixels(&original, main, 0, 0)
    );

    // Только uniform: маски и инстансы не перезагружались.
    let uploads_after = harness.renderer.upload_stats();
    assert_eq!(uploads_after.atlas_uploads, uploads_before.atlas_uploads);
    assert_eq!(uploads_after.bitmap_uploads, uploads_before.bitmap_uploads);
    assert_eq!(
        uploads_after.uniform_uploads,
        uploads_before.uniform_uploads + 2
    );
}

#[test]
fn uniform_offsets_move_both_groups_like_the_single_offset_did() {
    let mut harness = gpu_or_skip!();
    let overlay = two_group_overlay();
    let main = overlay
        .group_bounds(OverlayPlacementGroup::Main)
        .expect("main group");
    let fixed = overlay
        .group_bounds(OverlayPlacementGroup::Fixed)
        .expect("fixed group");
    let frame_with = |offsets| SubtitleOverlayFrame {
        overlay: &overlay,
        picture_rect: full_picture(),
        layer_offsets: offsets,
    };
    let (_, original) = harness.frame(Some(&frame_with(SubtitleLayerOffsets::NONE)), full_view());
    let shift = 5_i32;
    let (_, shifted) = harness.frame(
        Some(&frame_with(SubtitleLayerOffsets::uniform(
            SubtitleLayerOffset {
                x: shift as f32,
                y: 0.0,
            },
        ))),
        full_view(),
    );
    assert_eq!(
        region_pixels(&shifted, main, shift, 0),
        region_pixels(&original, main, 0, 0)
    );
    assert_eq!(
        region_pixels(&shifted, fixed, shift, 0),
        region_pixels(&original, fixed, 0, 0)
    );
}
