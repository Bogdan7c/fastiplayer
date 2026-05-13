use std::num::NonZeroUsize;

/// Дефолтный лимит последовательных corrupted packets до fatal ошибки.
pub const DEFAULT_MAX_CONSECUTIVE_CORRUPTED_PACKETS: usize = 64;

/// Runtime-настройки demuxer-а, независимые от TOML schema приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxerOptions {
    /// Сколько corrupted packets можно пропустить подряд до остановки demux loop.
    max_consecutive_corrupted_packets: NonZeroUsize,
}

impl DemuxerOptions {
    /// Создаёт настройки с явно валидированным ненулевым лимитом corrupted packets.
    #[must_use]
    pub const fn new(max_consecutive_corrupted_packets: NonZeroUsize) -> Self {
        Self {
            max_consecutive_corrupted_packets,
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
}

impl Default for DemuxerOptions {
    /// Возвращает fail-safe политику по умолчанию.
    fn default() -> Self {
        Self {
            max_consecutive_corrupted_packets: NonZeroUsize::new(
                DEFAULT_MAX_CONSECUTIVE_CORRUPTED_PACKETS,
            )
            .expect("default corrupted packet limit must be non-zero"),
        }
    }
}
