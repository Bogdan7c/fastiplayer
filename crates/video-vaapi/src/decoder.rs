use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
#[cfg(test)]
use codec_core::{BitDepth, ChromaSubsampling};
use codec_core::{VideoCodec, VideoColorMetadata, VideoDisplayOrientation};
#[cfg(test)]
use cros_codecs::decoder::DecodedDmaBufExportLayout;
#[cfg(test)]
use cros_codecs::libva::{VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV420_12};
use media_core::Packet;
use tracing::{debug, info, trace, warn};
use video_core::{
    DecodedFrame, DecodedPixelFormat, FrameResourceHandle, VideoDecoder,
    VideoDecoderActivityNotifier, VideoDecoderDiagnosticEvent, VideoDecoderDropReason,
    VideoFrameDiagnostics, VideoFrameTimingDiagnostics, VideoPrerollOutputFloor,
    VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult, VideoResourcePoolDiagnostics,
    VideoStreamDecodeConfig,
};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

#[cfg(test)]
use crate::codec_adapter::VaapiDecodedFormat;
use crate::codec_adapter::{
    VaapiAdapterDecodeError, VaapiCodecAdapter, VaapiCodecAdapterFactory, VaapiDecodedFrameHandle,
    VaapiDecoderEvent, VaapiPacketDecodeHints,
};
use crate::frame_pool::DmaFramePool;
use crate::resource_pool::FrameResourcePool;
use crate::shared_hardware_owner::{
    VaapiPlaybackHardwareReservation, VaapiSharedHardwareOwner, VaapiSharedHardwareOwnerContext,
};

mod config;
mod event_drain;
mod h264_recovery;
mod preroll;
mod suppressed_reclaim;
mod surface_contract;

pub use config::{
    DEFAULT_DECODER_READY_QUEUE_FRAMES, DEFAULT_DECODER_SURFACE_POOL_FRAMES,
    VaapiDecoderRuntimeConfig,
};
#[cfg(test)]
use config::{
    SUPPRESSED_RECLAIM_MARGIN_FRAMES, SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES,
    SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES, SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES,
    default_max_suppressed_reclaim_frames,
};
use event_drain::*;
use h264_recovery::H264DecodeRecovery;
use preroll::*;
use suppressed_reclaim::*;
use surface_contract::*;
/// Итог обработки packet-а для decoder thread-а.
pub(crate) enum VaapiDecodePacketOutcome {
    /// Packet принят adapter-ом; готовый frame может появиться сразу или позже.
    Accepted(Option<Box<DecodedFrame>>),

    /// Packet сохранён adapter-ом как pending и должен быть повторён после release/backpressure.
    OutputBackpressured,
}

/// Фатальная ошибка безопасного release path-а для VA surfaces.
///
/// Если forced sync/discard не смог дождаться surface completion, decoder нельзя
/// продолжать как будто handle безопасно освобождён: следующий reuse может
/// получить stale/busy surface от старого stream или lifecycle boundary.
#[derive(Debug)]
struct VaapiSurfaceLifecycleError {
    /// Человекочитаемая причина остановки decoder-а.
    detail: String,
}

/// Fatal ошибка доступа к poisoned zero-copy resource pool.
///
/// После poison содержимое пула нельзя считать согласованным: guard намеренно
/// не извлекается через `PoisonError::into_inner`, поэтому backend останавливает
/// текущий lifecycle вместо попытки продолжить работу с недостоверным state.
#[derive(Debug)]
struct VaapiResourcePoolPoisonError {
    /// Lifecycle operation, на которой обнаружена потеря инвариантов пула.
    operation: &'static str,
}

impl VaapiSurfaceLifecycleError {
    /// Создаёт ошибку lifecycle boundary с понятной причиной для лога/UI.
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for VaapiSurfaceLifecycleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for VaapiSurfaceLifecycleError {}

impl std::fmt::Display for VaapiResourcePoolPoisonError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "zero-copy resource pool mutex poisoned during {}",
            self.operation
        )
    }
}

impl std::error::Error for VaapiResourcePoolPoisonError {}

/// Захватывает resource-pool lock без controlled recovery poisoned state.
fn lock_resource_pool<'a>(
    resource_pool: &'a Mutex<FrameResourcePool>,
    operation: &'static str,
) -> Result<MutexGuard<'a, FrameResourcePool>> {
    resource_pool
        .lock()
        .map_err(|_| VaapiResourcePoolPoisonError { operation }.into())
}

/// Проверяет, что decode error требует остановить decoder thread.
pub(crate) fn is_fatal_decoder_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ZeroCopyContractViolation>().is_some()
        || error.downcast_ref::<VaapiSurfaceLifecycleError>().is_some()
        || error
            .downcast_ref::<VaapiResourcePoolPoisonError>()
            .is_some()
}

/// Забирает handles только из decoder-owned ready queue.
///
/// Renderer-owned frames намеренно не представлены в этой очереди: они уже
/// опубликованы наружу и освобождаются только через обычный release path.
fn drain_decoder_owned_ready_frame_handles(
    ready_queue: &mut VecDeque<DecodedFrame>,
) -> Vec<FrameResourceHandle> {
    ready_queue
        .drain(..)
        .map(|frame| frame.resource_handle)
        .collect()
}

/// VA-API hardware decoder с internal codec adapter shell.
///
/// Активный adapter владеет concrete cros-codecs decoder-ом, а этот слой владеет
/// internal VA surfaces, DMA-BUF resource pool и drain events внутри `decode()`.
pub struct VaapiVideoDecoder {
    /// VA display, через который создаются codec adapters при stream reconfigure.
    display: Rc<cros_codecs::libva::Display>,

    /// Активный codec adapter, который скрывает concrete cros-codecs decoder type.
    adapter: Box<dyn VaapiCodecAdapter>,

    /// Пул lightweight frame descriptors для выходных VA surfaces.
    frame_pool: DmaFramePool,

    /// Пул exported DMA-BUF resource descriptors для decoded surfaces.
    ///
    /// Arc<Mutex<>> потому что пул используется из decoder thread (DMA-BUF export)
    /// и из render thread (descriptor lookup / release).
    resource_pool: Arc<Mutex<FrameResourcePool>>,

    /// Shared VA owner boundary для playback branch reservation accounting.
    _shared_hardware_owner: VaapiSharedHardwareOwner,

    /// Playback reservation живёт столько же, сколько VA decoder.
    _playback_hardware_reservation: VaapiPlaybackHardwareReservation,

    /// Очередь готовых к отображению кадров.
    ///
    /// Кадры добавляются при обработке `FrameReady` event и возвращаются
    /// из `decode()` в порядке FIFO.
    ready_queue: VecDeque<DecodedFrame>,

    /// Bounded backend-local лимиты очередей и surface pool-а.
    runtime_config: VaapiDecoderRuntimeConfig,

    /// Bounded queue и accounting подавленных VA surfaces принадлежат отдельному state owner.
    suppressed_reclaim_state: SuppressedReclaimState,

    /// Handles кадров, которые сейчас удерживают decoded VA surface.
    ///
    /// Пока handle находится в этой map, VA surface не возвращается в frame pool
    /// и decoder не может перезаписать memory, которую может семплить renderer.
    zero_copy_guards: HashMap<u64, VaapiDecodedFrameHandle>,

    /// Был ли уже залогирован первый успешный zero-copy descriptor.
    zero_copy_success_logged: bool,

    /// Diagnostics events для player-core без зависимости от player-core.
    diagnostic_tx: Option<std::sync::mpsc::SyncSender<VideoDecoderDiagnosticEvent>>,

    /// Нейтральный notifier: decoder сообщает, что player-side wait может проснуться.
    activity_notifier: Option<VideoDecoderActivityNotifier>,

    /// Была ли уже залогирована проверенная P010 zero-copy boundary.
    p010_boundary_verified_logged: bool,

    /// Имя бэкенда для отображения в UI.
    backend_name: &'static str,

    /// Frame contract, выбранный capability layer и обязательный для export-а.
    expected_frame_contract: VideoFrameContract,

    /// Display orientation текущего stream-а; применяется renderer-ом, не decoder-ом.
    display_orientation: VideoDisplayOrientation,

    /// Active accurate-seek preroll output floor и его counters.
    preroll_output_floor: PrerollOutputFloorState,

    /// Последний pre-floor VA handle для EOF fallback promotion.
    preroll_fallback_candidate: Option<PrerollFallbackCandidate<VaapiDecodedFrameHandle>>,

    /// Не даёт подавать inter-frames в пустой после configure/flush/recovery H.264 DPB.
    h264_decode_recovery: H264DecodeRecovery,
}

impl VaapiVideoDecoder {
    /// Создаёт новый VA-API decoder с production adapter-ом по умолчанию.
    ///
    /// # Ошибки
    /// Возвращает ошибку если:
    /// - VA-API display недоступен,
    /// - не удалось создать production codec adapter,
    /// - не удалось создать GBM frame pool.
    pub fn new() -> Result<Self> {
        let resource_pool = Arc::new(Mutex::new(FrameResourcePool::new()));
        Self::new_with_pool(resource_pool, None, VaapiDecoderRuntimeConfig::default())
    }

    pub fn new_with_pool(
        resource_pool: Arc<Mutex<FrameResourcePool>>,
        diagnostic_tx: Option<std::sync::mpsc::SyncSender<VideoDecoderDiagnosticEvent>>,
        runtime_config: VaapiDecoderRuntimeConfig,
    ) -> Result<Self> {
        Self::new_with_pool_and_activity_notifier(
            resource_pool,
            diagnostic_tx,
            runtime_config,
            None,
        )
    }

    pub(crate) fn new_with_pool_and_activity_notifier(
        resource_pool: Arc<Mutex<FrameResourcePool>>,
        diagnostic_tx: Option<std::sync::mpsc::SyncSender<VideoDecoderDiagnosticEvent>>,
        runtime_config: VaapiDecoderRuntimeConfig,
        activity_notifier: Option<VideoDecoderActivityNotifier>,
    ) -> Result<Self> {
        let runtime_config = runtime_config.normalized();
        let reclaim_capacity = SuppressedReclaimCapacity::from_runtime_config(runtime_config);
        info!("Opening VA-API display");
        {
            let resource_pool = lock_resource_pool(&resource_pool, "decoder initialization")?;
            let reuse_contract = resource_pool.reuse_contract();
            info!(
                backend_contract = reuse_contract.backend_name,
                sample_only = reuse_contract.renderer_is_sample_only,
                waits_gpu_completion = reuse_contract.decoder_reuse_waits_for_gpu_completion,
                identity_is_surface_id = reuse_contract.import_identity_is_surface_id,
                dma_buf_identity_checked = reuse_contract.dma_buf_object_identity_checked,
                explicit_reuse_sync = reuse_contract.explicit_external_memory_reuse_sync,
                "Zero-copy resource lifecycle contract configured"
            );
        }

        // Открываем VA-API display. `Display::open()` возвращает `Option<Rc<Display>>`.
        // Если None — значит VA-API недоступна (нет драйвера, нет устройства).
        let display = cros_codecs::libva::Display::open()
            .ok_or_else(|| anyhow::anyhow!("Failed to open VA-API display: libva not available"))?;

        info!("Creating VA-API codec adapter");

        // Создаём production adapter через internal factory. Сейчас factory
        // выбирает VP9, а будущие codec adapters смогут заменить active adapter
        // без раскрытия cros-codecs generic типов наружу этого boundary.
        let adapter = VaapiCodecAdapterFactory::create_default_adapter(display.clone())?;
        let backend_name = adapter.backend_name();
        let adapter_codec = adapter.codec();

        info!("Creating internal VA frame pool");

        // Создаём пул выходных буферов. Декодер требует буферы до первого вызова decode().
        let frame_pool = DmaFramePool::new(
            INITIAL_WIDTH,
            INITIAL_HEIGHT,
            runtime_config.surface_pool_frames,
        )
        .map_err(|e| anyhow::anyhow!("Failed to create frame pool: {}", e))?;

        let shared_hardware_context = VaapiSharedHardwareOwnerContext::from_surface_accounting(
            runtime_config.surface_pool_frames,
        );
        let shared_hardware_owner = VaapiSharedHardwareOwner::new(shared_hardware_context);
        let playback_hardware_reservation = shared_hardware_owner
            .reserve_playback_branch()
            .map_err(|error| anyhow::anyhow!("Failed to reserve VAAPI playback branch: {error}"))?;

        info!(
            backend_name,
            codec = %adapter_codec,
            surface_pool_frames = runtime_config.surface_pool_frames,
            ready_queue_frames = runtime_config.ready_queue_frames,
            playback_reserved_surface_frames = playback_hardware_reservation.surface_frames().get(),
            max_suppressed_reclaim_frames = runtime_config.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots =
                reclaim_capacity.approximate_available_reclaim_slots(0),
            approximate_reserved_surface_headroom_frames =
                reclaim_capacity.approximate_reserved_surface_headroom_frames(),
            "VA-API decoder initialized successfully"
        );

        Ok(Self {
            display,
            adapter,
            frame_pool,
            resource_pool,
            _shared_hardware_owner: shared_hardware_owner,
            _playback_hardware_reservation: playback_hardware_reservation,
            ready_queue: VecDeque::new(),
            runtime_config,
            suppressed_reclaim_state: SuppressedReclaimState::new(runtime_config),
            zero_copy_guards: HashMap::new(),
            zero_copy_success_logged: false,
            diagnostic_tx,
            activity_notifier,
            p010_boundary_verified_logged: false,
            backend_name,
            expected_frame_contract: VideoFrameContract::dma_buf_nv12(
                DmaBufImageLayout::ComposedLayers,
            ),
            display_orientation: VideoDisplayOrientation::Identity,
            preroll_output_floor: PrerollOutputFloorState::default(),
            preroll_fallback_candidate: None,
            h264_decode_recovery: H264DecodeRecovery::default(),
        })
    }

    /// Освобождает decoder-owned frame, который не был отправлен renderer GPU work-у.
    ///
    /// Должен вызываться когда кадр больше не нужен (drop по A/V sync,
    /// замена present frame, очистка очереди и т.д.).
    /// Без этого resource pool исчерпается после bounded числа кадров.
    /// Освобождает lifecycle slot и возвращает VA surface после ack.
    ///
    /// Thread-safe: вызывается из decoder thread (через channel) или render thread.
    pub fn release_frame(&mut self, resource_handle: FrameResourceHandle) -> Result<()> {
        trace!(
            handle_id = resource_handle.0,
            "Releasing decoder-owned zero-copy frame"
        );
        lock_resource_pool(&self.resource_pool, "decoder-owned frame release")?
            .release_without_gpu_submission(resource_handle)
            .map_err(anyhow::Error::from)?;

        self.release_zero_copy_frame(resource_handle)
    }

    /// Возвращает статистику resource pool для отладки.
    pub fn resource_pool_stats(&self) -> Result<(usize, usize)> {
        let resource_pool = lock_resource_pool(&self.resource_pool, "statistics read")?;
        Ok((resource_pool.num_slots(), resource_pool.num_in_use()))
    }

    /// Забирает следующий backend-ready frame без submit-а нового packet-а.
    ///
    /// Decoder thread использует это после одного `decode()` call, чтобы
    /// опубликовать burst кадров, которые cros-codecs уже вернул через events.
    pub(crate) fn take_ready_frame(&mut self) -> Option<DecodedFrame> {
        self.ready_queue.pop_front()
    }

    /// Проверяет, есть ли backend-ready frames, которые decoder thread ещё не опубликовал.
    pub(crate) fn has_ready_frames(&self) -> bool {
        !self.ready_queue.is_empty()
    }

    /// Устанавливает active accurate-seek output floor внутри VAAPI backend-а.
    pub(crate) fn set_preroll_output_floor(
        &mut self,
        policy: VideoPrerollOutputFloor,
    ) -> VideoPrerollOutputFloorResult {
        let result = self.preroll_output_floor.set_floor(policy);
        if result == VideoPrerollOutputFloorResult::Applied {
            if let Err(error) = self
                .force_drain_preroll_candidate_and_suppressed_surfaces("set_preroll_output_floor")
            {
                let message = format!(
                    "VAAPI preroll output-floor set failed while draining old suppressed state: {error:#}"
                );
                warn!(
                    error = %message,
                    generation = policy.generation,
                    "Failed to safely clear old suppressed state while setting new floor"
                );
                return VideoPrerollOutputFloorResult::Fatal(video_core::DecodeThreadError::new(
                    message,
                ));
            }
            debug!(
                generation = policy.generation,
                floor_pts_ms = policy.floor_pts.as_millis(),
                retain_latest_before_floor = policy.retain_latest_before_floor,
                "VAAPI preroll output floor applied"
            );
        }
        result
    }

    /// Очищает active accurate-seek output floor, если clear policy совпала.
    pub(crate) fn clear_preroll_output_floor(
        &mut self,
        clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        let result = self.preroll_output_floor.clear_floor(clear);
        if result == VideoPrerollOutputFloorResult::Cleared {
            if let Err(error) = self
                .force_drain_preroll_candidate_and_suppressed_surfaces("clear_preroll_output_floor")
            {
                let message = format!(
                    "VAAPI preroll output-floor clear failed while draining suppressed state: {error:#}"
                );
                warn!(
                    error = %message,
                    ?clear,
                    "Failed to safely clear suppressed state while clearing floor"
                );
                return VideoPrerollOutputFloorResult::Fatal(video_core::DecodeThreadError::new(
                    message,
                ));
            }
            debug!(?clear, "VAAPI preroll output floor cleared");
        }
        result
    }

    /// Определяет decoded surface contract для текущего ready handle.
    fn current_decoded_contract(
        &self,
        handle: &VaapiDecodedFrameHandle,
    ) -> Result<DecodedSurfaceContract> {
        if let Some(stream_info) = self.adapter.stream_info() {
            return decoded_contract_for_stream_format(stream_info.format);
        }

        let backing_frame = handle.video_frame();
        decoded_contract_for_rt_format(backing_frame.rt_format())
    }

    /// Переключает active codec adapter под уже валидированный stream config.
    pub(crate) fn configure_stream(&mut self, config: &VideoStreamDecodeConfig) -> Result<()> {
        self.display_orientation = config.display_orientation;
        self.expected_frame_contract = config.frame_contract;

        if self.adapter.can_reuse_for_config(config) {
            return Ok(());
        }

        let floor_clear_result = self
            .preroll_output_floor
            .clear_floor(VideoPrerollOutputFloorClear::Any);
        if floor_clear_result == VideoPrerollOutputFloorResult::Cleared {
            debug!("Cleared VAAPI preroll output floor during stream reconfigure");
        }
        self.force_drain_preroll_candidate_and_suppressed_surfaces("configure_stream")?;
        self.release_decoder_owned_ready_frames("configure_stream")?;
        self.invalidate_idle_resource_pool_after_format_change()?;

        let adapter =
            VaapiCodecAdapterFactory::create_adapter_for_config(self.display.clone(), config)?;
        self.backend_name = adapter.backend_name();
        self.adapter = adapter;
        self.h264_decode_recovery
            .reset_for_stream(self.adapter.codec() == VideoCodec::H264);
        self.zero_copy_success_logged = false;
        self.p010_boundary_verified_logged = false;

        info!(
            backend_name = self.backend_name,
            codec = %self.adapter.codec(),
            "VA-API codec adapter configured for stream"
        );

        Ok(())
    }

    /// Освобождает VA handle, удерживаемый zero-copy кадром.
    pub fn release_zero_copy_frame(&mut self, resource_handle: FrameResourceHandle) -> Result<()> {
        let Some(handle) = self.zero_copy_guards.remove(&resource_handle.0) else {
            warn!(
                handle_id = resource_handle.0,
                "No zero-copy guard found for released frame"
            );
            return Err(anyhow::anyhow!(
                "zero-copy guard missing for released handle {}",
                resource_handle.0
            ));
        };

        self.return_frame_from_handle(handle);
        lock_resource_pool(&self.resource_pool, "decoder reuse acknowledgement")?
            .acknowledge_decoder_reuse(resource_handle)
            .map_err(anyhow::Error::from)?;
        Ok(())
    }

    /// Возвращает backing frame в pool после того, как decoded handle больше не нужен.
    fn return_frame_from_handle(&mut self, handle: VaapiDecodedFrameHandle) {
        return_frame_to_pool_from_handle(&mut self.frame_pool, handle);
    }

    /// Неблокирующе освобождает ready suppressed surfaces из backend-local queue.
    fn reclaim_ready_suppressed_surfaces(&mut self) -> Result<ReclaimPassReport> {
        self.suppressed_reclaim_state
            .reclaim_ready(&mut self.frame_pool)
    }

    /// Дешёвый reclaim pass, доступный только decoder thread boundary.
    pub(crate) fn reclaim_suppressed_surfaces_for_thread(&mut self) -> Result<()> {
        self.reclaim_ready_suppressed_surfaces().map(|_| ())
    }

    /// Проверяет, заполнена ли suppressed reclaim queue после cheap reclaim pass-а.
    fn suppressed_reclaim_queue_is_full(&self) -> bool {
        self.suppressed_reclaim_state.is_full()
    }

    /// Принудительно дренирует suppressed queue перед lifecycle boundary.
    fn force_drain_suppressed_surfaces(&mut self, reason: &'static str) -> Result<()> {
        self.suppressed_reclaim_state
            .force_drain(&mut self.frame_pool, reason)
    }

    /// Очищает retained candidate и suppressed queue перед lifecycle boundary.
    fn force_drain_preroll_candidate_and_suppressed_surfaces(
        &mut self,
        reason: &'static str,
    ) -> Result<()> {
        let candidate_result = self.drop_preroll_fallback_candidate(reason);
        let drain_result = self.force_drain_suppressed_surfaces(reason);

        match (candidate_result, drain_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(candidate_error), Ok(())) => Err(candidate_error),
            (Ok(()), Err(drain_error)) => Err(drain_error),
            (Err(candidate_error), Err(drain_error)) => Err(anyhow::Error::new(
                VaapiSurfaceLifecycleError::new(format!(
                    "failed to enqueue retained candidate during {reason}: {candidate_error:#}; \
                     additional force-drain error: {drain_error:#}"
                )),
            )),
        }
    }

    /// Ставит suppressed/candidate frame в backend-local reclaim queue.
    fn enqueue_suppressed_frame_for_reclaim(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        reason: &'static str,
        pts: Duration,
        generation: u64,
    ) -> Result<()> {
        self.suppressed_reclaim_state.enqueue(
            &mut self.frame_pool,
            handle,
            reason,
            PrerollFallbackCandidateMetadata { pts, generation },
        )
    }

    /// Проверяет, должен ли `FrameReady` быть подавлен active output floor-ом.
    fn should_suppress_ready_frame(
        &self,
        handle_pts: Duration,
        generation: u64,
    ) -> Option<ActivePrerollOutputFloor> {
        self.preroll_output_floor
            .suppression_floor(handle_pts, generation)
    }

    /// Удаляет retained pre-floor candidate без sync/export/publish.
    fn drop_preroll_fallback_candidate(&mut self, reason: &'static str) -> Result<()> {
        let Some(candidate) = self.preroll_fallback_candidate.take() else {
            return Ok(());
        };
        let metadata = candidate.metadata;

        debug!(
            pts_ms = metadata.pts.as_millis(),
            generation = metadata.generation,
            reason,
            "Queueing retained preroll fallback candidate for suppressed reclaim"
        );
        self.enqueue_suppressed_frame_for_reclaim(
            candidate.handle,
            reason,
            metadata.pts,
            metadata.generation,
        )
    }

    /// Сохраняет только самый поздний pre-floor candidate для EOF fallback.
    fn retain_preroll_fallback_candidate(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        pts: Duration,
        generation: u64,
    ) -> Result<()> {
        let incoming_candidate = PrerollFallbackCandidate::new(handle, pts, generation);
        match preroll_fallback_candidate_decision(
            self.preroll_fallback_candidate.as_ref(),
            incoming_candidate.metadata,
        ) {
            PrerollFallbackCandidateDecision::StoreFirst => {
                debug!(
                    pts_ms = incoming_candidate.metadata.pts.as_millis(),
                    generation = incoming_candidate.metadata.generation,
                    "Retaining first preroll fallback candidate"
                );
                self.preroll_fallback_candidate = Some(incoming_candidate);
                Ok(())
            }
            PrerollFallbackCandidateDecision::ReplaceExisting => {
                let Some(replaced_candidate) =
                    self.preroll_fallback_candidate.replace(incoming_candidate)
                else {
                    return Err(VaapiSurfaceLifecycleError::new(
                        "preroll candidate replacement selected without an existing candidate",
                    )
                    .into());
                };
                let replaced_metadata = replaced_candidate.metadata;
                self.preroll_output_floor.record_candidate_replaced();
                debug!(
                    old_pts_ms = replaced_metadata.pts.as_millis(),
                    new_pts_ms = pts.as_millis(),
                    generation,
                    candidate_replaced_count =
                        self.preroll_output_floor.counters.candidate_replaced_count,
                    "Replacing retained preroll fallback candidate via suppressed reclaim queue"
                );
                self.enqueue_suppressed_frame_for_reclaim(
                    replaced_candidate.handle,
                    "replace_preroll_fallback_candidate",
                    replaced_metadata.pts,
                    replaced_metadata.generation,
                )
            }
            PrerollFallbackCandidateDecision::DropIncoming => {
                let incoming_metadata = incoming_candidate.metadata;
                debug!(
                    incoming_pts_ms = incoming_metadata.pts.as_millis(),
                    retained_pts_ms = self
                        .preroll_fallback_candidate
                        .as_ref()
                        .map(|candidate| candidate.metadata.pts.as_millis())
                        .unwrap_or_default(),
                    generation,
                    "Queueing older incoming preroll fallback candidate for suppressed reclaim"
                );
                self.enqueue_suppressed_frame_for_reclaim(
                    incoming_candidate.handle,
                    "drop_incoming_preroll_fallback_candidate",
                    incoming_metadata.pts,
                    incoming_metadata.generation,
                )
            }
        }
    }

    /// Подавляет pre-floor ready frame без DMA-BUF export/publish.
    fn suppress_ready_frame(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        generation: u64,
        floor: ActivePrerollOutputFloor,
    ) -> Result<()> {
        let pts = Duration::from_micros(handle.timestamp());
        self.preroll_output_floor.record_suppressed_frame();
        self.send_diagnostic_event(VideoDecoderDiagnosticEvent::SeekPrerollFrameSuppressed {
            pts,
            generation,
            floor_pts: floor.floor_pts,
        });

        debug!(
            pts_ms = pts.as_millis(),
            generation,
            floor_pts_ms = floor.floor_pts.as_millis(),
            retain_latest_before_floor = floor.retain_latest_before_floor,
            suppressed_frame_count = self.preroll_output_floor.counters.suppressed_frame_count,
            "Suppressing decoder-ready preroll frame without DMA-BUF export"
        );

        if floor.retain_latest_before_floor {
            self.retain_preroll_fallback_candidate(handle, pts, generation)
        } else {
            self.enqueue_suppressed_frame_for_reclaim(
                handle,
                "suppress_ready_frame",
                pts,
                generation,
            )
        }
    }

    /// Отмечает successful publish target-or-after кадра и удаляет fallback candidate.
    fn record_target_or_after_frame_published(
        &mut self,
        pts: Duration,
        generation: u64,
    ) -> Result<()> {
        if self
            .preroll_output_floor
            .record_target_or_after_published(pts, generation)
        {
            self.drop_preroll_fallback_candidate("target_or_after_published")?;
            debug!(
                pts_ms = pts.as_millis(),
                generation,
                target_published_after_floor_count = self
                    .preroll_output_floor
                    .counters
                    .target_published_after_floor_count,
                "Published target-or-after frame for active preroll floor"
            );
        }
        Ok(())
    }

    /// Проверяет, должен ли matching target-or-after frame удалить retained candidate.
    fn should_drop_preroll_candidate_before_publish(&self, pts: Duration, generation: u64) -> bool {
        self.preroll_output_floor
            .is_target_or_after_for_active_floor(pts, generation)
    }

    /// Promotes retained fallback candidate через обычный publish path при EOF.
    fn promote_preroll_fallback_candidate_if_needed(&mut self, generation: u64) -> Result<()> {
        if !self
            .preroll_output_floor
            .should_promote_candidate(generation)
        {
            return Ok(());
        }

        let Some(candidate) = self.preroll_fallback_candidate.take() else {
            return Ok(());
        };

        if candidate.metadata.generation != generation {
            debug!(
                candidate_generation = candidate.metadata.generation,
                drain_generation = generation,
                "Dropping preroll fallback candidate with mismatched generation"
            );
            return self.enqueue_suppressed_frame_for_reclaim(
                candidate.handle,
                "promote_candidate_generation_mismatch",
                candidate.metadata.pts,
                candidate.metadata.generation,
            );
        }

        let promoted_pts = candidate.metadata.pts;
        self.process_ready_frame(candidate.handle, generation)?;
        self.preroll_output_floor
            .record_candidate_promoted(generation);
        debug!(
            pts_ms = promoted_pts.as_millis(),
            generation,
            candidate_promoted_count = self.preroll_output_floor.counters.candidate_promoted_count,
            "Promoted preroll fallback candidate through normal ready-frame path"
        );

        Ok(())
    }

    /// Добавляет zero-copy кадр в decoder ready queue, сбрасывая самый старый backlog.
    ///
    /// UI получает кадры через отдельный канал, поэтому decoder может временно
    /// накопить exported descriptors внутри `ready_queue`. Для видео важнее
    /// держать низкую задержку и не исчерпывать VA surfaces, чем сохранить каждый
    /// промежуточный кадр при backlog.
    fn push_ready_frame(&mut self, mut frame: DecodedFrame) -> Result<()> {
        if let Err(error) = frame.validate_contract() {
            let invalid_resource_handle = frame.resource_handle;
            warn!(
                error = %error,
                format = %frame.format(),
                memory_path = %frame.memory_path(),
                "Decoded frame contract validation failed before ready queue"
            );
            if let Err(release_error) = self.release_frame(invalid_resource_handle) {
                return Err(zero_copy_contract_violation(format!(
                    "Decoded frame contract validation failed before ready queue: {error}; release also failed: {release_error:#}"
                )));
            }
            return Err(zero_copy_contract_violation(format!(
                "Decoded frame contract validation failed before ready queue: {error}"
            )));
        }

        while self.ready_queue.len() >= self.runtime_config.ready_queue_frames {
            let Some(stale_frame) = self.ready_queue.pop_front() else {
                break;
            };
            let stale_pts_ms = stale_frame.pts.as_millis();
            self.release_frame(stale_frame.resource_handle)?;
            self.send_diagnostic_event(VideoDecoderDiagnosticEvent::FrameDropped {
                pts: stale_frame.pts,
                reason: VideoDecoderDropReason::ReadyQueueOverflow,
            });
            debug!(
                stale_pts_ms,
                ready_queue_limit = self.runtime_config.ready_queue_frames,
                "Dropping stale decoded frame from internal ready queue"
            );
        }

        frame.diagnostics.decoder_ready_queue_depth = Some(self.ready_queue.len() + 1);

        trace!(
            pts_ms = frame.pts.as_millis(),
            handle_id = frame.resource_handle.0,
            format = %frame.format(),
            bit_depth = ?frame.bit_depth(),
            chroma = ?frame.chroma(),
            color_origin = ?frame.color.origin,
            color_confidence = ?frame.color.confidence,
            queue_len = self.ready_queue.len() + 1,
            "Zero-copy frame queued for presentation"
        );

        self.ready_queue.push_back(frame);
        Ok(())
    }

    /// Отправляет diagnostics event, не влияя на decode hot path при dropped receiver.
    fn send_diagnostic_event(&self, event: VideoDecoderDiagnosticEvent) {
        if let Some(diagnostic_tx) = &self.diagnostic_tx {
            let _ = diagnostic_tx.try_send(event);
        }
        if let Some(activity_notifier) = &self.activity_notifier {
            let _ = activity_notifier.notify_activity();
        }
    }

    /// Очищает кадры, которыми всё ещё владеет decoder thread.
    ///
    /// Важно не трогать `zero_copy_guards` целиком: часть guards относится к
    /// кадрам, уже отданным player/render thread. Такие кадры освобождаются
    /// только через обычный release от session, иначе VA surface может быть
    /// переиспользован, пока renderer ещё семплит старый кадр.
    fn release_decoder_owned_ready_frames(&mut self, reason: &'static str) -> Result<()> {
        let decoder_owned_resource_handles =
            drain_decoder_owned_ready_frame_handles(&mut self.ready_queue);

        if decoder_owned_resource_handles.is_empty() {
            return Ok(());
        }

        let released_frame_count = decoder_owned_resource_handles.len();
        for resource_handle in decoder_owned_resource_handles {
            self.release_frame(resource_handle)?;
        }

        debug!(
            released_frame_count,
            reason, "Released decoder-owned ready frames"
        );
        Ok(())
    }

    /// Сбрасывает resource pool после смены формата только если нет live кадров.
    ///
    /// Render/player могут всё ещё держать старый кадр как stale frame во время
    /// seek или dynamic format change. Полный `invalidate_all()` в такой момент
    /// удалит handle mapping раньше обычного release и оставит zero-copy guard
    /// без пути возврата VA surface в pool.
    fn invalidate_idle_resource_pool_after_format_change(&mut self) -> Result<()> {
        let mut resource_pool =
            lock_resource_pool(&self.resource_pool, "format-change invalidation")?;
        let live_resource_count = resource_pool.num_in_use();

        if live_resource_count == 0 {
            if let Err(error) = resource_pool.invalidate_all() {
                warn!(
                    error = %error,
                    "Failed to invalidate idle zero-copy resource pool after format change"
                );
            }
            return Ok(());
        }

        debug!(
            live_resource_count,
            "Keeping resource pool entries because render-owned frames are still live"
        );
        Ok(())
    }

    /// Декодирует packet для decoder thread-а и сохраняет output-buffer backpressure.
    pub(crate) fn decode_packet_for_thread(
        &mut self,
        packet: &Packet,
        generation: u64,
    ) -> Result<VaapiDecodePacketOutcome> {
        // Конвертируем PTS из Duration в микросекунды (u64).
        // cros-codecs использует u64 timestamp для идентификации кадров.
        let timestamp_us = packet.pts.as_micros() as u64;
        let decode_start = std::time::Instant::now();

        trace!(
            timestamp_us = timestamp_us,
            pts_ms = packet.pts.as_millis(),
            keyframe = ?packet.keyframe,
            data_len = packet.data.len(),
            "decode() called"
        );

        // Перед submit нельзя полагаться на adapter backpressure: если suppressed
        // queue уже full, packet ещё не должен попасть внутрь codec adapter-а.
        let reclaim_report = self.reclaim_ready_suppressed_surfaces()?;
        if self.suppressed_reclaim_queue_is_full() {
            debug!(
                pts_ms = packet.pts.as_millis(),
                generation,
                current_depth = reclaim_report.current_depth,
                max_suppressed_reclaim_frames = reclaim_report.max_suppressed_reclaim_frames,
                approximate_available_reclaim_slots =
                    reclaim_report.approximate_available_reclaim_slots,
                approximate_reserved_surface_headroom_frames =
                    reclaim_report.approximate_reserved_surface_headroom_frames,
                reclaimed_this_pass = reclaim_report.reclaimed_this_pass,
                query_errors_this_pass = reclaim_report.query_errors_this_pass,
                reason = "pre_submit_suppressed_reclaim_full",
                "Output-backpressuring packet before decode submit"
            );
            return Ok(VaapiDecodePacketOutcome::OutputBackpressured);
        }

        let is_keyframe = packet.keyframe.is_known_keyframe();
        if self.h264_decode_recovery.should_drop(is_keyframe) {
            trace!(
                pts_ms = packet.pts.as_millis(),
                "Dropping H.264 inter-frame while decoder recovery waits for keyframe"
            );
            return Ok(VaapiDecodePacketOutcome::Accepted(None));
        }

        // Шаг 1-2: submit packet и drain pending events.
        // При `CheckEvents` тот же packet отправляется повторно после drain,
        // потому что нижний decoder ещё не обязан был consume-ить bitstream.
        let loop_report = match run_decode_with_event_retry(
            self,
            timestamp_us,
            &packet.data,
            is_keyframe,
            generation,
        ) {
            Ok(report) => report,
            Err(error) => {
                if !is_fatal_decoder_error(&error) && self.adapter.codec() == VideoCodec::H264 {
                    self.recover_h264_after_decode_error(&error)?;
                }
                return Err(error);
            }
        };
        if loop_report.output_backpressured {
            return Ok(VaapiDecodePacketOutcome::OutputBackpressured);
        }
        if loop_report.skipped_packet {
            return Ok(VaapiDecodePacketOutcome::Accepted(None));
        }
        if self.h264_decode_recovery.note_packet_accepted(is_keyframe) {
            debug!(
                pts_ms = packet.pts.as_millis(),
                "H.264 decoder recovery accepted a new keyframe"
            );
        }

        // Шаг 3: Возвращаем самый старый готовый кадр (FIFO).
        let mut result = self.ready_queue.pop_front();
        let decode_elapsed = decode_start.elapsed().as_millis();
        let submit_elapsed = loop_report.submit_elapsed.as_millis();
        let drain_elapsed = loop_report.drain_elapsed.as_millis();
        if let Some(frame) = result.as_mut() {
            frame.diagnostics.timings.decoder_submit_latency = Some(loop_report.submit_elapsed);
            frame.diagnostics.timings.decoder_event_drain_latency = Some(loop_report.drain_elapsed);
        }
        if let Some(ref frame) = result {
            debug!(
                pts_ms = frame.pts.as_millis(),
                width = frame.width,
                height = frame.height,
                decode_attempts = loop_report.attempts,
                processed_bytes = loop_report.processed_bytes,
                events = loop_report.events_count,
                format_changed = loop_report.format_changed,
                submit_ms = submit_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() returning frame"
            );
        } else if loop_report.events_count > 0 {
            debug!(
                decode_attempts = loop_report.attempts,
                processed_bytes = loop_report.processed_bytes,
                events = loop_report.events_count,
                format_changed = loop_report.format_changed,
                submit_ms = submit_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() completed: no frame ready"
            );
        } else {
            trace!(
                decode_attempts = loop_report.attempts,
                processed_bytes = loop_report.processed_bytes,
                submit_ms = submit_elapsed,
                drain_ms = drain_elapsed,
                decode_ms = decode_elapsed,
                "decode() completed: no events, no frame"
            );
        }

        Ok(VaapiDecodePacketOutcome::Accepted(result.map(Box::new)))
    }

    /// Запускает explicit EOF/DPB drain и оставляет tail frames в обычном publish path.
    pub(crate) fn begin_end_of_stream_drain_for_thread(&mut self, generation: u64) -> Result<()> {
        self.reclaim_ready_suppressed_surfaces()?;
        self.adapter
            .begin_end_of_stream_drain()
            .map_err(|error| anyhow::anyhow!("EOF drain error: {error}"))?;
        self.drain_decoder_events(DecoderEventDrainPolicy::Publish { generation })?;
        self.promote_preroll_fallback_candidate_if_needed(generation)?;
        Ok(())
    }

    /// Очищает повреждённый H.264 DPB и переводит backend в ожидание keyframe.
    fn recover_h264_after_decode_error(&mut self, decode_error: &anyhow::Error) -> Result<()> {
        if let Err(flush_error) = self.flush_decoder_owned_state("h264_decode_recovery") {
            return Err(VaapiSurfaceLifecycleError::new(format!(
                "H.264 decoder recovery flush failed after {decode_error:#}: {flush_error:#}"
            ))
            .into());
        }
        self.h264_decode_recovery.begin();
        warn!(
            error = %decode_error,
            "H.264 decoder flushed after recoverable decode error; waiting for keyframe"
        );
        Ok(())
    }

    /// Общий codec/resource flush без изменения владельца recovery policy.
    fn flush_decoder_owned_state(&mut self, reason: &'static str) -> Result<()> {
        self.force_drain_preroll_candidate_and_suppressed_surfaces(reason)?;
        self.adapter
            .flush()
            .map_err(|error| anyhow::anyhow!("Flush error: {error}"))?;
        self.drain_decoder_events(DecoderEventDrainPolicy::Discard { reason })?;
        self.force_drain_suppressed_surfaces(reason)?;
        self.release_decoder_owned_ready_frames(reason)?;
        Ok(())
    }
}

impl DecoderRetryDriver for VaapiVideoDecoder {
    /// Возвращает codec label активного adapter-а.
    fn codec_label(&self) -> &'static str {
        self.adapter.codec_label()
    }

    /// Отправляет packet в реальный VA-API decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        self.adapter.submit_packet(
            timestamp_us,
            packet_data,
            decode_hints,
            &mut self.frame_pool,
        )
    }

    /// Обрабатывает pending events реального decoder-а.
    fn drain_events(&mut self, policy: DecoderEventDrainPolicy) -> Result<DecoderDrainReport> {
        self.drain_decoder_events(policy)
    }
}

impl VideoDecoder for VaapiVideoDecoder {
    /// Декодирует один encoded video packet активным codec adapter-ом.
    ///
    /// Pipeline:
    /// 1. Submit bitstream в cros-codecs decoder.
    /// 2. Drain все pending events (FrameReady / FormatChanged).
    /// 3. Вернуть самый старый готовый кадр из очереди.
    fn decode(&mut self, packet: &Packet) -> Result<Option<DecodedFrame>> {
        match self.decode_packet_for_thread(packet, 0)? {
            VaapiDecodePacketOutcome::Accepted(frame) => Ok(frame.map(|boxed_frame| *boxed_frame)),
            VaapiDecodePacketOutcome::OutputBackpressured => Ok(None),
        }
    }

    /// Сбрасывает decoder: завершает все pending decode requests.
    ///
    /// После flush decoder требует keyframe перед возобновлением декодирования.
    fn flush(&mut self) -> Result<()> {
        self.flush_decoder_owned_state("flush")?;
        self.h264_decode_recovery
            .reset_for_stream(self.adapter.codec() == VideoCodec::H264);
        Ok(())
    }

    /// Возвращает имя бэкенда для отображения в UI.
    fn backend_name(&self) -> &'static str {
        self.backend_name
    }

    /// Downcast к конкретному типу для backend-specific операций.
    fn as_any(&self) -> &dyn Any {
        self
    }

    /// Mutable downcast к конкретному типу.
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Drop for VaapiVideoDecoder {
    /// Best-effort cleanup для suppressed handles при остановке decoder-а.
    fn drop(&mut self) {
        if let Err(error) =
            self.force_drain_preroll_candidate_and_suppressed_surfaces("decoder_drop")
        {
            warn!(
                error = %error,
                remaining_suppressed_handles = self.suppressed_reclaim_state.depth(),
                "Failed to force-drain suppressed VA surfaces during decoder drop"
            );
        }
    }
}

#[cfg(test)]
mod tests;
