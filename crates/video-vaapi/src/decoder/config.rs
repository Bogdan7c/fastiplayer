use std::env;

use tracing::{info, warn};

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
pub(super) const SUPPRESSED_RECLAIM_QUEUE_BOUND_OVERRIDE_ENV: &str =
    "RUSTIPLAYER_VAAPI_MAX_SUPPRESSED_RECLAIM_FRAMES";

/// Ориентировочный reserve под codec DPB/reference pressure.
///
/// Это не доказанный максимум для всех codec/driver комбинаций. Значение
/// оставляет default pool 24 достаточно широким для accurate-preroll замеров,
/// а Session E сможет проверить реальные bounds через env override.
pub(super) const SUPPRESSED_RECLAIM_REFERENCE_HEADROOM_FRAMES: usize = 5;

/// Reserve под кадры, которые renderer ещё может удерживать через zero-copy guards.
pub(super) const SUPPRESSED_RECLAIM_RENDER_HELD_HEADROOM_FRAMES: usize = 2;

/// Reserve под target frame, pending publish и небольшой ready-queue backlog.
///
/// Для suppressed preroll эта очередь не является основным sink-ом, поэтому
/// здесь не резервируется вся `ready_queue_frames` capacity.
pub(super) const SUPPRESSED_RECLAIM_READY_PUBLISH_HEADROOM_FRAMES: usize = 2;

/// Минимальный запас от off-by-one/in-flight accounting ошибок.
pub(super) const SUPPRESSED_RECLAIM_MARGIN_FRAMES: usize = 1;

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
    pub(super) fn normalized(self) -> Self {
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
pub(super) fn default_max_suppressed_reclaim_frames(
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
pub(super) fn suppressed_reclaim_bound_with_validation_override(
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
