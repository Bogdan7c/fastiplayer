use std::num::NonZeroUsize;
use std::time::Duration;

/// Дефолтный лимит последовательных corrupted packets до fatal ошибки.
pub const DEFAULT_MAX_CONSECUTIVE_CORRUPTED_PACKETS: usize = 64;

/// Дефолтное окно pre-roll для `DecodePointBefore` перед requested target-ом.
pub const DEFAULT_DECODE_POINT_BEFORE_PREROLL: Duration = Duration::from_secs(5);

/// Runtime-настройки demuxer-а, независимые от TOML schema приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxerOptions {
    /// Сколько corrupted packets можно пропустить подряд до остановки demux loop.
    max_consecutive_corrupted_packets: NonZeroUsize,

    /// Насколько раньше requested target-а начинать backend seek для decode-safe final video seek.
    decode_point_before_preroll: Duration,
}

impl DemuxerOptions {
    /// Создаёт настройки с явно валидированным ненулевым лимитом corrupted packets.
    #[must_use]
    pub const fn new(max_consecutive_corrupted_packets: NonZeroUsize) -> Self {
        Self {
            max_consecutive_corrupted_packets,
            decode_point_before_preroll: DEFAULT_DECODE_POINT_BEFORE_PREROLL,
        }
    }

    /// Возвращает `None` для нулевого лимита, потому что ноль превращает любой сбой в fatal.
    #[must_use]
    pub fn from_max_consecutive_corrupted_packets(
        max_consecutive_corrupted_packets: usize,
    ) -> Option<Self> {
        NonZeroUsize::new(max_consecutive_corrupted_packets).map(Self::new)
    }

    /// Возвращает лимит последовательных corrupted packets до fatal ошибки.
    #[must_use]
    pub const fn max_consecutive_corrupted_packets(self) -> usize {
        self.max_consecutive_corrupted_packets.get()
    }

    /// Возвращает pre-roll окно для `DecodePointBefore`.
    #[must_use]
    pub const fn decode_point_before_preroll(self) -> Duration {
        self.decode_point_before_preroll
    }

    /// Задаёт pre-roll окно для `DecodePointBefore` без изменения остальных demux options.
    #[must_use]
    pub const fn with_decode_point_before_preroll(
        mut self,
        decode_point_before_preroll: Duration,
    ) -> Self {
        self.decode_point_before_preroll = decode_point_before_preroll;
        self
    }
}

impl Default for DemuxerOptions {
    /// Возвращает fail-safe политику по умолчанию.
    fn default() -> Self {
        Self {
            max_consecutive_corrupted_packets: NonZeroUsize::new(
                DEFAULT_MAX_CONSECUTIVE_CORRUPTED_PACKETS,
            )
            .expect("default corrupted packet limit must be non-zero"),
            decode_point_before_preroll: DEFAULT_DECODE_POINT_BEFORE_PREROLL,
        }
    }
}
