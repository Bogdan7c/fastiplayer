//! Кусочное преобразование между output-clock аудиоустройства и media-time.
//!
//! Модуль хранит только нейтральную временную модель. Он ничего не знает о CPAL,
//! конкретном tempo processor-е или способе доставки PCM до устройства.

use std::time::Duration;

use crate::PlaybackRate;

/// Минимальная положительная задержка, которая не позволяет scheduler-у уйти в busy-loop.
const MIN_POSITIVE_OUTPUT_DELAY: Duration = Duration::from_nanos(1);

/// Уже запланированный output старого tempo segment-а, который появится после device tail-а.
///
/// `output_duration` измеряется на output-clock оси, а `playback_rate` задаёт,
/// сколько media-time будет пройдено за этот output-интервал.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct PlannedAudioOutputSpan {
    /// Длительность будущего PCM на output-clock оси.
    output_duration: Duration,

    /// Скорость segment-а, который произвёл или ещё произведёт этот PCM.
    playback_rate: PlaybackRate,
}

impl PlannedAudioOutputSpan {
    /// Создаёт явно типизированный кусок будущего output-а старого tempo lifecycle.
    #[must_use]
    pub(super) const fn new(output_duration: Duration, playback_rate: PlaybackRate) -> Self {
        Self {
            output_duration,
            playback_rate,
        }
    }
}

/// Один непрерывный линейный segment преобразования output-clock в media-time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AudioClockMappingSegment {
    /// Output-clock позиция, с которой начинает действовать segment.
    output_clock_start: Duration,

    /// Media position, точно соответствующая `output_clock_start`.
    media_position_start: Duration,

    /// Media-progress на единицу output-clock progress.
    playback_rate: PlaybackRate,
}

impl AudioClockMappingSegment {
    /// Вычисляет media position внутри открытого segment-а с насыщением.
    #[must_use]
    fn media_position_at(self, output_clock_position: Duration) -> Duration {
        let output_delta = output_clock_position.saturating_sub(self.output_clock_start);
        let media_delta = self
            .playback_rate
            .scale_wall_delta_to_media_delta(output_delta);

        add_duration_saturating(self.media_position_start, media_delta)
    }
}

/// Упорядоченное кусочное преобразование audio output-clock ↔ media-time.
///
/// Первый segment является текущим anchor-ом. Каждый следующий segment начинается
/// ровно там, где закончился предыдущий, поэтому преобразование остаётся непрерывным
/// и монотонным. Последний segment открыт в будущее.
#[derive(Debug, Clone)]
pub(super) struct AudioClockMediaMapping {
    /// Segment-ы отсортированы по `output_clock_start`; соседние одинаковые rate слиты.
    segments: Vec<AudioClockMappingSegment>,
}

impl AudioClockMediaMapping {
    /// Создаёт mapping с одним открытым segment-ом.
    #[must_use]
    pub(super) fn new(
        output_clock_position: Duration,
        media_position: Duration,
        playback_rate: PlaybackRate,
    ) -> Self {
        Self {
            segments: vec![AudioClockMappingSegment {
                output_clock_start: output_clock_position,
                media_position_start: media_position,
                playback_rate,
            }],
        }
    }

    /// Сбрасывает старую историю и устанавливает новый lifecycle anchor.
    pub(super) fn reset_anchor(
        &mut self,
        output_clock_position: Duration,
        media_position: Duration,
        playback_rate: PlaybackRate,
    ) {
        self.segments.clear();
        self.segments.push(AudioClockMappingSegment {
            output_clock_start: output_clock_position,
            media_position_start: media_position,
            playback_rate,
        });
    }

    /// Возвращает rate последнего открытого segment-а для lifecycle reset/install.
    #[must_use]
    pub(super) fn open_playback_rate(&self) -> PlaybackRate {
        self.segments
            .last()
            .expect("audio clock mapping всегда содержит открытый segment")
            .playback_rate
    }

    /// Возвращает media position, соответствующую заданному output-clock.
    ///
    /// Запрос до текущего anchor-а не экстраполируется назад: в таком случае
    /// возвращается media position anchor-а.
    #[must_use]
    pub(super) fn media_position_at_output_clock(
        &self,
        output_clock_position: Duration,
    ) -> Duration {
        self.segment_at_output_clock(output_clock_position)
            .media_position_at(output_clock_position)
    }

    /// Переводит будущий media deadline в задержку на output-clock/wall оси.
    ///
    /// Метод проходит все сохранённые границы rate, а не применяет только текущую
    /// скорость ко всему deadline, и возвращает самый ранний достижимый output tick.
    /// Уже достигнутый deadline возвращает zero; строго будущий deadline всегда
    /// возвращает хотя бы одну наносекунду.
    #[must_use]
    pub(super) fn output_delay_until_media_deadline(
        &self,
        current_output_clock: Duration,
        media_deadline: Duration,
    ) -> Duration {
        let current_media_position = self.media_position_at_output_clock(current_output_clock);
        if media_deadline <= current_media_position {
            return Duration::ZERO;
        }

        let first_segment = self
            .segments
            .first()
            .expect("audio clock mapping всегда содержит anchor segment");
        let search_output_clock = current_output_clock.max(first_segment.output_clock_start);
        let active_segment_index = self.segment_index_at_output_clock(search_output_clock);

        for (segment_index, segment) in self
            .segments
            .iter()
            .copied()
            .enumerate()
            .skip(active_segment_index)
        {
            let next_segment = self.segments.get(segment_index + 1).copied();
            if next_segment.is_some_and(|next| media_deadline > next.media_position_start) {
                continue;
            }

            // Инвертируем абсолютную координату от начала segment-а. Инверсия
            // delta от уже округлённой current media position могла пересыпать
            // одну наносекунду: floor(a*r) + floor(b*r) не равно floor((a+b)*r).
            let media_delta_from_segment_start =
                media_deadline.saturating_sub(segment.media_position_start);
            let output_delta_from_segment_start = segment
                .playback_rate
                .scale_media_delta_to_wall_delay(media_delta_from_segment_start);
            let target_output_clock = add_duration_saturating(
                segment.output_clock_start,
                output_delta_from_segment_start,
            );
            let output_delay = target_output_clock.saturating_sub(current_output_clock);

            return positive_delay_for_future_deadline(output_delay);
        }

        // Последний segment открыт, поэтому цикл всегда должен найти deadline.
        debug_assert!(false, "открытый audio clock segment не найден");
        Duration::MAX
    }

    /// Возвращает media position после заданной задержки на output-clock оси.
    #[must_use]
    pub(super) fn media_position_after_output_delay(
        &self,
        current_output_clock: Duration,
        output_delay: Duration,
    ) -> Duration {
        let target_output_clock = add_duration_saturating(current_output_clock, output_delay);

        self.media_position_at_output_clock(target_output_clock)
    }

    /// Переустанавливает rate, сохраняя точный уже submitted кусочный output tail.
    ///
    /// Mapping до `submitted_output_end` пересобирается от фактической пары
    /// `current_output_clock`/`current_media_position` с прежними границами и rate.
    /// Всё после хвоста отбрасывается, и в его конце открывается `new_playback_rate`.
    #[cfg(test)]
    pub(super) fn reanchor_for_rate_change(
        &mut self,
        current_output_clock: Duration,
        current_media_position: Duration,
        submitted_output_end: Duration,
        new_playback_rate: PlaybackRate,
    ) {
        self.reanchor_for_rate_change_with_planned_spans(
            current_output_clock,
            current_media_position,
            submitted_output_end,
            &[],
            new_playback_rate,
        );
    }

    /// Переустанавливает rate после submitted tail-а и ещё не submitted tempo spans.
    ///
    /// Planned spans начинаются сразу после `submitted_output_end` и сохраняют
    /// attribution старых DSP segment-ов. Новый открытый rate начинается только
    /// после последнего такого span-а.
    pub(super) fn reanchor_for_rate_change_with_planned_spans(
        &mut self,
        current_output_clock: Duration,
        current_media_position: Duration,
        submitted_output_end: Duration,
        planned_old_tempo_spans: &[PlannedAudioOutputSpan],
        new_playback_rate: PlaybackRate,
    ) {
        let submitted_tail_end = submitted_output_end.max(current_output_clock);
        let old_rate_at_current = self
            .segment_at_output_clock(current_output_clock)
            .playback_rate;
        let preserved_boundaries: Vec<_> = self
            .segments
            .iter()
            .copied()
            .filter(|segment| {
                segment.output_clock_start > current_output_clock
                    && segment.output_clock_start < submitted_tail_end
            })
            .collect();

        let mut rebuilt_segments =
            Vec::with_capacity(preserved_boundaries.len() + planned_old_tempo_spans.len() + 2);
        append_segment_merging_adjacent(
            &mut rebuilt_segments,
            AudioClockMappingSegment {
                output_clock_start: current_output_clock,
                media_position_start: current_media_position,
                playback_rate: old_rate_at_current,
            },
        );

        for preserved_boundary in preserved_boundaries {
            let boundary_media_position = media_position_from_last_segment(
                &rebuilt_segments,
                preserved_boundary.output_clock_start,
            );
            append_segment_merging_adjacent(
                &mut rebuilt_segments,
                AudioClockMappingSegment {
                    output_clock_start: preserved_boundary.output_clock_start,
                    media_position_start: boundary_media_position,
                    playback_rate: preserved_boundary.playback_rate,
                },
            );
        }

        let mut planned_output_cursor = submitted_tail_end;
        let mut planned_media_cursor =
            media_position_from_last_segment(&rebuilt_segments, submitted_tail_end);

        for planned_span in planned_old_tempo_spans
            .iter()
            .copied()
            .filter(|span| !span.output_duration.is_zero())
        {
            append_segment_merging_adjacent(
                &mut rebuilt_segments,
                AudioClockMappingSegment {
                    output_clock_start: planned_output_cursor,
                    media_position_start: planned_media_cursor,
                    playback_rate: planned_span.playback_rate,
                },
            );

            planned_output_cursor =
                add_duration_saturating(planned_output_cursor, planned_span.output_duration);
            // Соседние spans с одинаковым rate сливаются в один mapping segment.
            // Поэтому media cursor тоже выводим из объединённого segment-а, а не
            // складываем отдельно округлённые до наносекунд packet durations.
            planned_media_cursor =
                media_position_from_last_segment(&rebuilt_segments, planned_output_cursor);
        }

        append_segment_merging_adjacent(
            &mut rebuilt_segments,
            AudioClockMappingSegment {
                output_clock_start: planned_output_cursor,
                media_position_start: planned_media_cursor,
                playback_rate: new_playback_rate,
            },
        );

        self.segments = rebuilt_segments;
        self.debug_assert_invariants();
    }

    /// Находит индекс segment-а, активного в заданной output-clock позиции.
    #[must_use]
    fn segment_index_at_output_clock(&self, output_clock_position: Duration) -> usize {
        self.segments
            .partition_point(|segment| segment.output_clock_start <= output_clock_position)
            .saturating_sub(1)
    }

    /// Возвращает активный segment; до anchor-а возвращает первый segment без экстраполяции.
    #[must_use]
    fn segment_at_output_clock(&self, output_clock_position: Duration) -> AudioClockMappingSegment {
        self.segments[self.segment_index_at_output_clock(output_clock_position)]
    }

    /// Проверяет внутренние ordering/continuity инварианты в debug-сборках.
    fn debug_assert_invariants(&self) {
        debug_assert!(!self.segments.is_empty());

        for neighboring_segments in self.segments.windows(2) {
            let previous_segment = neighboring_segments[0];
            let next_segment = neighboring_segments[1];

            debug_assert!(previous_segment.output_clock_start < next_segment.output_clock_start);
            debug_assert!(
                previous_segment.media_position_start <= next_segment.media_position_start
            );
            debug_assert_ne!(previous_segment.playback_rate, next_segment.playback_rate);
            debug_assert_eq!(
                previous_segment.media_position_at(next_segment.output_clock_start),
                next_segment.media_position_start
            );
        }
    }
}

/// Добавляет segment, заменяя одинаковую границу и сливая соседние одинаковые rate.
fn append_segment_merging_adjacent(
    segments: &mut Vec<AudioClockMappingSegment>,
    segment: AudioClockMappingSegment,
) {
    if let Some(last_segment) = segments.last().copied() {
        debug_assert!(last_segment.output_clock_start <= segment.output_clock_start);
        debug_assert!(last_segment.media_position_start <= segment.media_position_start);

        if last_segment.output_clock_start == segment.output_clock_start {
            // Более позднее знание о той же границе заменяет старое, но не создаёт
            // искусственный стык, если rate совпал с предыдущим segment-ом.
            segments.pop();
            if let Some(previous_segment) = segments.last().copied() {
                debug_assert_eq!(
                    previous_segment.media_position_at(segment.output_clock_start),
                    segment.media_position_start
                );
                if previous_segment.playback_rate == segment.playback_rate {
                    return;
                }
            }

            segments.push(segment);
            return;
        }

        if last_segment.playback_rate == segment.playback_rate {
            return;
        }
    }

    segments.push(segment);
}

/// Продвигает media position от последнего построенного segment-а до output boundary.
#[must_use]
fn media_position_from_last_segment(
    segments: &[AudioClockMappingSegment],
    output_clock_position: Duration,
) -> Duration {
    segments
        .last()
        .copied()
        .expect("пересобираемый mapping всегда содержит текущий anchor")
        .media_position_at(output_clock_position)
}

/// Складывает две длительности без panic и wraparound.
#[must_use]
fn add_duration_saturating(left: Duration, right: Duration) -> Duration {
    left.checked_add(right).unwrap_or(Duration::MAX)
}

/// Защищает strictly-future deadline от нулевого scheduler timeout-а.
#[must_use]
fn positive_delay_for_future_deadline(output_delay: Duration) -> Duration {
    output_delay.max(MIN_POSITIVE_OUTPUT_DELAY)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Создаёт валидный rate и делает ошибку тестового fixture-а явной.
    fn playback_rate(multiplier: f32) -> PlaybackRate {
        PlaybackRate::new(multiplier).expect("тестовый playback rate должен быть валиден")
    }

    #[test]
    fn media_deadline_uses_rate_on_output_clock_axis() {
        let fast_mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, playback_rate(4.0));
        let slow_mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, playback_rate(0.5));

        assert_eq!(
            fast_mapping.output_delay_until_media_deadline(Duration::ZERO, Duration::from_secs(1)),
            Duration::from_millis(250)
        );
        assert_eq!(
            slow_mapping.output_delay_until_media_deadline(Duration::ZERO, Duration::from_secs(1)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn old_normal_tail_precedes_new_fast_rate() {
        let mut mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, PlaybackRate::NORMAL);

        mapping.reanchor_for_rate_change(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(200),
            playback_rate(4.0),
        );

        assert_eq!(
            mapping.output_delay_until_media_deadline(Duration::ZERO, Duration::from_secs(1)),
            Duration::from_millis(400)
        );
    }

    #[test]
    fn old_normal_tail_precedes_new_slow_rate() {
        let mut mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, PlaybackRate::NORMAL);

        mapping.reanchor_for_rate_change(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(200),
            playback_rate(0.5),
        );

        assert_eq!(
            mapping.output_delay_until_media_deadline(Duration::ZERO, Duration::from_secs(1)),
            Duration::from_millis(1_800)
        );
    }

    #[test]
    fn repeated_rate_change_preserves_intermediate_piecewise_boundaries() {
        let mut mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, PlaybackRate::NORMAL);
        mapping.reanchor_for_rate_change(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(200),
            playback_rate(4.0),
        );

        mapping.reanchor_for_rate_change(
            Duration::from_millis(100),
            Duration::from_millis(100),
            Duration::from_millis(300),
            playback_rate(0.5),
        );

        assert_eq!(mapping.segments.len(), 3);
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(200)),
            Duration::from_millis(200)
        );
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(250)),
            Duration::from_millis(400)
        );
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(300)),
            Duration::from_millis(600)
        );
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(500)),
            Duration::from_millis(700)
        );
    }

    #[test]
    fn planned_old_tempo_spans_are_inserted_before_new_open_segment() {
        let mut mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, PlaybackRate::NORMAL);
        let planned_spans = [
            PlannedAudioOutputSpan::new(Duration::from_millis(200), playback_rate(2.0)),
            PlannedAudioOutputSpan::new(Duration::from_millis(100), playback_rate(0.5)),
        ];

        mapping.reanchor_for_rate_change_with_planned_spans(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(100),
            &planned_spans,
            playback_rate(4.0),
        );

        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(100)),
            Duration::from_millis(100)
        );
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(300)),
            Duration::from_millis(500)
        );
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(400)),
            Duration::from_millis(550)
        );
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(425)),
            Duration::from_millis(650)
        );
    }

    #[test]
    fn adjacent_equal_rate_spans_use_one_canonical_rounding_domain() {
        let half_rate = playback_rate(0.5);
        let span_duration = Duration::from_nanos(1_000_001);
        let mut mapping = AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, half_rate);
        let planned_spans = [PlannedAudioOutputSpan::new(span_duration, half_rate)];

        mapping.reanchor_for_rate_change_with_planned_spans(
            Duration::ZERO,
            Duration::ZERO,
            span_duration,
            &planned_spans,
            playback_rate(2.0),
        );

        let planned_output_end = Duration::from_nanos(2_000_002);
        assert_eq!(mapping.segments.len(), 2);
        assert_eq!(
            mapping.media_position_at_output_clock(planned_output_end),
            Duration::from_nanos(1_000_001)
        );
        assert_eq!(
            mapping.segments[1].media_position_start,
            Duration::from_nanos(1_000_001)
        );
    }

    #[test]
    fn forward_and_inverse_mapping_are_exact_across_segments() {
        let mut mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, PlaybackRate::NORMAL);
        let planned_spans = [
            PlannedAudioOutputSpan::new(Duration::from_millis(200), playback_rate(2.0)),
            PlannedAudioOutputSpan::new(Duration::from_millis(100), playback_rate(0.5)),
        ];
        mapping.reanchor_for_rate_change_with_planned_spans(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(100),
            &planned_spans,
            playback_rate(4.0),
        );

        let current_output_clock = Duration::from_millis(50);
        let expected_target_output_clock = Duration::from_millis(425);
        let media_deadline = mapping.media_position_at_output_clock(expected_target_output_clock);
        let output_delay =
            mapping.output_delay_until_media_deadline(current_output_clock, media_deadline);

        assert_eq!(output_delay, Duration::from_millis(375));
        assert_eq!(
            mapping.media_position_after_output_delay(current_output_clock, output_delay),
            media_deadline
        );
    }

    #[test]
    fn inverse_mapping_returns_earliest_output_tick_across_fractional_segments() {
        let half_rate = playback_rate(0.5);
        let mut mapping = AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, half_rate);
        mapping.reanchor_for_rate_change(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_nanos(3),
            playback_rate(2.0),
        );

        let current_output_clock = Duration::from_nanos(1);
        for deadline_nanos in 1..=9 {
            let media_deadline = Duration::from_nanos(deadline_nanos);
            let output_delay =
                mapping.output_delay_until_media_deadline(current_output_clock, media_deadline);
            let reached_media_position =
                mapping.media_position_after_output_delay(current_output_clock, output_delay);

            assert!(reached_media_position >= media_deadline);
            assert!(!output_delay.is_zero());

            let previous_output_delay = output_delay.saturating_sub(Duration::from_nanos(1));
            let previous_media_position = mapping
                .media_position_after_output_delay(current_output_clock, previous_output_delay);
            assert!(previous_media_position < media_deadline);
        }

        assert_eq!(
            mapping
                .output_delay_until_media_deadline(current_output_clock, Duration::from_nanos(1)),
            Duration::from_nanos(1)
        );
    }

    #[test]
    fn equal_neighbor_rates_merge_without_losing_progress() {
        let mut mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, PlaybackRate::NORMAL);
        let planned_spans = [PlannedAudioOutputSpan::new(
            Duration::from_millis(100),
            PlaybackRate::NORMAL,
        )];

        mapping.reanchor_for_rate_change_with_planned_spans(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(100),
            &planned_spans,
            PlaybackRate::NORMAL,
        );

        assert_eq!(mapping.segments.len(), 1);
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_millis(500)),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn reset_anchor_discards_old_segments() {
        let mut mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, playback_rate(4.0));
        mapping.reanchor_for_rate_change(
            Duration::ZERO,
            Duration::ZERO,
            Duration::from_millis(200),
            playback_rate(0.5),
        );

        mapping.reset_anchor(
            Duration::from_secs(5),
            Duration::from_secs(7),
            playback_rate(0.5),
        );

        assert_eq!(mapping.segments.len(), 1);
        assert_eq!(
            mapping.media_position_at_output_clock(Duration::from_secs(7)),
            Duration::from_secs(8)
        );
    }

    #[test]
    fn future_deadline_never_collapses_to_zero_and_arithmetic_saturates() {
        let fast_mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, playback_rate(4.0));
        let slow_mapping =
            AudioClockMediaMapping::new(Duration::ZERO, Duration::ZERO, playback_rate(0.25));

        assert_eq!(
            fast_mapping.output_delay_until_media_deadline(Duration::ZERO, Duration::from_nanos(1)),
            MIN_POSITIVE_OUTPUT_DELAY
        );
        assert_eq!(
            slow_mapping.output_delay_until_media_deadline(Duration::ZERO, Duration::MAX),
            Duration::MAX
        );
        assert_eq!(
            fast_mapping.media_position_after_output_delay(Duration::MAX, Duration::from_nanos(1)),
            Duration::MAX
        );
    }
}
