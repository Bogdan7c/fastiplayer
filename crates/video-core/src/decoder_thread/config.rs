use std::{num::NonZeroUsize, time::Duration};
/// Production default packet channel capacity между worker и decoder thread.
const DEFAULT_DECODER_PACKET_CHANNEL_FRAMES: usize = 32;

/// Production default decoded frame channel capacity между decoder thread и worker.
const DEFAULT_DECODER_FRAME_CHANNEL_FRAMES: usize = 8;

/// Production default control/release channel capacity decoder thread-а.
const DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES: usize = 32;

/// Production default backend-local ready queue capacity.
const DEFAULT_DECODER_READY_QUEUE_FRAMES: usize = 8;

/// Production default output surface descriptor pool size.
const DEFAULT_DECODER_SURFACE_POOL_FRAMES: usize = 24;

/// Production default output frame pool size для software (host-frame) decode.
///
/// Намеренно меньше hardware surface pool: каждый software-кадр — это полный
/// host-буфер в RAM (для 4K YUV420 ~12 МБ), и удержание десятков таких кадров
/// создаёт давление на пропускную способность общей памяти iGPU, из-за чего
/// растёт стоимость host→GPU upload и проседает playback FPS. 8 даёт декодеру
/// достаточный запас впереди playback, не раздувая резидентный footprint.
const DEFAULT_SOFTWARE_FRAME_POOL_FRAMES: usize = 8;

/// Production default zero-copy external import slot capacity.
const DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 24;

/// Env-переменная для настройки flush timeout-а без перекомпиляции приложения.
const DECODER_FLUSH_TIMEOUT_ENV_VAR: &str = "VIDEOPLAYER_DECODER_FLUSH_TIMEOUT_MS";

/// Production default flush timeout decoder thread-а в миллисекундах.
const DEFAULT_DECODER_FLUSH_TIMEOUT_MS: u64 = 2_000;

/// Software decoder thread budget в нейтральной форме.
///
/// `Auto` хранит намерение "подобрать безопасное число потоков автоматически".
/// Его нужно резолвить через `resolved_thread_count()`, чтобы оставить CPU
/// headroom под render/upload/worker пути. `Fixed` используется только там, где
/// caller уже разрешил budget policy и доказал, что значение положительное и
/// меньше matching playback budget-а. Это намеренно не `usize` в config-структуре,
/// чтобы callsite явно показывал смысл числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftwareDecodeThreadBudget {
    /// Concrete software backend выбирает число потоков сам.
    Auto,

    /// Caller передал уже разрешённый положительный лимит потоков.
    Fixed(NonZeroUsize),
}

impl SoftwareDecodeThreadBudget {
    /// Создаёт автоматический budget без backend-specific деталей на callsite-е.
    #[must_use]
    pub const fn auto() -> Self {
        Self::Auto
    }

    /// Создаёт fixed budget из уже проверенного positive value.
    #[must_use]
    pub const fn fixed(thread_count: NonZeroUsize) -> Self {
        Self::Fixed(thread_count)
    }

    /// Возвращает concrete positive limit, если caller уже разрешил budget.
    #[must_use]
    pub const fn fixed_thread_count(self) -> Option<NonZeroUsize> {
        match self {
            Self::Auto => None,
            Self::Fixed(thread_count) => Some(thread_count),
        }
    }

    /// Резолвит budget в конкретное число потоков software-декода.
    ///
    /// `Auto` = `max(2, доступный параллелизм − 2)`: два hardware thread-а
    /// остаются render/upload/worker путям, иначе полный набор decode worker-ов
    /// вытесняет render-поток и UI-кадры регулярно вылетают за бюджет
    /// (замерено на 4K60 AV1 software: 7.8% → 0.7% кадров сверх 16.7мс).
    #[must_use]
    pub fn resolved_thread_count(self) -> NonZeroUsize {
        match self {
            Self::Auto => {
                let host_parallelism = std::thread::available_parallelism()
                    .map(NonZeroUsize::get)
                    .unwrap_or(1);
                let reserved_for_render_and_worker = 2;
                let auto_threads = host_parallelism
                    .saturating_sub(reserved_for_render_and_worker)
                    .max(2)
                    .min(host_parallelism);
                NonZeroUsize::new(auto_threads)
                    .unwrap_or_else(|| NonZeroUsize::new(1).expect("1 is non-zero"))
            }
            Self::Fixed(thread_count) => thread_count,
        }
    }
}

/// Backend-neutral runtime limits decoder thread-а.
///
/// Тип живёт в `video-core`, чтобы player/session code зависел от contract,
/// а конкретный backend получал эти значения через adapter conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoDecoderThreadConfig {
    /// Packet channel capacity между worker и decoder thread.
    pub packet_channel_frames: usize,

    /// Decoded frame channel capacity между decoder thread и worker.
    pub frame_channel_frames: usize,

    /// Control/release channel capacity для release/flush сообщений.
    pub control_channel_frames: usize,

    /// Backend-local ready queue capacity внутри decoder wrapper-а.
    pub decoder_ready_queue_frames: usize,

    /// Hardware decoder output surface descriptor pool size.
    pub decoder_surface_pool_frames: usize,

    /// Output frame pool size для software (host-frame) decode backends.
    ///
    /// Применяется только software-путём (`video-ffmpeg`): задаёт и decoded-frame
    /// channel, и host resource table, т.е. сколько полных host-кадров может жить
    /// одновременно (channel + present queue + render leases). Hardware backend
    /// (`video-vaapi`) этот лимит игнорирует и использует
    /// `decoder_surface_pool_frames`, потому что VA surface — это дешёвый GPU
    /// descriptor, а не RAM-буфер. Разделение позволяет держать software-пул
    /// маленьким (меньше memory-bandwidth pressure на iGPU) независимо от
    /// hardware surface pool.
    pub software_frame_pool_frames: usize,

    /// Thread budget только для software decoder backends.
    ///
    /// Playback default остаётся `Auto`, чтобы FFmpeg мог выбирать число потоков
    /// сам (`thread_count = 0`). Независимые decode-потребители могут передать
    /// сюда `Fixed`, если их budget уже проверен на уровне владельца runtime.
    pub software_decode_thread_budget: SoftwareDecodeThreadBudget,

    /// Zero-copy external import slot capacity.
    pub zero_copy_surface_pool_slots: usize,

    /// Максимальное время ожидания подтверждения flush от decoder thread.
    pub flush_timeout: Duration,
}

impl VideoDecoderThreadConfig {
    /// Загружает production defaults и overlay flush timeout-а из окружения.
    #[must_use]
    pub fn from_env() -> Self {
        let flush_timeout = match std::env::var(DECODER_FLUSH_TIMEOUT_ENV_VAR) {
            Ok(raw_value) => match Self::parse_flush_timeout(&raw_value) {
                Ok(timeout) => timeout,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        env_var = DECODER_FLUSH_TIMEOUT_ENV_VAR,
                        default_timeout_ms = DEFAULT_DECODER_FLUSH_TIMEOUT_MS,
                        "Invalid decoder flush timeout config; using default"
                    );
                    Self::default_flush_timeout()
                }
            },
            Err(std::env::VarError::NotPresent) => Self::default_flush_timeout(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    env_var = DECODER_FLUSH_TIMEOUT_ENV_VAR,
                    default_timeout_ms = DEFAULT_DECODER_FLUSH_TIMEOUT_MS,
                    "Cannot read decoder flush timeout config; using default"
                );
                Self::default_flush_timeout()
            }
        };

        Self {
            flush_timeout,
            ..Self::default()
        }
    }

    /// Возвращает default timeout как `Duration`.
    const fn default_flush_timeout() -> Duration {
        Duration::from_millis(DEFAULT_DECODER_FLUSH_TIMEOUT_MS)
    }

    /// Парсит env timeout в миллисекундах без изменения process env в тестах.
    pub(super) fn parse_flush_timeout(raw_value: &str) -> anyhow::Result<Duration> {
        let timeout_ms = raw_value.trim().parse::<u64>().map_err(|error| {
            anyhow::anyhow!(
                "expected positive integer milliseconds, got {:?}: {}",
                raw_value,
                error
            )
        })?;
        if timeout_ms == 0 {
            anyhow::bail!("decoder flush timeout must be greater than 0 ms");
        }
        Ok(Duration::from_millis(timeout_ms))
    }

    /// Нормализует direct API values, чтобы startup не получил zero-capacity queues.
    #[must_use]
    pub fn normalized(self) -> Self {
        Self {
            packet_channel_frames: self.packet_channel_frames.max(1),
            frame_channel_frames: self.frame_channel_frames.max(1),
            control_channel_frames: self.control_channel_frames.max(1),
            decoder_ready_queue_frames: self.decoder_ready_queue_frames.max(1),
            decoder_surface_pool_frames: self.decoder_surface_pool_frames.max(1),
            software_frame_pool_frames: self.software_frame_pool_frames.max(1),
            software_decode_thread_budget: self.software_decode_thread_budget,
            zero_copy_surface_pool_slots: self.zero_copy_surface_pool_slots.max(1),
            flush_timeout: self.flush_timeout.max(Duration::from_millis(1)),
        }
    }
}

impl Default for VideoDecoderThreadConfig {
    /// Возвращает production defaults без unbounded очередей.
    fn default() -> Self {
        Self {
            packet_channel_frames: DEFAULT_DECODER_PACKET_CHANNEL_FRAMES,
            frame_channel_frames: DEFAULT_DECODER_FRAME_CHANNEL_FRAMES,
            control_channel_frames: DEFAULT_DECODER_CONTROL_CHANNEL_FRAMES,
            decoder_ready_queue_frames: DEFAULT_DECODER_READY_QUEUE_FRAMES,
            decoder_surface_pool_frames: DEFAULT_DECODER_SURFACE_POOL_FRAMES,
            software_frame_pool_frames: DEFAULT_SOFTWARE_FRAME_POOL_FRAMES,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::Auto,
            zero_copy_surface_pool_slots: DEFAULT_ZERO_COPY_SURFACE_POOL_SLOTS,
            flush_timeout: VideoDecoderThreadConfig::default_flush_timeout(),
        }
    }
}
