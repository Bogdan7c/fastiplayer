use player_core::{
    PlayerRenderError, PlayerTimelineHoverPrepareBorrowOutcome,
    PlayerTimelineHoverPreparedFrameBorrow,
};
use render_core::RenderViewport;
use render_wgpu_video::{WgpuFrameTextureViewMaterializer, WgpuRenderableFrame};

use crate::state::RenderablePresentFrame;
use crate::ui::timeline::{TimelineHoverTarget, TimelineHoverVisualTarget};

use super::shared_frame_materialization::{
    SharedVideoFrameLeaseRole, SharedVideoFrameMaterializationOutcome,
    SharedVideoFrameMaterializationRequest, materialize_shared_video_frame,
};
use super::{build_render_input_video_frame, raw_viewport_from_ui_rect};

/// Ширина hover preview в logical points; это UI-presentational константа, не decode config.
const HOVER_PREVIEW_WIDTH_POINTS: f32 = 220.0;

/// Минимальная ширина preview, чтобы не получить нулевой viewport на маленьком окне.
const HOVER_PREVIEW_MIN_WIDTH_POINTS: f32 = 120.0;

/// Максимальная доля ширины окна, которую может занять hover preview.
const HOVER_PREVIEW_MAX_SCREEN_WIDTH_FRACTION: f32 = 0.45;

/// Aspect ratio preview surface-а; renderer внутри сохранит aspect ratio самого видео.
const HOVER_PREVIEW_ASPECT_RATIO: f32 = 16.0 / 9.0;

/// Отступ между верхом timeline и низом preview.
const HOVER_PREVIEW_TIMELINE_GAP_POINTS: f32 = 10.0;

/// App-owned visual state preview-а поверх timeline.
#[derive(Default)]
pub(crate) struct TimelineHoverPreviewRenderState {
    /// Последний materialized borrow, который можно показать или повторить при Busy.
    ready: Option<TimelineHoverPreviewReadyFrame>,
}

/// Materialized hover preview frame; ownership остаётся clone lease-а, не branch entry.
struct TimelineHoverPreviewReadyFrame {
    /// Visual target с placement; media target используется для stale/Busy checks.
    visual_target: TimelineHoverVisualTarget,

    /// Lease + WGPU views, полученные через shared S15 materialization helper.
    renderable_frame: RenderablePresentFrame,
}

/// Borrowed render input одного кадра для shell preview pass-а.
pub(crate) struct TimelineHoverPreviewRenderInput<'frame> {
    /// Renderer-facing frame, который borrow-ит state на время submit.
    pub(crate) video_frame: WgpuRenderableFrame<'frame>,

    /// Physical viewport preview surface-а.
    pub(crate) viewport: RenderViewport,
}

/// Результат обновления preview state; нужен для tests/diagnostics без bool-смешения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPreviewUpdateOutcome {
    /// Preview готов и заменил предыдущий ready frame.
    Ready,

    /// Materializer занят, но есть ready frame того же target-а.
    BusyKeptLastReady,

    /// Materializer занят, а безопасного ready frame для текущего target-а нет.
    BusyEmpty,

    /// Для borrow-а нет active WGPU materializer-а.
    MissingMaterializer,

    /// Working set не содержит entry для текущего target-а.
    WorkingSetMiss,

    /// Working set отверг entry по timing/exactness guard-ам.
    TimingRejected,

    /// Provider сообщил Missing resource.
    Missing,

    /// Provider сообщил Unsupported resource/contract.
    Unsupported,

    /// Provider/materializer сообщил ошибку.
    Error,
}

impl TimelineHoverPreviewRenderState {
    /// Полностью очищает visual preview borrow/render state.
    pub(crate) fn clear(&mut self) {
        self.ready = None;
    }

    /// Обновляет preview из shared prepared entry borrow-а.
    pub(crate) fn update_from_borrow(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        borrow_outcome: PlayerTimelineHoverPrepareBorrowOutcome,
        materializer: Option<&dyn WgpuFrameTextureViewMaterializer>,
    ) -> TimelineHoverPreviewUpdateOutcome {
        let Some(materializer) = materializer else {
            self.clear();
            return TimelineHoverPreviewUpdateOutcome::MissingMaterializer;
        };

        let borrowed_frame = match borrow_outcome {
            PlayerTimelineHoverPrepareBorrowOutcome::Borrowed(borrowed_frame) => borrowed_frame,
            PlayerTimelineHoverPrepareBorrowOutcome::Miss(_reason) => {
                self.clear();
                return TimelineHoverPreviewUpdateOutcome::WorkingSetMiss;
            }
            PlayerTimelineHoverPrepareBorrowOutcome::TimingRejected(_rejection) => {
                self.clear();
                return TimelineHoverPreviewUpdateOutcome::TimingRejected;
            }
        };

        self.materialize_borrowed_frame(visual_target, borrowed_frame, materializer)
    }

    /// Собирает borrowed render input для текущего shell frame-а.
    pub(crate) fn render_input(
        &self,
        screen: render_wgpu_shell::RenderScreenDescriptor,
    ) -> Result<Option<TimelineHoverPreviewRenderInput<'_>>, PlayerRenderError> {
        let Some(ready) = &self.ready else {
            return Ok(None);
        };
        let video_frame =
            build_render_input_video_frame(&ready.renderable_frame, "timeline_hover_preview")?;
        let viewport = hover_preview_viewport(ready.visual_target, screen);

        Ok(Some(TimelineHoverPreviewRenderInput {
            video_frame,
            viewport,
        }))
    }

    /// Отмечает cloned preview lease как отправленный renderer-у.
    pub(crate) fn mark_submitted_to_renderer(&self) {
        if let Some(ready) = &self.ready {
            ready
                .renderable_frame
                .present_frame
                .mark_submitted_to_renderer();
        }
    }

    /// Materialize выполняется только через shared helper и не трогает branch ownership.
    fn materialize_borrowed_frame(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        borrowed_frame: PlayerTimelineHoverPreparedFrameBorrow,
        materializer: &dyn WgpuFrameTextureViewMaterializer,
    ) -> TimelineHoverPreviewUpdateOutcome {
        let materialization = materialize_shared_video_frame(
            SharedVideoFrameMaterializationRequest::new(
                SharedVideoFrameLeaseRole::HoverPreview,
                borrowed_frame.lease().clone(),
            ),
            materializer,
        );

        match materialization.outcome {
            SharedVideoFrameMaterializationOutcome::Ready { materialized_frame } => {
                self.ready = Some(TimelineHoverPreviewReadyFrame {
                    visual_target,
                    renderable_frame: materialized_frame.into_renderable_present_frame(),
                });
                TimelineHoverPreviewUpdateOutcome::Ready
            }
            SharedVideoFrameMaterializationOutcome::Busy { .. } => {
                if self.ready_matches_target(visual_target.target()) {
                    if let Some(ready) = &mut self.ready {
                        ready.visual_target = visual_target;
                    }
                    TimelineHoverPreviewUpdateOutcome::BusyKeptLastReady
                } else {
                    self.clear();
                    TimelineHoverPreviewUpdateOutcome::BusyEmpty
                }
            }
            SharedVideoFrameMaterializationOutcome::Missing { .. } => {
                self.clear();
                TimelineHoverPreviewUpdateOutcome::Missing
            }
            SharedVideoFrameMaterializationOutcome::Unsupported { .. } => {
                self.clear();
                TimelineHoverPreviewUpdateOutcome::Unsupported
            }
            SharedVideoFrameMaterializationOutcome::Error { .. } => {
                self.clear();
                TimelineHoverPreviewUpdateOutcome::Error
            }
        }
    }

    /// Busy fallback допускается только для того же media target-а, без nearest preview.
    fn ready_matches_target(&self, target: TimelineHoverTarget) -> bool {
        self.ready
            .as_ref()
            .map(|ready| ready.visual_target.target() == target)
            .unwrap_or(false)
    }
}

/// Строит physical viewport preview-а из logical egui placement-а.
fn hover_preview_viewport(
    visual_target: TimelineHoverVisualTarget,
    screen: render_wgpu_shell::RenderScreenDescriptor,
) -> RenderViewport {
    let pixels_per_point = screen.pixels_per_point.max(1.0);
    let screen_width_points = screen.size_in_pixels[0] as f32 / pixels_per_point;
    let screen_height_points = screen.size_in_pixels[1] as f32 / pixels_per_point;
    let placement = visual_target.placement();
    let timeline_rect = placement.timeline_rect();
    let max_width = (screen_width_points * HOVER_PREVIEW_MAX_SCREEN_WIDTH_FRACTION)
        .max(HOVER_PREVIEW_MIN_WIDTH_POINTS);
    let preview_width = HOVER_PREVIEW_WIDTH_POINTS
        .min(max_width)
        .min(screen_width_points.max(1.0));
    let preview_height =
        (preview_width / HOVER_PREVIEW_ASPECT_RATIO).min(screen_height_points.max(1.0));
    let pointer_x = placement.pointer_position().x;
    let max_left = (screen_width_points - preview_width).max(0.0);
    let left = (pointer_x - preview_width / 2.0).clamp(0.0, max_left);
    let bottom = (timeline_rect.top() - HOVER_PREVIEW_TIMELINE_GAP_POINTS)
        .clamp(preview_height, screen_height_points);
    let top = (bottom - preview_height).max(0.0);
    let preview_rect = egui::Rect::from_min_size(
        egui::pos2(left, top),
        egui::vec2(preview_width, preview_height),
    );

    raw_viewport_from_ui_rect(preview_rect, pixels_per_point)
}
