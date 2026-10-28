//! Проверка счётчиков пути программного кадра (gles2-renderer 13a).
//!
//! По этим счётчикам живой прогон доказывает, что программные кадры идут без CPU-копии:
//! трасса кадра пишет их, замер сравнивает разность за окно. Поэтому рендер обязан
//! учитывать каждый показанный программный кадр ровно одним из двух путей, не считать
//! кадры без видео и не выдавать счётчики, когда программного импорта нет.

use crate::{EguiFrameContent, VideoImportPath};

use super::{
    ConformanceCheck, ConformanceViolation, RenderShellConformanceHarness, create_renderer,
    expect_presented, present_supported_frame, render_with_acquired_surface,
};

/// Проверка 13: программный кадр учтён ровно один раз; без декодера счётчиков нет.
pub fn check_software_upload_stats_count_shown_frames(
    harness: &mut dyn RenderShellConformanceHarness,
) -> Result<(), ConformanceViolation> {
    let check = ConformanceCheck::SoftwareUploadStatsCountShownFrames;
    let screen = harness.screen();
    let mut renderer = create_renderer(harness)?;
    if let Some(stats) = renderer.video_frames().software_upload_stats() {
        return Err(ConformanceViolation::new(
            check,
            format!("renderer without a decoder must not report software upload stats: {stats:?}"),
        ));
    }

    let presented = present_supported_frame(renderer.as_mut(), harness, check, screen)?;
    let stats = renderer.video_frames().software_upload_stats();
    let path = renderer.video_frames().active_video_import_path();
    match path {
        Some(VideoImportPath::SoftwareHostUpload) => {
            let Some(stats) = stats else {
                return Err(ConformanceViolation::new(
                    check,
                    "software import must report software upload stats",
                ));
            };
            if stats.frames_total() != 1 {
                return Err(ConformanceViolation::new(
                    check,
                    format!("one shown software frame must be counted exactly once: {stats:?}"),
                ));
            }
        }
        // Стенд дал аппаратный кадр: программных счётчиков быть не должно.
        Some(VideoImportPath::HardwareZeroCopy) | None => {
            if let Some(stats) = stats {
                return Err(ConformanceViolation::new(
                    check,
                    format!("non-software import must not report software stats: {stats:?}"),
                ));
            }
            return Ok(());
        }
    }

    // Кадр без видео (только интерфейс) не загружает программный кадр: счётчик тот же.
    let ui_only = render_with_acquired_surface(
        renderer.as_mut(),
        check,
        screen,
        None,
        EguiFrameContent::empty_fresh(),
    )?;
    expect_presented(check, "frame without video", &ui_only)?;
    let after_ui_only = renderer
        .video_frames()
        .software_upload_stats()
        .map(|stats| stats.frames_total());
    drop(presented);
    if after_ui_only != Some(1) {
        return Err(ConformanceViolation::new(
            check,
            format!("a frame without video must not count a software frame: {after_ui_only:?}"),
        ));
    }
    Ok(())
}
