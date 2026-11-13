//! Какой вид табло шкалы публикует поток (мост 16d1, решения 318–320).
//!
//! Табло шкалы есть у каждого потока, а не только у эфира: длительность `MediaSource` —
//! живая величина. Вид табло следует за словом страницы (MSE `duration`):
//! - `+Infinity` ([`PushedDuration::Live`]) — эфир, окно по полке (`crate::live_timeline`);
//! - конечная ([`PushedDuration::Finite`]) — запись от нуля до края. Край не меньше самого
//!   дальнего конца скачанного: по спеке MSE длительность сама растёт при дозаписи за
//!   конец, без сеттера, и хук страницы такого роста не видит;
//! - ещё не сообщена ([`PushedDuration::Unknown`]) — «пока неизвестно».
//!
//! Табло не уходит в эфир из другого вида (эфир решается при открытии) и не возвращается в
//! «неизвестно» (контракт `media_core::DynamicMediaTimelineView`). Такое слово страницы табло
//! не меняет: [`next_view`] отвечает «публиковать нечего».

use media_core::{
    DynamicMediaTimelineState, DynamicMediaTimelineView, DynamicMediaTimelineViewKind,
    RecordingTimelineState,
};

use crate::PushedDuration;
use crate::framing::MediaTime;

/// Вид табло по слову страницы; `live` — состояние эфира, если поток сейчас эфир.
///
/// `downloaded_end` — самый дальний конец скачанного: нижняя граница края записи.
pub(crate) fn view_for(
    declared: PushedDuration,
    live: impl FnOnce() -> DynamicMediaTimelineState,
    downloaded_end: Option<MediaTime>,
) -> DynamicMediaTimelineView {
    match declared {
        PushedDuration::Live => DynamicMediaTimelineView::Live(live()),
        PushedDuration::Finite(duration) => {
            let declared_end = media_core::MediaTime::from_duration(duration);
            let downloaded_end =
                downloaded_end.map_or(media_core::MediaTime::ZERO, MediaTime::to_player_time);
            // Длительность 0 (страница ещё ничего не знает) — пустая запись: «пока неизвестно».
            RecordingTimelineState::from_start(declared_end.max(downloaded_end))
                .map_or(DynamicMediaTimelineView::Undetermined, Into::into)
        }
        PushedDuration::Unknown => DynamicMediaTimelineView::Undetermined,
    }
}

/// Следующая публикация табло: `None` — вид, которого контракт после `published` не
/// допускает (в эфир из другого вида, что угодно → «неизвестно»), табло остаётся прежним.
pub(crate) fn next_view(
    published: DynamicMediaTimelineViewKind,
    candidate: DynamicMediaTimelineView,
) -> Option<DynamicMediaTimelineView> {
    use DynamicMediaTimelineViewKind::{Live, Recording, Undetermined};
    match (published, candidate.kind()) {
        (Undetermined | Recording, Live) | (Live | Recording, Undetermined) => None,
        _ => Some(candidate),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn live() -> DynamicMediaTimelineState {
        DynamicMediaTimelineState::without_dvr(media_core::MediaTime::from_secs(9))
    }

    fn shelf_up_to(seconds: i64) -> Option<MediaTime> {
        Some(MediaTime::from_micros(seconds * 1_000_000))
    }

    fn recording_end(view: DynamicMediaTimelineView) -> Option<Duration> {
        view.as_recording()
            .map(|recording| recording.duration().as_duration())
    }

    #[test]
    fn a_finite_duration_is_a_recording_from_zero() {
        let view = view_for(
            PushedDuration::Finite(Duration::from_secs(6)),
            live,
            shelf_up_to(2),
        );
        assert_eq!(recording_end(view), Some(Duration::from_secs(6)));
        let range = view.as_recording().expect("recording").available_range();
        assert_eq!(range.start, media_core::MediaTime::ZERO);
    }

    #[test]
    fn bytes_past_the_declared_duration_extend_the_recording() {
        let view = view_for(
            PushedDuration::Finite(Duration::from_secs(3)),
            live,
            shelf_up_to(5),
        );
        assert_eq!(recording_end(view), Some(Duration::from_secs(5)));
    }

    #[test]
    fn infinity_is_a_stream_and_an_unknown_duration_is_undetermined() {
        assert_eq!(
            view_for(PushedDuration::Live, live, shelf_up_to(5)),
            DynamicMediaTimelineView::Live(live())
        );
        assert_eq!(
            view_for(PushedDuration::Unknown, live, shelf_up_to(5)),
            DynamicMediaTimelineView::Undetermined
        );
    }

    #[test]
    fn a_zero_duration_with_nothing_downloaded_is_undetermined() {
        assert_eq!(
            view_for(PushedDuration::Finite(Duration::ZERO), live, None),
            DynamicMediaTimelineView::Undetermined
        );
    }

    #[test]
    fn a_recording_does_not_turn_back_into_a_stream() {
        assert_eq!(
            next_view(
                DynamicMediaTimelineViewKind::Recording,
                DynamicMediaTimelineView::Live(live())
            ),
            None
        );
        assert!(
            next_view(
                DynamicMediaTimelineViewKind::Live,
                view_for(PushedDuration::Finite(Duration::from_secs(4)), live, None)
            )
            .is_some(),
            "the end of a stream is published"
        );
    }
}
