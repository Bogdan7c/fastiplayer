//! Склад кадров рендера: `VideoFrameImport` для [`Gles2Renderer`] и возврат картинки ключа на
//! видеослой.
//!
//! Склад не отдельный объект, а часть рендера: подготовка кадра — это загрузка в текстуры или
//! импорт DMA-BUF в контексте рендера, поэтому ему нужны и контекст, и видеослой. Чистая логика
//! (подготовка импорта, таблицы исходов, учёт «что на слое») — в [`crate::video_import`].

use std::sync::Arc;
use std::time::Duration;

use render_shell_api::{
    FrameImportError, FrameImportStats, FrameImportUnsupportedReason, FrameMaterialization,
    FrameMaterializationOutcome, PendingVideoImport, PreparedVideoImport, ReleasedFrameCount,
    RenderFrameFailure, RenderableFrameKey, VideoFrameImport, VideoImportHandoff,
    VideoImportInstallError, VideoImportPath, VideoImportPlan, VideoImportPrepareError,
    VideoImportReleaseError,
};
use video_backend_api::StartedVideoBackend;
use video_present_core::VideoFrameLease;

use super::Gles2Renderer;
use super::frame::{OpenedVideoKey, renderer_shut_down};
use crate::frame_release::Gles2FrameReleaseBinding;
use crate::video_import::{
    ActiveImport, GLES2_FAMILY, Gles2ImportHandoff, Gles2PreparedFrame, ImportEnvironment,
    LayerShow, LayerShowOutcome, PendingGles2Import, PendingOrigin, show_frame_on_layer,
};
use crate::{dma_buf_import_capacity_for, frame_import_stats, renderable_frame};

/// Сколько ждём fence работы с кольцом слотов, если для возврата картинки ключа не нашлось
/// свободного слота: загрузки между кадрами показа ещё не закрыты fence-ом.
const SLOT_RETIREMENT_WAIT: Duration = Duration::from_millis(50);

impl Gles2Renderer {
    /// Окружение подготовки импорта: паспорт, текущая привязка освобождения, уровень zero-copy.
    pub(super) fn import_environment(&self) -> ImportEnvironment {
        let binding: Gles2FrameReleaseBinding = self.release.binding().clone();
        ImportEnvironment::new(self.instance_id, binding, self.hardware)
    }

    /// Гарантирует, что на видеослое сейчас картинка именно этого ключа.
    ///
    /// Обычный случай — ключ только что подготовлен `materialize` и кадр на слое. Если
    /// приложение рисует ключ, который перестал быть текущим (замороженный кадр, повтор прошлого
    /// кадра при `Busy`, scrub), картинка возвращается по рецепту ключа: ключ держит аренду, а
    /// значит ресурсы кадра у провайдера живы. Не вышло — отказ кадра: чужую или устаревшую
    /// картинку молча не рисуем никогда.
    pub(super) fn ensure_key_on_layer(
        &mut self,
        opened: &OpenedVideoKey<'_>,
    ) -> Result<(), RenderFrameFailure> {
        let prepared = opened.prepared;
        if self
            .shown
            .is_on_layer(prepared.epoch(), prepared.layer_key())
        {
            return Ok(());
        }
        let frame = opened.key.decoded_frame();
        let mut show = self.show_prepared_frame(prepared, frame)?;
        if matches!(show.outcome, LayerShowOutcome::Busy) {
            // Все слоты кольца заняты загрузками без fence: закрываем их fence-ом и пробуем ещё
            // раз (один раз — дальше честный отказ).
            self.retire_uploads_before_reshow();
            show = self.show_prepared_frame(prepared, frame)?;
        }
        match show.outcome {
            LayerShowOutcome::Shown => {
                self.counters.video_frames_reshown += 1;
                Ok(())
            }
            other => {
                self.shown.forget();
                Err(RenderFrameFailure::new(format!(
                    "the video frame is not on the video layer and cannot be shown again: {other:?}"
                )))
            }
        }
    }

    /// Показывает кадр рецепта на слое и отмечает его текущим.
    fn show_prepared_frame(
        &mut self,
        prepared: &Gles2PreparedFrame,
        frame: &video_core::DecodedFrame,
    ) -> Result<LayerShow, RenderFrameFailure> {
        let Some(layers) = self.layers.as_mut() else {
            return Err(renderer_shut_down());
        };
        self.shown
            .prepare_layer_for(prepared.epoch(), &mut layers.video);
        let show = show_frame_on_layer(&mut layers.video, &self.context, prepared.source(), frame);
        if matches!(show.outcome, LayerShowOutcome::Shown) {
            self.shown
                .mark_shown(prepared.epoch(), prepared.layer_key());
        }
        Ok(show)
    }

    /// Закрывает fence-ом загрузки, сделанные после последнего кадра показа, и ждёт его.
    fn retire_uploads_before_reshow(&mut self) {
        let fence = match self.context.insert_fence() {
            Ok(fence) => Arc::new(fence),
            Err(error) => {
                log::warn!("GLES2: fence загрузок не поставлен: {error}");
                return;
            }
        };
        if let Some(layers) = self.layers.as_mut() {
            layers.video.retire_submitted_work(Arc::clone(&fence));
        }
        if let Err(error) = fence.wait(SLOT_RETIREMENT_WAIT) {
            log::warn!("GLES2: ожидание fence загрузок отказало: {error}");
        }
    }
}

impl VideoFrameImport for Gles2Renderer {
    fn prepare_video_import(
        &mut self,
        backend: StartedVideoBackend,
        plan: VideoImportPlan,
    ) -> Result<PreparedVideoImport, VideoImportPrepareError> {
        self.import_environment().prepare(backend, plan)
    }

    fn install_video_import(
        &mut self,
        pending: PendingVideoImport,
    ) -> Result<(), VideoImportInstallError> {
        let PendingGles2Import {
            plan,
            source,
            origin,
        } = pending.into_state::<PendingGles2Import>(self.instance_id)?;
        let Some(layers) = self.layers.as_mut() else {
            return Err(VideoImportInstallError::SubmissionBindingFailed {
                message: "GLES2 renderer is shut down".to_owned(),
            });
        };
        match origin {
            PendingOrigin::RecreatedRenderer(parts) => {
                // Обёртка декодера, которую держит плеер, ссылается на привязку старого рендера:
                // новый рендер продолжает с ней (см. `video_import::handoff`).
                self.release.adopt(parts);
            }
            PendingOrigin::FreshDecoder { wrapped_with } => {
                // До любых изменений: обёртка должна быть сделана НАШЕЙ привязкой освобождения.
                if !wrapped_with.is_same_binding(self.release.binding()) {
                    return Err(VideoImportInstallError::SubmissionBindingFailed {
                        message: "the decoder wrapper was made by another release binding"
                            .to_owned(),
                    });
                }
            }
        }
        self.import_epoch = self.import_epoch.next();
        // Ключи кадров нового декодера начинаются заново, а текстуры кэша держат память
        // поверхностей прошлого: забываем загруженные кадры и сбрасываем импорты.
        layers.video.forget_uploaded_frames();
        if let Err(error) = layers.video.purge_dma_buf_imports(&self.context) {
            log::warn!("GLES2: кэш импортов DMA-BUF не сброшен при смене декодера: {error}");
        }
        if plan.path == VideoImportPath::HardwareZeroCopy
            && let Err(error) = layers.video.set_dma_buf_import_capacity(
                &self.context,
                dma_buf_import_capacity_for(plan.decoder_surface_pool.get()),
            )
        {
            log::warn!("GLES2: ёмкость кэша импортов DMA-BUF не задана: {error}");
        }
        self.shown.forget();
        self.active_import = Some(ActiveImport::new(plan, source, self.import_epoch));
        Ok(())
    }

    fn active_video_import_path(&self) -> Option<VideoImportPath> {
        self.active_import.as_ref().map(|active| active.plan().path)
    }

    fn materialize(&mut self, lease: &VideoFrameLease) -> FrameMaterialization {
        let frame = lease.decoded_frame();
        // Порядок проверок контракта: контракт объявлен → декодер подключён → провайдер.
        if !self
            .capabilities
            .supports_frame_contract(frame.frame_contract)
        {
            return FrameMaterialization::without_pool_wait(
                FrameMaterializationOutcome::Unsupported(
                    FrameImportUnsupportedReason::ContractNotDeclared {
                        frame_contract: frame.frame_contract,
                    },
                ),
            );
        }
        let Some(active) = self.active_import.clone() else {
            return FrameMaterialization::without_pool_wait(
                FrameMaterializationOutcome::NoVideoBackendBound,
            );
        };
        // Кадр, который `render_frame` не сможет описать правилами render-core, ловится здесь, до
        // показа (как у wgpu): иначе слой получил бы картинку, которую нельзя нарисовать.
        if let Err(error) = renderable_frame(frame) {
            log::error!(
                "GLES2: кадр не годится для рисования: {error} (format {})",
                frame.format()
            );
            return FrameMaterialization::without_pool_wait(
                FrameMaterializationOutcome::Unsupported(
                    FrameImportUnsupportedReason::LayoutNotHandledByActiveImport,
                ),
            );
        }
        let prepared = Gles2PreparedFrame::new(active.epoch(), frame, active.source().clone());
        let show = match self.show_prepared_frame(&prepared, frame) {
            Ok(show) => show,
            Err(failure) => {
                return FrameMaterialization::without_pool_wait(
                    FrameMaterializationOutcome::Error(FrameImportError::ImportFailed {
                        message: failure.message,
                    }),
                );
            }
        };
        let outcome =
            match show.outcome {
                LayerShowOutcome::Shown => FrameMaterializationOutcome::Ready(
                    RenderableFrameKey::new(self.instance_id, lease.clone(), prepared),
                ),
                LayerShowOutcome::Busy => FrameMaterializationOutcome::Busy,
                LayerShowOutcome::Missing => FrameMaterializationOutcome::Missing,
                LayerShowOutcome::Unsupported(reason) => {
                    FrameMaterializationOutcome::Unsupported(reason)
                }
                LayerShowOutcome::Failed(error) => {
                    // Фатальный провайдер отказывает ДО обращения к слою: слой не тронут. Отказ
                    // загрузки или импорта мог оставить его в неопределённом состоянии (импорт
                    // вытесняет записи кэша), поэтому прежний кадр придётся показать заново.
                    if matches!(error, FrameImportError::ImportFailed { .. }) {
                        self.shown.forget();
                    }
                    FrameMaterializationOutcome::Error(error)
                }
            };
        FrameMaterialization {
            outcome,
            resource_pool_lock_wait: show.resource_pool_lock_wait,
        }
    }

    fn import_stats(&self) -> Option<FrameImportStats> {
        let active = self.active_import.as_ref()?;
        // Счётчики кэша импорта есть только у аппаратного пути (как у wgpu).
        if active.plan().path != VideoImportPath::HardwareZeroCopy {
            return None;
        }
        let layers = self.layers.as_ref()?;
        Some(frame_import_stats(&layers.video.dma_buf_import_snapshot()))
    }

    fn export_video_import_for_recreation(&self) -> Option<VideoImportHandoff> {
        let active = self.active_import.as_ref()?;
        Some(VideoImportHandoff::new(
            GLES2_FAMILY,
            active.plan().path,
            Gles2ImportHandoff {
                plan: active.plan(),
                source: active.source().clone(),
                parts: self.release.parts_for_successor(),
            },
        ))
    }

    fn prepare_recreated_video_import(
        &mut self,
        handoff: VideoImportHandoff,
    ) -> Result<PendingVideoImport, VideoImportPrepareError> {
        let path = handoff.path();
        let Gles2ImportHandoff {
            plan,
            source,
            parts,
        } = handoff.into_state::<Gles2ImportHandoff>(GLES2_FAMILY)?;
        // Драйвер нового контекста мог потерять zero-copy: тогда честный отказ подготовки.
        self.import_environment().ensure_path_supported(path)?;
        Ok(PendingVideoImport::new(
            self.instance_id,
            path,
            PendingGles2Import {
                plan,
                source,
                origin: PendingOrigin::RecreatedRenderer(parts),
            },
        ))
    }

    fn release_after_device_lost(&mut self) -> Result<ReleasedFrameCount, VideoImportReleaseError> {
        Ok(ReleasedFrameCount::new(
            self.release.binding().release_after_context_lost(),
        ))
    }
}
