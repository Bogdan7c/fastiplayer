//! Очередь кадров «отправлено в GPU, ещё не отдано» (gles2-renderer, сессия 09a).
//!
//! cros-codecs работает в `NonBlocking` режиме (`STATELESS_DECODER_BLOCKING_MODE`):
//! `FrameReady` приходит сразу после `vaEndPicture`, когда GPU ещё может
//! декодировать картинку. Если сразу ждать `vaSyncSurface`, поток декодера не
//! успевает подать следующий packet, и GPU простаивает между кадрами.
//!
//! Эта очередь держит такие кадры в порядке выхода из cros-codecs (порядок показа)
//! и решает только одно: какой кадр пора доделать (sync → DMA-BUF export →
//! регистрация → `ready_queue`). Сам экспорт, preroll floor и публикацию делает
//! владелец — `VaapiVideoDecoder` (`decoder/gpu_pending_completion.rs`).
//!
//! Инварианты:
//! - кадры доделываются строго по порядку: только голова очереди;
//! - кадр прошлого packet-а доделывается, как только GPU получил следующий packet
//!   (тогда ожидание его готовности не оставляет GPU без работы);
//! - очередь ограничена ёмкостью `ready_queue_frames`: лишний кадр доделывается
//!   сразу, поэтому surfaces, удержанные декодером, не превышают прежний бюджет;
//! - каждый кадр уходит из очереди ровно один раз: доделан или выброшен.

use std::collections::VecDeque;
use std::sync::Arc;

use tracing::warn;

use super::surface_contract::DecodedSurfaceContract;
use crate::codec_adapter::VaapiDecodedFrameHandle;
use crate::gpu_pending_stats::{
    GpuPendingFrameCounters, GpuPendingFrameExit, GpuPendingFrameStats,
};

/// Номер packet-а, в decode-вызове которого кадр вышел из cros-codecs.
///
/// Нужен, чтобы отличать кадры «прошлых» packet-ов (их GPU уже не задерживает)
/// от кадров текущего вызова (на lifecycle boundary они ведут себя как раньше).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct PacketSubmissionSequence(u64);

/// Кадр, ожидающий конца GPU-работы, вместе с контекстом решения о публикации.
///
/// Контракт поверхности снимается в момент `FrameReady`: следующий packet может
/// принести новый SPS, и тогда `stream_info()` адаптера уже описывает новый поток.
pub(super) struct GpuPendingFrame {
    /// Handle декодированной картинки; surface вернётся в пул, когда его отпустят все.
    pub(super) handle: VaapiDecodedFrameHandle,
    /// Seek generation drain-а, в котором кадр вышел из декодера.
    pub(super) generation: u64,
    /// Формат поверхности на момент выхода кадра.
    pub(super) decoded_contract: DecodedSurfaceContract,
    /// Packet, в decode-вызове которого кадр вышел.
    submission: PacketSubmissionSequence,
}

/// Почему кадр был снят с головы очереди для доделки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GpuPendingCompletionReason {
    /// GPU уже получил следующий packet: ожидание этого кадра его не останавливает.
    NextPacketSubmitted,
    /// Неблокирующий запрос показал, что GPU кадр уже закончил.
    SurfaceAlreadyReady,
    /// Неблокирующий запрос вернул ошибку: обычный sync-путь доделки сообщит её честно.
    ReadinessQueryFailed,
    /// Очередь превысила ёмкость: самый старый кадр доделывается сразу.
    QueueCapacity,
    /// Декодеру нечего подавать (нет packet-ов): откладывать ожидание GPU больше незачем.
    DecoderIdle,
    /// Пул выходных surfaces исчерпан: кадры в очереди держат его surfaces.
    OutputBackpressure,
    /// EOF drain, смена seek-floor, смена потока/формата: очередь доделывается целиком.
    LifecycleBoundary,
}

/// Какие кадры переживают lifecycle boundary, освобождающую decoder-owned кадры.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GpuPendingSurvivorScope {
    /// Все кадры очереди: boundary приходит между decode-вызовами (`configure_stream`).
    AllPending,
    /// Только кадры прошлых packet-ов: boundary возникла внутри decode-вызова
    /// (`FormatChanged`, восстановление H.264). Кадры текущего вызова раньше попадали
    /// в `ready_queue` и освобождались этой же boundary — так и остаётся.
    EarlierPacketsOnly,
}

impl GpuPendingCompletionReason {
    /// Счётчик диагностики, соответствующий причине доделки.
    fn exit(self) -> GpuPendingFrameExit {
        match self {
            Self::NextPacketSubmitted => GpuPendingFrameExit::CompletedAfterNextPacket,
            Self::SurfaceAlreadyReady => GpuPendingFrameExit::CompletedAlreadyReady,
            Self::ReadinessQueryFailed => GpuPendingFrameExit::CompletedAfterQueryError,
            Self::QueueCapacity => GpuPendingFrameExit::CompletedOnCapacity,
            Self::DecoderIdle => GpuPendingFrameExit::CompletedOnIdle,
            Self::OutputBackpressure => GpuPendingFrameExit::CompletedOnOutputBackpressure,
            Self::LifecycleBoundary => GpuPendingFrameExit::CompletedOnLifecycle,
        }
    }
}

/// Ограниченная FIFO-очередь кадров, которые GPU, возможно, ещё декодирует.
pub(super) struct GpuPendingFrames {
    /// Кадры в порядке выхода из cros-codecs.
    frames: VecDeque<GpuPendingFrame>,
    /// Предел глубины; не меньше 1.
    capacity: usize,
    /// Номер текущего decode-вызова; растёт перед каждым submit-ом packet-а.
    current_submission: PacketSubmissionSequence,
    /// Счётчики диагностики, общие с handle-ом decoder thread-а.
    counters: Arc<GpuPendingFrameCounters>,
}

impl GpuPendingFrames {
    /// Создаёт пустую очередь с ёмкостью `capacity` (0 трактуется как 1).
    pub(super) fn new(capacity: usize, counters: Arc<GpuPendingFrameCounters>) -> Self {
        Self {
            frames: VecDeque::new(),
            capacity: capacity.max(1),
            current_submission: PacketSubmissionSequence::default(),
            counters,
        }
    }

    /// Отмечает начало decode-вызова нового packet-а.
    ///
    /// Всё, что уже лежит в очереди, становится кадрами «прошлых» packet-ов.
    pub(super) fn begin_packet_submission(&mut self) {
        self.current_submission.0 = self.current_submission.0.wrapping_add(1);
    }

    /// Ставит кадр в конец очереди, помечая его текущим decode-вызовом.
    pub(super) fn enqueue(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        generation: u64,
        decoded_contract: DecodedSurfaceContract,
    ) {
        self.frames.push_back(GpuPendingFrame {
            handle,
            generation,
            decoded_contract,
            submission: self.current_submission,
        });
        self.counters.record_enqueued(self.frames.len());
    }

    /// Снимает голову, если очередь превысила ёмкость.
    pub(super) fn pop_over_capacity(&mut self) -> Option<GpuPendingFrame> {
        if self.frames.len() <= self.capacity {
            return None;
        }
        self.pop_front_for(GpuPendingCompletionReason::QueueCapacity)
    }

    /// Снимает голову, которую можно доделать, не оставляя GPU без работы.
    ///
    /// Вызывается после submit-а packet-а: голова из прошлого вызова доделывается
    /// всегда (GPU уже занят следующей картинкой), голова текущего вызова — только
    /// если GPU её уже закончил. Ошибка запроса не прячется: такой кадр уходит в
    /// обычный путь доделки, где `sync()` вернёт настоящую ошибку драйвера.
    pub(super) fn pop_completable_after_packet_submit(&mut self) -> Option<GpuPendingFrame> {
        let head = self.frames.front()?;
        if head.submission < self.current_submission {
            return self.pop_front_for(GpuPendingCompletionReason::NextPacketSubmitted);
        }
        match head.handle.surface_ready() {
            Ok(true) => self.pop_front_for(GpuPendingCompletionReason::SurfaceAlreadyReady),
            Ok(false) => None,
            Err(error) => {
                warn!(
                    error = %error,
                    pts_ms = head.handle.timestamp() / 1000,
                    "VA surface readiness query failed for a pending frame; completing it through sync"
                );
                self.pop_front_for(GpuPendingCompletionReason::ReadinessQueryFailed)
            }
        }
    }

    /// Снимает голову безусловно: вызывающий код готов ждать GPU по `reason`.
    pub(super) fn pop_front_for(
        &mut self,
        reason: GpuPendingCompletionReason,
    ) -> Option<GpuPendingFrame> {
        let frame = self.frames.pop_front()?;
        self.counters
            .record_exits(reason.exit(), 1, self.frames.len());
        Some(frame)
    }

    /// Забирает все кадры для выбрасывания без публикации (seek flush, drop).
    pub(super) fn take_all_for_discard(&mut self) -> Vec<GpuPendingFrame> {
        let discarded: Vec<GpuPendingFrame> = self.frames.drain(..).collect();
        self.counters
            .record_exits(GpuPendingFrameExit::Discarded, discarded.len(), 0);
        discarded
    }

    /// Забирает для выбрасывания кадры, которые не переживают boundary по `scope`.
    ///
    /// Оставшиеся в очереди кадры вызывающий код доделывает как «уже отданные».
    pub(super) fn take_non_survivors_for_discard(
        &mut self,
        scope: GpuPendingSurvivorScope,
    ) -> Vec<GpuPendingFrame> {
        let first_non_survivor = match scope {
            GpuPendingSurvivorScope::AllPending => self.frames.len(),
            // Очередь упорядочена по номеру вызова: кадры текущего вызова — хвост.
            GpuPendingSurvivorScope::EarlierPacketsOnly => self
                .frames
                .iter()
                .position(|frame| frame.submission == self.current_submission)
                .unwrap_or(self.frames.len()),
        };
        let discarded: Vec<GpuPendingFrame> = self.frames.drain(first_non_survivor..).collect();
        self.counters.record_exits(
            GpuPendingFrameExit::Discarded,
            discarded.len(),
            self.frames.len(),
        );
        discarded
    }

    /// Есть ли кадры, ожидающие GPU.
    pub(super) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Текущая глубина очереди.
    pub(super) fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Снимок счётчиков диагностики.
    pub(super) fn stats(&self) -> GpuPendingFrameStats {
        self.counters.snapshot()
    }
}
