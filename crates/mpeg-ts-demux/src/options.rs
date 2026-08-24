use std::num::NonZeroUsize;

use crate::MpegTsOptionsError;

/// Размер одного MPEG-TS transport packet без M2TS-префикса.
const MPEG_TS_PACKET_BYTES: usize = 188;

/// Named non-zero safety limit; callsites не передают неочевидные голые числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpegTsLimit(NonZeroUsize);

impl MpegTsLimit {
    /// Проверяет, что limit не отключает соответствующую safety boundary.
    pub fn new(value: usize, name: &'static str) -> Result<Self, MpegTsOptionsError> {
        NonZeroUsize::new(value)
            .map(Self)
            .ok_or(MpegTsOptionsError::ZeroLimit { name })
    }

    /// Возвращает проверенное значение.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

/// Все memory/work bounds MPEG-TS parser-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MpegTsDemuxOptions {
    /// Пакеты, доступные initial PAT/PMT discovery.
    pub initial_probe_packets: MpegTsLimit,
    /// Максимум отброшенных bytes за один resync.
    pub resync_bytes: MpegTsLimit,
    /// Максимум bytes одного незавершённого PES.
    pub pes_bytes: MpegTsLimit,
    /// Максимум bytes одного собираемого video access unit между PES boundaries.
    pub video_access_unit_bytes: MpegTsLimit,
    /// Максимум sparse PCR/keyframe anchors.
    pub index_entries: MpegTsLimit,
    /// Максимум TS packets одного on-demand seek scan.
    pub seek_scan_packets: MpegTsLimit,
}

impl MpegTsDemuxOptions {
    /// Связывает initial topology probe с уже проверенным byte budget входного resource-а.
    ///
    /// Ordered HLS segment может разнести один AAC PES между тысячами video TS packets.
    /// Фиксированный default остаётся безопасным для обычного local/stream input, а владелец
    /// bounded resource-а может разрешить parser-у дочитать topology evidence до его конца.
    #[must_use]
    pub fn with_initial_probe_byte_budget(mut self, byte_budget: NonZeroUsize) -> Self {
        let packet_budget = byte_budget.get().div_ceil(MPEG_TS_PACKET_BYTES);
        self.initial_probe_packets = MpegTsLimit(
            NonZeroUsize::new(packet_budget)
                .expect("non-zero byte budget даёт хотя бы один packet"),
        );
        self
    }
}

impl Default for MpegTsDemuxOptions {
    fn default() -> Self {
        Self {
            // 4096 TS packets ~= 752 KiB: достаточно для обычного PAT/PMT cadence.
            initial_probe_packets: MpegTsLimit(NonZeroUsize::new(4_096).expect("non-zero")),
            // Resync ограничен шестнадцатью transport packets.
            resync_bytes: MpegTsLimit(
                NonZeroUsize::new(MPEG_TS_PACKET_BYTES * 16).expect("non-zero"),
            ),
            // PES больше 16 MiB считается повреждённым, а не бесконечно буферизуется.
            pes_bytes: MpegTsLimit(NonZeroUsize::new(16 * 1024 * 1024).expect("non-zero")),
            // AU ограничен отдельно: PES boundary не является границей кадра.
            video_access_unit_bytes: MpegTsLimit(
                NonZeroUsize::new(32 * 1024 * 1024).expect("non-zero"),
            ),
            // Sparse index не может удерживать больше 8192 anchors.
            index_entries: MpegTsLimit(NonZeroUsize::new(8_192).expect("non-zero")),
            // Один seek не сканирует больше 32768 transport packets.
            seek_scan_packets: MpegTsLimit(NonZeroUsize::new(32_768).expect("non-zero")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_probe_byte_budget_rounds_up_to_whole_transport_packets() {
        let options = MpegTsDemuxOptions::default().with_initial_probe_byte_budget(
            NonZeroUsize::new(MPEG_TS_PACKET_BYTES + 1).expect("test byte budget"),
        );

        assert_eq!(options.initial_probe_packets.get(), 2);
    }
}
