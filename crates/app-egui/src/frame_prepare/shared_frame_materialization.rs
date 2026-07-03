//! Общий helper materialization-а для shared `VideoFrameLease`.
//!
//! Модуль намеренно не выбирает playback/scrub priority и не трогает UI layout.
//! Он только связывает shared lease, renderer materializer lookup и typed app policy.

use std::time::{Duration, Instant};

use render_wgpu_video::{
    WgpuFrameTextureViewLookup, WgpuFrameTextureViewMaterializer, WgpuFrameTextureViews,
};
use video_core::DecodedFrame;
use video_present_core::VideoFrameLease;

use crate::state::RenderablePresentFrame;

use super::TextureViewLookupKind;

/// Источник shared lease-а на app boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SharedVideoFrameLeaseRole {
    /// Обычный playback frame, полученный из `PlayerWorker`.
    Playback,

    /// Временный visual override во время scrub transaction.
    ScrubOverride,
}

impl SharedVideoFrameLeaseRole {
    /// Все роли, которые этот helper должен принимать без playback-specific assumptions.
    const ALL: [Self; 2] = [Self::Playback, Self::ScrubOverride];

    /// Возвращает короткую диагностическую метку роли lease-а.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Playback => "playback",
            Self::ScrubOverride => "scrub_override",
        }
    }

    /// Защищает future edits от случайного удаления поддерживаемой роли.
    fn is_supported(self) -> bool {
        Self::ALL.contains(&self)
    }
}

/// Запрос на materialization уже выбранного shared lease-а.
pub(super) struct SharedVideoFrameMaterializationRequest {
    /// Какая app workflow ветка принесла lease.
    role: SharedVideoFrameLeaseRole,

    /// Shared RAII lease, владеющий release/submitted semantics.
    lease: VideoFrameLease,
}

impl SharedVideoFrameMaterializationRequest {
    /// Собирает запрос без привязки к playback acquisition type.
    pub(super) fn new(role: SharedVideoFrameLeaseRole, lease: VideoFrameLease) -> Self {
        debug_assert!(role.is_supported());
        Self { role, lease }
    }
}

/// Timing-и, которые helper измеряет внутри общей materialization discipline.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct SharedVideoFrameMaterializationTimings {
    /// Сколько занял renderer-specific lookup texture views.
    pub(super) texture_view_lookup: Duration,

    /// Сколько заняла запись lookup diagnostics в owner sink.
    pub(super) resource_lookup_report: Duration,
}

/// Texture-view lookup без привязки тестов к реальным WGPU objects.
pub(super) enum SharedTextureViewLookup<TextureViews> {
    /// Renderer materializer вернул готовый payload views.
    Ready {
        /// Views payload: в production это WGPU views, в unit tests может быть fake.
        views: TextureViews,

        /// Время ожидания backend texture/resource pool lock.
        texture_pool_lock_wait: Duration,
    },

    /// Backend pool занят; app-level policy решает, можно ли показать previous frame.
    Busy {
        /// Время non-blocking lookup попытки.
        texture_pool_lock_wait: Duration,
    },

    /// Resource отсутствует у backend-а.
    Missing {
        /// Время lookup попытки.
        texture_pool_lock_wait: Duration,
    },

    /// Materializer не поддерживает такой resource kind.
    Unsupported {
        /// Время lookup попытки.
        texture_pool_lock_wait: Duration,
    },

    /// Fatal/error lookup state на renderer boundary.
    Error {
        /// Время lookup попытки.
        texture_pool_lock_wait: Duration,
    },
}

impl<TextureViews> SharedTextureViewLookup<TextureViews> {
    /// Возвращает app-level вид lookup-а без GPU payload.
    const fn kind(&self) -> TextureViewLookupKind {
        match self {
            Self::Ready { .. } => TextureViewLookupKind::Ready,
            Self::Busy { .. } => TextureViewLookupKind::Busy,
            Self::Missing { .. } => TextureViewLookupKind::Missing,
            Self::Unsupported { .. } => TextureViewLookupKind::Unsupported,
            Self::Error { .. } => TextureViewLookupKind::Error,
        }
    }

    /// Возвращает lock wait sample независимо от lookup result.
    const fn texture_pool_lock_wait(&self) -> Duration {
        match self {
            Self::Ready {
                texture_pool_lock_wait,
                ..
            }
            | Self::Busy {
                texture_pool_lock_wait,
            }
            | Self::Missing {
                texture_pool_lock_wait,
            }
            | Self::Unsupported {
                texture_pool_lock_wait,
            }
            | Self::Error {
                texture_pool_lock_wait,
            } => *texture_pool_lock_wait,
        }
    }

    /// Возвращает, был ли lookup typed Busy.
    const fn lookup_was_busy(&self) -> bool {
        matches!(self, Self::Busy { .. })
    }
}

impl From<WgpuFrameTextureViewLookup> for SharedTextureViewLookup<WgpuFrameTextureViews> {
    fn from(lookup: WgpuFrameTextureViewLookup) -> Self {
        match lookup {
            WgpuFrameTextureViewLookup::Ready {
                views,
                texture_pool_lock_wait,
            } => Self::Ready {
                views,
                texture_pool_lock_wait,
            },
            WgpuFrameTextureViewLookup::Busy {
                texture_pool_lock_wait,
            } => Self::Busy {
                texture_pool_lock_wait,
            },
            WgpuFrameTextureViewLookup::Missing {
                texture_pool_lock_wait,
            } => Self::Missing {
                texture_pool_lock_wait,
            },
            WgpuFrameTextureViewLookup::Unsupported {
                texture_pool_lock_wait,
                ..
            } => Self::Unsupported {
                texture_pool_lock_wait,
            },
            WgpuFrameTextureViewLookup::Error {
                texture_pool_lock_wait,
            } => Self::Error {
                texture_pool_lock_wait,
            },
        }
    }
}

/// Уже materialized shared frame: lease и views остаются одной ownership единицей.
pub(super) struct MaterializedSharedVideoFrame<TextureViews> {
    /// Lease удерживает backend resource до renderer submission/drop.
    present_frame: VideoFrameLease,

    /// Texture views, полученные для этого lease-а.
    texture_views: TextureViews,
}

impl MaterializedSharedVideoFrame<WgpuFrameTextureViews> {
    /// Конвертирует generic helper output в текущий app renderable frame.
    pub(super) fn into_renderable_present_frame(self) -> RenderablePresentFrame {
        RenderablePresentFrame::new(self.present_frame, self.texture_views)
    }
}

/// Итог materialization-а с сохранённой typed lookup state.
pub(super) enum SharedVideoFrameMaterializationOutcome<TextureViews> {
    /// Текущий shared lease получил renderable views.
    Ready {
        /// Lease + views, которые можно передать renderer boundary.
        materialized_frame: MaterializedSharedVideoFrame<TextureViews>,
    },

    /// Texture pool busy; lease сохраняется до решения caller-а о fallback.
    Busy {
        /// Lease, для которого lookup был Busy.
        present_frame: VideoFrameLease,
    },

    /// Resource отсутствует; caller должен report missing boundary error.
    Missing {
        /// Lease, по которому строится typed render error.
        present_frame: VideoFrameLease,
    },

    /// Resource kind неподдержан текущим materializer-ом.
    Unsupported {
        /// Lease, по которому строится typed render error.
        present_frame: VideoFrameLease,
    },

    /// Fatal/error lookup state на render boundary.
    Error {
        /// Lease, по которому строится typed render error.
        present_frame: VideoFrameLease,
    },
}

impl<TextureViews> SharedVideoFrameMaterializationOutcome<TextureViews> {
    /// Возвращает lookup kind без раскрытия texture payload.
    pub(super) const fn lookup_kind(&self) -> TextureViewLookupKind {
        match self {
            Self::Ready { .. } => TextureViewLookupKind::Ready,
            Self::Busy { .. } => TextureViewLookupKind::Busy,
            Self::Missing { .. } => TextureViewLookupKind::Missing,
            Self::Unsupported { .. } => TextureViewLookupKind::Unsupported,
            Self::Error { .. } => TextureViewLookupKind::Error,
        }
    }
}

/// Result helper-а: typed outcome плюс измеренные подстадии lookup-а.
pub(super) struct SharedVideoFrameMaterialization<TextureViews> {
    /// Typed результат lookup/materialization-а.
    pub(super) outcome: SharedVideoFrameMaterializationOutcome<TextureViews>,

    /// Подробные timing-и для существующей frame_prepare diagnostics.
    pub(super) timings: SharedVideoFrameMaterializationTimings,
}

/// Materialize shared lease через production WGPU materializer.
pub(super) fn materialize_shared_video_frame(
    request: SharedVideoFrameMaterializationRequest,
    materializer: &dyn WgpuFrameTextureViewMaterializer,
) -> SharedVideoFrameMaterialization<WgpuFrameTextureViews> {
    materialize_shared_video_frame_with_lookup(request, |decoded_frame| {
        materializer.try_texture_view_lookup(decoded_frame).into()
    })
}

/// Общая реализация, тестируемая fake lookup payload-ом без реальных WGPU handles.
fn materialize_shared_video_frame_with_lookup<TextureViews>(
    request: SharedVideoFrameMaterializationRequest,
    lookup_texture_views: impl FnOnce(&DecodedFrame) -> SharedTextureViewLookup<TextureViews>,
) -> SharedVideoFrameMaterialization<TextureViews> {
    let role = request.role;
    let present_frame = request.lease;

    tracing::trace!(
        lease_role = role.as_str(),
        render_generation = present_frame.render_generation(),
        resource_handle = present_frame.resource_handle().0,
        "Materializing shared video frame lease"
    );

    let lookup_started_at = Instant::now();
    let texture_view_lookup = lookup_texture_views(present_frame.decoded_frame());
    let texture_view_lookup_duration = lookup_started_at.elapsed();

    let report_started_at = Instant::now();
    present_frame.report_resource_lookup_sample(
        texture_view_lookup.texture_pool_lock_wait(),
        texture_view_lookup.lookup_was_busy(),
    );
    let resource_lookup_report_duration = report_started_at.elapsed();

    let lookup_kind = texture_view_lookup.kind();
    let outcome = match texture_view_lookup {
        SharedTextureViewLookup::Ready { views, .. } => {
            SharedVideoFrameMaterializationOutcome::Ready {
                materialized_frame: MaterializedSharedVideoFrame {
                    present_frame,
                    texture_views: views,
                },
            }
        }
        SharedTextureViewLookup::Busy { .. } => {
            SharedVideoFrameMaterializationOutcome::Busy { present_frame }
        }
        SharedTextureViewLookup::Missing { .. } => {
            SharedVideoFrameMaterializationOutcome::Missing { present_frame }
        }
        SharedTextureViewLookup::Unsupported { .. } => {
            SharedVideoFrameMaterializationOutcome::Unsupported { present_frame }
        }
        SharedTextureViewLookup::Error { .. } => {
            SharedVideoFrameMaterializationOutcome::Error { present_frame }
        }
    };

    debug_assert_eq!(outcome.lookup_kind(), lookup_kind);

    SharedVideoFrameMaterialization {
        outcome,
        timings: SharedVideoFrameMaterializationTimings {
            texture_view_lookup: texture_view_lookup_duration,
            resource_lookup_report: resource_lookup_report_duration,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
    use video_core::{FrameResourceHandle, VideoFrameDiagnostics};
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
    use video_present_core::{
        VideoFrameLeaseConfig, VideoFrameLeaseDiagnosticsSink, VideoFrameRelease,
        VideoFrameReleaseOutcome, VideoFrameReleaseSink, VideoFrameResourceLookupSample,
    };

    use super::super::{
        VideoFrameTexturePreparationAction, video_frame_texture_preparation_action,
    };

    /// Fake texture views, чтобы Ready-path проверял helper без WGPU constructors.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeTextureViews {
        /// Наблюдаемый id, подтверждающий перенос payload-а в output.
        id: u64,
    }

    /// Release sink, который считает drop lease-а без backend side effects.
    #[derive(Default)]
    struct ReleaseCounter {
        /// Сколько releases получил sink.
        releases: Mutex<Vec<VideoFrameRelease>>,
    }

    impl VideoFrameReleaseSink for ReleaseCounter {
        fn release_frame(&self, release: VideoFrameRelease) -> VideoFrameReleaseOutcome {
            self.releases
                .lock()
                .expect("release counter lock must stay healthy")
                .push(release);
            VideoFrameReleaseOutcome::Accepted
        }
    }

    /// Diagnostics sink для проверки, что helper репортит lookup sample один раз.
    #[derive(Default)]
    struct LookupDiagnostics {
        /// Все samples, полученные от lease-а.
        samples: Mutex<Vec<VideoFrameResourceLookupSample>>,
    }

    impl VideoFrameLeaseDiagnosticsSink for LookupDiagnostics {
        fn report_resource_lookup_sample(&self, sample: VideoFrameResourceLookupSample) {
            self.samples
                .lock()
                .expect("lookup diagnostics lock must stay healthy")
                .push(sample);
        }
    }

    /// Собирает минимальный decoded frame с renderer-neutral DMA-BUF contract.
    fn decoded_frame_for_tests(resource_handle: u64) -> DecodedFrame {
        DecodedFrame {
            generation: 3,
            pts: Duration::from_millis(125),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
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

    /// Собирает shared lease с release и lookup diagnostics sinks.
    fn lease_for_tests(
        resource_handle: u64,
        release_counter: Arc<ReleaseCounter>,
        diagnostics: Arc<LookupDiagnostics>,
    ) -> VideoFrameLease {
        let config = VideoFrameLeaseConfig::new(
            11,
            decoded_frame_for_tests(resource_handle),
            release_counter,
        )
        .with_diagnostics_sink(diagnostics);

        VideoFrameLease::new(config)
    }

    /// Проверяет Ready materialization для выбранной app роли.
    fn assert_ready_materializes_for_role(role: SharedVideoFrameLeaseRole) {
        let release_counter = Arc::new(ReleaseCounter::default());
        let diagnostics = Arc::new(LookupDiagnostics::default());
        let lease = lease_for_tests(42, release_counter.clone(), diagnostics.clone());

        let materialization = materialize_shared_video_frame_with_lookup(
            SharedVideoFrameMaterializationRequest::new(role, lease),
            |decoded_frame| {
                assert_eq!(decoded_frame.resource_handle, FrameResourceHandle(42));
                SharedTextureViewLookup::Ready {
                    views: FakeTextureViews { id: 9001 },
                    texture_pool_lock_wait: Duration::from_micros(7),
                }
            },
        );

        match materialization.outcome {
            SharedVideoFrameMaterializationOutcome::Ready { materialized_frame } => {
                assert_eq!(
                    materialized_frame.present_frame.resource_handle(),
                    FrameResourceHandle(42)
                );
                assert_eq!(
                    materialized_frame.texture_views,
                    FakeTextureViews { id: 9001 }
                );
            }
            _ => panic!("Ready lookup must produce materialized frame"),
        }

        let samples = diagnostics
            .samples
            .lock()
            .expect("lookup diagnostics lock must stay healthy");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].wait(), Duration::from_micros(7));
        assert!(!samples[0].lookup_was_busy());
        drop(samples);

        assert_eq!(
            release_counter
                .releases
                .lock()
                .expect("release counter lock must stay healthy")
                .len(),
            1
        );
    }

    /// Playback Ready использует тот же shared helper, что будущие override paths.
    #[test]
    fn playback_lease_ready_materializes() {
        assert_ready_materializes_for_role(SharedVideoFrameLeaseRole::Playback);
    }

    /// Scrub override Ready не требует playback-specific acquisition type.
    #[test]
    fn scrub_override_lease_ready_materializes() {
        assert_ready_materializes_for_role(SharedVideoFrameLeaseRole::ScrubOverride);
    }

    /// Busy остаётся typed Busy и просит previous-frame reuse только если caller дал cache.
    #[test]
    fn busy_lookup_reuses_previous_frame_only_when_allowed() {
        let release_counter = Arc::new(ReleaseCounter::default());
        let diagnostics = Arc::new(LookupDiagnostics::default());
        let lease = lease_for_tests(43, release_counter.clone(), diagnostics.clone());

        let materialization = materialize_shared_video_frame_with_lookup(
            SharedVideoFrameMaterializationRequest::new(SharedVideoFrameLeaseRole::Playback, lease),
            |_decoded_frame| SharedTextureViewLookup::<FakeTextureViews>::Busy {
                texture_pool_lock_wait: Duration::from_micros(11),
            },
        );

        assert_eq!(
            video_frame_texture_preparation_action(
                materialization.outcome.lookup_kind(),
                true,
                false
            ),
            VideoFrameTexturePreparationAction::ReusePreviousFrameForTextureBusy {
                record_repeated_frame: true
            }
        );
        assert_eq!(
            video_frame_texture_preparation_action(
                materialization.outcome.lookup_kind(),
                false,
                false
            ),
            VideoFrameTexturePreparationAction::SkipVideoFrameForTextureBusy
        );

        let samples = diagnostics
            .samples
            .lock()
            .expect("lookup diagnostics lock must stay healthy");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].wait(), Duration::from_micros(11));
        assert!(samples[0].lookup_was_busy());
        drop(samples);

        drop(materialization);
        assert_eq!(
            release_counter
                .releases
                .lock()
                .expect("release counter lock must stay healthy")
                .len(),
            1
        );
    }

    /// Missing/Unsupported/Error сохраняют разные actions и требуют cache clear policy.
    #[test]
    fn non_ready_lookup_states_keep_distinct_clear_cache_actions() {
        let cases: [(
            SharedTextureViewLookup<FakeTextureViews>,
            TextureViewLookupKind,
            VideoFrameTexturePreparationAction,
        ); 3] = [
            (
                SharedTextureViewLookup::Missing {
                    texture_pool_lock_wait: Duration::ZERO,
                },
                TextureViewLookupKind::Missing,
                VideoFrameTexturePreparationAction::ReportMissingRenderResources,
            ),
            (
                SharedTextureViewLookup::Unsupported {
                    texture_pool_lock_wait: Duration::ZERO,
                },
                TextureViewLookupKind::Unsupported,
                VideoFrameTexturePreparationAction::ReportUnsupportedRenderResource,
            ),
            (
                SharedTextureViewLookup::Error {
                    texture_pool_lock_wait: Duration::ZERO,
                },
                TextureViewLookupKind::Error,
                VideoFrameTexturePreparationAction::ReportRenderResourceLookupFailure,
            ),
        ];

        for (index, (lookup, expected_kind, expected_action)) in cases.into_iter().enumerate() {
            let release_counter = Arc::new(ReleaseCounter::default());
            let diagnostics = Arc::new(LookupDiagnostics::default());
            let lease = lease_for_tests(100 + index as u64, release_counter.clone(), diagnostics);

            let materialization = materialize_shared_video_frame_with_lookup(
                SharedVideoFrameMaterializationRequest::new(
                    SharedVideoFrameLeaseRole::Playback,
                    lease,
                ),
                |_decoded_frame| lookup,
            );

            assert_eq!(materialization.outcome.lookup_kind(), expected_kind);

            let preparation_action =
                video_frame_texture_preparation_action(expected_kind, false, false);
            assert_eq!(preparation_action, expected_action);
            assert!(preparation_action.clears_cached_renderable_frame());

            drop(materialization);
            assert_eq!(
                release_counter
                    .releases
                    .lock()
                    .expect("release counter lock must stay healthy")
                    .len(),
                1
            );
        }
    }
}
