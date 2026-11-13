//! Вид «табло шкалы» (мост 16d1, решения 318–320): эфир, запись или «пока неизвестно».
//!
//! Раньше port умел только эфир: live edge и окно DVR. Но длительность ролика из браузера —
//! живая величина: YouTube растит конечную длительность эфира, открытые плееры (hls.js,
//! Shaka, dash.js) в конце эфира ставят конечную длительность в том же `MediaSource`.
//! Поэтому тот же port теперь несёт и запись: её край (длительность) и доступный для
//! перемотки диапазон. Источник публикует новый вид — потребитель перестраивает шкалу на
//! лету, не пересоздавая media.
//!
//! Переходы, которые источник может опубликовать (`check_view_transition`):
//! - «неизвестно» → запись: источник узнал длину;
//! - эфир → запись: эфир кончился и стал записью;
//! - запись → запись: длина выросла или уточнилась;
//! - эфир → эфир: прежнее скольжение окна.
//!
//! Запрещены переходы В эфир из любого другого вида («неизвестно → эфир», «запись → эфир») и
//! возврат в «неизвестно». Таких сайтов не встречалось, а вход в эфир посреди воспроизведения —
//! это часы на позиции входа и другая установка media: эфир решается один раз, при открытии.
//! Источник получает типизированный отказ и решает сам.

use crate::{MediaTime, TimelineRange};

use super::{DynamicMediaTimelineState, DynamicMediaTimelineValidationError};

/// Что сейчас показывает табло шкалы ролика.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DynamicMediaTimelineView {
    /// Эфир: живой край и окно перемотки (контракт S31L без изменений).
    Live(DynamicMediaTimelineState),
    /// Запись: у шкалы есть край, перемотка — по доступному диапазону.
    Recording(RecordingTimelineState),
    /// Источник ещё не знает ни вида потока, ни длины: перемотки по шкале нет.
    Undetermined,
}

impl DynamicMediaTimelineView {
    /// Состояние эфира, если табло сейчас показывает эфир.
    #[must_use]
    pub const fn as_live(self) -> Option<DynamicMediaTimelineState> {
        match self {
            Self::Live(state) => Some(state),
            Self::Recording(_) | Self::Undetermined => None,
        }
    }

    /// Состояние записи, если табло сейчас показывает запись.
    #[must_use]
    pub const fn as_recording(self) -> Option<RecordingTimelineState> {
        match self {
            Self::Recording(recording) => Some(recording),
            Self::Live(_) | Self::Undetermined => None,
        }
    }

    /// Вид без содержимого — для журналов и типизированных отказов.
    #[must_use]
    pub const fn kind(self) -> DynamicMediaTimelineViewKind {
        match self {
            Self::Live(_) => DynamicMediaTimelineViewKind::Live,
            Self::Recording(_) => DynamicMediaTimelineViewKind::Recording,
            Self::Undetermined => DynamicMediaTimelineViewKind::Undetermined,
        }
    }
}

impl From<DynamicMediaTimelineState> for DynamicMediaTimelineView {
    /// Источники эфира публикуют прежнее состояние эфира без обёрток на месте вызова.
    fn from(state: DynamicMediaTimelineState) -> Self {
        Self::Live(state)
    }
}

impl From<RecordingTimelineState> for DynamicMediaTimelineView {
    fn from(recording: RecordingTimelineState) -> Self {
        Self::Recording(recording)
    }
}

/// Вид табло без содержимого.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DynamicMediaTimelineViewKind {
    /// Эфир.
    Live,
    /// Запись.
    Recording,
    /// Пока неизвестно.
    Undetermined,
}

/// Запись: край шкалы и доступный для перемотки диапазон.
///
/// Край — это длительность media. Доступный диапазон лежит внутри `[0, край]`:
/// - MSE: страница может скачать любую часть записи, диапазон — вся запись `[0, край]`;
/// - скользящий HLS-эфир, получивший `ENDLIST` (16d1, решение 325): начало — первое место
///   окна, где плеер видел ключевой кадр (не ноль), конец — докуда плеер уже прочитал. Край
///   при этом сразу окончательный, хотя последние куски ещё дочитываются.
///
/// Поля закрыты: пустой диапазон или диапазон за краем собрать в обход проверки нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordingTimelineState {
    end: MediaTime,
    available_range: TimelineRange,
}

impl RecordingTimelineState {
    /// Запись, доступная целиком: от нуля до `duration`.
    pub fn from_start(duration: MediaTime) -> Result<Self, DynamicMediaTimelineValidationError> {
        Self::ending_at(
            duration,
            TimelineRange::from_bounds_saturating(MediaTime::ZERO, duration),
        )
    }

    /// Запись с краем `end`, по которой пока можно перематывать только внутри `available_range`.
    ///
    /// Диапазон не пуст и не выходит за край: перематывать за конец записи нельзя.
    pub fn ending_at(
        end: MediaTime,
        available_range: TimelineRange,
    ) -> Result<Self, DynamicMediaTimelineValidationError> {
        if available_range.start >= available_range.end {
            return Err(DynamicMediaTimelineValidationError::EmptyRecordingRange {
                available_range,
            });
        }
        if available_range.end > end {
            return Err(
                DynamicMediaTimelineValidationError::RecordingRangeBeyondEnd {
                    end,
                    available_range,
                },
            );
        }
        Ok(Self {
            end,
            available_range,
        })
    }

    /// Край записи на шкале источника — её длительность.
    #[must_use]
    pub const fn duration(self) -> MediaTime {
        self.end
    }

    /// Диапазон, по которому можно перематывать.
    #[must_use]
    pub const fn available_range(self) -> TimelineRange {
        self.available_range
    }
}

/// Проверяет, может ли источник сменить вид табло с `from` на `to`.
pub(super) fn check_view_transition(
    from: DynamicMediaTimelineView,
    to: DynamicMediaTimelineView,
) -> Result<(), ForbiddenViewTransition> {
    use DynamicMediaTimelineViewKind::{Live, Recording, Undetermined};
    match (from.kind(), to.kind()) {
        (Undetermined, Undetermined | Recording)
        | (Live, Live | Recording)
        | (Recording, Recording) => Ok(()),
        (Undetermined | Recording, Live) | (Live | Recording, Undetermined) => {
            Err(ForbiddenViewTransition {
                from: from.kind(),
                to: to.kind(),
            })
        }
    }
}

/// Запрещённая смена вида табло (см. модульную документацию).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ForbiddenViewTransition {
    /// Вид, который уже опубликован.
    pub(super) from: DynamicMediaTimelineViewKind,
    /// Вид, который источник пытался опубликовать.
    pub(super) to: DynamicMediaTimelineViewKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(value: u64) -> MediaTime {
        MediaTime::from_duration(std::time::Duration::from_secs(value))
    }

    fn live() -> DynamicMediaTimelineView {
        DynamicMediaTimelineState::without_dvr(seconds(10)).into()
    }

    fn recording(duration: u64) -> DynamicMediaTimelineView {
        RecordingTimelineState::from_start(seconds(duration))
            .expect("non-empty recording")
            .into()
    }

    #[test]
    fn a_recording_from_start_is_seekable_from_zero_to_its_duration() {
        let recording = RecordingTimelineState::from_start(seconds(42)).expect("recording");
        assert_eq!(recording.duration(), seconds(42));
        assert_eq!(recording.available_range().start, MediaTime::ZERO);
        assert_eq!(recording.available_range().end, seconds(42));
    }

    #[test]
    fn an_empty_recording_is_rejected() {
        let error = RecordingTimelineState::from_start(MediaTime::ZERO).expect_err("empty");
        assert!(matches!(
            error,
            DynamicMediaTimelineValidationError::EmptyRecordingRange { .. }
        ));
    }

    #[test]
    fn a_sliding_window_that_ended_keeps_its_non_zero_start() {
        let window = TimelineRange::new(seconds(100), seconds(130)).expect("range");
        let recording = RecordingTimelineState::ending_at(seconds(130), window).expect("recording");
        assert_eq!(recording.duration(), seconds(130));
        assert_eq!(recording.available_range().start, seconds(100));
    }

    #[test]
    fn an_ended_stream_still_being_read_has_its_final_end_and_a_shorter_seek_range() {
        // ENDLIST пришёл, плеер дочитал до 124 с из 130 (решение 325).
        let read_so_far = TimelineRange::new(seconds(100), seconds(124)).expect("range");
        let recording =
            RecordingTimelineState::ending_at(seconds(130), read_so_far).expect("recording");
        assert_eq!(recording.duration(), seconds(130));
        assert_eq!(recording.available_range(), read_so_far);
    }

    #[test]
    fn a_seek_range_past_the_end_of_the_recording_is_rejected() {
        let window = TimelineRange::new(seconds(100), seconds(131)).expect("range");
        let error = RecordingTimelineState::ending_at(seconds(130), window).expect_err("beyond");
        assert_eq!(
            error,
            DynamicMediaTimelineValidationError::RecordingRangeBeyondEnd {
                end: seconds(130),
                available_range: window,
            }
        );
    }

    #[test]
    fn allowed_transitions_follow_how_sites_end_and_grow_a_stream() {
        let undetermined = DynamicMediaTimelineView::Undetermined;
        assert!(check_view_transition(undetermined, recording(5)).is_ok());
        assert!(check_view_transition(live(), recording(5)).is_ok());
        assert!(check_view_transition(recording(5), recording(9)).is_ok());
        assert!(check_view_transition(live(), live()).is_ok());
    }

    #[test]
    fn a_recording_never_turns_back_into_a_stream_or_unknown() {
        assert_eq!(
            check_view_transition(recording(5), live()),
            Err(ForbiddenViewTransition {
                from: DynamicMediaTimelineViewKind::Recording,
                to: DynamicMediaTimelineViewKind::Live,
            })
        );
        assert!(
            check_view_transition(recording(5), DynamicMediaTimelineView::Undetermined).is_err()
        );
        assert!(check_view_transition(live(), DynamicMediaTimelineView::Undetermined).is_err());
        // Эфир решается при открытии: «неизвестно» в эфир посреди игры не превращается.
        assert!(check_view_transition(DynamicMediaTimelineView::Undetermined, live()).is_err());
    }
}

#[cfg(test)]
mod publisher_tests {
    use std::num::NonZeroU64;

    use super::*;
    use crate::{
        DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePortGeneration,
        DynamicMediaTimelinePublishError, DynamicMediaTimelinePublishOutcome,
        dynamic_media_timeline,
    };

    fn seconds(value: u64) -> MediaTime {
        MediaTime::from_duration(std::time::Duration::from_secs(value))
    }

    fn undetermined_pair() -> (
        crate::DynamicMediaTimelinePort,
        crate::DynamicMediaTimelinePublisher,
    ) {
        dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: DynamicMediaTimelinePortGeneration::new(NonZeroU64::MIN),
            source_epoch: DynamicMediaTimelineEpoch::new(0),
            state: DynamicMediaTimelineView::Undetermined,
        })
    }

    #[test]
    fn a_live_stream_that_ends_is_published_as_a_recording_to_the_same_port() {
        // Эфир решается при открытии: пара открывается сразу эфиром, как у источников live.
        let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
            port_generation: DynamicMediaTimelinePortGeneration::new(NonZeroU64::MIN),
            source_epoch: DynamicMediaTimelineEpoch::new(0),
            state: DynamicMediaTimelineState::without_dvr(seconds(30)).into(),
        });
        let epoch = DynamicMediaTimelineEpoch::new(0);
        let ended = RecordingTimelineState::from_start(seconds(31)).expect("recording");
        let outcome = publisher
            .publish_view(epoch, ended.into())
            .expect("the end of a stream is published");
        assert!(matches!(
            outcome,
            DynamicMediaTimelinePublishOutcome::Published { .. }
        ));
        assert_eq!(port.observe().snapshot.state.as_recording(), Some(ended));
        // Табло уже запись, но открылся источник эфиром (решение 328).
        assert_eq!(port.opened_as(), DynamicMediaTimelineViewKind::Live);
    }

    #[test]
    fn a_source_that_opened_undetermined_keeps_that_opening_after_learning_the_length() {
        let (port, publisher) = undetermined_pair();
        let recording = RecordingTimelineState::from_start(seconds(20)).expect("recording");
        publisher
            .publish_view(DynamicMediaTimelineEpoch::new(0), recording.into())
            .expect("recording is published");
        assert_eq!(port.opened_as(), DynamicMediaTimelineViewKind::Undetermined);
    }

    #[test]
    fn a_forbidden_transition_changes_neither_the_snapshot_nor_the_revision() {
        let (port, publisher) = undetermined_pair();
        let epoch = DynamicMediaTimelineEpoch::new(0);
        let recording = RecordingTimelineState::from_start(seconds(20)).expect("recording");
        publisher
            .publish_view(epoch, recording.into())
            .expect("recording is published");
        let before = port.observe().snapshot;

        let refused = publisher
            .publish(epoch, DynamicMediaTimelineState::without_dvr(seconds(25)))
            .expect_err("a recording never turns into a stream");

        assert_eq!(
            refused,
            DynamicMediaTimelinePublishError::ForbiddenViewTransition {
                from: DynamicMediaTimelineViewKind::Recording,
                to: DynamicMediaTimelineViewKind::Live,
            }
        );
        assert_eq!(port.observe().snapshot, before);
        assert!(
            port.activity_receiver().try_recv().is_ok(),
            "only the accepted publish woke"
        );
        assert!(port.activity_receiver().try_recv().is_err());
    }
}
