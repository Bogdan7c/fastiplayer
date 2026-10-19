use std::num::NonZeroUsize;
use std::time::Duration;

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
    /// Сколько байт с конца seekable файла читает tail-проба длительности,
    /// когда начальный индекс не дошёл до EOF (файл больше окна `seek_scan_packets`).
    /// Из этого хвоста берётся максимальный PTS — «точная» длительность контейнера.
    pub tail_probe_bytes: MpegTsLimit,
    /// Сколько байт читает один зонд бисекции далёкого seek-а в поисках PES.
    pub far_seek_probe_bytes: MpegTsLimit,
    /// Запас назад от цели далёкого seek-а: локальное сканирование начинается за
    /// столько до цели, чтобы захватить keyframe не позже цели (длина GOP).
    pub far_seek_preroll: Duration,
    /// Сколько байт AC-3 / E-AC-3 payload-а можно отбросить подряд при поиске
    /// синхронизации (битый кадр, мусор, висящие dependent-ы), прежде чем PID
    /// считается не-AC-3 (typed `Ac3SyncLost`).
    pub ac3_resync_bytes: MpegTsLimit,
    /// Когда скачок PTS/DTS/PCR считается разрывом шкалы (склейка записей),
    /// а не обычным ходом времени. См. `TimestampDiscontinuityThresholds`.
    pub timestamp_discontinuity: TimestampDiscontinuityThresholds,
}

/// Пороги распознавания разрыва timestamps внутри одной программы.
///
/// Склеенные `cat`-ом записи начинают PTS/PCR заново (обычно снова с 1.4 с), а
/// player-core ведёт аудио-часы по проигранным сэмплам и не умеет «прыгать»
/// назад. Поэтому demuxer сам переносит новую запись на продолжение шкалы
/// (`timeline_continuity.rs`). Обычный ход времени при этом трогать нельзя:
/// небольшой скачок вперёд (потеря пакетов в эфире) остаётся настоящим временем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampDiscontinuityThresholds {
    /// Насколько timestamp может уйти назад от предыдущего в том же потоке,
    /// не считаясь разрывом. Decode timestamps монотонны, так что это запас
    /// на неаккуратные muxer-ы, а не на B-кадры (по ним сравнивается DTS).
    pub backward_tolerance: Duration,
    /// Насколько timestamp может уйти вперёд от ожидаемого продолжения,
    /// не считаясь разрывом. Больший скачок — новая шкала, а не пропуск.
    pub forward_threshold: Duration,
}

impl Default for TimestampDiscontinuityThresholds {
    fn default() -> Self {
        Self {
            // В корректном потоке decode timestamps назад не идут совсем;
            // 0.1 с — тот же запас, что у ffmpeg для «DTS ушёл назад».
            // Больше нельзя: склейка короткой записи прыгает назад всего на
            // её длину.
            backward_tolerance: Duration::from_millis(100),
            // Как `-dts_delta_threshold` у ffmpeg для форматов с разрывами
            // (AVFMT_TS_DISCONT): меньший скачок вперёд — реальный пропуск.
            forward_threshold: Duration::from_secs(10),
        }
    }
}

impl MpegTsDemuxOptions {
    /// Связывает initial topology probe с уже проверенным byte budget входного resource-а.
    ///
    /// Ordered HLS segment может разнести один AAC PES между тысячами video TS packets.
    /// Фиксированный default остаётся безопасным для обычного local/stream input, а владелец
    /// bounded resource-а может разрешить parser-у дочитать topology evidence до его конца.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "div_ceil ненулевого byte budget на положительный размер пакета даёт не меньше одного \
                  пакета"
    )]
    pub fn with_initial_probe_byte_budget(mut self, byte_budget: NonZeroUsize) -> Self {
        let packet_budget = byte_budget.get().div_ceil(MPEG_TS_PACKET_BYTES);
        self.initial_probe_packets = MpegTsLimit(
            NonZeroUsize::new(packet_budget)
                .expect("The non-zero byte budget provides at least one packet."),
        );
        self
    }
}

impl Default for MpegTsDemuxOptions {
    fn default() -> Self {
        Self {
            // 4096 TS packets ~= 752 KiB: достаточно для обычного PAT/PMT cadence.
            initial_probe_packets: MpegTsLimit(
                const { NonZeroUsize::new(4_096).expect("non-zero") },
            ),
            // Resync ограничен шестнадцатью transport packets.
            resync_bytes: MpegTsLimit(
                const { NonZeroUsize::new(MPEG_TS_PACKET_BYTES * 16).expect("non-zero") },
            ),
            // PES больше 16 MiB считается повреждённым, а не бесконечно буферизуется.
            pes_bytes: MpegTsLimit(
                const { NonZeroUsize::new(16 * 1024 * 1024).expect("non-zero") },
            ),
            // AU ограничен отдельно: PES boundary не является границей кадра.
            video_access_unit_bytes: MpegTsLimit(
                const { NonZeroUsize::new(32 * 1024 * 1024).expect("non-zero") },
            ),
            // Sparse index не может удерживать больше 8192 anchors.
            index_entries: MpegTsLimit(const { NonZeroUsize::new(8_192).expect("non-zero") }),
            // Один seek не сканирует больше 32768 transport packets.
            seek_scan_packets: MpegTsLimit(const { NonZeroUsize::new(32_768).expect("non-zero") }),
            // 2 MiB с конца файла: хватает на сотни кадров при типичных битрейтах,
            // а чтение остаётся ограниченным (десятки миллисекунд на локальном диске).
            tail_probe_bytes: MpegTsLimit(
                const { NonZeroUsize::new(2 * 1024 * 1024).expect("non-zero") },
            ),
            // Зонд: 512 KiB хватает на PES следующего кадра даже у тяжёлых I-кадров.
            far_seek_probe_bytes: MpegTsLimit(
                const { NonZeroUsize::new(512 * 1024).expect("non-zero") },
            ),
            // GOP вещательных/камерных записей редко длиннее 2-5 с.
            far_seek_preroll: Duration::from_secs(4),
            // 256 KiB ≈ 3 с самого плотного AC-3 (640 кбит/с) и десятки максимальных
            // syncframe-ов (≤ 4 KiB): повреждение кадра восстанавливается за один
            // кадр, а не-AC-3 payload не сканируется бесконечно.
            ac3_resync_bytes: MpegTsLimit(
                const { NonZeroUsize::new(256 * 1024).expect("non-zero") },
            ),
            timestamp_discontinuity: TimestampDiscontinuityThresholds::default(),
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
