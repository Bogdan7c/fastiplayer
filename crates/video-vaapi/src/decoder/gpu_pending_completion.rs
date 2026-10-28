//! Доделка кадров из очереди «в полёте» владельцем — `VaapiVideoDecoder`.
//!
//! Очередь (`gpu_pending_frames`) решает, *какой* кадр пора доделать; здесь —
//! *что* с ним делать: та же логика публикации, что раньше шла прямо в event
//! drain (preroll floor, отбрасывание кандидата, sync/export/регистрация), и
//! правила lifecycle boundary-ей (seek flush, EOF, смена потока/формата, drop).

use super::gpu_pending_frames::{
    GpuPendingCompletionReason, GpuPendingFrame, GpuPendingSurvivorScope,
};
use super::suppressed_reclaim::sync_discard_ready_frame;
use super::*;

/// Почему поток декодера собирается ждать (и поэтому может дождаться GPU).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoderGpuWaitReason {
    /// В packet-канале пусто: подать GPU следующую работу нечем.
    NoPacketToSubmit,
    /// Пул выходных surfaces исчерпан, а кадры очереди держат его surfaces.
    OutputBackpressure,
}

/// Куда кладётся доделанный кадр.
enum CompletedFrameDestination<'survivors> {
    /// Обычный путь: в `ready_queue`, откуда его публикует decoder thread.
    ReadyQueue,
    /// Кадр переживает lifecycle boundary, которая освобождает `ready_queue`.
    Survivors(&'survivors mut Vec<DecodedFrame>),
}

impl super::VaapiVideoDecoder {
    /// Ставит `FrameReady` в очередь «в полёте» вместо немедленного ожидания GPU.
    ///
    /// Контракт поверхности снимается сейчас: следующий packet может сменить
    /// `stream_info()`. Ошибка контракта обрабатывается как раньше ошибка
    /// `process_ready_frame`: fatal останавливает decoder, остальное — warn и
    /// кадр отпускается (surface вернётся в пул через `wait_until_idle`).
    pub(super) fn enqueue_gpu_pending_frame(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        generation: u64,
    ) -> Result<()> {
        let decoded_contract = match self.current_decoded_contract(&handle) {
            Ok(decoded_contract) => decoded_contract,
            Err(error) if is_fatal_decoder_error(&error) => return Err(error),
            Err(error) => {
                warn!(error = %error, "Failed to process ready frame");
                return Ok(());
            }
        };
        self.gpu_pending_frames
            .enqueue(handle, generation, decoded_contract);
        while let Some(frame) = self.gpu_pending_frames.pop_over_capacity() {
            self.complete_gpu_pending_frame(frame, CompletedFrameDestination::ReadyQueue)?;
        }
        Ok(())
    }

    /// Отмечает начало decode-вызова packet-а (кадры в очереди становятся «прошлыми»).
    pub(super) fn begin_gpu_pending_packet_submission(&mut self) {
        self.gpu_pending_frames.begin_packet_submission();
    }

    /// После submit-а packet-а доделывает кадры, ожидание которых не тормозит GPU.
    pub(super) fn complete_gpu_pending_frames_after_packet_submit(&mut self) -> Result<()> {
        while let Some(frame) = self
            .gpu_pending_frames
            .pop_completable_after_packet_submit()
        {
            self.complete_gpu_pending_frame(frame, CompletedFrameDestination::ReadyQueue)?;
        }
        Ok(())
    }

    /// Доделывает всю очередь по порядку; `reason` попадает в счётчики.
    pub(super) fn complete_all_gpu_pending_frames(
        &mut self,
        reason: GpuPendingCompletionReason,
    ) -> Result<()> {
        while let Some(frame) = self.gpu_pending_frames.pop_front_for(reason) {
            self.complete_gpu_pending_frame(frame, CompletedFrameDestination::ReadyQueue)?;
        }
        Ok(())
    }

    /// Есть ли кадры, которые GPU, возможно, ещё декодирует.
    pub(crate) fn has_gpu_pending_frames(&self) -> bool {
        !self.gpu_pending_frames.is_empty()
    }

    /// Сколько кадров декодер держит впереди показа: выданные и ещё не возвращённые
    /// (зарегистрированы в пуле импорта) плюс кадры «в полёте» на GPU.
    ///
    /// Тот же учёт, что видит плеер в `decoder_resource_snapshot`; по нему такт показа
    /// (`decoder_thread::present_pacing`) решает, есть ли у декодера запас, чтобы ждать.
    pub(crate) fn decoded_frames_ahead(&self) -> Result<crate::decoder_thread::DecodedFramesAhead> {
        let (_, registered_frames) = self.resource_pool_stats()?;
        Ok(crate::decoder_thread::DecodedFramesAhead(
            registered_frames.saturating_add(self.gpu_pending_frames.depth()),
        ))
    }

    /// Поток декодера собирается ждать: сначала отдаёт все кадры очереди.
    ///
    /// Без этого кадр, ожидающий следующего packet-а, застрял бы на паузе/в конце
    /// входа, а при исчерпанном пуле его surface никогда бы не вернулась.
    pub(crate) fn complete_gpu_pending_frames_before_waiting(
        &mut self,
        wait_reason: DecoderGpuWaitReason,
    ) -> Result<()> {
        let completion_reason = match wait_reason {
            DecoderGpuWaitReason::NoPacketToSubmit => GpuPendingCompletionReason::DecoderIdle,
            DecoderGpuWaitReason::OutputBackpressure => {
                GpuPendingCompletionReason::OutputBackpressure
            }
        };
        self.complete_all_gpu_pending_frames(completion_reason)
    }

    /// Выбрасывает всю очередь без публикации (seek flush, остановка decoder-а).
    ///
    /// Как и DPB-хвост при flush, кадр сначала синхронизируется: surface не должна
    /// вернуться в пул, пока GPU в неё пишет.
    pub(super) fn discard_all_gpu_pending_frames(&mut self, reason: &'static str) -> Result<()> {
        for frame in self.gpu_pending_frames.take_all_for_discard() {
            sync_discard_ready_frame(frame.handle, reason)?;
        }
        Ok(())
    }

    /// Доделывает кадры, которые должны пережить boundary, освобождающую `ready_queue`.
    ///
    /// Остальные кадры очереди (по `scope`) выбрасываются: раньше они попадали в
    /// `ready_queue` и освобождались этой же boundary. Возвращённые кадры
    /// вызывающий код кладёт обратно через [`Self::restore_gpu_pending_survivors`].
    pub(super) fn take_gpu_pending_survivors(
        &mut self,
        scope: GpuPendingSurvivorScope,
        reason: &'static str,
    ) -> Result<Vec<DecodedFrame>> {
        for frame in self
            .gpu_pending_frames
            .take_non_survivors_for_discard(scope)
        {
            sync_discard_ready_frame(frame.handle, reason)?;
        }
        let mut survivors = Vec::with_capacity(self.gpu_pending_frames.depth());
        while let Some(frame) = self
            .gpu_pending_frames
            .pop_front_for(GpuPendingCompletionReason::LifecycleBoundary)
        {
            self.complete_gpu_pending_frame(
                frame,
                CompletedFrameDestination::Survivors(&mut survivors),
            )?;
        }
        Ok(survivors)
    }

    /// Возвращает пережившие boundary кадры в обычный путь публикации.
    pub(super) fn restore_gpu_pending_survivors(
        &mut self,
        survivors: Vec<DecodedFrame>,
    ) -> Result<()> {
        for frame in survivors {
            self.push_ready_frame(frame)?;
        }
        Ok(())
    }

    /// Перед сменой seek-floor доделывает очередь со старым floor-ом.
    ///
    /// Кадры в очереди старый код уже обработал бы при прежнем floor: смена
    /// floor-а не должна задним числом подавить или пропустить их. `Some` —
    /// fatal-итог для control-ответа, `None` — можно менять floor.
    pub(super) fn complete_gpu_pending_frames_before_floor_change(
        &mut self,
        operation: &'static str,
    ) -> Option<VideoPrerollOutputFloorResult> {
        let error = self
            .complete_all_gpu_pending_frames(GpuPendingCompletionReason::LifecycleBoundary)
            .err()?;
        let message = format!(
            "VAAPI preroll output-floor {operation} failed while completing in-flight frames: {error:#}"
        );
        warn!(error = %message, "Failed to complete in-flight VA frames before floor change");
        Some(VideoPrerollOutputFloorResult::Fatal(
            video_core::DecodeThreadError::new(message),
        ))
    }

    /// Best-effort выброс очереди при уничтожении decoder-а.
    ///
    /// Ошибка sync не прерывает остановку: оставшиеся handles отпускаются, а
    /// пул выходных surfaces сам дождётся простоя surface (`wait_until_idle`).
    pub(super) fn discard_gpu_pending_frames_on_decoder_drop(&mut self) {
        let stats = self.gpu_pending_frames.stats();
        debug!(
            ?stats,
            "VA-API in-flight frame queue counters at decoder drop"
        );
        if let Err(error) = self.discard_all_gpu_pending_frames("decoder_drop") {
            warn!(error = %error, "Failed to sync in-flight VA frames during decoder drop");
        }
    }

    /// Публикует один кадр очереди по прежним правилам `FrameReady`.
    ///
    /// Решение о preroll floor принимается здесь, а не при `FrameReady`: floor
    /// меняется только через control-сообщения, а они сначала доделывают очередь,
    /// поэтому кадр видит тот же floor, что увидел бы раньше.
    fn complete_gpu_pending_frame(
        &mut self,
        frame: GpuPendingFrame,
        destination: CompletedFrameDestination<'_>,
    ) -> Result<()> {
        let GpuPendingFrame {
            handle,
            generation,
            decoded_contract,
            ..
        } = frame;
        let pts = Duration::from_micros(handle.timestamp());
        if let Some(floor) = self.should_suppress_ready_frame(pts, generation) {
            return self.suppress_ready_frame(handle, generation, floor);
        }
        if self.should_drop_preroll_candidate_before_publish(pts, generation) {
            self.drop_preroll_fallback_candidate("target_or_after_ready_frame")?;
        }

        let completion_result = self
            .export_ready_frame(handle, generation, decoded_contract)
            .and_then(|decoded_frame| match destination {
                CompletedFrameDestination::ReadyQueue => self.push_ready_frame(decoded_frame),
                CompletedFrameDestination::Survivors(survivors) => {
                    survivors.push(decoded_frame);
                    Ok(())
                }
            });
        match completion_result {
            Ok(()) => self.record_target_or_after_frame_published(pts, generation),
            Err(error) if is_fatal_decoder_error(&error) => Err(error),
            Err(error) => {
                warn!(error = %error, "Failed to process ready frame");
                Ok(())
            }
        }
    }
}
