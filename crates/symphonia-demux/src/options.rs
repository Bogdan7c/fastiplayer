use std::num::NonZeroUsize;
use std::time::Duration;

/// Дефолтный лимит последовательных corrupted packets до fatal ошибки.
pub const DEFAULT_MAX_CONSECUTIVE_CORRUPTED_PACKETS: usize = 64;

/// Дефолтное окно pre-roll для `DecodePointBefore` перед requested target-ом.
pub const DEFAULT_DECODE_POINT_BEFORE_PREROLL: Duration = Duration::from_secs(5);

/// Максимальный pre-roll, который `DecodePointBefore` может принять без rescue retry.
pub const DEFAULT_DECODE_POINT_BEFORE_MAX_ACCEPTED_PREROLL: Duration = Duration::from_secs(10);

/// Сколько post-seek packets можно прочитать для доказательства video decode point.
pub const DEFAULT_DECODE_POINT_BEFORE_VERIFICATION_PACKET_LIMIT: usize = 64;

/// Runtime-настройки demuxer-а, независимые от TOML schema приложения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DemuxerOptions {
    /// Сколько corrupted packets можно пропустить подряд до остановки demux loop.
    max_consecutive_corrupted_packets: NonZeroUsize,

    /// Насколько раньше requested target-а начинать backend seek для decode-safe final video seek.
    decode_point_before_preroll: Duration,

    /// Насколько далеко до requested target-а можно принять найденный decode-start.
    decode_point_before_max_accepted_preroll: Duration,

    /// Сколько packets после seek можно prebuffer-нуть для проверки selected video track.
    decode_point_before_verification_packet_limit: usize,
}

impl DemuxerOptions {
    /// Создаёт настройки с явно валидированным ненулевым лимитом corrupted packets.
    #[must_use]
    pub const fn new(max_consecutive_corrupted_packets: NonZeroUsize) -> Self {
        Self {
            max_consecutive_corrupted_packets,
            decode_point_before_preroll: DEFAULT_DECODE_POINT_BEFORE_PREROLL,
            decode_point_before_max_accepted_preroll:
                DEFAULT_DECODE_POINT_BEFORE_MAX_ACCEPTED_PREROLL,
            decode_point_before_verification_packet_limit:
                DEFAULT_DECODE_POINT_BEFORE_VERIFICATION_PACKET_LIMIT,
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

    /// Возвращает максимальный принимаемый pre-roll для `DecodePointBefore`.
    #[must_use]
    pub fn decode_point_before_max_accepted_preroll(self) -> Duration {
        self.decode_point_before_max_accepted_preroll
            .max(self.decode_point_before_preroll)
    }

    /// Возвращает лимит post-seek packets для packet-level проверки `DecodePointBefore`.
    #[must_use]
    pub const fn decode_point_before_verification_packet_limit(self) -> usize {
        self.decode_point_before_verification_packet_limit
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

    /// Задаёт максимальный принимаемый pre-roll для `DecodePointBefore`.
    #[must_use]
    pub const fn with_decode_point_before_max_accepted_preroll(
        mut self,
        max_accepted_preroll: Duration,
    ) -> Self {
        self.decode_point_before_max_accepted_preroll = max_accepted_preroll;
        self
    }

    /// Задаёт bounded лимит packet-level проверки `DecodePointBefore`.
    #[must_use]
    pub const fn with_decode_point_before_verification_packet_limit(
        mut self,
        packet_limit: usize,
    ) -> Option<Self> {
        if packet_limit == 0 {
            return None;
        }

        self.decode_point_before_verification_packet_limit = packet_limit;
        Some(self)
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
            decode_point_before_max_accepted_preroll:
                DEFAULT_DECODE_POINT_BEFORE_MAX_ACCEPTED_PREROLL,
            decode_point_before_verification_packet_limit:
                DEFAULT_DECODE_POINT_BEFORE_VERIFICATION_PACKET_LIMIT,
        }
    }
}
