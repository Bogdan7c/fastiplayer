//! Картинки, обрезанные краем кадра, уходят на GPU окном в общей маске (смещение + шаг
//! исходной маски): пиксели на экране обязаны совпасть с CPU-композитом растеризатора.

use super::group_offsets::cues_of_script;
use super::*;

/// Диалог у правого нижнего угла холста 256x144: буквы выходят за правый и нижний край.
const EDGE_SCRIPT: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 256\nPlayResY: 144\nScaledBorderAndShadow: yes\nWrapStyle: 2\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Noto Sans,40,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,7,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,{\\pos(200,110)}EDGE\n";

pub(crate) fn edge_overlay() -> SubtitleOverlay {
    let cues = cues_of_script(EDGE_SCRIPT);
    let renderables: Vec<RenderableCue<'_>> = cues
        .iter()
        .map(|(key, layer, cue, timing)| RenderableCue::ass(*key, *layer, cue, *timing))
        .collect();
    rasterizer()
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

#[test]
fn pictures_cut_by_the_canvas_edge_reach_the_screen_pixel_for_pixel() {
    let mut harness = gpu_or_skip!();
    let overlay = edge_overlay();
    // Сцена действительно обрезана краем: окно начинается не с начала общего буфера или
    // шаг строки больше ширины, то есть картинка не плотная копия.
    let is_window = |bitmap: &subtitle_raster::SubtitleBitmap| {
        bitmap.shared_coverage().len() > bitmap.coverage().len()
            || bitmap.stride() > bitmap.width() as usize
    };
    assert!(overlay.bitmaps().iter().any(is_window));
    let (_, video_only) = harness.frame(None, full_view());
    let (outcome, pixels) = harness.frame(
        Some(&SubtitleOverlayFrame {
            overlay: &overlay,
            picture_rect: super::group_offsets::full_picture(),
            layer_offsets: SubtitleLayerOffsets::NONE,
        }),
        full_view(),
    );
    assert!(matches!(outcome, SubtitleOverlayOutcome::Drawn { .. }));

    let reference = overlay.composite_rgba_over_transparent();
    let mut painted = 0;
    for y in 0..TARGET_HEIGHT {
        for x in 0..TARGET_WIDTH {
            let video = pixel(&video_only, x, y);
            let offset = ((y * TARGET_WIDTH + x) * 4) as usize;
            let [r, g, b, a] = [0, 1, 2, 3].map(|i| i32::from(reference[offset + i]));
            let expected = [
                (r * a + i32::from(video[0]) * (255 - a)) / 255,
                (g * a + i32::from(video[1]) * (255 - a)) / 255,
                (b * a + i32::from(video[2]) * (255 - a)) / 255,
            ];
            let actual = pixel(&pixels, x, y);
            for channel in 0..3 {
                assert!(
                    (i32::from(actual[channel]) - expected[channel]).abs() <= BLEND_TOLERANCE,
                    "pixel ({x},{y}) channel {channel}: actual {} expected {}",
                    actual[channel],
                    expected[channel]
                );
            }
            if actual != video {
                painted += 1;
            }
        }
    }
    assert!(
        painted > 50,
        "the cut text must still be visible: {painted}"
    );
}
