//! Растеризатор субтитров для тестов рендера: настоящая реплика → `SubtitleOverlay`.

use subtitle_core::{
    CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEvent, SubtitleLayer,
    SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan, TimelineInsertOutcome,
};
use subtitle_raster::{
    AssRenderOptions, CanvasSize, CueKey, CueSourceId, FontLookupCacheLimits, RasterCacheLimits,
    RasterInputLimits, RasterLimits, RenderRequest, RenderableCue, SubtitleAppearance,
    SubtitleFontLibrary, SubtitleOverlay, SubtitleRasterizer, TextLanguagePreference,
};

/// Растеризатор со встроенными шрифтами и небольшими лимитами.
pub(super) fn rasterizer() -> SubtitleRasterizer {
    let fonts = SubtitleFontLibrary::with_embedded_fonts(FontLookupCacheLimits {
        max_cached_chars: 4096,
    })
    .expect("embedded fonts");
    SubtitleRasterizer::new(
        fonts,
        RasterLimits {
            cache: RasterCacheLimits {
                cue_bitmaps_bytes: 16 * 1024 * 1024,
                shaper_faces: 8,
                blur_scratch_retained_bytes: 32 * 1024 * 1024,
                glyph_bitmaps_bytes: 40 * 1024 * 1024,
                glyph_composites_bytes: 20 * 1024 * 1024,
                glyph_outline_items: 10_000,
                shaping_cache_bytes: 4 * 1024 * 1024,
            },
            input: RasterInputLimits {
                max_chars_per_cue: 1000,
                max_cues_per_frame: 8,
                max_ass_events_per_frame: 8,
                max_canvas_side: 8192,
            },
        },
    )
}

/// Растеризует одну текстовую реплику на холсте `canvas`.
pub(super) fn rasterize_text(
    rasterizer: &mut SubtitleRasterizer,
    text: &str,
    canvas: CanvasSize,
) -> SubtitleOverlay {
    let mut timeline = SubtitleTimeline::new();
    let cue = SubtitleTextCue {
        text: StyledText::new(vec![TextSpan::text(text, SpanStyle::default())]),
        placement: CuePlacement::default(),
    };
    let event = SubtitleEvent::new(
        SubtitleTime::from_millis(0),
        SubtitleTime::from_millis(5),
        SubtitleLayer::DEFAULT,
        SubtitleContent::Text(cue.clone()),
    )
    .expect("interval");
    let TimelineInsertOutcome::Inserted(id) = timeline.insert(event) else {
        panic!("event must be inserted");
    };
    let renderable = [RenderableCue::text(
        CueKey::new(CueSourceId::new(1), id),
        &cue,
    )];
    rasterizer
        .render(&RenderRequest {
            moment: SubtitleTime::ZERO,
            cues: &renderable,
            canvas,
            appearance: &SubtitleAppearance::youtube_like(),
            text_language: TextLanguagePreference::unknown(),
            ass: AssRenderOptions::default(),
        })
        .clone()
}
