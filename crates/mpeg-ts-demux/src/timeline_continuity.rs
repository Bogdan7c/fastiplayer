//! Одна монотонная шкала презентации программы поверх разрывов MPEG-TS timestamps.
//!
//! Зачем. Склеенные `cat`-ом записи (и вещательные потоки после переключения
//! источника) начинают PTS/DTS/PCR заново: вторая ffmpeg-запись снова стартует с
//! 1.4 с. Состав программы при этом тот же, поэтому `TracksChanged` (в player-core
//! это seek-подобный полный сброс) — неверная реакция. Но и отдать «сырые»
//! timestamps нельзя: player-core ведёт аудио-часы по проигранным сэмплам, и после
//! скачка назад видео с PTS 1.4 с при часах 10 с считалось бы безнадёжно
//! опоздавшим. Поэтому demuxer переносит новую запись на продолжение шкалы.
//!
//! Модель.
//! - Эпоха — участок входа с одним сдвигом `raw → presentation`. Вход начинается
//!   с эпохи `StreamStart` со сдвигом 0 (обычный файл не меняется ни на тик).
//! - Каждый elementary PID — отдельная «полоса» со своим decode-временем: DTS
//!   видео (монотонен и с B-кадрами) или PTS аудио. Полоса знает, где ожидается
//!   её следующий timestamp: конец последнего аудиокадра или «DTS + шаг кадра».
//! - Разрыв — timestamp ушёл назад дальше `backward_tolerance` или вперёд дальше
//!   `forward_threshold` от ожидаемого. Полоса, увидевшая разрыв первой, создаёт
//!   новую эпоху так, чтобы её timestamp встал ровно в ожидаемое продолжение;
//!   остальные полосы, дойдя до своего разрыва, переходят в ту же эпоху
//!   (один сдвиг на всю запись сохраняет её собственную A/V синхронизацию).
//! - Привязка по звуку точнее: аудио-часы player-core непрерывны по сэмплам,
//!   поэтому новый звук должен начаться ровно в конце старого. Если эпоху создало
//!   видео (его PES обычно идёт первым), её сдвиг предварительный, и первый
//!   аудиокадр эпохи уточняет его (`EpochRevision::OffsetRefined`); seek anchor-ы
//!   этой эпохи владелец (`MpegTsDemuxer`) сдвигает на ту же разницу.
//! - PCR — только опора sparse seek index-а, он эпох не создаёт: после своего
//!   скачка ждёт эпоху от elementary полос.
//! - Ordered segments (HLS) — шкалой владеет transport (`#EXT-X-DISCONTINUITY`),
//!   поэтому для них сдвиг всегда 0 (`TimelineOwnership::OwnedByTransport`).
//!
//! Идентичность эпохи — байтовая позиция пакета, на котором она началась. Она
//! одинакова при index scan-е, обычном чтении и после seek-а, поэтому уточнение
//! сдвига находит anchor-ы, записанные раньше (в т.ч. откатанным scan-ом).

use std::collections::HashMap;
use std::time::Duration;

use crate::options::TimestampDiscontinuityThresholds;

/// Тиков MPEG clock (90 кГц) в одной микросекунде: 90_000 / 1_000_000 = 9 / 100.
const TICKS_PER_MICROSECOND_NUMERATOR: u128 = 9;
const TICKS_PER_MICROSECOND_DENOMINATOR: u128 = 100;

/// Переводит порог из `Duration` в тики 90 кГц с насыщением.
fn duration_to_90khz(duration: Duration) -> i64 {
    let ticks = duration
        .as_micros()
        .saturating_mul(TICKS_PER_MICROSECOND_NUMERATOR)
        / TICKS_PER_MICROSECOND_DENOMINATOR;
    i64::try_from(ticks).unwrap_or(i64::MAX)
}

/// Модуль 33-битного MPEG timestamp-а и его половина (граница «вперёд/назад»).
const TIMESTAMP_MODULUS: i64 = 1 << 33;
const HALF_TIMESTAMP_MODULUS: i64 = 1 << 32;

/// Пороги разрыва в тиках 90 кГц.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiscontinuityWindow {
    backward_tolerance_90khz: i64,
    forward_threshold_90khz: i64,
}

impl DiscontinuityWindow {
    pub(crate) fn new(thresholds: TimestampDiscontinuityThresholds) -> Self {
        Self {
            backward_tolerance_90khz: duration_to_90khz(thresholds.backward_tolerance),
            forward_threshold_90khz: duration_to_90khz(thresholds.forward_threshold),
        }
    }

    /// `presentation` не дальше порогов от прошлого (`last`) и ожидаемого
    /// (`expected`) timestamp-а — это ход времени, а не разрыв.
    fn admits(self, last_90khz: i64, expected_90khz: i64, presentation: i64) -> bool {
        presentation >= last_90khz.saturating_sub(self.backward_tolerance_90khz)
            && presentation <= expected_90khz.saturating_add(self.forward_threshold_90khz)
    }

    /// Два соседних raw 33-битных PTS одного PID-а принадлежат разным шкалам
    /// (началась склеенная запись), а не идут друг за другом. Rollover через
    /// 2^33 — обычный ход времени.
    pub(crate) fn raw_timestamps_restart(self, previous_raw: u64, next_raw: u64) -> bool {
        let previous = (previous_raw as i64).rem_euclid(TIMESTAMP_MODULUS);
        let next = (next_raw as i64).rem_euclid(TIMESTAMP_MODULUS);
        let mut delta = (next - previous).rem_euclid(TIMESTAMP_MODULUS);
        if delta >= HALF_TIMESTAMP_MODULUS {
            delta -= TIMESTAMP_MODULUS;
        }
        !self.admits(0, 0, delta)
    }
}

/// Кто владеет шкалой времени этого входа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineOwnership {
    /// Локальный файл / последовательный поток: разрывы склеивает demuxer.
    RebasedByDemuxer,
    /// Ordered segments: разрывы объявляет transport, timestamps не трогаются.
    OwnedByTransport,
}

/// Чьё decode-время наблюдает полоса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ElementaryClock {
    /// PTS аудиокадра; длительность кадра известна точно.
    Audio,
    /// DTS (или PTS без DTS) access unit-а; шаг кадра выводится из истории.
    Video,
}

/// Детерминированная идентичность эпохи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimelineEpochId {
    /// Начало входа (сдвиг 0).
    StreamStart,
    /// Эпоха началась на elementary unit-е с этим byte offset-ом.
    StartsAtByte(u64),
}

/// Где на шкале презентации лежит эпоха.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EpochPlacement {
    pub(crate) id: TimelineEpochId,
    /// Прибавляется к raw (unwrapped) timestamp-у, чтобы получить presentation.
    pub(crate) offset_90khz: i64,
}

impl EpochPlacement {
    /// Эпоха начала входа без сдвига.
    pub(crate) const STREAM_START: Self = Self {
        id: TimelineEpochId::StreamStart,
        offset_90khz: 0,
    };

    /// Переносит raw timestamp на шкалу презентации.
    pub(crate) const fn present(self, raw_90khz: i64) -> i64 {
        raw_90khz.saturating_add(self.offset_90khz)
    }
}

/// Насколько окончателен сдвиг эпохи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EpochAnchoring {
    /// Начало входа, привязка по звуку или восстановление из seek anchor-а.
    Final,
    /// Создана видео по шагу кадра; первый аудиокадр эпохи уточнит сдвиг.
    ProvisionalFromVideo,
}

#[derive(Debug, Clone, Copy)]
struct TimelineEpoch {
    placement: EpochPlacement,
    anchoring: EpochAnchoring,
}

/// Состояние одной elementary полосы (PID) на шкале презентации.
#[derive(Debug, Clone, Copy)]
struct ElementaryLane {
    clock: ElementaryClock,
    epoch_index: usize,
    /// Последний принятый decode timestamp (presentation).
    last_90khz: i64,
    /// Где ожидается следующий decode timestamp (presentation).
    expected_next_90khz: i64,
    /// Последний правдоподобный шаг видео (для аудио не используется).
    video_step_90khz: Option<i64>,
}

/// Состояние PCR программы.
#[derive(Debug, Clone, Copy)]
enum ProgramClockLane {
    /// PCR ещё не наблюдался с начала/перезапуска шкалы.
    Unobserved,
    /// PCR идёт в эпохе `epoch_index`.
    Following { epoch_index: usize, last_90khz: i64 },
    /// PCR скакнул; ждёт, пока elementary полоса создаст следующую эпоху.
    AwaitingEpoch { after_epoch_index: usize },
}

/// Изменил ли вызов сдвиг уже существовавшей эпохи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpochRevision {
    /// Сдвиг эпохи прежний.
    Unchanged,
    /// Первый аудиокадр уточнил предварительный (видео) сдвиг: всё, что уже
    /// записано в этой эпохе (seek anchor-ы), надо перенести на новый сдвиг.
    OffsetRefined,
}

/// Результат размещения elementary timestamp-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ElementaryPlacement {
    pub(crate) epoch: EpochPlacement,
    pub(crate) revision: EpochRevision,
}

/// Результат размещения PCR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgramClockPlacement {
    /// PCR на шкале презентации.
    OnTimeline {
        pcr_90khz: i64,
        epoch: EpochPlacement,
    },
    /// PCR после скачка, эпоха ещё не известна: anchor не записывать.
    AwaitingEpoch,
}

/// Владелец шкалы презентации одной программы.
#[derive(Debug, Clone)]
pub(crate) struct PresentationTimeline {
    ownership: TimelineOwnership,
    window: DiscontinuityWindow,
    /// Эпохи в порядке появления; всегда хотя бы одна.
    epochs: Vec<TimelineEpoch>,
    lanes: HashMap<u16, ElementaryLane>,
    program_clock: ProgramClockLane,
}

impl PresentationTimeline {
    pub(crate) fn new(
        ownership: TimelineOwnership,
        thresholds: TimestampDiscontinuityThresholds,
    ) -> Self {
        Self {
            ownership,
            window: DiscontinuityWindow::new(thresholds),
            epochs: vec![TimelineEpoch {
                placement: EpochPlacement::STREAM_START,
                anchoring: EpochAnchoring::Final,
            }],
            lanes: HashMap::new(),
            program_clock: ProgramClockLane::Unobserved,
        }
    }

    /// Чтение снова начинается с начала входа (rewind после initial index scan).
    pub(crate) fn restart_from_stream_start(&mut self) {
        self.restart_in_epoch(EpochPlacement::STREAM_START);
    }

    /// Чтение продолжается с seek anchor-а, записанного в эпохе `epoch`.
    ///
    /// История полос сбрасывается: пакеты после anchor-а сравниваются только
    /// между собой, а следующие разрывы создают эпохи заново (с теми же id).
    pub(crate) fn restart_in_epoch(&mut self, epoch: EpochPlacement) {
        self.epochs.clear();
        self.epochs.push(TimelineEpoch {
            placement: epoch,
            anchoring: EpochAnchoring::Final,
        });
        self.lanes.clear();
        self.program_clock = ProgramClockLane::Unobserved;
    }

    /// Размещает decode timestamp elementary unit-а PID-а на шкале.
    ///
    /// `raw_decode_90khz` — unwrapped DTS видео (или PTS без DTS) либо PTS аудио;
    /// `duration_90khz` — длительность аудиокадра, если известна;
    /// `byte_offset` — позиция unit-а (идентичность эпохи, если он её начнёт).
    pub(crate) fn place_elementary(
        &mut self,
        pid: u16,
        clock: ElementaryClock,
        raw_decode_90khz: i64,
        duration_90khz: Option<i64>,
        byte_offset: u64,
    ) -> ElementaryPlacement {
        if self.ownership == TimelineOwnership::OwnedByTransport {
            return unchanged(EpochPlacement::STREAM_START);
        }
        let Some(lane) = self.lanes.get(&pid).copied() else {
            // Новая полоса (старт, seek, поздно появившийся поток) входит в
            // самую свежую эпоху: сравнивать её пока не с чем.
            let epoch_index = self.latest_epoch_index();
            let epoch = self.epochs[epoch_index].placement;
            let first = ElementaryLane {
                clock,
                epoch_index,
                last_90khz: epoch.present(raw_decode_90khz),
                expected_next_90khz: epoch
                    .present(raw_decode_90khz)
                    .saturating_add(duration_90khz.unwrap_or(0)),
                video_step_90khz: None,
            };
            self.lanes.insert(pid, first);
            return unchanged(epoch);
        };
        let current = self.epochs[lane.epoch_index].placement;
        let (epoch_index, revision) = if self.continues(&lane, current.present(raw_decode_90khz)) {
            (lane.epoch_index, EpochRevision::Unchanged)
        } else {
            self.cross_discontinuity(&lane, raw_decode_90khz, byte_offset)
        };
        let epoch = self.epochs[epoch_index].placement;
        let advanced = self.advance_lane(
            lane,
            epoch_index,
            epoch.present(raw_decode_90khz),
            duration_90khz,
        );
        self.lanes.insert(pid, advanced);
        ElementaryPlacement { epoch, revision }
    }

    /// Размещает PCR программы; после его скачка ждёт эпоху от elementary полос.
    pub(crate) fn place_program_clock(&mut self, raw_pcr_90khz: i64) -> ProgramClockPlacement {
        if self.ownership == TimelineOwnership::OwnedByTransport {
            return ProgramClockPlacement::OnTimeline {
                pcr_90khz: raw_pcr_90khz,
                epoch: EpochPlacement::STREAM_START,
            };
        }
        let epoch_index = match self.program_clock {
            ProgramClockLane::Unobserved => Some(self.latest_epoch_index()),
            ProgramClockLane::Following {
                epoch_index,
                last_90khz,
            } => {
                let pcr = self.epochs[epoch_index].placement.present(raw_pcr_90khz);
                if self.window.admits(last_90khz, last_90khz, pcr) {
                    Some(epoch_index)
                } else {
                    self.next_epoch_after(epoch_index)
                        .or_else(|| self.await_epoch_after(epoch_index))
                }
            }
            ProgramClockLane::AwaitingEpoch { after_epoch_index } => self
                .next_epoch_after(after_epoch_index)
                .or_else(|| self.await_epoch_after(after_epoch_index)),
        };
        let Some(epoch_index) = epoch_index else {
            return ProgramClockPlacement::AwaitingEpoch;
        };
        let epoch = self.epochs[epoch_index].placement;
        let pcr_90khz = epoch.present(raw_pcr_90khz);
        self.program_clock = ProgramClockLane::Following {
            epoch_index,
            last_90khz: pcr_90khz,
        };
        ProgramClockPlacement::OnTimeline { pcr_90khz, epoch }
    }

    fn latest_epoch_index(&self) -> usize {
        self.epochs.len().saturating_sub(1)
    }

    fn next_epoch_after(&self, epoch_index: usize) -> Option<usize> {
        let next = epoch_index.saturating_add(1);
        (next < self.epochs.len()).then_some(next)
    }

    /// Переводит PCR в ожидание эпохи; всегда возвращает `None`.
    fn await_epoch_after(&mut self, after_epoch_index: usize) -> Option<usize> {
        self.program_clock = ProgramClockLane::AwaitingEpoch { after_epoch_index };
        None
    }

    fn continues(&self, lane: &ElementaryLane, presentation: i64) -> bool {
        self.window
            .admits(lane.last_90khz, lane.expected_next_90khz, presentation)
    }

    /// Полоса увидела разрыв: переходит в следующую эпоху или создаёт её.
    fn cross_discontinuity(
        &mut self,
        lane: &ElementaryLane,
        raw_decode_90khz: i64,
        byte_offset: u64,
    ) -> (usize, EpochRevision) {
        // Сдвиг, при котором этот timestamp встаёт ровно в ожидаемое продолжение.
        let seamless_offset = lane.expected_next_90khz.saturating_sub(raw_decode_90khz);
        if let Some(next_index) = self.next_epoch_after(lane.epoch_index) {
            let next = self.epochs[next_index];
            if lane.clock == ElementaryClock::Audio
                && next.anchoring == EpochAnchoring::ProvisionalFromVideo
            {
                self.epochs[next_index] = TimelineEpoch {
                    placement: EpochPlacement {
                        id: next.placement.id,
                        offset_90khz: seamless_offset,
                    },
                    anchoring: EpochAnchoring::Final,
                };
                let revision = if seamless_offset == next.placement.offset_90khz {
                    EpochRevision::Unchanged
                } else {
                    EpochRevision::OffsetRefined
                };
                return (next_index, revision);
            }
            if self.continues(lane, next.placement.present(raw_decode_90khz)) {
                return (next_index, EpochRevision::Unchanged);
            }
        }
        let anchoring = match lane.clock {
            ElementaryClock::Audio => EpochAnchoring::Final,
            ElementaryClock::Video => EpochAnchoring::ProvisionalFromVideo,
        };
        self.epochs.push(TimelineEpoch {
            placement: EpochPlacement {
                id: TimelineEpochId::StartsAtByte(byte_offset),
                offset_90khz: seamless_offset,
            },
            anchoring,
        });
        (self.latest_epoch_index(), EpochRevision::Unchanged)
    }

    fn advance_lane(
        &self,
        lane: ElementaryLane,
        epoch_index: usize,
        presentation: i64,
        duration_90khz: Option<i64>,
    ) -> ElementaryLane {
        let video_step_90khz = match lane.clock {
            ElementaryClock::Audio => None,
            ElementaryClock::Video => {
                let step = presentation.saturating_sub(lane.last_90khz);
                // Шаг через разрыв или повтор не описывает частоту кадров.
                (step > 0 && step <= self.window.forward_threshold_90khz)
                    .then_some(step)
                    .or(lane.video_step_90khz)
            }
        };
        let continuation = match lane.clock {
            ElementaryClock::Audio => duration_90khz.unwrap_or(0),
            ElementaryClock::Video => video_step_90khz.unwrap_or(0),
        };
        ElementaryLane {
            clock: lane.clock,
            epoch_index,
            last_90khz: presentation,
            expected_next_90khz: presentation.saturating_add(continuation),
            video_step_90khz,
        }
    }
}

const fn unchanged(epoch: EpochPlacement) -> ElementaryPlacement {
    ElementaryPlacement {
        epoch,
        revision: EpochRevision::Unchanged,
    }
}

#[cfg(test)]
#[path = "tests/timeline_continuity.rs"]
mod tests;
