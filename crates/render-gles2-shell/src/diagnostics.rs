//! Диагностика кадра GLES2 в нейтральных типах контракта (сессия 08): те же имена и смысл, что
//! у wgpu, чтобы трасса `fastiplayer::render_frame_timing`, панель телеметрии и инструменты замера
//! (`docs/benchmarks/tools/`) читали прогон GLES2 без правок.
//!
//! - Стадии кадра ([`render_frame_stage_timings`]): стадий, которых у GLES2 нет
//!   (`encoder_creation`, `surface_view_creation`, `queue_submit`, `device_poll`,
//!   `pre_present_notify`), — ноль, а не пропуск поля (договор `RenderFrameStageTimings`).
//!   `surface_acquire` — `eglMakeCurrent` + привязка цели, `surface_present` — `eglSwapBuffers`.
//! - Исход `RenderFrameOutcome::Presented` строится только из `UiFrameOutcome::Presented`, а его
//!   `FrameCompositor` возвращает только после успешного показа (`eglSwapBuffers`): по нему
//!   приложение пишет `fastiplayer::video_render_acceptance`.
//! - Диагностика видеопрохода ([`video_pass_diagnostics`]) и счётчики импорта
//!   ([`frame_import_stats`]).

use std::time::Duration;

use codec_core::HdrMetadata;
use gles2_egui::UiFrameTimings;
use gles2_video::DmaBufImportCacheSnapshot;
use render_core::{
    HdrMetadataDiagnosticMarker, HdrReferenceDefaultDiagnostics, RenderDiagnostics, RenderableFrame,
};
use render_shell_api::{
    FrameImportCacheSnapshot, FrameImportStats, RenderFrameStageTimings, RenderFrameTiming,
};

use crate::draw_plan::VideoDrawPlan;

/// Стадии кадра GLES2 под именами `RenderFrameStageTimings`.
#[must_use]
pub fn render_frame_stage_timings(timings: &UiFrameTimings) -> RenderFrameStageTimings {
    RenderFrameStageTimings {
        egui_texture_update: timings.egui_texture_update,
        egui_buffer_update: timings.egui_buffer_update,
        surface_acquire: timings.surface_acquire,
        video_render: timings.video_render,
        subtitle_overlay: timings.subtitle_overlay,
        egui_render: timings.egui_render,
        surface_present: timings.surface_present,
        // У GLES2 этих стадий нет: команды уходят драйверу прямо в вызовах GL, кадр показывает
        // `eglSwapBuffers` (он — `surface_present`).
        encoder_creation: Duration::ZERO,
        surface_view_creation: Duration::ZERO,
        queue_submit: Duration::ZERO,
        device_poll: Duration::ZERO,
        pre_present_notify: Duration::ZERO,
    }
}

/// Тайминг показанного кадра. `renderer_elapsed` — всё время рендера на кадр, как у wgpu
/// (включая `surface_acquire`): его меряет вызывающий вокруг всего кадра.
#[must_use]
pub fn render_frame_timing(
    timings: &UiFrameTimings,
    renderer_elapsed: Duration,
) -> RenderFrameTiming {
    RenderFrameTiming::new(render_frame_stage_timings(timings), renderer_elapsed)
}

/// Диагностика видеопрохода кадра `frame`, нарисованного по плану `plan`: цветовой путь, число
/// прямоугольников, метки HDR-метаданных «взято по умолчанию» (только у HDR → SDR) — как
/// `WgpuVideoRenderer::diagnostics`. Кадр без видео — `RenderDiagnostics::default()`.
#[must_use]
pub fn video_pass_diagnostics(frame: &RenderableFrame, plan: &VideoDrawPlan) -> RenderDiagnostics {
    RenderDiagnostics {
        active_color_path: Some(plan.active_color_path().clone()),
        hdr_reference_defaults: plan
            .converts_hdr_to_sdr()
            .then(|| hdr_reference_defaults(frame.color.hdr_metadata.as_ref())),
        video_draw_rect_count: plan.visible_rect_count(),
    }
}

/// Какие HDR-метаданные кадр дал сам, а какие рендер взял по умолчанию (правило wgpu
/// `host_yuv420_optional_metadata_uniforms`: поле есть — `Confirmed`, нет — `ReferenceDefault`).
fn hdr_reference_defaults(metadata: Option<&HdrMetadata>) -> HdrReferenceDefaultDiagnostics {
    let marker = |present: bool| {
        if present {
            HdrMetadataDiagnosticMarker::Confirmed
        } else {
            HdrMetadataDiagnosticMarker::ReferenceDefault
        }
    };
    HdrReferenceDefaultDiagnostics {
        mastering_max_luminance: marker(metadata.is_some_and(|m| m.max_luminance_nits.is_some())),
        mastering_min_luminance: marker(metadata.is_some_and(|m| m.min_luminance_nits.is_some())),
        max_content_light_level: marker(
            metadata.is_some_and(|m| m.max_content_light_level_nits.is_some()),
        ),
        max_frame_average_light_level: marker(
            metadata.is_some_and(|m| m.max_frame_average_light_level_nits.is_some()),
        ),
    }
}

/// Счётчики кэша импорта DMA-BUF под именами трассы (`dma_buf_vulkan_imports`,
/// `dma_buf_cache_hits`, `dma_buf_cache_entries` — имена исторические, общие для рендеров).
/// Кэш GLES2 всегда переиспользует импорт одного буфера (`image_reuse_enabled`).
#[must_use]
pub fn frame_import_stats(snapshot: &DmaBufImportCacheSnapshot) -> FrameImportStats {
    FrameImportStats {
        imports_performed: snapshot.imports,
        import_failures: snapshot.import_failures,
        image_reuse_enabled: true,
        cache: FrameImportCacheSnapshot {
            entries: snapshot.entries,
            capacity: snapshot.capacity,
            hits: snapshot.hits,
            replaced_same_resource: snapshot.replaced_same_resource,
            evicted_format_change: snapshot.evicted_format_change,
            evicted_capacity: snapshot.evicted_capacity,
        },
    }
}

#[cfg(test)]
mod tests;
