use player_core::{
    PlayerRenderError, PlayerTimelineHoverPrepareBorrowOutcome,
    PlayerTimelineHoverPreparedFrameBorrow,
};
use render_core::RenderViewport;
use render_wgpu_video::{WgpuFrameTextureViewMaterializer, WgpuRenderableFrame};
use video_present_core::VideoFrameLease;

use crate::state::RenderablePresentFrame;
use crate::timeline_hover_approx_preview::TimelineHoverApproximatePreviewBorrow;
use crate::timeline_hover_prepare::DecodeDependencySpan;
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

    /// Текущий visual target, для которого remote source ещё открывается.
    loading: Option<TimelineHoverVisualTarget>,
}

/// Read-only diagnostics visual preview state-а без доступа к leases/materializer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TimelineHoverPreviewRenderDiagnosticsSnapshot {
    /// Есть ли готовый preview frame для overlay render pass-а.
    pub(crate) ready: bool,

    /// Есть ли preview-only loading state для network hover open-а.
    pub(crate) loading_preview_only: bool,
}

/// Materialized hover preview frame; ownership остаётся clone lease-а, не branch entry.
struct TimelineHoverPreviewReadyFrame {
    /// Visual target с placement; media target используется для stale/Busy checks.
    visual_target: TimelineHoverVisualTarget,

    /// Источник ready frame-а нужен для безопасного Busy reuse.
    source: TimelineHoverPreviewReadyFrameSource,

    /// Lease + WGPU views, полученные через shared S15 materialization helper.
    renderable_frame: RenderablePresentFrame,
}

/// Семантика ready preview frame-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineHoverPreviewReadyFrameSource {
    /// Exact prepared frame валиден только для того же semantic target-а.
    ExactTargetOrAfter,

    /// Approximate keyframe валиден для span-ов с тем же decode-safe start.
    ApproximateKeyframe { span: DecodeDependencySpan },
}

/// Guard, который описывает допустимое переиспользование ready frame-а при Busy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineHoverPreviewReadyReuseGuard {
    /// Exact кадр нельзя переносить на соседний hover target.
    ExactTarget,

    /// Approximate кадр можно переносить внутри того же decode-safe span anchor-а.
    ApproximateSpan { span: DecodeDependencySpan },
}

impl TimelineHoverPreviewReadyReuseGuard {
    /// Строит reuse guard из источника нового materialization запроса.
    #[must_use]
    const fn from_source(source: TimelineHoverPreviewReadyFrameSource) -> Self {
        match source {
            TimelineHoverPreviewReadyFrameSource::ExactTargetOrAfter => Self::ExactTarget,
            TimelineHoverPreviewReadyFrameSource::ApproximateKeyframe { span } => {
                Self::ApproximateSpan { span }
            }
        }
    }
}

/// Borrowed render input одного кадра для shell preview pass-а.
pub(crate) struct TimelineHoverPreviewRenderInput<'frame> {
    /// Renderer-facing frame, который borrow-ит state на время submit.
    pub(crate) video_frame: WgpuRenderableFrame<'frame>,

    /// Physical viewport preview surface-а.
    pub(crate) viewport: RenderViewport,
}

/// Preview-only loading state: он не попадает в timeline inline status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum TimelineHoverPreviewLoadState {
    #[default]
    Idle,
    NetworkOpening {
        target: TimelineHoverTarget,
    },
}

impl TimelineHoverPreviewLoadState {
    fn matches_visual_target(self, visual_target: TimelineHoverVisualTarget) -> bool {
        matches!(
            self,
            Self::NetworkOpening { target } if target == visual_target.target()
        )
    }
}

/// Результат обновления preview state; нужен для tests/diagnostics без bool-смешения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineHoverPreviewUpdateOutcome {
    /// Remote source ещё открывается; loading хранится только в preview state.
    Loading,

    /// Preview готов и заменил предыдущий ready frame.
    Ready,

    /// Approximate keyframe готов и заменил пустой/loading preview.
    ApproximateReady,

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
    /// Возвращает compact snapshot без раскрытия renderable frame/lease.
    pub(crate) fn diagnostics_snapshot(&self) -> TimelineHoverPreviewRenderDiagnosticsSnapshot {
        TimelineHoverPreviewRenderDiagnosticsSnapshot {
            ready: self.ready.is_some(),
            loading_preview_only: self.loading.is_some(),
        }
    }

    /// Полностью очищает visual preview borrow/render state.
    pub(crate) fn clear(&mut self) {
        self.ready = None;
        self.loading = None;
    }

    /// Обновляет preview из shared prepared entry borrow-а.
    pub(crate) fn update_from_borrow(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        borrow_outcome: PlayerTimelineHoverPrepareBorrowOutcome,
        approximate_borrow: Option<TimelineHoverApproximatePreviewBorrow>,
        load_state: TimelineHoverPreviewLoadState,
        materializer: Option<&dyn WgpuFrameTextureViewMaterializer>,
    ) -> TimelineHoverPreviewUpdateOutcome {
        let borrowed_frame = match borrow_outcome {
            PlayerTimelineHoverPrepareBorrowOutcome::Borrowed(borrowed_frame) => borrowed_frame,
            PlayerTimelineHoverPrepareBorrowOutcome::Miss(_reason) => {
                if let Some(approximate_borrow) = approximate_borrow {
                    let Some(materializer) = materializer else {
                        self.clear();
                        return TimelineHoverPreviewUpdateOutcome::MissingMaterializer;
                    };

                    return self.materialize_approximate_frame(
                        visual_target,
                        approximate_borrow,
                        materializer,
                    );
                }

                if load_state.matches_visual_target(visual_target) {
                    self.show_loading(visual_target);
                    return TimelineHoverPreviewUpdateOutcome::Loading;
                }

                self.clear();
                return TimelineHoverPreviewUpdateOutcome::WorkingSetMiss;
            }
            PlayerTimelineHoverPrepareBorrowOutcome::TimingRejected(_rejection) => {
                self.clear();
                return TimelineHoverPreviewUpdateOutcome::TimingRejected;
            }
        };

        let Some(materializer) = materializer else {
            self.clear();
            return TimelineHoverPreviewUpdateOutcome::MissingMaterializer;
        };

        self.materialize_exact_frame(visual_target, borrowed_frame, materializer)
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
    fn materialize_exact_frame(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        borrowed_frame: PlayerTimelineHoverPreparedFrameBorrow,
        materializer: &dyn WgpuFrameTextureViewMaterializer,
    ) -> TimelineHoverPreviewUpdateOutcome {
        self.materialize_lease(
            visual_target,
            borrowed_frame.lease().clone(),
            TimelineHoverPreviewReadyFrameSource::ExactTargetOrAfter,
            materializer,
            TimelineHoverPreviewUpdateOutcome::Ready,
        )
    }

    /// Materialize approximate keyframe-а тем же shared helper-ом, что и exact.
    fn materialize_approximate_frame(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        approximate_borrow: TimelineHoverApproximatePreviewBorrow,
        materializer: &dyn WgpuFrameTextureViewMaterializer,
    ) -> TimelineHoverPreviewUpdateOutcome {
        tracing::trace!(
            actual_pts = ?approximate_borrow.actual_pts(),
            "Materializing approximate hover preview keyframe"
        );
        self.materialize_lease(
            visual_target,
            approximate_borrow.lease().clone(),
            TimelineHoverPreviewReadyFrameSource::ApproximateKeyframe {
                span: approximate_borrow.span(),
            },
            materializer,
            TimelineHoverPreviewUpdateOutcome::ApproximateReady,
        )
    }

    /// Общий materialization path для exact и approximate leases.
    fn materialize_lease(
        &mut self,
        visual_target: TimelineHoverVisualTarget,
        lease: VideoFrameLease,
        source: TimelineHoverPreviewReadyFrameSource,
        materializer: &dyn WgpuFrameTextureViewMaterializer,
        ready_outcome: TimelineHoverPreviewUpdateOutcome,
    ) -> TimelineHoverPreviewUpdateOutcome {
        let materialization = materialize_shared_video_frame(
            SharedVideoFrameMaterializationRequest::new(
                SharedVideoFrameLeaseRole::HoverPreview,
                lease,
            ),
            materializer,
        );

        match materialization.outcome {
            SharedVideoFrameMaterializationOutcome::Ready { materialized_frame } => {
                self.loading = None;
                self.ready = Some(TimelineHoverPreviewReadyFrame {
                    visual_target,
                    source,
                    renderable_frame: materialized_frame.into_renderable_present_frame(),
                });
                ready_outcome
            }
            SharedVideoFrameMaterializationOutcome::Busy { .. } => {
                if self.ready_matches_reuse_guard(
                    visual_target.target(),
                    TimelineHoverPreviewReadyReuseGuard::from_source(source),
                ) {
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

    /// Busy fallback допускается только для source-specific совместимого ready frame-а.
    fn ready_matches_reuse_guard(
        &self,
        target: TimelineHoverTarget,
        guard: TimelineHoverPreviewReadyReuseGuard,
    ) -> bool {
        self.ready
            .as_ref()
            .map(|ready| match (ready.source, guard) {
                (
                    TimelineHoverPreviewReadyFrameSource::ExactTargetOrAfter,
                    TimelineHoverPreviewReadyReuseGuard::ExactTarget,
                ) => ready.visual_target.target() == target,
                (
                    TimelineHoverPreviewReadyFrameSource::ApproximateKeyframe { span: ready_span },
                    TimelineHoverPreviewReadyReuseGuard::ApproximateSpan { span: guard_span },
                ) => ready_span.matches_approximate_preview_anchor(guard_span),
                _ => false,
            })
            .unwrap_or(false)
    }

    fn show_loading(&mut self, visual_target: TimelineHoverVisualTarget) {
        // Loading не должен показывать старый кадр как nearest/approximate preview.
        self.ready = None;
        self.loading = Some(visual_target);
    }

    #[cfg(test)]
    fn is_loading_for_target(&self, target: TimelineHoverTarget) -> bool {
        self.loading
            .is_some_and(|loading| loading.target() == target)
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
    use frame_server_core::{
        BackendRevision, FrameExactnessPolicy, PlaybackGeneration, ScrubGeneration,
        ScrubGenerationToken, ScrubTrackSelection, SourceRevision, TimelineHoverFrameBucket,
        TimelineHoverPrepareAdmissionMode, TimelineHoverPrepareAdmissionRequest,
        TimelineHoverPrepareFrameKey, TimelineHoverPrepareFrameLookupRequest,
        TimelineHoverPrepareLookupMissReason, TimelineHoverPrepareProviderBudget,
    };
    use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};
    use player_core::{PlayerTimelineHoverPrepareHandoff, PlayerTimelineHoverPrepareInsertOutcome};
    use render_wgpu_video::WgpuFrameTextureViewLookup;
    use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
    use video_frame_contract::VideoFrameContract;
    use video_present_core::{
        VideoFrameLease, VideoFrameLeaseConfig, VideoFrameRelease, VideoFrameReleaseOutcome,
        VideoFrameReleaseSink,
    };

    use super::*;

    fn visual_target(seconds: u64) -> TimelineHoverVisualTarget {
        TimelineHoverVisualTarget::new(
            TimelineHoverTarget::new(MediaTime::from_secs(seconds)),
            crate::ui::timeline::TimelineHoverPreviewPlacement::new(
                egui::pos2(50.0, 20.0),
                egui::Rect::from_min_size(egui::pos2(0.0, 10.0), egui::vec2(100.0, 12.0)),
            ),
        )
    }

    fn timestamp(millis: i64) -> TrackTimestamp {
        TrackTimestamp::new(
            TrackId::new(1),
            millis,
            TimeBase::new(1, 1_000).expect("valid millisecond timebase"),
        )
    }

    fn preview_span(target_millis: i64, decode_safe_start_millis: i64) -> DecodeDependencySpan {
        let target_context = crate::timeline_hover_prepare::TimelineHoverPrepareTargetContext::new(
            SourceRevision::new(1),
            BackendRevision::new(2),
            ScrubTrackSelection::video_only(TrackId::new(1)),
            ScrubGenerationToken::new(PlaybackGeneration::new(3), ScrubGeneration::new(4)),
            FrameExactnessPolicy::TargetOrAfter,
        );
        crate::timeline_hover_prepare::TimelineHoverPrepareTarget::new(
            target_context,
            timestamp(target_millis),
            TimelineHoverFrameBucket::new(target_millis),
            timestamp(decode_safe_start_millis),
            timestamp(target_millis + 100),
            0,
            crate::timeline_hover_prepare::TimelineHoverPreparePlaybackMode::ActivePlayback,
        )
        .expect("test target must build")
        .dependency_span()
        .expect("test target must be resolved")
    }

    fn decoded_frame(pts_millis: u64, resource_handle: u64) -> DecodedFrame {
        DecodedFrame {
            generation: 1,
            pts: Duration::from_millis(pts_millis),
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(resource_handle),
            diagnostics: VideoFrameDiagnostics::default(),
        }
    }

    fn lease(pts_millis: u64, resource_handle: u64) -> VideoFrameLease {
        VideoFrameLease::new(VideoFrameLeaseConfig::new(
            1,
            decoded_frame(pts_millis, resource_handle),
            Arc::new(NoopReleaseSink),
        ))
    }

    fn exact_borrow_for_handle(resource_handle: u64) -> PlayerTimelineHoverPrepareBorrowOutcome {
        let handoff = PlayerTimelineHoverPrepareHandoff::default();
        let prepared_key = TimelineHoverPrepareFrameKey::new(
            SourceRevision::new(1),
            ScrubTrackSelection::video_only(TrackId::new(1)),
            BackendRevision::new(2),
            ScrubGenerationToken::new(PlaybackGeneration::new(3), ScrubGeneration::new(4)),
            FrameExactnessPolicy::TargetOrAfter,
            TimelineHoverFrameBucket::new(12_000),
        );
        let admission = TimelineHoverPrepareAdmissionRequest::new(
            prepared_key,
            prepared_key,
            TimelineHoverPrepareAdmissionMode::NormalHover,
            TimelineHoverPrepareProviderBudget::SpareSlotAvailable,
        );
        let insert_outcome = handoff.insert_hover_prepared_frame(
            admission,
            lease(12_000, resource_handle),
            timestamp(12_000),
        );
        assert!(matches!(
            insert_outcome,
            PlayerTimelineHoverPrepareInsertOutcome::Inserted { .. }
        ));

        handoff.borrow_prepared_frame(TimelineHoverPrepareFrameLookupRequest::new(
            prepared_key,
            timestamp(12_000),
        ))
    }

    struct NoopReleaseSink;

    impl VideoFrameReleaseSink for NoopReleaseSink {
        fn release_frame(&self, _release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
            VideoFrameReleaseOutcome::NoOp
        }
    }

    struct RecordingBusyMaterializer {
        looked_up_handles: Arc<Mutex<Vec<u64>>>,
    }

    impl WgpuFrameTextureViewMaterializer for RecordingBusyMaterializer {
        fn try_texture_view_lookup(&self, frame: &DecodedFrame) -> WgpuFrameTextureViewLookup {
            self.looked_up_handles
                .lock()
                .expect("materializer log lock")
                .push(frame.resource_handle.0);
            WgpuFrameTextureViewLookup::Busy {
                texture_pool_lock_wait: Duration::ZERO,
            }
        }
    }

    #[test]
    fn network_opening_sets_preview_only_loading_state() {
        let mut state = TimelineHoverPreviewRenderState::default();
        let visual_target = visual_target(12);
        let borrow_outcome = PlayerTimelineHoverPrepareBorrowOutcome::Miss(
            TimelineHoverPrepareLookupMissReason::NoEntryForKey,
        );

        let outcome = state.update_from_borrow(
            visual_target,
            borrow_outcome,
            None,
            TimelineHoverPreviewLoadState::NetworkOpening {
                target: visual_target.target(),
            },
            None,
        );

        assert_eq!(outcome, TimelineHoverPreviewUpdateOutcome::Loading);
        assert!(state.is_loading_for_target(visual_target.target()));
    }

    #[test]
    fn network_opening_for_other_target_stays_working_set_miss() {
        let mut state = TimelineHoverPreviewRenderState::default();
        let current_visual_target = visual_target(12);
        let other_target = visual_target(13).target();
        let borrow_outcome = PlayerTimelineHoverPrepareBorrowOutcome::Miss(
            TimelineHoverPrepareLookupMissReason::NoEntryForKey,
        );

        let outcome = state.update_from_borrow(
            current_visual_target,
            borrow_outcome,
            None,
            TimelineHoverPreviewLoadState::NetworkOpening {
                target: other_target,
            },
            None,
        );

        assert_eq!(outcome, TimelineHoverPreviewUpdateOutcome::WorkingSetMiss);
        assert!(!state.is_loading_for_target(current_visual_target.target()));
    }

    #[test]
    fn miss_uses_approximate_borrow_and_exact_hit_takes_priority() {
        let mut state = TimelineHoverPreviewRenderState::default();
        let visual_target = visual_target(12);
        let span = preview_span(12_000, 9_400);
        let approximate_borrow =
            TimelineHoverApproximatePreviewBorrow::new(lease(9_400, 77), span, timestamp(9_400));
        let looked_up_handles = Arc::new(Mutex::new(Vec::new()));
        let materializer = RecordingBusyMaterializer {
            looked_up_handles: Arc::clone(&looked_up_handles),
        };

        let approximate_outcome = state.update_from_borrow(
            visual_target,
            PlayerTimelineHoverPrepareBorrowOutcome::Miss(
                TimelineHoverPrepareLookupMissReason::NoEntryForKey,
            ),
            Some(approximate_borrow.clone()),
            TimelineHoverPreviewLoadState::Idle,
            Some(&materializer),
        );

        assert_eq!(
            approximate_outcome,
            TimelineHoverPreviewUpdateOutcome::BusyEmpty
        );
        assert_eq!(
            looked_up_handles
                .lock()
                .expect("materializer log lock")
                .as_slice(),
            &[77],
            "borrow Miss must try to materialize the approximate keyframe"
        );

        let exact_outcome = state.update_from_borrow(
            visual_target,
            exact_borrow_for_handle(88),
            Some(approximate_borrow),
            TimelineHoverPreviewLoadState::Idle,
            Some(&materializer),
        );

        assert_eq!(exact_outcome, TimelineHoverPreviewUpdateOutcome::BusyEmpty);
        assert_eq!(
            looked_up_handles
                .lock()
                .expect("materializer log lock")
                .as_slice(),
            &[77, 88],
            "exact borrow must replace approximate as the materialization source"
        );
    }
}
