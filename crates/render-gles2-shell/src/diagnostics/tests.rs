use std::time::Duration;

use codec_core::{
    ColorPrimaries, ColorRange, HdrMetadata, MatrixCoefficients, TransferFunction,
    VideoColorMetadata,
};
use render_core::{RenderLiveSettings, RenderViewport};
use video_frame_contract::VideoFramePixelLayout;

use super::*;
use crate::draw_plan::VideoPlacement;
use crate::frame_metadata::renderable_frame;
use crate::test_support::{high_bit_host_frame, host_frame};

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// Тайминги, где каждая стадия своя: перепутанное поле даст другое число.
fn distinct_timings() -> UiFrameTimings {
    UiFrameTimings {
        egui_texture_update: ms(1),
        egui_buffer_update: ms(2),
        surface_acquire: ms(3),
        video_render: ms(4),
        subtitle_overlay: ms(5),
        egui_render: ms(6),
        window_corner_mask: ms(7),
        surface_present: ms(8),
    }
}

#[test]
fn stages_keep_their_names_and_missing_stages_are_zero() {
    let stages = render_frame_stage_timings(&distinct_timings());
    assert_eq!(
        stages,
        RenderFrameStageTimings {
            egui_texture_update: ms(1),
            encoder_creation: Duration::ZERO,
            egui_buffer_update: ms(2),
            surface_acquire: ms(3),
            surface_view_creation: Duration::ZERO,
            video_render: ms(4),
            egui_render: ms(6),
            subtitle_overlay: ms(5),
            window_corner_mask: ms(7),
            queue_submit: Duration::ZERO,
            device_poll: Duration::ZERO,
            pre_present_notify: Duration::ZERO,
            surface_present: ms(8),
        }
    );
    let slowest = stages.slowest_stage();
    assert_eq!((slowest.name, slowest.elapsed), ("surface_present", ms(8)));
}

#[test]
fn frame_timing_keeps_the_callers_renderer_time_and_present_as_submit() {
    let timing = render_frame_timing(&distinct_timings(), ms(40));
    assert_eq!(timing.renderer_elapsed, ms(40));
    // Сумма submit/present у GLES2 — только показ (submit/poll/notify нулевые).
    assert_eq!(timing.submit_present_elapsed, ms(8));
    assert_eq!(timing.stages.subtitle_overlay, ms(5));
}

fn placement<'a>(surface: (u32, u32), exclusions: &'a [RenderViewport]) -> VideoPlacement<'a> {
    VideoPlacement {
        surface_size: surface,
        video_viewport: RenderViewport::full_surface(surface.0, surface.1),
        exclusions,
    }
}

#[test]
fn sdr_pass_reports_its_colour_path_and_rect_count_without_hdr_markers() {
    let test_frame = host_frame(
        VideoFramePixelLayout::Yuv420Planar8,
        [32, 16],
        0,
        [16, 128, 128],
    );
    let frame = renderable_frame(&test_frame.frame).expect("YUV frame");
    let sidebar = [RenderViewport::new(24, 0, 8, 16)];
    let settings = RenderLiveSettings::default();
    let plan =
        VideoDrawPlan::for_frame(&frame, placement((32, 16), &sidebar), &settings).expect("plan");
    let diagnostics = video_pass_diagnostics(&frame, &plan);
    assert_eq!(
        diagnostics.active_color_path.as_ref(),
        Some(plan.active_color_path())
    );
    assert_eq!(
        diagnostics.video_draw_rect_count, 1,
        "сайдбар справа — одна часть"
    );
    assert_eq!(diagnostics.hdr_reference_defaults, None);

    let middle = [RenderViewport::new(10, 0, 4, 16)];
    let split =
        VideoDrawPlan::for_frame(&frame, placement((32, 16), &middle), &settings).expect("plan");
    assert_eq!(
        video_pass_diagnostics(&frame, &split).video_draw_rect_count,
        2
    );
}

#[test]
fn hdr_pass_marks_which_metadata_came_from_reference_defaults() {
    let mut test_frame = high_bit_host_frame(
        VideoFramePixelLayout::Yuv420Planar10Le,
        [16, 16],
        0,
        [500, 512, 512],
    );
    test_frame.frame.color = VideoColorMetadata {
        range: ColorRange::Limited,
        matrix: MatrixCoefficients::Bt2020,
        primaries: ColorPrimaries::Bt2020,
        transfer: TransferFunction::Pq,
        hdr_metadata: Some(HdrMetadata {
            color_primaries: ColorPrimaries::Bt2020,
            transfer_function: TransferFunction::Pq,
            max_luminance_nits: Some(1_000.0),
            min_luminance_nits: None,
            max_content_light_level_nits: Some(800),
            max_frame_average_light_level_nits: None,
        }),
        ..VideoColorMetadata::sdr_bt709_limited()
    };
    let frame = renderable_frame(&test_frame.frame).expect("YUV frame");
    let plan = VideoDrawPlan::for_frame(
        &frame,
        placement((16, 16), &[]),
        &RenderLiveSettings::default(),
    )
    .expect("HDR plan");
    assert!(plan.converts_hdr_to_sdr());
    let diagnostics = video_pass_diagnostics(&frame, &plan);
    assert_eq!(
        diagnostics.hdr_reference_defaults,
        Some(HdrReferenceDefaultDiagnostics {
            mastering_max_luminance: HdrMetadataDiagnosticMarker::Confirmed,
            mastering_min_luminance: HdrMetadataDiagnosticMarker::ReferenceDefault,
            max_content_light_level: HdrMetadataDiagnosticMarker::Confirmed,
            max_frame_average_light_level: HdrMetadataDiagnosticMarker::ReferenceDefault,
        })
    );
    assert!(
        diagnostics
            .active_color_path
            .is_some_and(|path| path.hdr_to_sdr.is_some()),
        "цветовой путь отмечает HDR → SDR"
    );
}

#[test]
fn import_stats_carry_every_cache_counter_under_trace_names() {
    let snapshot = DmaBufImportCacheSnapshot {
        entries: 5,
        capacity: 24,
        hits: 100,
        imports: 21,
        import_failures: 2,
        replaced_same_resource: 3,
        evicted_format_change: 4,
        evicted_capacity: 6,
        purged: 9,
    };
    assert_eq!(
        frame_import_stats(&snapshot),
        FrameImportStats {
            imports_performed: 21,
            import_failures: 2,
            image_reuse_enabled: true,
            cache: FrameImportCacheSnapshot {
                entries: 5,
                capacity: 24,
                hits: 100,
                replaced_same_resource: 3,
                evicted_format_change: 4,
                evicted_capacity: 6,
            },
        }
    );
}
