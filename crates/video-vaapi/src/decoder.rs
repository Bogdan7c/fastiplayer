use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use codec_core::{BitDepth, ChromaSubsampling, VideoColorMetadata, VideoDisplayOrientation};
use cros_codecs::libva::{
    VA_RT_FORMAT_YUV420, VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV420_12, VA_RT_FORMAT_YUV422,
    VA_RT_FORMAT_YUV422_10, VA_RT_FORMAT_YUV422_12, VA_RT_FORMAT_YUV444, VA_RT_FORMAT_YUV444_10,
    VA_RT_FORMAT_YUV444_12,
};
use media_core::Packet;
use tracing::{debug, info, trace, warn};
use video_core::{
    DecodedFrame, DecodedPixelFormat, FrameMemoryPath, FrameResourceHandle, VideoDecoder,
    VideoDecoderActivityNotifier, VideoDecoderDiagnosticEvent, VideoDecoderDropReason,
    VideoFrameDiagnostics, VideoFrameTimingDiagnostics, VideoPrerollOutputFloor,
    VideoPrerollOutputFloorClear, VideoPrerollOutputFloorResult, VideoResourcePoolDiagnostics,
    VideoStreamDecodeConfig,
};

use crate::codec_adapter::{
    VaapiAdapterDecodeError, VaapiCodecAdapter, VaapiCodecAdapterFactory, VaapiDecodedFormat,
    VaapiDecodedFrameHandle, VaapiDecoderEvent, VaapiPacketDecodeHints,
};
use crate::frame_pool::DmaFramePool;
use crate::resource_pool::FrameResourcePool;

/// Production default количества кадров в VA DMA-пуле.
///
/// Текущий VP9 adapter может держать до 8 reference frames. 24 descriptors дают
/// запас для 4k60 burst-ов, но остаются bounded через `VaapiDecoderRuntimeConfig`.
pub const DEFAULT_DECODER_SURFACE_POOL_FRAMES: usize = 24;

/// Production default кадров, которые decoder держит импортированными до publish boundary.
///
/// cros-codecs может вернуть несколько `FrameReady` events за один decode call.
/// 8 кадров принимают burst без немедленного overflow, но не скрывают memory
/// growth: лимит явно прокидывается из config и виден в diagnostics.
pub const DEFAULT_DECODER_READY_QUEUE_FRAMES: usize = 8;

/// Validation-only override для Session E throughput замеров suppressed reclaim queue.
///
/// Это не user-facing TOML config: переменная нужна, чтобы быстро сравнить
/// несколько bounds на одной сборке без изменения `video-core` API.
const SUPPRESSED_RECLAIM_QUEUE_BOUND_OVERRIDE_ENV: &str =
    "RUSTIPLAYER_VAAPI_MAX_SUPPRESSED_RECLAIM_FRAMES";

/// Ориентировочный reserve под codec DPB/reference pressure.
///
/// Это не доказанный максимум для всех codec/driver комбинаций. Значение
/// оставляет default pool 24 достаточно широким для accurate-preroll замеров,
/// а Session E сможет проверить реальные bounds через env override.
const SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES: usize = 5;

/// Reserve под кадры, которые renderer ещё может удерживать через zero-copy guards.
const SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES: usize = 2;

/// Reserve под target frame, pending publish и небольшой ready-queue backlog.
///
/// Для suppressed preroll эта очередь не является основным sink-ом, поэтому
/// здесь не резервируется вся `ready_queue_frames` capacity.
const SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES: usize = 2;

/// Минимальный запас от off-by-one/in-flight accounting ошибок.
const SUPPRESSED_RECLAIM_MARGIN_FRAMES: usize = 1;

/// Runtime-limits VA-API decoder-а, которые относятся к backend-local очередям.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaapiDecoderRuntimeConfig {
    /// Количество output surface descriptors, доступных hardware decoder-у.
    pub surface_pool_frames: usize,

    /// Максимум готовых frames внутри backend ready queue до publish boundary.
    pub ready_queue_frames: usize,

    /// Максимум suppressed VA handles, ожидающих non-blocking reclaim.
    ///
    /// Это главный backend-local throughput knob для accurate-preroll. Default
    /// считается от surface accounting, а не как маленький safety лимит.
    pub max_suppressed_reclaim_frames: usize,
}

impl Default for VaapiDecoderRuntimeConfig {
    /// Возвращает production defaults без unbounded backend-local очередей.
    fn default() -> Self {
        Self::from_surface_accounting(
            DEFAULT_DECODER_SURFACE_POOL_FRAMES,
            DEFAULT_DECODER_READY_QUEUE_FRAMES,
        )
    }
}

impl VaapiDecoderRuntimeConfig {
    /// Создаёт backend-local config, выводя suppressed reclaim bound из surface accounting.
    #[must_use]
    pub(crate) fn from_surface_accounting(
        surface_pool_frames: usize,
        ready_queue_frames: usize,
    ) -> Self {
        let normalized_surface_pool_frames = surface_pool_frames.max(1);
        let normalized_ready_queue_frames = ready_queue_frames.max(1);
        Self {
            surface_pool_frames: normalized_surface_pool_frames,
            ready_queue_frames: normalized_ready_queue_frames,
            max_suppressed_reclaim_frames: default_max_suppressed_reclaim_frames(
                normalized_surface_pool_frames,
                normalized_ready_queue_frames,
            ),
        }
    }

    /// Нормализует public config, чтобы прямой вызов backend API не создал нулевые очереди.
    #[must_use]
    fn normalized(self) -> Self {
        let surface_pool_frames = self.surface_pool_frames.max(1);
        let ready_queue_frames = self.ready_queue_frames.max(1);
        let configured_suppressed_reclaim_frames = if self.max_suppressed_reclaim_frames == 0 {
            default_max_suppressed_reclaim_frames(surface_pool_frames, ready_queue_frames)
        } else {
            self.max_suppressed_reclaim_frames
        };
        let max_suppressed_reclaim_frames = suppressed_reclaim_bound_with_validation_override(
            configured_suppressed_reclaim_frames,
            surface_pool_frames,
        );

        Self {
            surface_pool_frames,
            ready_queue_frames,
            max_suppressed_reclaim_frames,
        }
    }
}

/// Считает default bound suppressed reclaim queue из surface accounting.
fn default_max_suppressed_reclaim_frames(
    surface_pool_frames: usize,
    ready_queue_frames: usize,
) -> usize {
    let ready_publish_headroom =
        ready_queue_frames.clamp(1, SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES);
    let accounting_headroom = SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES
        + SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES
        + ready_publish_headroom
        + SUPPRESSED_RECLAIM_MARGIN_FRAMES;

    surface_pool_frames
        .saturating_sub(accounting_headroom)
        .clamp(1, surface_pool_frames.max(1))
}

/// Применяет validation-only env override без расширения user config/API.
fn suppressed_reclaim_bound_with_validation_override(
    configured_bound: usize,
    surface_pool_frames: usize,
) -> usize {
    let normalized_configured_bound = configured_bound.clamp(1, surface_pool_frames.max(1));
    let override_value = match env::var(SUPPRESSED_RECLAIM_QUEUE_BOUND_OVERRIDE_ENV) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => return normalized_configured_bound,
        Err(error) => {
            warn!(
                env = SUPPRESSED_RECLAIM_QUEUE_BOUND_OVERRIDE_ENV,
                error = %error,
                configured_bound = normalized_configured_bound,
                "Ignoring invalid suppressed reclaim queue override"
            );
            return normalized_configured_bound;
        }
    };

    match override_value.parse::<usize>() {
        Ok(parsed_bound) if parsed_bound > 0 => {
            let normalized_override = parsed_bound.clamp(1, surface_pool_frames.max(1));
            if normalized_override != parsed_bound {
                warn!(
                    env = SUPPRESSED_RECLAIM_QUEUE_BOUND_OVERRIDE_ENV,
                    requested_bound = parsed_bound,
                    normalized_bound = normalized_override,
                    surface_pool_frames,
                    approximate_reserved_surface_headroom_frames =
                        surface_pool_frames.saturating_sub(normalized_override),
                    "Clamped suppressed reclaim queue override to surface pool accounting"
                );
            } else {
                info!(
                    env = SUPPRESSED_RECLAIM_QUEUE_BOUND_OVERRIDE_ENV,
                    normalized_bound = normalized_override,
                    surface_pool_frames,
                    approximate_reserved_surface_headroom_frames =
                        surface_pool_frames.saturating_sub(normalized_override),
                    "Using validation-only suppressed reclaim queue override"
                );
            }
            normalized_override
        }
        Ok(_) | Err(_) => {
            warn!(
                env = SUPPRESSED_RECLAIM_QUEUE_BOUND_OVERRIDE_ENV,
                value = %override_value,
                configured_bound = normalized_configured_bound,
                "Ignoring non-positive or non-numeric suppressed reclaim queue override"
            );
            normalized_configured_bound
        }
    }
}

/// Максимум повторных submit попыток после adapter-level `CheckEvents`.
///
/// `cros-codecs` использует `CheckEvents` как backpressure-сигнал:
/// вызывающий код должен обработать pending events и повторить тот же bitstream.
/// Лимит защищает decoder thread от бесконечного цикла при поломанном backend state.
const MAX_CHECK_EVENTS_RETRIES: usize = 4;

/// Начальное разрешение для создания пула кадров.
///
/// VA-API декодер требует выходные буферы до первого decode call.
/// Используем 1920x1080 как разумный default — при смене разрешения
/// пул будет пересоздан через `FormatChanged` event.
const INITIAL_WIDTH: u32 = 1920;
const INITIAL_HEIGHT: u32 = 1080;

/// Итог обработки pending decoder events.
///
/// Отдельный report нужен, чтобы retry-loop мог видеть, был ли `FormatChanged`,
/// и писать диагностический лог без знания деталей `FrameReady` import path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DecoderDrainReport {
    /// Количество событий, прочитанных через `next_event()`.
    events_count: usize,

    /// Был ли среди событий `FormatChanged`.
    format_changed: bool,
}

/// Накопительные counters backend-local suppressed reclaim queue.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SuppressedReclaimCounters {
    /// Сколько suppressed/candidate handles было поставлено в reclaim queue.
    total_enqueued: usize,

    /// Сколько handles было освобождено через readiness/sync reclaim path.
    total_reclaimed: usize,

    /// Сколько раз non-blocking readiness query вернул ошибку.
    query_errors: usize,

    /// Сколько blocking `sync()` вызвано forced reclaim path-ом.
    forced_sync_count: usize,

    /// Сколько раз queue была полной при попытке enqueue.
    ring_full_count: usize,
}

/// Backend-local snapshot ёмкости suppressed reclaim queue для диагностики Session E.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SuppressedReclaimCapacity {
    /// Сколько output descriptors доступно VA decoder-у.
    surface_pool_frames: usize,

    /// Сколько frames может временно удерживать backend ready queue.
    ready_queue_frames: usize,

    /// Настроенный bound suppressed reclaim queue после env override/normalization.
    max_suppressed_reclaim_frames: usize,
}

impl SuppressedReclaimCapacity {
    /// Собирает snapshot из runtime config без чтения mutable decoder state.
    #[must_use]
    fn from_runtime_config(runtime_config: VaapiDecoderRuntimeConfig) -> Self {
        Self::new(
            runtime_config.surface_pool_frames,
            runtime_config.ready_queue_frames,
            runtime_config.max_suppressed_reclaim_frames,
        )
    }

    /// Нормализует capacity поля так же, как decoder hot path нормализует queue bound.
    #[must_use]
    fn new(
        surface_pool_frames: usize,
        ready_queue_frames: usize,
        max_suppressed_reclaim_frames: usize,
    ) -> Self {
        let normalized_surface_pool_frames = surface_pool_frames.max(1);
        let normalized_ready_queue_frames = ready_queue_frames.max(1);
        let normalized_reclaim_frames = max_suppressed_reclaim_frames
            .max(1)
            .min(normalized_surface_pool_frames);

        Self {
            surface_pool_frames: normalized_surface_pool_frames,
            ready_queue_frames: normalized_ready_queue_frames,
            max_suppressed_reclaim_frames: normalized_reclaim_frames,
        }
    }

    /// Оценивает свободные места в reclaim queue без тяжёлых VA/resource-pool запросов.
    #[must_use]
    fn approximate_available_reclaim_slots(self, current_depth: usize) -> usize {
        self.max_suppressed_reclaim_frames
            .saturating_sub(current_depth)
    }

    /// Оценивает surface headroom, который не отдан suppressed reclaim queue.
    #[must_use]
    fn approximate_reserved_surface_headroom_frames(self) -> usize {
        self.surface_pool_frames
            .saturating_sub(self.max_suppressed_reclaim_frames)
    }
}

/// Snapshot одного reclaim pass-а вместе с накопительными counters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ReclaimPassReport {
    /// Текущая глубина suppressed reclaim queue после pass-а.
    current_depth: usize,

    /// Сколько output descriptors доступно VA decoder-у.
    surface_pool_frames: usize,

    /// Сколько frames может временно удерживать backend ready queue.
    ready_queue_frames: usize,

    /// Настроенный bound suppressed reclaim queue.
    max_suppressed_reclaim_frames: usize,

    /// Приблизительно свободные места в reclaim queue после pass-а.
    approximate_available_reclaim_slots: usize,

    /// Приблизительный reserve output descriptors вне reclaim queue.
    approximate_reserved_surface_headroom_frames: usize,

    /// Сколько handles было поставлено в очередь за жизнь decoder-а.
    total_enqueued: usize,

    /// Сколько handles было освобождено за жизнь decoder-а.
    total_reclaimed: usize,

    /// Сколько readiness query errors было за жизнь decoder-а.
    query_errors: usize,

    /// Сколько forced `sync()` было за жизнь decoder-а.
    forced_sync_count: usize,

    /// Сколько overflow ситуаций было за жизнь decoder-а.
    ring_full_count: usize,

    /// Сколько handles освободил конкретный reclaim pass.
    reclaimed_this_pass: usize,

    /// Сколько query errors увидел конкретный reclaim pass.
    query_errors_this_pass: usize,
}

impl SuppressedReclaimCounters {
    /// Собирает report без раскрытия mutable counters наружу decoder-а.
    fn report(
        self,
        current_depth: usize,
        capacity: SuppressedReclaimCapacity,
        reclaimed_this_pass: usize,
        query_errors_this_pass: usize,
    ) -> ReclaimPassReport {
        ReclaimPassReport {
            current_depth,
            surface_pool_frames: capacity.surface_pool_frames,
            ready_queue_frames: capacity.ready_queue_frames,
            max_suppressed_reclaim_frames: capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots: capacity
                .approximate_available_reclaim_slots(current_depth),
            approximate_reserved_surface_headroom_frames: capacity
                .approximate_reserved_surface_headroom_frames(),
            total_enqueued: self.total_enqueued,
            total_reclaimed: self.total_reclaimed,
            query_errors: self.query_errors,
            forced_sync_count: self.forced_sync_count,
            ring_full_count: self.ring_full_count,
            reclaimed_this_pass,
            query_errors_this_pass,
        }
    }
}

/// Политика обработки `FrameReady` events во время drain-а decoder-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecoderEventDrainPolicy {
    /// Обычный playback/EOF путь: кадры экспортируются и получают seek generation.
    Publish {
        /// Seek generation, к которому принадлежат все кадры этого drain-а.
        generation: u64,
    },

    /// Seek flush/reconfigure cleanup: кадры из backend tail не публикуются.
    Discard {
        /// Короткая причина для diagnostics/logging.
        reason: &'static str,
    },
}

/// Сводка одного вызова decode state machine.
///
/// `decode()` использует её только для логов и решения, был ли packet пропущен
/// как recoverable parse error. Все готовые кадры по-прежнему лежат в `ready_queue`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct DecodeLoopReport {
    /// Сколько раз packet был отправлен в active codec adapter.
    attempts: usize,

    /// Количество обработанных decoder events за весь вызов.
    events_count: usize,

    /// Был ли обработан `FormatChanged`.
    format_changed: bool,

    /// Сколько байт backend сообщил как обработанные.
    processed_bytes: usize,

    /// Был ли packet пропущен из-за recoverable parse error.
    skipped_packet: bool,

    /// Был ли packet оставлен на retry из-за временной нехватки output buffers.
    output_backpressured: bool,

    /// Суммарное время внутри submit attempts.
    submit_elapsed: Duration,

    /// Суммарное время внутри drain events.
    drain_elapsed: Duration,
}

impl DecodeLoopReport {
    /// Добавляет результат одного drain прохода к общей сводке.
    fn record_drain(&mut self, drain_report: DecoderDrainReport, drain_elapsed: Duration) {
        self.events_count += drain_report.events_count;
        self.format_changed |= drain_report.format_changed;
        self.drain_elapsed += drain_elapsed;
    }
}

/// Active accurate-seek output floor, которым владеет VAAPI decoder backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActivePrerollOutputFloor {
    /// Seek generation, для которой backend подавляет pre-floor output.
    generation: u64,

    /// Минимальный PTS, который можно публиковать наружу.
    floor_pts: Duration,

    /// Нужно ли сохранить последний кадр перед floor как EOF fallback.
    retain_latest_before_floor: bool,

    /// Был ли уже успешно опубликован кадр с `pts >= floor_pts`.
    target_or_after_published: bool,
}

impl ActivePrerollOutputFloor {
    /// Создаёт backend-local state из нейтрального video-core policy.
    fn from_policy(policy: VideoPrerollOutputFloor) -> Self {
        Self {
            generation: policy.generation,
            floor_pts: policy.floor_pts,
            retain_latest_before_floor: policy.retain_latest_before_floor,
            target_or_after_published: false,
        }
    }

    /// Проверяет, совпадает ли active floor с новым запросом без сброса флагов.
    fn matches_policy(self, policy: VideoPrerollOutputFloor) -> bool {
        self.generation == policy.generation
            && self.floor_pts == policy.floor_pts
            && self.retain_latest_before_floor == policy.retain_latest_before_floor
    }
}

/// Накопительные diagnostics counters для decoder-side preroll output floor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PrerollOutputFloorCounters {
    /// Сколько pre-floor кадров подавлено без DMA-BUF export/publish.
    suppressed_frame_count: u64,

    /// Сколько раз более поздний pre-floor candidate заменил старый.
    candidate_replaced_count: u64,

    /// Сколько fallback candidates было опубликовано через EOF promotion.
    candidate_promoted_count: u64,

    /// Сколько раз backend впервые дошёл до publish кадра `pts >= floor`.
    target_published_after_floor_count: u64,
}

/// Pure state accurate-seek output floor без concrete VA handle payload-а.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PrerollOutputFloorState {
    /// Active floor отсутствует вне accurate-seek preroll window.
    active: Option<ActivePrerollOutputFloor>,

    /// Counters остаются накопительными, чтобы debug logs были полезны вручную.
    counters: PrerollOutputFloorCounters,
}

impl PrerollOutputFloorState {
    /// Устанавливает новый floor или возвращает `Unchanged` для того же policy.
    fn set_floor(&mut self, policy: VideoPrerollOutputFloor) -> VideoPrerollOutputFloorResult {
        if self
            .active
            .is_some_and(|active_floor| active_floor.matches_policy(policy))
        {
            return VideoPrerollOutputFloorResult::Unchanged;
        }

        self.active = Some(ActivePrerollOutputFloor::from_policy(policy));
        VideoPrerollOutputFloorResult::Applied
    }

    /// Очищает active floor, сохраняя distinct `Unchanged` для generation mismatch.
    fn clear_floor(
        &mut self,
        clear: VideoPrerollOutputFloorClear,
    ) -> VideoPrerollOutputFloorResult {
        let Some(active_floor) = self.active else {
            return VideoPrerollOutputFloorResult::Unchanged;
        };

        let should_clear = match clear {
            VideoPrerollOutputFloorClear::MatchingGeneration(generation) => {
                active_floor.generation == generation
            }
            VideoPrerollOutputFloorClear::Any => true,
        };

        if !should_clear {
            return VideoPrerollOutputFloorResult::Unchanged;
        }

        self.active = None;
        VideoPrerollOutputFloorResult::Cleared
    }

    /// Возвращает active floor только если кадр принадлежит той же generation и ниже floor.
    fn suppression_floor(
        self,
        frame_pts: Duration,
        generation: u64,
    ) -> Option<ActivePrerollOutputFloor> {
        let active_floor = self.active?;

        if active_floor.generation == generation && frame_pts < active_floor.floor_pts {
            Some(active_floor)
        } else {
            None
        }
    }

    /// Проверяет, что кадр закрывает retained candidate для той же active generation.
    fn is_target_or_after_for_active_floor(self, frame_pts: Duration, generation: u64) -> bool {
        let Some(active_floor) = self.active else {
            return false;
        };

        active_floor.generation == generation && frame_pts >= active_floor.floor_pts
    }

    /// Проверяет, является ли кадр первым успешно опубликованным target-or-after output.
    fn record_target_or_after_published(&mut self, frame_pts: Duration, generation: u64) -> bool {
        let Some(active_floor) = self.active.as_mut() else {
            return false;
        };

        if active_floor.generation != generation
            || frame_pts < active_floor.floor_pts
            || active_floor.target_or_after_published
        {
            return false;
        }

        active_floor.target_or_after_published = true;
        self.counters.target_published_after_floor_count = self
            .counters
            .target_published_after_floor_count
            .saturating_add(1);
        true
    }

    /// Учитывает suppression без связывания pure state с VA handle ownership.
    fn record_suppressed_frame(&mut self) {
        self.counters.suppressed_frame_count =
            self.counters.suppressed_frame_count.saturating_add(1);
    }

    /// Учитывает замену pre-floor candidate-а более поздним кадром.
    fn record_candidate_replaced(&mut self) {
        self.counters.candidate_replaced_count =
            self.counters.candidate_replaced_count.saturating_add(1);
    }

    /// Проверяет, нужно ли EOF drain публиковать fallback candidate.
    fn should_promote_candidate(self, generation: u64) -> bool {
        let Some(active_floor) = self.active else {
            return false;
        };

        active_floor.generation == generation && !active_floor.target_or_after_published
    }

    /// Учитывает успешную EOF promotion и закрывает повторную promotion для generation.
    fn record_candidate_promoted(&mut self, generation: u64) {
        self.counters.candidate_promoted_count =
            self.counters.candidate_promoted_count.saturating_add(1);

        if let Some(active_floor) = self.active.as_mut() {
            if active_floor.generation == generation {
                active_floor.target_or_after_published = true;
            }
        }
    }
}

/// Metadata fallback candidate-а без concrete VA handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrerollFallbackCandidateMetadata {
    /// PTS candidate-а, по которому выбирается самый поздний pre-floor frame.
    pts: Duration,

    /// Seek generation candidate-а; promotion разрешена только для matching generation.
    generation: u64,
}

/// Последний pre-floor frame, который можно promoted на EOF без target-or-after output.
struct PrerollFallbackCandidate<T> {
    /// Concrete handle остаётся backend-local payload-ом.
    handle: T,

    /// Metadata нужна для generation checks/logs без чтения handle после move.
    metadata: PrerollFallbackCandidateMetadata,
}

impl<T> PrerollFallbackCandidate<T> {
    /// Собирает candidate из handle и metadata одной generation.
    fn new(handle: T, pts: Duration, generation: u64) -> Self {
        Self {
            handle,
            metadata: PrerollFallbackCandidateMetadata { pts, generation },
        }
    }
}

/// Решение по сохранению нового pre-floor candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrerollFallbackCandidateDecision {
    /// Candidate slot пуст, incoming становится baseline fallback-ом.
    StoreFirst,

    /// Incoming позже или равен старому PTS и заменяет текущий candidate.
    ReplaceExisting,

    /// Incoming старее текущего candidate-а и сразу отбрасывается.
    DropIncoming,
}

/// Выбирает самый поздний pre-floor candidate без знания concrete handle type.
fn preroll_fallback_candidate_decision<T>(
    current_candidate: Option<&PrerollFallbackCandidate<T>>,
    incoming_metadata: PrerollFallbackCandidateMetadata,
) -> PrerollFallbackCandidateDecision {
    let Some(current_candidate) = current_candidate else {
        return PrerollFallbackCandidateDecision::StoreFirst;
    };

    if incoming_metadata.pts >= current_candidate.metadata.pts {
        PrerollFallbackCandidateDecision::ReplaceExisting
    } else {
        PrerollFallbackCandidateDecision::DropIncoming
    }
}

/// Итог обработки packet-а для decoder thread-а.
pub(crate) enum VaapiDecodePacketOutcome {
    /// Packet принят adapter-ом; готовый frame может появиться сразу или позже.
    Accepted(Option<Box<DecodedFrame>>),

    /// Packet сохранён adapter-ом как pending и должен быть повторён после release/backpressure.
    OutputBackpressured,
}

/// Минимальный интерфейс, который нужен retry state machine.
///
/// Production implementation живёт на `VaapiVideoDecoder`, а unit test подставляет
/// fake driver без VA-API, чтобы проверить контракт `CheckEvents -> retry same packet`.
trait DecoderRetryDriver {
    /// Короткое имя codec-а для diagnostics retry-loop-а.
    fn codec_label(&self) -> &'static str;

    /// Отправляет один packet в backend decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError>;

    /// Обрабатывает все pending decoder events.
    fn drain_events(&mut self, policy: DecoderEventDrainPolicy) -> Result<DecoderDrainReport>;
}

/// Выполняет submit packet с bounded retry после `CheckEvents`.
///
/// Важно: `CheckEvents` не означает, что packet consumed. По контракту `cros-codecs`
/// вызывающий код обязан обработать события и повторить `decode()` с теми же данными.
fn run_decode_with_event_retry<D>(
    driver: &mut D,
    timestamp_us: u64,
    packet_data: &[u8],
    keyframe: bool,
    generation: u64,
) -> Result<DecodeLoopReport>
where
    D: DecoderRetryDriver + ?Sized,
{
    let pts_ms = timestamp_us / 1000;
    let mut report = DecodeLoopReport::default();
    let codec_label = driver.codec_label();
    let decode_hints = VaapiPacketDecodeHints {
        inject_parameter_sets: keyframe,
    };

    loop {
        report.attempts += 1;
        let attempt = report.attempts;
        let submit_start = std::time::Instant::now();
        let submit_result = driver.submit_packet(timestamp_us, packet_data, decode_hints);
        report.submit_elapsed += submit_start.elapsed();

        match submit_result {
            Ok(processed_bytes) => {
                report.processed_bytes = processed_bytes;
                trace!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    processed_bytes = processed_bytes,
                    "decode() accepted bitstream"
                );
                let drain_start = std::time::Instant::now();
                let drain_report =
                    driver.drain_events(DecoderEventDrainPolicy::Publish { generation })?;
                report.record_drain(drain_report, drain_start.elapsed());
                return Ok(report);
            }
            Err(VaapiAdapterDecodeError::CheckEvents) => {
                let drain_start = std::time::Instant::now();
                let drain_report =
                    driver.drain_events(DecoderEventDrainPolicy::Publish { generation })?;
                let format_changed = drain_report.format_changed;
                report.record_drain(drain_report, drain_start.elapsed());

                if attempt > MAX_CHECK_EVENTS_RETRIES {
                    return Err(anyhow::anyhow!(
                        "Decoder repeatedly requested event drain after {attempt} attempts"
                    ));
                }

                debug!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    format_changed = format_changed,
                    codec = codec_label,
                    "retrying same packet after decoder event drain"
                );
            }
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(needed)) => {
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    needed = needed,
                    "Decoder out of output buffers"
                );
                let drain_start = std::time::Instant::now();
                let drain_report =
                    driver.drain_events(DecoderEventDrainPolicy::Publish { generation })?;
                report.record_drain(drain_report, drain_start.elapsed());
                report.output_backpressured = true;
                return Ok(report);
            }
            Err(VaapiAdapterDecodeError::ParseFrameError(message)) => {
                report.skipped_packet = true;
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    codec = codec_label,
                    %message,
                    "parse error, skipping packet"
                );
                return Ok(report);
            }
            Err(error) => {
                warn!(
                    pts_ms = pts_ms,
                    keyframe = keyframe,
                    attempt = attempt,
                    error = ?error,
                    codec = codec_label,
                    "Decode error"
                );
                return Err(anyhow::Error::from(error));
            }
        }
    }
}

/// Typed контракт decoded surface, который VA-API backend отдаёт renderer boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodedSurfaceContract {
    /// Pixel format renderer boundary.
    format: DecodedPixelFormat,

    /// Bit depth decoded samples.
    bit_depth: BitDepth,

    /// Chroma subsampling decoded frame-а.
    chroma: ChromaSubsampling,
}

/// Фатальное нарушение zero-copy video boundary contract.
///
/// Любой decoded video frame нельзя безопасно отправлять в CPU fallback: так
/// pipeline скрывает отсутствие production DMA-BUF export/materialization и ломает
/// диагностику плавности. Поэтому такие ошибки останавливают decoder thread.
#[derive(Debug)]
struct ZeroCopyContractViolation {
    /// Человекочитаемое объяснение конкретной причины отказа.
    detail: String,
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

impl ZeroCopyContractViolation {
    /// Создаёт ошибку zero-copy boundary с понятной причиной для лога/UI.
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl VaapiSurfaceLifecycleError {
    /// Создаёт ошибку lifecycle boundary с понятной причиной для лога/UI.
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ZeroCopyContractViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for ZeroCopyContractViolation {}

impl std::fmt::Display for VaapiSurfaceLifecycleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for VaapiSurfaceLifecycleError {}

/// Проверяет, что decode error требует остановить decoder thread.
pub(crate) fn is_fatal_decoder_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ZeroCopyContractViolation>().is_some()
        || error.downcast_ref::<VaapiSurfaceLifecycleError>().is_some()
}

/// Создаёт typed anyhow error для фатальной zero-copy boundary ошибки.
fn zero_copy_contract_violation(detail: impl Into<String>) -> anyhow::Error {
    ZeroCopyContractViolation::new(detail).into()
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

/// Проверяет fullness reclaim queue без чтения decoder fields извне.
fn is_suppressed_reclaim_queue_full(
    current_depth: usize,
    max_suppressed_reclaim_frames: usize,
) -> bool {
    current_depth >= max_suppressed_reclaim_frames.max(1)
}

impl DecodedSurfaceContract {
    /// Создаёт контракт для текущего production NV12 path.
    const fn nv12() -> Self {
        Self {
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
        }
    }

    /// Создаёт контракт для P010 zero-copy boundary.
    const fn p010() -> Self {
        Self {
            format: DecodedPixelFormat::P010,
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
        }
    }
}

/// Преобразует adapter decoded format в внешний frame contract.
///
/// Важно: `cros-codecs::DecodedFormat::I010` здесь приходит из VA `P010`
/// image format mapping. Для renderer boundary это не planar I010 upload path,
/// а P010 DMA-BUF zero-copy contract.
fn decoded_contract_for_stream_format(
    decoded_format: VaapiDecodedFormat,
) -> Result<DecodedSurfaceContract> {
    match decoded_format {
        VaapiDecodedFormat::Nv12 | VaapiDecodedFormat::I420 => Ok(DecodedSurfaceContract::nv12()),
        VaapiDecodedFormat::I010 => Ok(DecodedSurfaceContract::p010()),
        other => Err(anyhow::anyhow!(
            "Unsupported decoded stream format for VA-API renderer boundary: {:?}",
            other
        )),
    }
}

/// Преобразует VA RT format в тот же внешний frame contract.
fn decoded_contract_for_rt_format(rt_format: u32) -> Result<DecodedSurfaceContract> {
    match rt_format {
        VA_RT_FORMAT_YUV420 => Ok(DecodedSurfaceContract::nv12()),
        VA_RT_FORMAT_YUV420_10 => Ok(DecodedSurfaceContract::p010()),
        other => Err(anyhow::anyhow!(
            "Unsupported VA RT format for VA-API renderer boundary: {:#x}",
            other
        )),
    }
}

/// Преобразует decoded output format из `StreamInfo` в VA RT format для surface pool.
fn rt_format_for_decoded_format(decoded_format: VaapiDecodedFormat) -> Result<u32> {
    match decoded_format {
        VaapiDecodedFormat::Nv12 | VaapiDecodedFormat::I420 => Ok(VA_RT_FORMAT_YUV420),
        VaapiDecodedFormat::I010 => Ok(VA_RT_FORMAT_YUV420_10),
        VaapiDecodedFormat::I012 => Ok(VA_RT_FORMAT_YUV420_12),
        VaapiDecodedFormat::I422 => Ok(VA_RT_FORMAT_YUV422),
        VaapiDecodedFormat::I210 => Ok(VA_RT_FORMAT_YUV422_10),
        VaapiDecodedFormat::I212 => Ok(VA_RT_FORMAT_YUV422_12),
        VaapiDecodedFormat::I444 => Ok(VA_RT_FORMAT_YUV444),
        VaapiDecodedFormat::I410 => Ok(VA_RT_FORMAT_YUV444_10),
        VaapiDecodedFormat::I412 => Ok(VA_RT_FORMAT_YUV444_12),
        other => Err(anyhow::anyhow!(
            "Unsupported VA decoded format for internal surface pool: {:?}",
            other
        )),
    }
}

/// Возвращает backing frame в pool после того, как decoded handle больше не нужен.
fn return_frame_to_pool_from_handle(
    frame_pool: &mut DmaFramePool,
    handle: VaapiDecodedFrameHandle,
) {
    let frame_arc = handle.video_frame();
    drop(handle);

    if let Ok(frame) = Arc::try_unwrap(frame_arc) {
        frame_pool.return_frame(frame);
        trace!("Frame returned to pool");
    } else {
        debug!("Frame still referenced by decoder, cannot return to pool yet");
    }
}

/// Синхронно выбрасывает ready handle из редкого discard/lifecycle path-а.
///
/// Обычный suppressed hot path сюда не заходит: там используется дешёвый
/// `surface_ready()` pass. Этот helper нужен для flush/reconfigure tail events,
/// где очередь reclaim после boundary должна остаться пустой.
fn sync_discard_ready_frame(
    frame_pool: &mut DmaFramePool,
    handle: VaapiDecodedFrameHandle,
    reason: &'static str,
) -> Result<Duration> {
    let pts_ms = handle.timestamp() / 1000;
    let sync_start = std::time::Instant::now();

    if let Err(error) = handle.sync() {
        warn!(
            error = %error,
            pts_ms,
            reason,
            "Discarded ready VA surface sync failed"
        );
        return Err(anyhow::Error::new(VaapiSurfaceLifecycleError::new(
            format!("discard ready VA surface sync failed during {reason}: {error}"),
        )));
    }

    let sync_latency = sync_start.elapsed();
    return_frame_to_pool_from_handle(frame_pool, handle);
    debug!(
        pts_ms,
        reason,
        sync_us = sync_latency.as_micros(),
        "Discarded decoder-ready VA surface after sync"
    );
    Ok(sync_latency)
}

/// Неблокирующе освобождает suppressed handles, чьи VA surfaces уже готовы.
fn reclaim_ready_suppressed_surfaces_from_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reclaim_capacity: SuppressedReclaimCapacity,
) -> Result<ReclaimPassReport> {
    let queued_handles_to_scan = suppressed_reclaim_queue.len();
    let mut reclaimed_this_pass = 0;
    let mut query_errors_this_pass = 0;

    for _ in 0..queued_handles_to_scan {
        let Some(handle) = suppressed_reclaim_queue.pop_front() else {
            break;
        };
        let pts_ms = handle.timestamp() / 1000;

        match handle.surface_ready() {
            Ok(true) => {
                return_frame_to_pool_from_handle(frame_pool, handle);
                suppressed_reclaim_counters.total_reclaimed += 1;
                reclaimed_this_pass += 1;
            }
            Ok(false) => {
                suppressed_reclaim_queue.push_back(handle);
            }
            Err(error) => {
                suppressed_reclaim_counters.query_errors += 1;
                query_errors_this_pass += 1;
                let retained_depth = suppressed_reclaim_queue.len() + 1;
                warn!(
                    error = %error,
                    pts_ms,
                    current_depth = retained_depth,
                    max_suppressed_reclaim_frames =
                        reclaim_capacity.max_suppressed_reclaim_frames,
                    approximate_available_reclaim_slots = reclaim_capacity
                        .approximate_available_reclaim_slots(retained_depth),
                    approximate_reserved_surface_headroom_frames = reclaim_capacity
                        .approximate_reserved_surface_headroom_frames(),
                    "Suppressed VA surface readiness query failed; keeping handle queued"
                );
                suppressed_reclaim_queue.push_back(handle);
            }
        }
    }

    let report = suppressed_reclaim_counters.report(
        suppressed_reclaim_queue.len(),
        reclaim_capacity,
        reclaimed_this_pass,
        query_errors_this_pass,
    );
    if queued_handles_to_scan > 0
        || report.reclaimed_this_pass > 0
        || report.query_errors_this_pass > 0
    {
        debug!(
            scanned_this_pass = queued_handles_to_scan,
            current_depth = report.current_depth,
            max_suppressed_reclaim_frames = report.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots = report.approximate_available_reclaim_slots,
            approximate_reserved_surface_headroom_frames =
                report.approximate_reserved_surface_headroom_frames,
            surface_pool_frames = report.surface_pool_frames,
            ready_queue_frames = report.ready_queue_frames,
            total_enqueued = report.total_enqueued,
            total_reclaimed = report.total_reclaimed,
            query_errors = report.query_errors,
            forced_sync_count = report.forced_sync_count,
            ring_full_count = report.ring_full_count,
            reclaimed_this_pass = report.reclaimed_this_pass,
            query_errors_this_pass = report.query_errors_this_pass,
            "Suppressed reclaim pass completed"
        );
    }

    Ok(report)
}

/// Blocking fallback для одного oldest suppressed handle.
fn force_reclaim_oldest_suppressed_surface_from_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reason: &'static str,
    reclaim_capacity: SuppressedReclaimCapacity,
) -> Result<bool> {
    let Some(handle) = suppressed_reclaim_queue.pop_front() else {
        return Ok(false);
    };
    let pts_ms = handle.timestamp() / 1000;
    suppressed_reclaim_counters.forced_sync_count += 1;
    let sync_start = std::time::Instant::now();

    if let Err(error) = handle.sync() {
        suppressed_reclaim_queue.push_front(handle);
        warn!(
            error = %error,
            pts_ms,
            reason,
            current_depth = suppressed_reclaim_queue.len(),
            max_suppressed_reclaim_frames =
                reclaim_capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots = reclaim_capacity
                .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
            approximate_reserved_surface_headroom_frames = reclaim_capacity
                .approximate_reserved_surface_headroom_frames(),
            "Forced suppressed reclaim sync failed; keeping oldest handle queued"
        );
        return Err(anyhow::Error::new(VaapiSurfaceLifecycleError::new(
            format!("forced suppressed reclaim sync failed during {reason}: {error}"),
        )));
    }

    let sync_latency = sync_start.elapsed();
    return_frame_to_pool_from_handle(frame_pool, handle);
    suppressed_reclaim_counters.total_reclaimed += 1;
    debug!(
        pts_ms,
        reason,
        current_depth = suppressed_reclaim_queue.len(),
        max_suppressed_reclaim_frames = reclaim_capacity.max_suppressed_reclaim_frames,
        approximate_available_reclaim_slots =
            reclaim_capacity.approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
        approximate_reserved_surface_headroom_frames =
            reclaim_capacity.approximate_reserved_surface_headroom_frames(),
        sync_us = sync_latency.as_micros(),
        forced_sync_count = suppressed_reclaim_counters.forced_sync_count,
        "Forced reclaimed oldest suppressed VA surface"
    );
    Ok(true)
}

/// Blocking lifecycle drain для suppressed handles, которые нельзя переносить дальше.
fn force_drain_suppressed_surfaces_from_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reason: &'static str,
    reclaim_capacity: SuppressedReclaimCapacity,
) -> Result<()> {
    let initial_depth = suppressed_reclaim_queue.len();
    if initial_depth > 0 {
        debug!(
            reason,
            initial_depth,
            max_suppressed_reclaim_frames = reclaim_capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots =
                reclaim_capacity.approximate_available_reclaim_slots(initial_depth),
            approximate_reserved_surface_headroom_frames =
                reclaim_capacity.approximate_reserved_surface_headroom_frames(),
            "Force-draining suppressed reclaim queue"
        );
    }

    while force_reclaim_oldest_suppressed_surface_from_queue(
        suppressed_reclaim_queue,
        suppressed_reclaim_counters,
        frame_pool,
        reason,
        reclaim_capacity,
    )? {}

    if initial_depth > 0 {
        debug!(
            reason,
            drained_handles = initial_depth,
            current_depth = suppressed_reclaim_queue.len(),
            max_suppressed_reclaim_frames = reclaim_capacity.max_suppressed_reclaim_frames,
            approximate_available_reclaim_slots = reclaim_capacity
                .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
            approximate_reserved_surface_headroom_frames =
                reclaim_capacity.approximate_reserved_surface_headroom_frames(),
            forced_sync_count = suppressed_reclaim_counters.forced_sync_count,
            total_reclaimed = suppressed_reclaim_counters.total_reclaimed,
            "Force-drained suppressed reclaim queue"
        );
    }

    Ok(())
}

/// Ставит suppressed/candidate handle в bounded reclaim queue.
fn enqueue_suppressed_frame_for_reclaim_in_queue(
    suppressed_reclaim_queue: &mut VecDeque<VaapiDecodedFrameHandle>,
    suppressed_reclaim_counters: &mut SuppressedReclaimCounters,
    frame_pool: &mut DmaFramePool,
    reclaim_capacity: SuppressedReclaimCapacity,
    handle: VaapiDecodedFrameHandle,
    reason: &'static str,
    metadata: PrerollFallbackCandidateMetadata,
) -> Result<()> {
    reclaim_ready_suppressed_surfaces_from_queue(
        suppressed_reclaim_queue,
        suppressed_reclaim_counters,
        frame_pool,
        reclaim_capacity,
    )?;

    let normalized_reclaim_bound = reclaim_capacity.max_suppressed_reclaim_frames;
    if suppressed_reclaim_queue.len() >= normalized_reclaim_bound {
        suppressed_reclaim_counters.ring_full_count += 1;
        debug!(
            reason,
            generation = metadata.generation,
            incoming_pts_ms = metadata.pts.as_millis(),
            current_depth = suppressed_reclaim_queue.len(),
            max_suppressed_reclaim_frames = normalized_reclaim_bound,
            approximate_available_reclaim_slots = reclaim_capacity
                .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
            approximate_reserved_surface_headroom_frames =
                reclaim_capacity.approximate_reserved_surface_headroom_frames(),
            ring_full_count = suppressed_reclaim_counters.ring_full_count,
            "Suppressed reclaim queue full; forcing oldest handle sync"
        );

        if let Err(error) = force_reclaim_oldest_suppressed_surface_from_queue(
            suppressed_reclaim_queue,
            suppressed_reclaim_counters,
            frame_pool,
            reason,
            reclaim_capacity,
        ) {
            suppressed_reclaim_queue.push_back(handle);
            suppressed_reclaim_counters.total_enqueued += 1;
            warn!(
                error = %error,
                reason,
                generation = metadata.generation,
                incoming_pts_ms = metadata.pts.as_millis(),
                current_depth = suppressed_reclaim_queue.len(),
                max_suppressed_reclaim_frames = normalized_reclaim_bound,
                approximate_available_reclaim_slots = reclaim_capacity
                    .approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
                approximate_reserved_surface_headroom_frames = reclaim_capacity
                    .approximate_reserved_surface_headroom_frames(),
                "Kept incoming suppressed handle queued after forced reclaim failure"
            );
            return Err(error);
        }
    }

    suppressed_reclaim_queue.push_back(handle);
    suppressed_reclaim_counters.total_enqueued += 1;
    debug!(
        reason,
        generation = metadata.generation,
        pts_ms = metadata.pts.as_millis(),
        current_depth = suppressed_reclaim_queue.len(),
        max_suppressed_reclaim_frames = normalized_reclaim_bound,
        approximate_available_reclaim_slots =
            reclaim_capacity.approximate_available_reclaim_slots(suppressed_reclaim_queue.len()),
        approximate_reserved_surface_headroom_frames =
            reclaim_capacity.approximate_reserved_surface_headroom_frames(),
        surface_pool_frames = reclaim_capacity.surface_pool_frames,
        ready_queue_frames = reclaim_capacity.ready_queue_frames,
        total_enqueued = suppressed_reclaim_counters.total_enqueued,
        total_reclaimed = suppressed_reclaim_counters.total_reclaimed,
        forced_sync_count = suppressed_reclaim_counters.forced_sync_count,
        ring_full_count = suppressed_reclaim_counters.ring_full_count,
        "Queued suppressed VA handle for readiness-based reclaim"
    );
    Ok(())
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

    /// Очередь готовых к отображению кадров.
    ///
    /// Кадры добавляются при обработке `FrameReady` event и возвращаются
    /// из `decode()` в порядке FIFO.
    ready_queue: VecDeque<DecodedFrame>,

    /// Bounded backend-local лимиты очередей и surface pool-а.
    runtime_config: VaapiDecoderRuntimeConfig,

    /// Suppressed pre-floor handles, которые ждут безопасного backend-local reclaim.
    suppressed_reclaim_queue: VecDeque<VaapiDecodedFrameHandle>,

    /// Накопительная статистика suppressed reclaim queue.
    suppressed_reclaim_counters: SuppressedReclaimCounters,

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

    /// Display orientation текущего stream-а; применяется renderer-ом, не decoder-ом.
    display_orientation: VideoDisplayOrientation,

    /// Active accurate-seek preroll output floor и его counters.
    preroll_output_floor: PrerollOutputFloorState,

    /// Последний pre-floor VA handle для EOF fallback promotion.
    preroll_fallback_candidate: Option<PrerollFallbackCandidate<VaapiDecodedFrameHandle>>,
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
        if let Ok(resource_pool) = resource_pool.lock() {
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

        info!(
            backend_name,
            codec = %adapter_codec,
            surface_pool_frames = runtime_config.surface_pool_frames,
            ready_queue_frames = runtime_config.ready_queue_frames,
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
            ready_queue: VecDeque::new(),
            runtime_config,
            suppressed_reclaim_queue: VecDeque::new(),
            suppressed_reclaim_counters: SuppressedReclaimCounters::default(),
            zero_copy_guards: HashMap::new(),
            zero_copy_success_logged: false,
            diagnostic_tx,
            activity_notifier,
            p010_boundary_verified_logged: false,
            backend_name,
            display_orientation: VideoDisplayOrientation::Identity,
            preroll_output_floor: PrerollOutputFloorState::default(),
            preroll_fallback_candidate: None,
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
        self.resource_pool
            .lock()
            .map_err(|error| {
                anyhow::anyhow!("zero-copy resource pool mutex poisoned during release: {error}")
            })?
            .release_without_gpu_submission(resource_handle)
            .map_err(anyhow::Error::from)?;

        self.release_zero_copy_frame(resource_handle)
    }

    /// Возвращает статистику resource pool для отладки.
    pub fn resource_pool_stats(&self) -> (usize, usize) {
        let resource_pool = self.resource_pool.lock().unwrap();
        (resource_pool.num_slots(), resource_pool.num_in_use())
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
        self.invalidate_idle_resource_pool_after_format_change();

        let adapter =
            VaapiCodecAdapterFactory::create_adapter_for_config(self.display.clone(), config)?;
        self.backend_name = adapter.backend_name();
        self.adapter = adapter;
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
        self.resource_pool
            .lock()
            .map_err(|error| {
                anyhow::anyhow!(
                    "zero-copy resource pool mutex poisoned during decoder reuse ack: {error}"
                )
            })?
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
        reclaim_ready_suppressed_surfaces_from_queue(
            &mut self.suppressed_reclaim_queue,
            &mut self.suppressed_reclaim_counters,
            &mut self.frame_pool,
            SuppressedReclaimCapacity::from_runtime_config(self.runtime_config),
        )
    }

    /// Дешёвый reclaim pass, доступный только decoder thread boundary.
    pub(crate) fn reclaim_suppressed_surfaces_for_thread(&mut self) -> Result<()> {
        self.reclaim_ready_suppressed_surfaces().map(|_| ())
    }

    /// Проверяет, заполнена ли suppressed reclaim queue после cheap reclaim pass-а.
    fn suppressed_reclaim_queue_is_full(&self) -> bool {
        is_suppressed_reclaim_queue_full(
            self.suppressed_reclaim_queue.len(),
            self.runtime_config.max_suppressed_reclaim_frames,
        )
    }

    /// Принудительно дренирует suppressed queue перед lifecycle boundary.
    fn force_drain_suppressed_surfaces(&mut self, reason: &'static str) -> Result<()> {
        force_drain_suppressed_surfaces_from_queue(
            &mut self.suppressed_reclaim_queue,
            &mut self.suppressed_reclaim_counters,
            &mut self.frame_pool,
            reason,
            SuppressedReclaimCapacity::from_runtime_config(self.runtime_config),
        )
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
        enqueue_suppressed_frame_for_reclaim_in_queue(
            &mut self.suppressed_reclaim_queue,
            &mut self.suppressed_reclaim_counters,
            &mut self.frame_pool,
            SuppressedReclaimCapacity::from_runtime_config(self.runtime_config),
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
                let replaced_candidate = self
                    .preroll_fallback_candidate
                    .replace(incoming_candidate)
                    .expect("candidate exists when replacement was selected");
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
                format = %frame.format,
                memory_path = %frame.memory_path,
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
            format = %frame.format,
            bit_depth = %frame.bit_depth,
            chroma = %frame.chroma,
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
    fn invalidate_idle_resource_pool_after_format_change(&mut self) {
        let mut resource_pool = self.resource_pool.lock().unwrap();
        let live_resource_count = resource_pool.num_in_use();

        if live_resource_count == 0 {
            if let Err(error) = resource_pool.invalidate_all() {
                warn!(
                    error = %error,
                    "Failed to invalidate idle zero-copy resource pool after format change"
                );
            }
            return;
        }

        debug!(
            live_resource_count,
            "Keeping resource pool entries because render-owned frames are still live"
        );
    }

    /// Обрабатывает готовый кадр от decoder: sync, DMA-BUF export и resource registration.
    ///
    /// # Аргументы
    /// * `handle` — handle декодированного кадра от cros-codecs.
    /// * `generation` — seek generation, которому принадлежит event drain.
    ///
    /// # Ошибки
    /// Возвращает ошибку если sync, DMA-BUF export или registration не удался.
    fn process_ready_frame(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        generation: u64,
    ) -> Result<()> {
        // Получаем разрешения кадра ДО sync (sync может потребовать mutable borrow).
        let resolution = handle.coded_resolution();
        let display_resolution = handle.display_resolution();
        let timestamp = handle.timestamp();

        debug!(
            pts_ms = timestamp / 1000,
            coded_width = resolution.width,
            coded_height = resolution.height,
            display_width = display_resolution.width,
            display_height = display_resolution.height,
            "FrameReady: processing decoded frame"
        );

        // Шаг 1: Синхронизируемся с завершением GPU-декодирования.
        // `sync()` блокируется до тех пор, пока VA-API не закончит decode job.
        let sync_start = std::time::Instant::now();
        if let Err(e) = handle.sync() {
            warn!(error = %e, "GPU decode sync failed — dropping frame");
            return Err(anyhow::anyhow!("GPU decode sync failed: {}", e));
        }
        let hardware_sync_latency = sync_start.elapsed();
        let decoded_contract = self.current_decoded_contract(&handle)?;

        // Шаг 2: Экспортируем VA surface как DMA-BUF. Отсутствие export-а — fatal contract error.
        let export_start = std::time::Instant::now();
        let dma_buf_image = match handle.dma_buf_image() {
            Ok(Some(dma_buf_image)) => dma_buf_image,
            Ok(None) => {
                return Err(zero_copy_contract_violation(format!(
                    "{} decoded handle does not expose DMA-BUF export",
                    decoded_contract.format
                )));
            }
            Err(export_error) => {
                let export_error_chain = format!("{:#}", export_error);
                warn!(
                    error = %export_error_chain,
                    format = %decoded_contract.format,
                    "VA surface DMA-BUF export failed; CPU fallback is disabled"
                );
                return Err(zero_copy_contract_violation(format!(
                    "{} VA surface DMA-BUF export failed: {}",
                    decoded_contract.format, export_error_chain
                )));
            }
        };
        let dma_buf_export_latency = export_start.elapsed();

        // Шаг 3: Регистрируем descriptor; renderer сам выполнит graphics import.
        let (resource_handle, resource_pool_diagnostics) = {
            let mut resource_pool = self.resource_pool.lock().map_err(|error| {
                zero_copy_contract_violation(format!(
                    "Zero-copy resource pool mutex is poisoned during DMA-BUF registration: {error}"
                ))
            })?;
            let resource_handle = match resource_pool.register_dma_buf_image(dma_buf_image) {
                Ok(resource_handle) => resource_handle,
                Err(registration_error) => {
                    let resource_stats = resource_pool.stats();
                    let registration_error_chain = format!("{:#}", registration_error);
                    warn!(
                        error = %registration_error_chain,
                        format = %decoded_contract.format,
                            zero_copy_capacity = resource_stats.capacity,
                            zero_copy_slots = resource_stats.slots,
                            zero_copy_in_use = resource_stats.in_use,
                            zero_copy_free_surfaces = resource_stats.free_surfaces,
                            zero_copy_waiting_gpu_completion =
                                resource_stats.waiting_gpu_completion,
                            zero_copy_waiting_decoder_reuse =
                                resource_stats.waiting_decoder_reuse,
                            zero_copy_import_failures = resource_stats.import_failures,
                            zero_copy_imports_created = resource_stats.imports_created,
                            zero_copy_imports_reused = resource_stats.imports_reused,
                            zero_copy_imports_replaced = resource_stats.imports_replaced,
                        "DMA-BUF zero-copy resource registration failed; CPU fallback is disabled"
                    );
                    return Err(zero_copy_contract_violation(format!(
                        "{} DMA-BUF zero-copy resource registration failed: {}",
                        decoded_contract.format, registration_error_chain
                    )));
                }
            };
            let resource_stats = resource_pool.stats();
            (
                resource_handle,
                VideoResourcePoolDiagnostics {
                    capacity: resource_stats.capacity,
                    slots: resource_stats.slots,
                    in_use: resource_stats.in_use,
                    free_surfaces: resource_stats.free_surfaces,
                    waiting_gpu_completion: resource_stats.waiting_gpu_completion,
                    waiting_decoder_reuse: resource_stats.waiting_decoder_reuse,
                    import_failures: resource_stats.import_failures,
                    imports_created: resource_stats.imports_created,
                    imports_reused: resource_stats.imports_reused,
                    imports_replaced: resource_stats.imports_replaced,
                },
            )
        };

        if !self.zero_copy_success_logged {
            self.zero_copy_success_logged = true;
            info!(
                handle_id = resource_handle.0,
                format = %decoded_contract.format,
                sync_ms = hardware_sync_latency.as_millis(),
                "Zero-copy DMA-BUF resource registered"
            );
        }
        if decoded_contract.format == DecodedPixelFormat::P010
            && !self.p010_boundary_verified_logged
        {
            self.p010_boundary_verified_logged = true;
            info!(
                handle_id = resource_handle.0,
                width = resolution.width,
                height = resolution.height,
                bit_depth = %decoded_contract.bit_depth,
                chroma = %decoded_contract.chroma,
                "P010 zero-copy boundary verified"
            );
        }

        // Шаг 4: Удерживаем VA handle, пока renderer не подтвердит release после GPU work.
        self.zero_copy_guards.insert(resource_handle.0, handle);

        // Шаг 5: Публикуем только zero-copy frame metadata.
        self.push_ready_frame(DecodedFrame {
            generation,
            pts: Duration::from_micros(timestamp),
            format: decoded_contract.format,
            bit_depth: decoded_contract.bit_depth,
            chroma: decoded_contract.chroma,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: resolution.width,
            height: resolution.height,
            render_width: display_resolution.width,
            render_height: display_resolution.height,
            display_orientation: self.display_orientation,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: VideoFrameDiagnostics {
                timings: VideoFrameTimingDiagnostics {
                    hardware_sync_latency: Some(hardware_sync_latency),
                    dma_buf_export_latency: Some(dma_buf_export_latency),
                    ..VideoFrameTimingDiagnostics::default()
                },
                decoder_ready_queue_depth: None,
                resource_pool: Some(resource_pool_diagnostics),
            },
        })?;

        Ok(())
    }

    /// Выбрасывает готовый backend frame без DMA-BUF export-а и publish-а.
    ///
    /// Seek flush использует этот путь для H.264 DPB tail, который `cros-codecs`
    /// делает видимым через `FrameReady` после `flush()`. Это редкий lifecycle
    /// path: handle синхронизируется и возвращает backing frame сразу, не через
    /// обычную suppressed reclaim queue.
    fn discard_ready_frame(
        &mut self,
        handle: VaapiDecodedFrameHandle,
        reason: &'static str,
    ) -> Result<()> {
        let pts_ms = handle.timestamp() / 1000;
        debug!(
            pts_ms,
            reason, "Discarding decoder-ready frame without DMA-BUF export"
        );
        sync_discard_ready_frame(&mut self.frame_pool, handle, reason)?;
        Ok(())
    }

    /// Обрабатывает `FormatChanged` одинаково для publish и discard drain-а.
    ///
    /// Этот event меняет lifecycle decoder-owned resources, поэтому его нельзя
    /// игнорировать даже при seek flush cleanup.
    fn handle_format_changed_event(&mut self) -> Result<()> {
        info!("Format changed, invalidating resource pool and frame pool");

        // Retained candidate мог относиться к старому surface/format contract.
        self.force_drain_preroll_candidate_and_suppressed_surfaces("format_changed")?;

        // Сначала освобождаем decoder-owned кадры из `ready_queue`.
        // Иначе `invalidate_all()` удалит mappings, а VA handles,
        // удерживаемые этими кадрами, останутся без release path.
        self.release_decoder_owned_ready_frames("format_changed")?;
        self.invalidate_idle_resource_pool_after_format_change();

        // Пересоздаём frame pool под новое разрешение/формат.
        // `stream_info()` уже обновлён внутри cros-codecs перед event-ом.
        if let Some(stream_info) = self.adapter.stream_info() {
            let res = stream_info.coded_resolution;
            let rt_format = match rt_format_for_decoded_format(stream_info.format) {
                Ok(rt_format) => rt_format,
                Err(error) => {
                    warn!(
                        error = %error,
                        decoded_format = ?stream_info.format,
                        "Cannot map decoded format to VA RT format"
                    );
                    return Ok(());
                }
            };
            if let Err(error) = self.frame_pool.resize_with_rt_format(
                res.width,
                res.height,
                self.runtime_config.surface_pool_frames,
                rt_format,
            ) {
                warn!(
                    error = %error,
                    width = res.width,
                    height = res.height,
                    rt_format,
                    "Failed to resize frame pool after format change"
                );
            } else {
                info!(
                    width = res.width,
                    height = res.height,
                    decoded_format = ?stream_info.format,
                    rt_format,
                    "Frame pool resized for new format"
                );
            }
        } else {
            warn!("FormatChanged event without stream_info — cannot resize frame pool");
        }

        Ok(())
    }

    /// Обрабатывает все pending events из `cros-codecs`.
    ///
    /// `FrameReady` либо превращается в `DecodedFrame`, либо отбрасывается
    /// по явно выбранной policy.
    /// `FormatChanged` инвалидирует старые resource descriptors и пересоздаёт frame pool
    /// под новое coded resolution/decoded format.
    fn drain_decoder_events(
        &mut self,
        policy: DecoderEventDrainPolicy,
    ) -> Result<DecoderDrainReport> {
        let mut report = DecoderDrainReport::default();
        self.reclaim_ready_suppressed_surfaces()?;

        while let Some(event) = self.adapter.next_event() {
            report.events_count += 1;
            match event {
                VaapiDecoderEvent::FrameReady(handle) => {
                    let pts_ms = handle.timestamp() / 1000;
                    trace!(pts_ms = pts_ms, "DecoderEvent::FrameReady");
                    match policy {
                        DecoderEventDrainPolicy::Publish { generation } => {
                            let pts = Duration::from_micros(handle.timestamp());
                            if let Some(floor) = self.should_suppress_ready_frame(pts, generation) {
                                self.suppress_ready_frame(handle, generation, floor)?;
                                continue;
                            }

                            if self.should_drop_preroll_candidate_before_publish(pts, generation) {
                                self.drop_preroll_fallback_candidate(
                                    "target_or_after_ready_frame",
                                )?;
                            }
                            if let Err(error) = self.process_ready_frame(handle, generation) {
                                if is_fatal_decoder_error(&error) {
                                    return Err(error);
                                }
                                warn!(error = %error, "Failed to process ready frame");
                            } else {
                                self.record_target_or_after_frame_published(pts, generation)?;
                            }
                        }
                        DecoderEventDrainPolicy::Discard { reason } => {
                            self.discard_ready_frame(handle, reason)?;
                        }
                    }
                }
                VaapiDecoderEvent::FormatChanged => {
                    report.format_changed = true;
                    self.handle_format_changed_event()?;
                }
            }
        }

        self.reclaim_ready_suppressed_surfaces()?;
        Ok(report)
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

        // Шаг 1-2: submit packet и drain pending events.
        // При `CheckEvents` тот же packet отправляется повторно после drain,
        // потому что нижний decoder ещё не обязан был consume-ить bitstream.
        let loop_report = run_decode_with_event_retry(
            self,
            timestamp_us,
            &packet.data,
            packet.keyframe.is_known_keyframe(),
            generation,
        )?;
        if loop_report.output_backpressured {
            return Ok(VaapiDecodePacketOutcome::OutputBackpressured);
        }
        if loop_report.skipped_packet {
            return Ok(VaapiDecodePacketOutcome::Accepted(None));
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
        self.force_drain_preroll_candidate_and_suppressed_surfaces("flush_before_adapter")?;
        self.adapter
            .flush()
            .map_err(|error| anyhow::anyhow!("Flush error: {error}"))?;
        self.drain_decoder_events(DecoderEventDrainPolicy::Discard { reason: "flush" })?;
        self.force_drain_suppressed_surfaces("flush_after_discard")?;
        self.release_decoder_owned_ready_frames("flush")?;
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
                remaining_suppressed_handles = self.suppressed_reclaim_queue.len(),
                "Failed to force-drain suppressed VA surfaces during decoder drop"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::codec_adapter::test_support::{FakeSurfaceReadiness, fake_decoded_frame_handle};

    /// Создаёт neutral decoded frame без реального VA resource-а для ownership tests.
    fn decoded_frame_for_tests(resource_handle: FrameResourceHandle) -> DecodedFrame {
        DecodedFrame {
            generation: 0,
            pts: Duration::ZERO,
            format: DecodedPixelFormat::Nv12,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            memory_path: FrameMemoryPath::DmaBufZeroCopy,
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle,
            diagnostics: VideoFrameDiagnostics::default(),
        }
    }

    /// Создаёт пустой frame pool для reclaim tests без реального VA display.
    fn reclaim_frame_pool_for_tests() -> DmaFramePool {
        DmaFramePool::new(16, 16, 0).expect("test frame pool должен создаваться")
    }

    /// Создаёт diagnostics capacity для focused reclaim tests без runtime decoder-а.
    fn reclaim_capacity_for_tests(
        max_suppressed_reclaim_frames: usize,
    ) -> SuppressedReclaimCapacity {
        SuppressedReclaimCapacity::new(
            max_suppressed_reclaim_frames.max(1),
            1,
            max_suppressed_reclaim_frames,
        )
    }

    /// Проверяет, что конкретный drop reason ставит handle в reclaim queue.
    fn assert_reclaim_enqueue_for_reason(reason: &'static str) {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let initial_free_frames = frame_pool.num_free();
        let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));

        enqueue_suppressed_frame_for_reclaim_in_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(4),
            handle,
            reason,
            PrerollFallbackCandidateMetadata {
                pts: Duration::from_millis(700),
                generation: 31,
            },
        )
        .expect("enqueue suppressed handle должен пройти");

        assert_eq!(
            suppressed_reclaim_queue.len(),
            1,
            "drop path должен поставить handle в reclaim queue"
        );
        assert_eq!(
            frame_pool.num_free(),
            initial_free_frames,
            "enqueue не должен освобождать surface сразу"
        );
        assert!(
            !sync_called.get(),
            "обычный enqueue не должен вызывать blocking sync"
        );
        assert_eq!(suppressed_reclaim_counters.total_enqueued, 1);
        assert_eq!(suppressed_reclaim_counters.total_reclaimed, 0);
    }

    /// Проверяет, что default bound выводится из surface accounting, а не из малого magic limit.
    #[test]
    fn suppressed_reclaim_default_bound_uses_surface_accounting() {
        let default_bound = default_max_suppressed_reclaim_frames(
            DEFAULT_DECODER_SURFACE_POOL_FRAMES,
            DEFAULT_DECODER_READY_QUEUE_FRAMES,
        );

        assert_eq!(
            default_bound,
            DEFAULT_DECODER_SURFACE_POOL_FRAMES
                - SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES
                - SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES
                - SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES
                - SUPPRESSED_RECLAIM_MARGIN_FRAMES
        );
        assert!(
            default_bound > 12,
            "default pool 24 не должен снова закреплять bound в диапазоне 8..12 без замеров"
        );

        let capacity = SuppressedReclaimCapacity::new(
            DEFAULT_DECODER_SURFACE_POOL_FRAMES,
            DEFAULT_DECODER_READY_QUEUE_FRAMES,
            default_bound,
        );
        assert_eq!(
            capacity.approximate_available_reclaim_slots(0),
            default_bound,
            "диагностика должна показывать стартовую ёмкость reclaim queue"
        );
        assert_eq!(
            capacity.approximate_reserved_surface_headroom_frames(),
            SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES
                + SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES
                + SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES
                + SUPPRESSED_RECLAIM_MARGIN_FRAMES,
            "диагностика должна показывать surface headroom вне reclaim queue"
        );
    }

    /// Проверяет pre-submit decision: full reclaim queue должна остановить submit.
    #[test]
    fn pre_submit_full_reclaim_queue_backpressures_before_packet_submit() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let (busy_handle, sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        suppressed_reclaim_queue.push_back(busy_handle);

        let report = reclaim_ready_suppressed_surfaces_from_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(1),
        )
        .expect("pre-submit reclaim должен оставить busy handle queued");

        assert!(
            !sync_called.get(),
            "pre-submit full check не должен делать forced sync"
        );
        assert_eq!(report.current_depth, 1);
        assert_eq!(report.max_suppressed_reclaim_frames, 1);
        assert_eq!(report.approximate_available_reclaim_slots, 0);
        assert!(
            is_suppressed_reclaim_queue_full(
                report.current_depth,
                report.max_suppressed_reclaim_frames,
            ),
            "full queue после cheap reclaim должна дать OutputBackpressured до adapter submit"
        );
        assert!(
            !is_suppressed_reclaim_queue_full(1, 2),
            "queue с запасом не должна блокировать submit"
        );
        assert!(
            is_suppressed_reclaim_queue_full(1, 0),
            "нулевой bound нормализуется до 1 и не создаёт unbounded queue"
        );
    }

    /// Проверяет pre-submit reclaim: ready handles освобождают место до submit.
    #[test]
    fn pre_submit_reclaim_that_frees_ready_handles_allows_packet_submit() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let (ready_handle, sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(true));
        suppressed_reclaim_queue.push_back(ready_handle);

        let report = reclaim_ready_suppressed_surfaces_from_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(1),
        )
        .expect("pre-submit reclaim должен освободить ready handle");

        assert_eq!(report.current_depth, 0);
        assert_eq!(report.max_suppressed_reclaim_frames, 1);
        assert_eq!(report.approximate_available_reclaim_slots, 1);
        assert!(
            !is_suppressed_reclaim_queue_full(
                report.current_depth,
                report.max_suppressed_reclaim_frames,
            ),
            "после reclaim submit снова разрешён"
        );
        assert!(
            !sync_called.get(),
            "pre-submit reclaim остаётся non-blocking"
        );
    }

    /// Проверяет discard helper: flush tail frame синхронизируется и не попадает в reclaim queue.
    #[test]
    fn sync_discard_ready_frame_syncs_and_returns_handle_to_pool() {
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let initial_free_frames = frame_pool.num_free();
        let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));

        sync_discard_ready_frame(&mut frame_pool, handle, "flush")
            .expect("flush discard должен sync-нуть ready tail frame");

        assert!(
            sync_called.get(),
            "discard path обязан вызвать blocking sync перед release"
        );
        assert_eq!(
            frame_pool.num_free(),
            initial_free_frames + 1,
            "discard path должен вернуть backing frame в pool"
        );
    }

    /// Test-only event type для проверки retry state machine без реальной VA-API.
    enum FakeDecoderEvent {
        /// Имитирует `DecoderEvent::FormatChanged`.
        FormatChanged,

        /// Имитирует `DecoderEvent::FrameReady` с исходным PTS.
        FrameReady { pts: Duration },
    }

    /// Fake decoded frame после publish policy.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeReadyFrame {
        /// PTS, который пришёл из fake event-а.
        pts: Duration,

        /// Generation, назначенный event drain policy.
        generation: u64,
    }

    /// Минимальный fake-драйвер для `CheckEvents -> drain -> retry same packet`.
    struct FakeRetryDriver {
        /// Заранее заданные ответы fake `decode()`.
        decode_results: VecDeque<std::result::Result<usize, VaapiAdapterDecodeError>>,

        /// Пакеты событий, которые вернёт каждый fake drain.
        drain_batches: VecDeque<Vec<FakeDecoderEvent>>,

        /// История submit-ов: timestamp и копия bitstream-а.
        submissions: Vec<(u64, Vec<u8>)>,

        /// Очередь fake ready frames, по которой проверяем publish policy.
        ready_frames: VecDeque<FakeReadyFrame>,

        /// PTS кадров, отброшенных discard policy.
        discarded_pts: Vec<Duration>,
    }

    impl FakeRetryDriver {
        /// Создаёт fake-драйвер с управляемыми decode/drain шагами.
        fn new(
            decode_results: Vec<std::result::Result<usize, VaapiAdapterDecodeError>>,
            drain_batches: Vec<Vec<FakeDecoderEvent>>,
        ) -> Self {
            Self {
                decode_results: VecDeque::from(decode_results),
                drain_batches: VecDeque::from(drain_batches),
                submissions: Vec::new(),
                ready_frames: VecDeque::new(),
                discarded_pts: Vec::new(),
            }
        }
    }

    impl DecoderRetryDriver for FakeRetryDriver {
        /// Возвращает codec label fake-драйвера для retry diagnostics.
        fn codec_label(&self) -> &'static str {
            "fake"
        }

        /// Записывает submit и возвращает следующий заранее заданный результат.
        fn submit_packet(
            &mut self,
            timestamp_us: u64,
            packet_data: &[u8],
            _decode_hints: VaapiPacketDecodeHints,
        ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
            self.submissions.push((timestamp_us, packet_data.to_vec()));
            self.decode_results
                .pop_front()
                .expect("fake decode result must be provided")
        }

        /// Обрабатывает один пакет fake events.
        fn drain_events(&mut self, policy: DecoderEventDrainPolicy) -> Result<DecoderDrainReport> {
            let mut report = DecoderDrainReport::default();
            let Some(events) = self.drain_batches.pop_front() else {
                return Ok(report);
            };

            for event in events {
                report.events_count += 1;
                match event {
                    FakeDecoderEvent::FormatChanged => {
                        report.format_changed = true;
                    }
                    FakeDecoderEvent::FrameReady { pts } => match policy {
                        DecoderEventDrainPolicy::Publish { generation } => {
                            self.ready_frames
                                .push_back(FakeReadyFrame { pts, generation });
                        }
                        DecoderEventDrainPolicy::Discard { reason: _ } => {
                            self.discarded_pts.push(pts);
                        }
                    },
                }
            }

            Ok(report)
        }
    }

    /// Проверяет, что `CheckEvents` не теряет стартовый packet после `FormatChanged`.
    #[test]
    fn check_events_format_change_retries_same_packet_and_preserves_pts() {
        let packet_data = vec![0x82, 0x49, 0x83, 0x42];
        let mut driver = FakeRetryDriver::new(
            vec![
                Err(VaapiAdapterDecodeError::CheckEvents),
                Ok(packet_data.len()),
            ],
            vec![
                vec![FakeDecoderEvent::FormatChanged],
                vec![FakeDecoderEvent::FrameReady {
                    pts: Duration::ZERO,
                }],
            ],
        );

        let report = run_decode_with_event_retry(&mut driver, 0, &packet_data, true, 11).unwrap();

        assert_eq!(report.attempts, 2);
        assert_eq!(report.events_count, 2);
        assert!(report.format_changed);
        assert!(!report.skipped_packet);
        assert_eq!(
            driver.submissions,
            vec![(0, packet_data.clone()), (0, packet_data)]
        );
        assert_eq!(
            driver.ready_frames.pop_front(),
            Some(FakeReadyFrame {
                pts: Duration::ZERO,
                generation: 11,
            })
        );
    }

    /// Проверяет, что `NotEnoughOutputBuffers` не маскируется под принятый packet.
    #[test]
    fn not_enough_output_buffers_marks_packet_as_backpressured() {
        let packet_data = vec![0x82, 0x49, 0x83, 0x42];
        let mut driver = FakeRetryDriver::new(
            vec![Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))],
            vec![Vec::new()],
        );

        let report = run_decode_with_event_retry(&mut driver, 0, &packet_data, true, 17).unwrap();

        assert_eq!(report.attempts, 1);
        assert!(report.output_backpressured);
        assert!(!report.skipped_packet);
        assert_eq!(driver.submissions, vec![(0, packet_data)]);
    }

    /// Проверяет seek flush policy: H.264 tail event не попадает в ready queue.
    #[test]
    fn seek_flush_discard_policy_drops_tail_frame_events() {
        let tail_pts = Duration::from_millis(33);
        let mut driver = FakeRetryDriver::new(
            Vec::new(),
            vec![vec![FakeDecoderEvent::FrameReady { pts: tail_pts }]],
        );

        let report = driver
            .drain_events(DecoderEventDrainPolicy::Discard { reason: "flush" })
            .unwrap();

        assert_eq!(report.events_count, 1);
        assert!(!report.format_changed);
        assert!(driver.ready_frames.is_empty());
        assert_eq!(driver.discarded_pts, vec![tail_pts]);
    }

    /// Проверяет EOF drain policy: tail frame публикуется с generation запроса.
    #[test]
    fn eof_drain_publish_policy_keeps_tail_frame_generation() {
        let tail_pts = Duration::from_millis(67);
        let mut driver = FakeRetryDriver::new(
            Vec::new(),
            vec![vec![FakeDecoderEvent::FrameReady { pts: tail_pts }]],
        );

        let report = driver
            .drain_events(DecoderEventDrainPolicy::Publish { generation: 23 })
            .unwrap();

        assert_eq!(report.events_count, 1);
        assert_eq!(
            driver.ready_frames.pop_front(),
            Some(FakeReadyFrame {
                pts: tail_pts,
                generation: 23,
            })
        );
        assert!(driver.discarded_pts.is_empty());
    }

    /// Проверяет burst из нескольких `FrameReady`: весь drain получает одну generation.
    #[test]
    fn burst_frames_drained_in_one_generation_keep_that_generation() {
        let mut driver = FakeRetryDriver::new(
            vec![Ok(4)],
            vec![vec![
                FakeDecoderEvent::FrameReady {
                    pts: Duration::from_millis(10),
                },
                FakeDecoderEvent::FrameReady {
                    pts: Duration::from_millis(20),
                },
                FakeDecoderEvent::FrameReady {
                    pts: Duration::from_millis(30),
                },
            ]],
        );

        let report = run_decode_with_event_retry(&mut driver, 0, &[1, 2, 3, 4], true, 31).unwrap();

        assert_eq!(report.events_count, 3);
        assert_eq!(
            driver.ready_frames.into_iter().collect::<Vec<_>>(),
            vec![
                FakeReadyFrame {
                    pts: Duration::from_millis(10),
                    generation: 31,
                },
                FakeReadyFrame {
                    pts: Duration::from_millis(20),
                    generation: 31,
                },
                FakeReadyFrame {
                    pts: Duration::from_millis(30),
                    generation: 31,
                },
            ]
        );
    }

    /// Проверяет reconfigure/flush scope: release берёт только decoder-owned ready frames.
    #[test]
    fn reconfigure_release_scope_drains_only_decoder_owned_ready_frames() {
        let renderer_owned_handle = FrameResourceHandle(99);
        let mut ready_queue = VecDeque::from([
            decoded_frame_for_tests(FrameResourceHandle(1)),
            decoded_frame_for_tests(FrameResourceHandle(2)),
        ]);

        let decoder_owned_handles = drain_decoder_owned_ready_frame_handles(&mut ready_queue);

        assert_eq!(
            decoder_owned_handles,
            vec![FrameResourceHandle(1), FrameResourceHandle(2)]
        );
        assert!(!decoder_owned_handles.contains(&renderer_owned_handle));
        assert!(ready_queue.is_empty());
    }

    /// Создаёт нейтральный floor policy для focused state-machine тестов.
    fn preroll_floor_policy(
        generation: u64,
        floor_pts: Duration,
        retain_latest_before_floor: bool,
    ) -> VideoPrerollOutputFloor {
        VideoPrerollOutputFloor {
            generation,
            floor_pts,
            retain_latest_before_floor,
        }
    }

    /// Проверяет set/clear states без concrete VA handle-а.
    #[test]
    fn preroll_output_floor_set_repeat_and_clear_preserve_distinct_results() {
        let floor = preroll_floor_policy(7, Duration::from_millis(1_500), true);
        let mut state = PrerollOutputFloorState::default();

        assert_eq!(
            state.set_floor(floor),
            VideoPrerollOutputFloorResult::Applied
        );
        assert_eq!(
            state.set_floor(floor),
            VideoPrerollOutputFloorResult::Unchanged
        );
        assert_eq!(
            state.clear_floor(VideoPrerollOutputFloorClear::MatchingGeneration(8)),
            VideoPrerollOutputFloorResult::Unchanged
        );
        assert_eq!(
            state.clear_floor(VideoPrerollOutputFloorClear::MatchingGeneration(7)),
            VideoPrerollOutputFloorResult::Cleared
        );
        assert_eq!(
            state.clear_floor(VideoPrerollOutputFloorClear::Any),
            VideoPrerollOutputFloorResult::Unchanged
        );
    }

    /// Проверяет, что pre-floor кадр подавляется и не считается published target-ом.
    #[test]
    fn preroll_output_floor_suppresses_pre_floor_frame_without_publish_marker() {
        let mut state = PrerollOutputFloorState::default();
        let floor = preroll_floor_policy(11, Duration::from_millis(1_000), true);
        assert_eq!(
            state.set_floor(floor),
            VideoPrerollOutputFloorResult::Applied
        );

        let suppression_floor = state
            .suppression_floor(Duration::from_millis(900), 11)
            .expect("matching pre-floor frame must be suppressed");
        state.record_suppressed_frame();

        assert_eq!(suppression_floor.generation, 11);
        assert_eq!(suppression_floor.floor_pts, Duration::from_millis(1_000));
        assert_eq!(state.counters.suppressed_frame_count, 1);
        assert!(!state.is_target_or_after_for_active_floor(Duration::from_millis(900), 11));
        assert!(state.should_promote_candidate(11));
    }

    /// Проверяет normal publish path для target-or-after кадра и одноразовый counter.
    #[test]
    fn preroll_output_floor_target_or_after_publishes_and_closes_fallback_window() {
        let mut state = PrerollOutputFloorState::default();
        let floor = preroll_floor_policy(12, Duration::from_millis(1_000), true);
        assert_eq!(
            state.set_floor(floor),
            VideoPrerollOutputFloorResult::Applied
        );

        assert!(
            state
                .suppression_floor(Duration::from_millis(1_000), 12)
                .is_none()
        );
        assert!(state.is_target_or_after_for_active_floor(Duration::from_millis(1_000), 12));
        assert!(state.record_target_or_after_published(Duration::from_millis(1_000), 12));
        assert!(!state.record_target_or_after_published(Duration::from_millis(1_033), 12));
        assert!(!state.should_promote_candidate(12));
        assert_eq!(state.counters.target_published_after_floor_count, 1);
    }

    /// Проверяет, что generation mismatch не подавляет кадр и не трогает active fallback window.
    #[test]
    fn preroll_output_floor_generation_mismatch_publishes_normally() {
        let mut state = PrerollOutputFloorState::default();
        let floor = preroll_floor_policy(21, Duration::from_millis(2_000), true);
        assert_eq!(
            state.set_floor(floor),
            VideoPrerollOutputFloorResult::Applied
        );

        assert!(
            state
                .suppression_floor(Duration::from_millis(1_000), 20)
                .is_none()
        );
        assert!(!state.is_target_or_after_for_active_floor(Duration::from_millis(3_000), 20));
        assert!(!state.record_target_or_after_published(Duration::from_millis(3_000), 20));
        assert!(state.should_promote_candidate(21));
    }

    /// Проверяет candidate policy: backend хранит только самый поздний pre-floor кадр.
    #[test]
    fn preroll_fallback_candidate_replaces_only_with_latest_pts() {
        let mut candidate = Some(PrerollFallbackCandidate::new(
            "older",
            Duration::from_millis(700),
            31,
        ));

        let older_incoming = PrerollFallbackCandidateMetadata {
            pts: Duration::from_millis(650),
            generation: 31,
        };
        assert_eq!(
            preroll_fallback_candidate_decision(candidate.as_ref(), older_incoming),
            PrerollFallbackCandidateDecision::DropIncoming
        );

        let newer_candidate =
            PrerollFallbackCandidate::new("newer", Duration::from_millis(900), 31);
        assert_eq!(
            preroll_fallback_candidate_decision(candidate.as_ref(), newer_candidate.metadata),
            PrerollFallbackCandidateDecision::ReplaceExisting
        );
        let replaced_candidate = candidate
            .replace(newer_candidate)
            .expect("test starts with an older candidate");
        assert_eq!(replaced_candidate.handle, "older");
        assert_eq!(
            candidate.as_ref().map(|candidate| candidate.handle),
            Some("newer")
        );
    }

    /// Проверяет suppressed drop: handle ставится в queue и не освобождается сразу.
    #[test]
    fn suppressed_drop_frame_enqueues_for_reclaim_without_immediate_release() {
        assert_reclaim_enqueue_for_reason("suppress_ready_frame");
    }

    /// Проверяет обычный reclaim pass: ready surface возвращается в frame pool.
    #[test]
    fn ready_suppressed_handle_reclaimed_by_nonblocking_pass() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let initial_free_frames = frame_pool.num_free();
        let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(true));
        suppressed_reclaim_queue.push_back(handle);

        let report = reclaim_ready_suppressed_surfaces_from_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(4),
        )
        .expect("ready reclaim pass должен пройти");

        assert_eq!(suppressed_reclaim_queue.len(), 0);
        assert_eq!(
            frame_pool.num_free(),
            initial_free_frames + 1,
            "ready handle должен вернуться в frame pool"
        );
        assert!(
            !sync_called.get(),
            "non-blocking reclaim pass не должен вызывать sync"
        );
        assert_eq!(report.current_depth, 0);
        assert_eq!(report.reclaimed_this_pass, 1);
        assert_eq!(suppressed_reclaim_counters.total_reclaimed, 1);
    }

    /// Проверяет обычный reclaim pass: busy surface остаётся в queue.
    #[test]
    fn not_ready_suppressed_handle_stays_queued() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let initial_free_frames = frame_pool.num_free();
        let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        suppressed_reclaim_queue.push_back(handle);

        let report = reclaim_ready_suppressed_surfaces_from_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(4),
        )
        .expect("busy reclaim pass должен пройти без blocking sync");

        assert_eq!(suppressed_reclaim_queue.len(), 1);
        assert_eq!(frame_pool.num_free(), initial_free_frames);
        assert!(
            !sync_called.get(),
            "busy reclaim pass не должен вызывать sync"
        );
        assert_eq!(report.current_depth, 1);
        assert_eq!(report.reclaimed_this_pass, 0);
        assert_eq!(suppressed_reclaim_counters.total_reclaimed, 0);
    }

    /// Проверяет query error: handle не теряется и не считается ready.
    #[test]
    fn query_error_keeps_suppressed_handle_queued() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let initial_free_frames = frame_pool.num_free();
        let (handle, sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::QueryError("query failed"));
        suppressed_reclaim_queue.push_back(handle);

        let report = reclaim_ready_suppressed_surfaces_from_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(4),
        )
        .expect("query error должен остаться счетчиком, а не Result error");

        assert_eq!(suppressed_reclaim_queue.len(), 1);
        assert_eq!(frame_pool.num_free(), initial_free_frames);
        assert!(
            !sync_called.get(),
            "query error path не должен делать blocking sync"
        );
        assert_eq!(report.current_depth, 1);
        assert_eq!(report.query_errors_this_pass, 1);
        assert_eq!(suppressed_reclaim_counters.query_errors, 1);
        assert_eq!(suppressed_reclaim_counters.total_reclaimed, 0);
    }

    /// Проверяет ReplaceExisting path: replaced candidate уходит в reclaim queue.
    #[test]
    fn replaced_fallback_candidate_enqueues_for_reclaim() {
        assert_reclaim_enqueue_for_reason("replace_preroll_fallback_candidate");
    }

    /// Проверяет DropIncoming path: older incoming candidate уходит в reclaim queue.
    #[test]
    fn dropped_incoming_older_candidate_enqueues_for_reclaim() {
        assert_reclaim_enqueue_for_reason("drop_incoming_preroll_fallback_candidate");
    }

    /// Проверяет retained-candidate drop path: candidate уходит в reclaim queue.
    #[test]
    fn dropping_retained_candidate_enqueues_for_reclaim() {
        assert_reclaim_enqueue_for_reason("target_or_after_ready_frame");
    }

    /// Проверяет, что bounded reclaim queue не растёт выше configured bound.
    #[test]
    fn suppressed_reclaim_queue_respects_bound() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();

        for generation in 1..=3 {
            let (handle, _sync_called) =
                fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
            enqueue_suppressed_frame_for_reclaim_in_queue(
                &mut suppressed_reclaim_queue,
                &mut suppressed_reclaim_counters,
                &mut frame_pool,
                reclaim_capacity_for_tests(2),
                handle,
                "bound_test",
                PrerollFallbackCandidateMetadata {
                    pts: Duration::from_millis(generation),
                    generation,
                },
            )
            .expect("bounded enqueue должен пройти");
        }

        assert_eq!(suppressed_reclaim_queue.len(), 2);
        assert_eq!(suppressed_reclaim_counters.total_enqueued, 3);
        assert_eq!(suppressed_reclaim_counters.total_reclaimed, 1);
        assert_eq!(suppressed_reclaim_counters.ring_full_count, 1);
        assert_eq!(suppressed_reclaim_counters.forced_sync_count, 1);
    }

    /// Проверяет overflow fallback: sync вызывается только для oldest handle.
    #[test]
    fn overflow_forces_sync_of_oldest_only_as_fallback() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let (oldest_handle, oldest_sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        let (incoming_handle, incoming_sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));

        enqueue_suppressed_frame_for_reclaim_in_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(1),
            oldest_handle,
            "overflow_test_first",
            PrerollFallbackCandidateMetadata {
                pts: Duration::from_millis(1),
                generation: 1,
            },
        )
        .expect("первый enqueue должен пройти без overflow");
        enqueue_suppressed_frame_for_reclaim_in_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(1),
            incoming_handle,
            "overflow_test_second",
            PrerollFallbackCandidateMetadata {
                pts: Duration::from_millis(2),
                generation: 2,
            },
        )
        .expect("overflow enqueue должен освободить oldest и принять incoming");

        assert_eq!(suppressed_reclaim_queue.len(), 1);
        assert!(
            oldest_sync_called.get(),
            "overflow должен forced-sync только oldest handle"
        );
        assert!(
            !incoming_sync_called.get(),
            "incoming handle нельзя sync-ать как обычную per-frame policy"
        );
        assert_eq!(suppressed_reclaim_counters.ring_full_count, 1);
        assert_eq!(suppressed_reclaim_counters.forced_sync_count, 1);
        assert_eq!(suppressed_reclaim_counters.total_enqueued, 2);
        assert_eq!(suppressed_reclaim_counters.total_reclaimed, 1);
    }

    /// Проверяет classification: forced sync failure должен идти в fatal decoder path.
    #[test]
    fn surface_lifecycle_sync_error_is_fatal_decoder_error() {
        let error = anyhow::Error::new(VaapiSurfaceLifecycleError::new("synthetic sync failure"));

        assert!(
            is_fatal_decoder_error(&error),
            "surface lifecycle errors нельзя продолжать как non-fatal decode warning"
        );
    }

    /// Проверяет flush force-drain: lifecycle boundary не оставляет queued handles.
    #[test]
    fn flush_force_drain_leaves_suppressed_reclaim_queue_empty() {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let (first_handle, first_sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        let (second_handle, second_sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        suppressed_reclaim_queue.push_back(first_handle);
        suppressed_reclaim_queue.push_back(second_handle);

        force_drain_suppressed_surfaces_from_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            "flush",
            reclaim_capacity_for_tests(4),
        )
        .expect("flush force-drain должен очистить queue");

        assert!(suppressed_reclaim_queue.is_empty());
        assert!(first_sync_called.get());
        assert!(second_sync_called.get());
        assert_eq!(suppressed_reclaim_counters.forced_sync_count, 2);
        assert_eq!(suppressed_reclaim_counters.total_reclaimed, 2);
    }

    /// Проверяет lifecycle cleanup retained candidate-а и queue перед сменой stream/resources.
    fn assert_lifecycle_force_drain_clears_candidate_and_queue(reason: &'static str) {
        let mut suppressed_reclaim_queue = VecDeque::new();
        let mut suppressed_reclaim_counters = SuppressedReclaimCounters::default();
        let mut frame_pool = reclaim_frame_pool_for_tests();
        let (candidate_handle, candidate_sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        let (queued_handle, queued_sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));
        let mut candidate = Some(PrerollFallbackCandidate::new(
            candidate_handle,
            Duration::from_millis(10),
            7,
        ));
        suppressed_reclaim_queue.push_back(queued_handle);

        let retained_candidate = candidate
            .take()
            .expect("test starts with retained candidate");
        enqueue_suppressed_frame_for_reclaim_in_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reclaim_capacity_for_tests(4),
            retained_candidate.handle,
            reason,
            retained_candidate.metadata,
        )
        .expect("candidate enqueue перед lifecycle drain должен пройти");
        force_drain_suppressed_surfaces_from_queue(
            &mut suppressed_reclaim_queue,
            &mut suppressed_reclaim_counters,
            &mut frame_pool,
            reason,
            reclaim_capacity_for_tests(4),
        )
        .expect("lifecycle force-drain должен очистить suppressed state");

        assert!(candidate.is_none());
        assert!(suppressed_reclaim_queue.is_empty());
        assert!(candidate_sync_called.get());
        assert!(queued_sync_called.get());
    }

    /// Проверяет configure_stream cleanup перед adapter replacement.
    #[test]
    fn configure_stream_force_drain_leaves_no_stale_suppressed_handles() {
        assert_lifecycle_force_drain_clears_candidate_and_queue("configure_stream");
    }

    /// Проверяет clear output-floor cleanup при Cleared.
    #[test]
    fn clear_preroll_output_floor_force_drain_clears_candidate_and_queue() {
        assert_lifecycle_force_drain_clears_candidate_and_queue("clear_preroll_output_floor");
    }

    /// Проверяет FormatChanged cleanup перед resize/invalidate.
    #[test]
    fn format_change_force_drain_leaves_no_stale_suppressed_handles() {
        assert_lifecycle_force_drain_clears_candidate_and_queue("format_changed");
    }

    /// Проверяет EOF fallback semantics: promotion разрешена ровно один раз.
    #[test]
    fn preroll_fallback_candidate_promotes_exactly_once_after_eof_without_target() {
        let mut state = PrerollOutputFloorState::default();
        let floor = preroll_floor_policy(41, Duration::from_millis(5_000), true);
        assert_eq!(
            state.set_floor(floor),
            VideoPrerollOutputFloorResult::Applied
        );

        assert!(state.should_promote_candidate(41));
        if state.should_promote_candidate(41) {
            state.record_candidate_promoted(41);
        }

        assert_eq!(state.counters.candidate_promoted_count, 1);
        assert!(!state.should_promote_candidate(41));
        if state.should_promote_candidate(41) {
            state.record_candidate_promoted(41);
        }
        assert_eq!(state.counters.candidate_promoted_count, 1);
    }

    /// Проверяет, что VA 10-bit 4:2:0 surface становится P010 boundary contract.
    #[test]
    fn va_yuv420_10_rt_format_maps_to_p010_decoded_contract() {
        let contract = decoded_contract_for_rt_format(VA_RT_FORMAT_YUV420_10).unwrap();

        assert_eq!(contract.format, DecodedPixelFormat::P010);
        assert_eq!(contract.bit_depth, BitDepth::Ten);
        assert_eq!(contract.chroma, ChromaSubsampling::Yuv420);
    }

    /// Проверяет `cros-codecs` I010 alias, который VA-API отдаёт для P010 FourCC.
    #[test]
    fn i010_stream_format_maps_to_p010_decoded_contract() {
        let contract = decoded_contract_for_stream_format(VaapiDecodedFormat::I010).unwrap();

        assert_eq!(contract.format, DecodedPixelFormat::P010);
        assert_eq!(contract.bit_depth, BitDepth::Ten);
        assert_eq!(contract.chroma, ChromaSubsampling::Yuv420);
    }

    /// Проверяет, что 12-bit VA format не маскируется под P010.
    #[test]
    fn va_yuv420_12_rt_format_is_not_p010_contract() {
        let error = decoded_contract_for_rt_format(VA_RT_FORMAT_YUV420_12).unwrap_err();

        assert!(
            error.to_string().contains("Unsupported VA RT format"),
            "unexpected error: {error}"
        );
    }
}
