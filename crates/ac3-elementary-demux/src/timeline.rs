//! Время сырого потока: PTS, длительность и оценка позиции для seek.
//!
//! У elementary stream-а нет timestamp-ов и индекса. Поэтому:
//!
//! - PTS пакета при последовательном чтении — точное накопленное число
//!   сэмплов от первого кадра (кадр AC-3 всегда 1536 сэмплов, E-AC-3 — 256·numblks);
//! - длительность и seek опираются на «байт на сэмпл», измеренное на кадрах
//!   окна открытия ([`ByteRateEstimate`]). Полный проход по файлу ради точного
//!   индекса не делается: на двухчасовом файле это сотни мегабайт чтения при
//!   открытии. Для потоков с постоянной длиной кадра (AC-3 и E-AC-3 при 48 и
//!   32 кГц — так кодируют DVD/Blu-ray/вещание) оценка точна: кадр `k` лежит
//!   ровно на `k·L`, и seek попадает в его начало с точным PTS. При 44.1 кГц
//!   длина кадра чередуется на одно слово, и после далёкого seek-а PTS может
//!   отличаться от «настоящего» на несколько кадров — для звука без видео
//!   это незаметно, а последовательное воспроизведение остаётся точным.

use std::num::NonZeroU64;
use std::time::Duration;

use media_core::TimeBase;

/// Измеренное соотношение «байты : сэмплы» на кадрах окна открытия.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ByteRateEstimate {
    /// Сумма длин измеренных пакетов (вместе с дополнительными программами E-AC-3).
    measured_bytes: NonZeroU64,
    /// Сэмплы (в единицах временной базы дорожки) в тех же пакетах.
    measured_samples: NonZeroU64,
    /// Число измеренных пакетов (для средней длины пакета).
    measured_units: NonZeroU64,
}

impl ByteRateEstimate {
    /// Начинает измерение с первого пакета окна (`None` для пустого пакета).
    pub(crate) fn from_first_unit(unit_bytes: u64, unit_samples: u64) -> Option<Self> {
        Some(Self {
            measured_bytes: NonZeroU64::new(unit_bytes)?,
            measured_samples: NonZeroU64::new(unit_samples)?,
            measured_units: NonZeroU64::MIN,
        })
    }

    /// Добавляет в измерение очередной пакет окна.
    ///
    /// Считается сумма длин пакетов, а не расстояние между ними: повреждённый
    /// кусок внутри окна не должен искажать «байт на сэмпл» самого кодека
    /// (иначе и длительность, и сдвиг часов через дыру считались бы неверно).
    pub(crate) fn add_unit(&mut self, unit_bytes: u64, unit_samples: u64) {
        self.measured_bytes = self.measured_bytes.saturating_add(unit_bytes);
        self.measured_samples = self.measured_samples.saturating_add(unit_samples);
        self.measured_units = self.measured_units.saturating_add(1);
    }

    /// Сколько сэмплов приходится на `bytes` байт потока (с округлением вниз).
    pub(crate) fn samples_for_bytes(self, bytes: u64) -> u64 {
        scale_floor(
            bytes,
            self.measured_samples.get(),
            self.measured_bytes.get(),
        )
    }

    /// Сколько байт потока приходится на `samples` сэмплов (с округлением вниз).
    pub(crate) fn bytes_for_samples(self, samples: u64) -> u64 {
        scale_floor(
            samples,
            self.measured_bytes.get(),
            self.measured_samples.get(),
        )
    }

    /// Средняя длина пакета (вверх: шаг отката seek-а не должен быть короче кадра).
    pub(crate) fn average_unit_bytes(self) -> u64 {
        self.measured_bytes
            .get()
            .div_ceil(self.measured_units.get())
    }
}

/// `value · numerator / denominator` без переполнения и с округлением вниз.
fn scale_floor(value: u64, numerator: u64, denominator: u64) -> u64 {
    let scaled = u128::from(value) * u128::from(numerator) / u128::from(denominator);
    u64::try_from(scaled).unwrap_or(u64::MAX)
}

/// Временная ось одного потока: где он начинается в байтах и как байты переводятся во время.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamTimeline {
    /// Временная база дорожки: 1 / частота первого кадра.
    time_base: TimeBase,
    /// Частота первого кадра.
    sample_rate: NonZeroU64,
    /// Абсолютное смещение первого пакета (после ID3 и мусора).
    data_start_offset: u64,
    /// Размер источника, если известен.
    content_length: Option<u64>,
    /// Оценка «байт на сэмпл».
    estimate: ByteRateEstimate,
}

impl StreamTimeline {
    /// Собирает ось из измерений окна открытия.
    pub(crate) fn new(
        time_base: TimeBase,
        sample_rate: NonZeroU64,
        data_start_offset: u64,
        content_length: Option<u64>,
        estimate: ByteRateEstimate,
    ) -> Self {
        Self {
            time_base,
            sample_rate,
            data_start_offset,
            content_length,
            estimate,
        }
    }

    /// Временная база дорожки.
    pub(crate) const fn time_base(&self) -> TimeBase {
        self.time_base
    }

    /// Начало данных (первый пакет) в байтах источника.
    pub(crate) const fn data_start_offset(&self) -> u64 {
        self.data_start_offset
    }

    /// Переводит длительность пакета из частоты его кадра в единицы дорожки.
    ///
    /// Частота внутри одного файла обычно постоянна; если она всё же сменилась,
    /// время продолжает идти в единицах первой частоты, а не прыгает.
    pub(crate) fn unit_samples_in_track_units(&self, samples: u32, unit_sample_rate: u32) -> u64 {
        match NonZeroU64::new(u64::from(unit_sample_rate)) {
            Some(rate) if rate != self.sample_rate => {
                scale_floor(u64::from(samples), self.sample_rate.get(), rate.get())
            }
            _ => u64::from(samples),
        }
    }

    /// Единицы дорожки → время.
    pub(crate) fn samples_to_duration(&self, samples: u64) -> Duration {
        self.time_base.timestamp_to_duration(samples)
    }

    /// Время → единицы дорожки (вниз: точка не позже запрошенной).
    pub(crate) fn duration_to_samples(&self, position: Duration) -> u64 {
        let nanos = position.as_nanos();
        let samples = nanos * u128::from(self.sample_rate.get()) / 1_000_000_000;
        u64::try_from(samples).unwrap_or(u64::MAX)
    }

    /// Оценка длительности: размер данных × измеренное «сэмплов на байт».
    ///
    /// `None`, если размер источника неизвестен (последовательный поток).
    pub(crate) fn estimated_duration(&self) -> Option<Duration> {
        let payload_bytes = self.content_length?.saturating_sub(self.data_start_offset);
        Some(self.samples_to_duration(self.estimate.samples_for_bytes(payload_bytes)))
    }

    /// Оценка позиции пакета в сэмплах по его абсолютному смещению.
    pub(crate) fn samples_at_offset(&self, offset: u64) -> u64 {
        self.estimate
            .samples_for_bytes(offset.saturating_sub(self.data_start_offset))
    }

    /// Сколько времени (в единицах дорожки) занимали байты, потерянные посреди потока.
    ///
    /// После повреждения часы сдвигаются на оценку пропущенного, а не стоят на
    /// месте: иначе всё, что после дыры, играло бы раньше своей позиции в файле
    /// и расходилось бы с seek-ом и длительностью, которые считаются по байтам.
    pub(crate) fn samples_for_lost_bytes(&self, lost_bytes: u64) -> u64 {
        self.estimate.samples_for_bytes(lost_bytes)
    }

    /// С какого байта искать кадр для цели `target_samples`.
    ///
    /// Оценочная позиция цели лежит внутри кадра `k`; поиск вперёд с точки на
    /// (длина кадра − 1) раньше гарантированно находит начало кадра `k`, а не
    /// `k + 1`. Каждый `backoff_step` отступает ещё на средний кадр — так
    /// исправляется оценка, промахнувшаяся вперёд на потоке с переменной длиной кадра.
    pub(crate) fn seek_search_start(&self, target_samples: u64, backoff_step: u64) -> u64 {
        let average_unit = self.estimate.average_unit_bytes();
        let mut guess = self
            .data_start_offset
            .saturating_add(self.estimate.bytes_for_samples(target_samples));
        if let Some(length) = self.content_length {
            // Цель за концом файла: ищем последний кадр.
            guess = guess.min(length.saturating_sub(1));
        }
        let back_off = average_unit
            .saturating_mul(backoff_step.saturating_add(1))
            .saturating_sub(1);
        guess.saturating_sub(back_off).max(self.data_start_offset)
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::time::Duration;

    use media_core::TimeBase;

    use super::{ByteRateEstimate, StreamTimeline};

    const RATE: u64 = 48_000;

    /// 16 кадров AC-3 по 768 байт × 1536 сэмплов, данные с байта 100, файл 100 + 16·768.
    fn constant_frame_timeline() -> StreamTimeline {
        let mut estimate = ByteRateEstimate::from_first_unit(768, 1536).expect("estimate");
        for _ in 2..=16_u64 {
            estimate.add_unit(768, 1536);
        }
        StreamTimeline::new(
            TimeBase::new(1, 48_000).expect("time base"),
            NonZeroU64::new(RATE).expect("rate"),
            100,
            Some(100 + 16 * 768),
            estimate,
        )
    }

    #[test]
    fn constant_frame_stream_duration_is_exact() {
        let timeline = constant_frame_timeline();
        assert_eq!(
            timeline.estimated_duration(),
            Some(Duration::from_nanos(16 * 1536 * 1_000_000_000 / 48_000))
        );
    }

    /// Цель внутри кадра 7 (и ровно на его начале) — поиск стартует не позже начала кадра 7
    /// и не раньше конца кадра 6, поэтому первое найденное начало — кадр 7.
    #[test]
    fn search_start_never_skips_the_frame_containing_the_target() {
        let timeline = constant_frame_timeline();
        let frame_seven = 100 + 7 * 768;
        for target in [7 * 1536, 7 * 1536 + 1, 8 * 1536 - 1] {
            let start = timeline.seek_search_start(target, 0);
            assert!(start <= frame_seven, "target {target}: start {start}");
            assert!(start > frame_seven - 768, "target {target}: start {start}");
        }
        assert_eq!(timeline.samples_at_offset(frame_seven), 7 * 1536);
    }

    #[test]
    fn backoff_moves_one_frame_earlier_and_stops_at_data_start() {
        let timeline = constant_frame_timeline();
        let first = timeline.seek_search_start(7 * 1536, 0);
        assert_eq!(timeline.seek_search_start(7 * 1536, 1), first - 768);
        assert_eq!(timeline.seek_search_start(7 * 1536, 100), 100);
        assert_eq!(timeline.seek_search_start(0, 0), 100);
    }

    #[test]
    fn target_past_end_searches_from_the_last_frame() {
        let timeline = constant_frame_timeline();
        let start = timeline.seek_search_start(1_000 * 1536, 0);
        assert_eq!(start, 100 + 15 * 768);
    }

    #[test]
    fn unknown_content_length_has_no_duration() {
        let estimate = ByteRateEstimate::from_first_unit(768, 1536).expect("estimate");
        let timeline = StreamTimeline::new(
            TimeBase::new(1, 48_000).expect("time base"),
            NonZeroU64::new(RATE).expect("rate"),
            0,
            None,
            estimate,
        );
        assert_eq!(timeline.estimated_duration(), None);
    }

    #[test]
    fn foreign_rate_unit_is_rescaled_to_track_units() {
        let timeline = constant_frame_timeline();
        assert_eq!(timeline.unit_samples_in_track_units(1536, 48_000), 1536);
        assert_eq!(timeline.unit_samples_in_track_units(1536, 24_000), 3072);
    }

    #[test]
    fn duration_to_samples_rounds_down() {
        let timeline = constant_frame_timeline();
        assert_eq!(
            timeline.duration_to_samples(Duration::from_millis(32)),
            1536
        );
        assert_eq!(
            timeline.duration_to_samples(Duration::from_nanos(31_999_999)),
            1535
        );
    }
}
