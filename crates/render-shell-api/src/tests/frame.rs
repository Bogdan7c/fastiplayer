use std::time::Duration;

use super::*;

/// Проверяет, что старый aggregate counter остаётся суммой submit/poll/present стадий.
#[test]
fn render_timing_keeps_submit_present_aggregate() {
    let stages = RenderFrameStageTimings {
        queue_submit: Duration::from_millis(1),
        device_poll: Duration::from_millis(2),
        pre_present_notify: Duration::from_millis(3),
        surface_present: Duration::from_millis(4),
        ..RenderFrameStageTimings::default()
    };

    let timing = RenderFrameTiming::new(stages, Duration::from_millis(20));

    assert_eq!(timing.submit_present_elapsed, Duration::from_millis(10));
    assert_eq!(timing.renderer_elapsed, Duration::from_millis(20));
}

/// Проверяет, что диагностика указывает самую дорогую renderer-owned стадию.
#[test]
fn render_stage_timing_reports_slowest_stage() {
    let stages = RenderFrameStageTimings {
        surface_acquire: Duration::from_millis(7),
        device_poll: Duration::from_millis(11),
        queue_submit: Duration::from_millis(13),
        surface_present: Duration::from_millis(5),
        ..RenderFrameStageTimings::default()
    };

    let slowest_stage = stages.slowest_stage();

    assert_eq!(slowest_stage.name, "queue_submit");
    assert_eq!(slowest_stage.elapsed, Duration::from_millis(13));
}

/// Имена стадий — диагностический контракт трассы `render_frame_timing`: каждая стадия
/// побеждает под своим прежним именем, когда она самая дорогая.
#[test]
fn every_stage_reports_its_stable_trace_name() {
    let one_slow = |set: fn(&mut RenderFrameStageTimings)| {
        let mut stages = RenderFrameStageTimings::default();
        set(&mut stages);
        stages.slowest_stage().name
    };
    assert_eq!(
        one_slow(|s| s.egui_texture_update = Duration::from_millis(9)),
        "egui_texture_update"
    );
    assert_eq!(
        one_slow(|s| s.encoder_creation = Duration::from_millis(9)),
        "encoder_creation"
    );
    assert_eq!(
        one_slow(|s| s.egui_buffer_update = Duration::from_millis(9)),
        "egui_buffer_update"
    );
    assert_eq!(
        one_slow(|s| s.surface_acquire = Duration::from_millis(9)),
        "surface_acquire"
    );
    assert_eq!(
        one_slow(|s| s.surface_view_creation = Duration::from_millis(9)),
        "surface_view_creation"
    );
    assert_eq!(
        one_slow(|s| s.video_render = Duration::from_millis(9)),
        "video_render"
    );
    assert_eq!(
        one_slow(|s| s.subtitle_overlay = Duration::from_millis(9)),
        "subtitle_overlay"
    );
    assert_eq!(
        one_slow(|s| s.egui_render = Duration::from_millis(9)),
        "egui_render"
    );
    assert_eq!(
        one_slow(|s| s.queue_submit = Duration::from_millis(9)),
        "queue_submit"
    );
    assert_eq!(
        one_slow(|s| s.device_poll = Duration::from_millis(9)),
        "device_poll"
    );
    assert_eq!(
        one_slow(|s| s.pre_present_notify = Duration::from_millis(9)),
        "pre_present_notify"
    );
    assert_eq!(
        one_slow(|s| s.surface_present = Duration::from_millis(9)),
        "surface_present"
    );
}

/// `RenderFrameFailure::new` сохраняет прежнюю семантику «ошибка видео-пути», а отказы по
/// чужому билету/ключу различимы по типу.
#[test]
fn render_frame_failure_kinds_stay_distinct() {
    let video = RenderFrameFailure::new("P010 HDR renderer rejected strict metadata");
    let foreign_surface = RenderFrameFailure::foreign_surface_frame();
    let foreign_video = RenderFrameFailure::foreign_video_frame();

    assert_eq!(video.kind, RenderFrameFailureKind::VideoRender);
    assert_eq!(video.message, "P010 HDR renderer rejected strict metadata");
    assert_eq!(
        foreign_surface.kind,
        RenderFrameFailureKind::ForeignSurfaceFrame
    );
    assert_eq!(
        foreign_video.kind,
        RenderFrameFailureKind::ForeignVideoFrame
    );
    assert_ne!(foreign_surface, foreign_video);
}

/// Пустой свежий интерфейс — действительно свежий проход без примитивов и текстур.
#[test]
fn empty_fresh_egui_content_has_no_primitives_or_texture_changes() {
    match EguiFrameContent::empty_fresh() {
        EguiFrameContent::Fresh {
            paint_jobs,
            mut textures_delta,
        } => {
            assert!(paint_jobs.is_empty());
            assert!(textures_delta.set.is_empty());
            assert!(textures_delta.free.is_empty());
            textures_delta.clear();
        }
        EguiFrameContent::ReusePrevious => panic!("empty_fresh must be a fresh egui pass"),
    }
}
