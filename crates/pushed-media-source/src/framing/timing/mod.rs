//! Время медиасегментов: какой отрезок времени несёт каждый целый сегмент.
//!
//! Полке сегментов (сессия 03b) нужно знать время каждого принятого сегмента:
//! только так плеер перематывает внутри уже полученных данных, а источник
//! сообщает странице «буферизовано на N секунд вперёд». Этот модуль — чистый
//! разборщик времени: байты на входе, отрезок времени или типизированная ошибка
//! на выходе, никакого состояния потока.
//!
//! Устройство:
//! - [`SegmentTimingContext`] строится один раз на init-сегмент (fMP4 — шкалы
//!   времени дорожек и значения по умолчанию из `trex`; WebM — `TimecodeScale` и
//!   `DefaultDuration` дорожек; MPEG-TS — контекста нет);
//! - [`SegmentTimingContext::media_segment_timing`] читает один целый медиасегмент
//!   ровно в том виде, в каком его выдаёт сканер, и возвращает [`SegmentTiming`].
//!
//! Время — «сырое» время контейнера, ДО `timestampOffset` MSE. Полка применяет
//! смещение сама через [`MediaTimeSpan::shifted_by`]: смещение у полосы может
//! смениться между сегментами, а разборщик о полосе ничего не знает.
//!
//! Страница — враг (инвариант 1 плана): каждая длина и смещение проверяются,
//! арифметика целочисленная с проверкой переполнения, вложенность разбирается
//! явными циклами фиксированной глубины (без рекурсии), память не выделяется
//! пропорционально объявленным в байтах числам.
//!
//! Известное ограничение: MPEG-TS время не размечается ([`SegmentTiming::Untimed`]):
//! метки PES разбросаны по пакетам, и для полки TS пока не нужен (решение 03a —
//! `timestampOffset` для TS и так отвергается).

mod byte_cursor;
mod iso_bmff;
mod matroska;

#[cfg(test)]
mod iso_bmff_tests;
#[cfg(test)]
mod matroska_tests;
#[cfg(test)]
mod robustness_tests;
#[cfg(test)]
mod test_support;

use crate::container::PushedContainer;
use crate::time::TimestampOffset;

use self::iso_bmff::IsoBmffTimingContext;
use self::matroska::MatroskaTimingContext;

/// Микросекунд в секунде: общая единица времени разборщика.
const MICROS_PER_SECOND: i128 = 1_000_000;

/// Момент сырого времени контейнера в целых микросекундах со знаком.
///
/// Знак нужен: время композиции fMP4 (`trun` версии 1) и относительная метка
/// блока WebM бывают отрицательными, а смещение MSE может увести время ниже нуля.
/// Микросекунды — та же единица, что у [`TimestampOffset`], поэтому смещение
/// складывается точно, без округлений.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct MediaTime {
    /// Микросекунды со знаком.
    micros: i64,
}

impl MediaTime {
    /// Момент из целых микросекунд.
    #[must_use]
    pub(crate) const fn from_micros(micros: i64) -> Self {
        Self { micros }
    }

    /// Момент в микросекундах.
    #[must_use]
    pub(crate) const fn as_micros(self) -> i64 {
        self.micros
    }

    /// Момент из позиции плеера (у плеера время неотрицательное; гигантские
    /// значения насыщаются).
    #[must_use]
    pub(crate) fn from_player_position(position: std::time::Duration) -> Self {
        Self::from_micros(i64::try_from(position.as_micros()).unwrap_or(i64::MAX))
    }

    /// Тот же момент на шкале плеера: время раньше нуля прижимается к нулю
    /// (шкала плеера неотрицательна).
    #[must_use]
    pub(crate) fn to_player_time(self) -> media_core::MediaTime {
        let micros = u64::try_from(self.micros).unwrap_or(0);
        media_core::MediaTime::from_duration(std::time::Duration::from_micros(micros))
    }

    /// Переводит число тиков шкалы `ticks_per_second` в микросекунды.
    ///
    /// Округление — вниз (к минус бесконечности) и для начала, и для конца
    /// отрезка: тогда соседние сегменты с общей границей в тиках остаются
    /// соседними и в микросекундах, без дыр и наложений в 1 мкс.
    fn from_ticks(ticks: i128, ticks_per_second: u64) -> Result<Self, SegmentTimingError> {
        let scaled = ticks
            .checked_mul(MICROS_PER_SECOND)
            .ok_or(SegmentTimingError::TimeOutOfRange)?;
        let divisor = i128::from(ticks_per_second);
        if divisor == 0 {
            // Нулевая шкала отвергается при разборе init; здесь — защита от деления на ноль.
            return Err(SegmentTimingError::TimeOutOfRange);
        }
        let micros = scaled.div_euclid(divisor);
        i64::try_from(micros)
            .map(Self::from_micros)
            .map_err(|_| SegmentTimingError::TimeOutOfRange)
    }
}

/// Полуоткрытый отрезок сырого времени сегмента `[start, end)`; `start <= end`.
///
/// Время — время ПОКАЗА (presentation time), как у `buffered` в MSE: у fMP4
/// учтены смещения композиции `trun`, у WebM метки блоков и так время показа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MediaTimeSpan {
    /// Начало (включительно).
    start: MediaTime,
    /// Конец (не включительно).
    end: MediaTime,
}

impl MediaTimeSpan {
    /// Отрезок из границ; `None`, если конец раньше начала.
    #[must_use]
    pub(crate) fn new(start: MediaTime, end: MediaTime) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }

    /// Пустой отрезок в точке (сегмент без кадров).
    #[must_use]
    pub(crate) const fn empty_at(moment: MediaTime) -> Self {
        Self {
            start: moment,
            end: moment,
        }
    }

    /// Начало отрезка.
    #[must_use]
    pub(crate) const fn start(self) -> MediaTime {
        self.start
    }

    /// Конец отрезка (не включительно).
    #[must_use]
    pub(crate) const fn end(self) -> MediaTime {
        self.end
    }

    /// Нет ни одной микросекунды внутри.
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "полка считает пустые отрезки арифметикой наложения; проверка — для тестов разметки"
        )
    )]
    pub(crate) fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Наименьший отрезок, покрывающий оба (дорожки одного сегмента).
    #[must_use]
    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Тот же отрезок после `timestampOffset` полосы.
    ///
    /// Результат может оказаться отрицательным — что делать с такими сегментами
    /// (MSE выбрасывает кадры раньше нуля), решает полка. `None` — переполнение i64.
    #[must_use]
    pub(crate) fn shifted_by(self, offset: TimestampOffset) -> Option<Self> {
        let shift = offset.as_micros();
        Some(Self {
            start: MediaTime::from_micros(self.start.micros.checked_add(shift)?),
            end: MediaTime::from_micros(self.end.micros.checked_add(shift)?),
        })
    }
}

/// Насколько точно известен конец сегмента.
///
/// У fMP4 длительность каждого кадра записана — конец всегда точный. У WebM
/// длительность последнего блока может быть не записана вовсе (ни
/// `BlockDuration`, ни `DefaultDuration`): так пишут многие плееры страниц.
/// Если считать конец кластера началом его последнего блока, между кластерами
/// появляется дыра в один кадр — при < 10 к/с она больше наибольшей дыры полки,
/// и подача встаёт. Поэтому разборщик честно говорит, что про конец известно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentEnd {
    /// Конец записан в контейнере.
    Exact,
    /// Длительность последнего кадра не записана; конец отрезка оценён по шагу
    /// между двумя последними кадрами той же дорожки. Точно известно время до
    /// `certain_end` (начало последнего кадра без длительности).
    Estimated {
        /// Докуда время известно точно.
        certain_end: MediaTime,
    },
    /// Длительность последнего кадра не записана, и мерить не по чему (у дорожки
    /// один кадр в сегменте): конец отрезка — лишь нижняя граница настоящего
    /// конца, сегмент «тянется» до следующего (правило — у полки, `ShelfRules`).
    Open {
        /// Докуда время известно точно.
        certain_end: MediaTime,
    },
}

/// Отрезок времени сегмента вместе с тем, насколько точно известен его конец.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimedSpan {
    /// Отрезок: начало точное, конец — лучшее, что известно (см. `end_kind`).
    span: MediaTimeSpan,
    /// Как получен конец.
    end_kind: SegmentEnd,
}

impl TimedSpan {
    /// Отрезок с точным концом.
    #[must_use]
    pub(crate) const fn exact(span: MediaTimeSpan) -> Self {
        Self {
            span,
            end_kind: SegmentEnd::Exact,
        }
    }

    /// Отрезок с концом из `end_kind`. Точно известный конец не выходит за
    /// границы отрезка (прижимается к ним) — инвариант для полки.
    #[must_use]
    pub(crate) fn new(span: MediaTimeSpan, end_kind: SegmentEnd) -> Self {
        let clamp = |certain_end: MediaTime| certain_end.clamp(span.start(), span.end());
        let end_kind = match end_kind {
            SegmentEnd::Exact => SegmentEnd::Exact,
            SegmentEnd::Estimated { certain_end } => SegmentEnd::Estimated {
                certain_end: clamp(certain_end),
            },
            SegmentEnd::Open { certain_end } => SegmentEnd::Open {
                certain_end: clamp(certain_end),
            },
        };
        Self { span, end_kind }
    }

    /// Отрезок (конец — лучшее известное).
    #[must_use]
    pub(crate) const fn span(self) -> MediaTimeSpan {
        self.span
    }

    /// Как получен конец.
    #[must_use]
    pub(crate) const fn end_kind(self) -> SegmentEnd {
        self.end_kind
    }

    /// Докуда время сегмента известно точно (у точного конца — сам конец).
    #[must_use]
    pub(crate) const fn certain_end(self) -> MediaTime {
        match self.end_kind {
            SegmentEnd::Exact => self.span.end(),
            SegmentEnd::Estimated { certain_end } | SegmentEnd::Open { certain_end } => certain_end,
        }
    }

    /// Тот же отрезок после `timestampOffset` полосы; `None` — переполнение i64.
    #[must_use]
    pub(crate) fn shifted_by(self, offset: TimestampOffset) -> Option<Self> {
        let span = self.span.shifted_by(offset)?;
        let shift = |moment: MediaTime| {
            moment
                .micros
                .checked_add(offset.as_micros())
                .map(MediaTime::from_micros)
        };
        let end_kind = match self.end_kind {
            SegmentEnd::Exact => SegmentEnd::Exact,
            SegmentEnd::Estimated { certain_end } => SegmentEnd::Estimated {
                certain_end: shift(certain_end)?,
            },
            SegmentEnd::Open { certain_end } => SegmentEnd::Open {
                certain_end: shift(certain_end)?,
            },
        };
        Some(Self { span, end_kind })
    }
}

/// Итог разметки одного медиасегмента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SegmentTiming {
    /// Сегмент несёт этот отрезок сырого времени.
    Timed(TimedSpan),
    /// Контейнер полосы время не размечает (MPEG-TS) — это не ошибка.
    Untimed,
}

/// Какая структура контейнера оказалась испорчена — для диагностики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingStructure {
    /// Боксы верхнего уровня init fMP4.
    IsoInitialization,
    /// `moov` и его потомки до `trak`/`mvex`.
    IsoMovie,
    /// `trak`/`mdia` — описание дорожки.
    IsoTrack,
    /// `tkhd` — заголовок дорожки.
    IsoTrackHeader,
    /// `mdhd` — шкала времени дорожки.
    IsoMediaHeader,
    /// `trex` — значения по умолчанию для фрагментов.
    IsoTrackExtends,
    /// Боксы верхнего уровня медиасегмента fMP4.
    IsoMediaSegment,
    /// `moof` — заголовок фрагмента.
    IsoMovieFragment,
    /// `traf` — фрагмент одной дорожки.
    IsoTrackFragment,
    /// `tfhd` — заголовок фрагмента дорожки.
    IsoTrackFragmentHeader,
    /// `tfdt` — время декодирования первого кадра.
    IsoDecodeTime,
    /// `trun` — список кадров.
    IsoTrackRun,
    /// Элементы верхнего уровня init WebM.
    MatroskaInitialization,
    /// `Segment` WebM.
    MatroskaSegment,
    /// `Info` — `TimecodeScale`.
    MatroskaInfo,
    /// `Tracks`/`TrackEntry`.
    MatroskaTracks,
    /// `Cluster` и его дети.
    MatroskaCluster,
    /// `Cluster/Timecode` — время начала кластера.
    MatroskaClusterTimecode,
    /// `BlockGroup`.
    MatroskaBlockGroup,
    /// `SimpleBlock`/`Block` — заголовок блока.
    MatroskaBlock,
    /// Целое беззнаковое число в элементе EBML.
    MatroskaUnsignedInteger,
}

/// Время сегмента прочитать нельзя: байты не описывают корректное время.
///
/// Через [`super::FramingError::Timing`] попадает в класс `UnparsableMedia`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SegmentTimingError {
    /// Структура кончилась раньше, чем объявлено.
    #[error("a {structure:?} structure is truncated")]
    Truncated {
        /// Где.
        structure: TimingStructure,
    },
    /// Заголовок бокса/элемента неверен (размер меньше заголовка, кривое число EBML,
    /// неизвестный размер там, где он недопустим).
    #[error("a {structure:?} structure has an invalid header")]
    InvalidHeader {
        /// Где.
        structure: TimingStructure,
    },
    /// Версия полного бокса, которую спецификация не описывает.
    #[error("a {structure:?} box has unsupported version {version}")]
    UnsupportedVersion {
        /// Где.
        structure: TimingStructure,
        /// Версия из бокса.
        version: u8,
    },
    /// Обязательная часть отсутствует.
    #[error("a required {structure:?} structure is missing")]
    Missing {
        /// Что отсутствует.
        structure: TimingStructure,
    },
    /// Одна и та же часть встретилась дважды там, где она единственна.
    #[error("a {structure:?} structure appears more than once")]
    Duplicate {
        /// Что повторилось.
        structure: TimingStructure,
    },
    /// Шкала времени дорожки fMP4 равна нулю.
    #[error("track {track} declares a zero timescale")]
    ZeroTimescale {
        /// Номер дорожки (`track_ID`).
        track: u32,
    },
    /// `TimecodeScale` WebM равна нулю.
    #[error("the WebM TimecodeScale is zero")]
    ZeroTimecodeScale,
    /// Сегмент ссылается на дорожку, которой нет в init.
    #[error("a media segment refers to track {track}, absent from the initialization segment")]
    UnknownTrack {
        /// Номер дорожки из сегмента.
        track: u64,
    },
    /// Init описывает одну дорожку дважды.
    #[error("the initialization segment declares track {track} twice")]
    DuplicateTrack {
        /// Номер дорожки.
        track: u64,
    },
    /// Объявлено кадров больше, чем помещается в байты `trun`.
    #[error("a track run declares {sample_count} samples, its body holds {available_bytes} bytes")]
    SampleCountExceedsBox {
        /// Объявленное число кадров.
        sample_count: u32,
        /// Байтов на записи кадров.
        available_bytes: usize,
    },
    /// Время не помещается в 64-битные микросекунды.
    #[error("a timestamp does not fit into 64-bit microseconds")]
    TimeOutOfRange,
}

/// Контекст времени одного init-сегмента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SegmentTimingContext {
    /// fMP4: шкалы времени и значения по умолчанию дорожек.
    IsoBmff(IsoBmffTimingContext),
    /// WebM: `TimecodeScale` и длительности по умолчанию дорожек.
    Matroska(MatroskaTimingContext),
    /// MPEG-TS: время не размечается (известное ограничение).
    Untimed,
}

impl SegmentTimingContext {
    /// Читает контекст из init-сегмента полосы (байты ровно как от сканера).
    ///
    /// Для MPEG-TS init-сегмента нет: байты не читаются, контекст — `Untimed`.
    pub(crate) fn from_initialization(
        container: PushedContainer,
        initialization: &[u8],
    ) -> Result<Self, SegmentTimingError> {
        match container {
            PushedContainer::FragmentedMp4 => {
                IsoBmffTimingContext::parse(initialization).map(Self::IsoBmff)
            }
            PushedContainer::WebM => {
                MatroskaTimingContext::parse(initialization).map(Self::Matroska)
            }
            PushedContainer::MpegTs => Ok(Self::Untimed),
        }
    }

    /// Отрезок сырого времени одного целого медиасегмента (`moof`+`mdat` или `Cluster`).
    pub(crate) fn media_segment_timing(
        &self,
        segment: &[u8],
    ) -> Result<SegmentTiming, SegmentTimingError> {
        match self {
            Self::IsoBmff(context) => context
                .segment_span(segment)
                .map(|span| SegmentTiming::Timed(TimedSpan::exact(span))),
            Self::Matroska(context) => context.cluster_span(segment).map(SegmentTiming::Timed),
            Self::Untimed => Ok(SegmentTiming::Untimed),
        }
    }
}
