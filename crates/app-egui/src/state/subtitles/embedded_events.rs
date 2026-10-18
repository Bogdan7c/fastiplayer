//! Пакет встроенной дорожки -> событие таймлайна (чистая функция, без состояния).
//!
//! # Время реплики
//! Начало = `presentation_time` пакета (та же шкала, что `pts` кадров). Конец = начало +
//! длительность пакета. Если контейнер длительность не сообщил либо она нулевая, реплика
//! показывается `cue_max_open_duration` из конфига.
//!
//! **Отклонение от «до следующей реплики»:** потоковый читатель не знает, когда придёт
//! следующая реплика (она ещё не прочитана), поэтому «открытая» реплика не может быть закрыта
//! задним числом. Фиксированный предел безопасен: реплика не залипнет на экране, если
//! следующая не придёт. Для SRT/ASS/WebVTT в MKV длительность в пакете есть всегда
//! (`BlockDuration`), предел нужен редким источникам без неё.

use std::time::Duration;

use subtitle_core::{SubtitleContent, SubtitleEvent, SubtitleTime};
use subtitle_formats::{
    EmbeddedCueError, EmbeddedCueFormat, EmbeddedCueLimits, EmbeddedCueOutcome, parse_embedded_cue,
};

use player_core::SubtitleFeedPacket;

use super::active_track::subtitle_time_from_pts;

/// Что получилось из пакета.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PacketEventOutcome {
    /// Реплика готова к добавлению в таймлайн.
    Event(SubtitleEvent),
    /// Пакет корректен, но текста нет (пустой промежуток mov_text): пропускаем без ошибки.
    Empty,
    /// ASS-событие с уже виданным `ReadOrder` (повтор блока или перечитанный после seek
    /// пакет): по правилу libass игнорируется.
    DuplicateReadOrder,
    /// ASS-событие без длительности или с нулевой: libass делает его невидимым
    /// (`end == start`), поэтому запасного показа на `cue_max_open_duration` нет.
    ZeroDuration,
    /// Пакет пропущен по типизированной причине.
    Rejected(PacketRejection),
}

/// Почему пакет пропущен (счётчики и лог без текста реплик).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PacketRejection {
    /// Пакет не разобрался (битый или больше лимита).
    Malformed(EmbeddedCueError),
    /// Не удалось собрать интервал (время вне диапазона).
    InvalidInterval,
}

/// Собирает событие из пакета. `max_open_duration` подставляется для пакета без длительности.
pub(super) fn event_from_packet(
    packet: &SubtitleFeedPacket,
    format: EmbeddedCueFormat,
    cue_limits: &EmbeddedCueLimits,
    max_open_duration: Duration,
) -> PacketEventOutcome {
    let embedded = match parse_embedded_cue(&packet.payload, format, cue_limits) {
        Ok(EmbeddedCueOutcome::Cue(embedded)) => embedded,
        Ok(EmbeddedCueOutcome::Empty) => return PacketEventOutcome::Empty,
        Err(error) => return PacketEventOutcome::Rejected(PacketRejection::Malformed(error)),
    };
    let Some((start, end)) = cue_interval(packet, max_open_duration) else {
        return PacketEventOutcome::Rejected(PacketRejection::InvalidInterval);
    };
    match SubtitleEvent::new(
        start,
        end,
        embedded.layer,
        SubtitleContent::Text(embedded.cue),
    ) {
        Ok(event) => PacketEventOutcome::Event(event),
        Err(_) => PacketEventOutcome::Rejected(PacketRejection::InvalidInterval),
    }
}

/// Интервал показа `[start, end)` или `None`, если время не помещается в модель.
pub(super) fn cue_interval(
    packet: &SubtitleFeedPacket,
    max_open_duration: Duration,
) -> Option<(SubtitleTime, SubtitleTime)> {
    let start = subtitle_time_from_pts(packet.presentation_time);
    let shown_for = match packet.duration {
        Some(duration) if !duration.is_zero() => duration,
        // Нет длительности или она нулевая: предел из конфига (см. документацию модуля).
        Some(_) | None => max_open_duration,
    };
    let end_pts = packet.presentation_time.checked_add(shown_for)?;
    let end_micros = i64::try_from(end_pts.as_micros()).ok()?;
    Some((start, SubtitleTime::from_micros(end_micros)))
}

/// Длительность ASS-события, если она есть и ненулевая (правило libass, см. модуль).
/// `None`: событие невидимо (`end == start`).
pub(super) fn ass_event_duration(packet: &SubtitleFeedPacket) -> Option<Duration> {
    packet.duration.filter(|duration| !duration.is_zero())
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use bytes::Bytes;
    use media_core::TrackId;
    use player_core::{MediaInstanceId, SubtitleReadRun};

    use super::*;

    const LIMITS: EmbeddedCueLimits = EmbeddedCueLimits::new(4096, 500);
    const OPEN: Duration = Duration::from_millis(5_000);

    fn packet(payload: &[u8], at_ms: u64, duration_ms: Option<u64>) -> SubtitleFeedPacket {
        SubtitleFeedPacket {
            media_instance_id: MediaInstanceId::from_non_zero(
                NonZeroU64::new(1).expect("non-zero id"),
            ),
            track_id: TrackId::new(1),
            presentation_time: Duration::from_millis(at_ms),
            duration: duration_ms.map(Duration::from_millis),
            read_run: SubtitleReadRun::INITIAL,
            payload: Bytes::copy_from_slice(payload),
        }
    }

    fn interval_of(outcome: PacketEventOutcome) -> (i64, i64) {
        match outcome {
            PacketEventOutcome::Event(event) => {
                (event.start().as_micros(), event.end().as_micros())
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn packet_duration_defines_the_end_of_the_cue() {
        let outcome = event_from_packet(
            &packet(b"Hello", 2_000, Some(1_500)),
            EmbeddedCueFormat::SrtText,
            &LIMITS,
            OPEN,
        );
        assert_eq!(interval_of(outcome), (2_000_000, 3_500_000));
    }

    #[test]
    fn missing_or_zero_duration_uses_the_configured_open_duration() {
        for duration in [None, Some(0)] {
            let outcome = event_from_packet(
                &packet(b"Hello", 2_000, duration),
                EmbeddedCueFormat::SrtText,
                &LIMITS,
                OPEN,
            );
            assert_eq!(interval_of(outcome), (2_000_000, 7_000_000), "{duration:?}");
        }
    }

    #[test]
    fn empty_mov_text_sample_is_not_an_error() {
        // Два нулевых байта длины = пустой текст.
        let outcome = event_from_packet(
            &packet(&[0, 0], 1_000, Some(500)),
            EmbeddedCueFormat::MovText,
            &LIMITS,
            OPEN,
        );
        assert_eq!(outcome, PacketEventOutcome::Empty);
    }

    #[test]
    fn oversized_payload_is_rejected_with_the_parse_error() {
        let outcome = event_from_packet(
            &packet(&[b'a'; 5000], 0, Some(100)),
            EmbeddedCueFormat::SrtText,
            &LIMITS,
            OPEN,
        );
        assert!(matches!(
            outcome,
            PacketEventOutcome::Rejected(PacketRejection::Malformed(
                EmbeddedCueError::PayloadTooLarge { .. }
            ))
        ));
    }

    #[test]
    fn duration_that_overflows_the_time_model_is_rejected() {
        let mut huge = packet(b"x", 0, Some(1));
        huge.presentation_time = Duration::MAX;
        let outcome = event_from_packet(&huge, EmbeddedCueFormat::SrtText, &LIMITS, OPEN);
        assert_eq!(
            outcome,
            PacketEventOutcome::Rejected(PacketRejection::InvalidInterval)
        );
    }
}
