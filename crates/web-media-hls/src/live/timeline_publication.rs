//! Что табло шкалы HLS-эфира показывает по текущим плейлистам (мост 16d1, решение 325).
//!
//! Пока плейлисты эфира открыты, табло — эфир: край и окно перемотки, доказанное пакетами.
//! Когда плейлист получил `EXT-X-ENDLIST` (у отдельного звука — оба плейлиста), эфир кончился:
//! табло становится записью. Край записи сразу окончательный (край последнего плейлиста), а
//! перематывать можно только по доказанному диапазону: HLS-эфир садится только туда, где
//! плеер уже видел ключевой кадр (`prepare_seek_replacement`).
//!
//! Модуль чистый: решение не трогает ни замков, ни publisher-а — его исполняет
//! `timeline::publish_latest`, единственная точка публикации.

use media_core::{DynamicMediaTimelineState, MediaTime, RecordingTimelineState, TimelineRange};

use super::HlsLiveComponentSnapshot;

/// Идёт ли ещё эфир по принятым плейлистам.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HlsStreamEnd {
    /// Хотя бы один выбранный плейлист без `ENDLIST`: эфир идёт.
    OnAir,
    /// У главного плейлиста и у отдельного звука (если он есть) стоит `ENDLIST`.
    Ended,
}

impl HlsStreamEnd {
    /// Эфир кончился, только когда кончились все выбранные компоненты: пока звук
    /// ещё дописывается, край общего табло может расти.
    pub(super) fn of(
        main: &HlsLiveComponentSnapshot,
        audio: Option<&HlsLiveComponentSnapshot>,
    ) -> Self {
        let audio_ended = audio.is_none_or(|audio| audio.end_list);
        if main.end_list && audio_ended {
            Self::Ended
        } else {
            Self::OnAir
        }
    }
}

/// Какой вид табло уже опубликован этим coordinator-ом.
///
/// Нужен, потому что контракт запрещает возврат из записи в эфир: после первой записи
/// любая следующая публикация — тоже запись или ничего (иначе отказ publisher-а уронил бы
/// чтение demuxer-а, которое публикует на каждом пакете).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublishedHlsView {
    /// Опубликован эфир (с открытия).
    Live,
    /// Опубликована запись: эфир кончился.
    Recording,
}

/// Решение `publish_latest`: что опубликовать сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HlsTimelinePublication {
    /// Эфир: край и окно (позицию входа добавляет вызывающий).
    Live(DynamicMediaTimelineState),
    /// Запись с окончательным краем.
    Recording(RecordingTimelineState),
    /// Запись уже опубликована, а нового доказанного диапазона нет: табло не трогаем.
    KeepPublishedRecording,
}

/// Чистое решение по текущему виду плейлистов, краю и доказанному окну перемотки.
pub(super) fn timeline_publication(
    stream_end: HlsStreamEnd,
    published: PublishedHlsView,
    live_edge: MediaTime,
    proven_range: Option<TimelineRange>,
) -> HlsTimelinePublication {
    // Окно годится, только если оно не пусто и не выходит за край: так требует и эфир
    // (`with_dvr`), и запись (`ending_at`).
    let usable_range =
        proven_range.filter(|range| range.start < range.end && range.end <= live_edge);

    if stream_end == HlsStreamEnd::Ended
        && let Some(range) = usable_range
        && let Ok(recording) = RecordingTimelineState::ending_at(live_edge, range)
    {
        return HlsTimelinePublication::Recording(recording);
    }
    if published == PublishedHlsView::Recording {
        // Эфир кончился раньше, но сейчас нечего сказать о перемотке (или снимок без
        // `ENDLIST` после замены адреса): последняя запись остаётся честной.
        return HlsTimelinePublication::KeepPublishedRecording;
    }
    // Эфир идёт — или кончился, но пакеты ещё ничего не доказали: до первого доказанного
    // диапазона табло остаётся эфиром, запись без диапазона перемотки собрать нельзя.
    let live = usable_range
        .and_then(|range| DynamicMediaTimelineState::with_dvr(live_edge, range).ok())
        .unwrap_or_else(|| DynamicMediaTimelineState::without_dvr(live_edge));
    HlsTimelinePublication::Live(live)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::HlsLiveComponentKind;
    use super::*;

    fn component(kind: HlsLiveComponentKind, end_list: bool) -> HlsLiveComponentSnapshot {
        HlsLiveComponentSnapshot {
            kind,
            target_duration: Duration::from_secs(2),
            manifest_live_edge: Duration::from_secs(130),
            end_list,
            segments: Vec::new(),
        }
    }

    #[test]
    fn the_stream_ends_only_when_the_main_playlist_and_the_separate_audio_both_ended() {
        let main = |end_list| component(HlsLiveComponentKind::Main, end_list);
        let audio = |end_list| component(HlsLiveComponentKind::AlternateAudio, end_list);
        assert_eq!(HlsStreamEnd::of(&main(true), None), HlsStreamEnd::Ended);
        assert_eq!(HlsStreamEnd::of(&main(false), None), HlsStreamEnd::OnAir);
        assert_eq!(
            HlsStreamEnd::of(&main(true), Some(&audio(true))),
            HlsStreamEnd::Ended
        );
        // Звук ещё дописывается — общий край может расти, эфир не кончился.
        assert_eq!(
            HlsStreamEnd::of(&main(true), Some(&audio(false))),
            HlsStreamEnd::OnAir
        );
        assert_eq!(
            HlsStreamEnd::of(&main(false), Some(&audio(true))),
            HlsStreamEnd::OnAir
        );
    }

    fn seconds(value: u64) -> MediaTime {
        MediaTime::from_secs(value)
    }

    fn range(start: u64, end: u64) -> Option<TimelineRange> {
        TimelineRange::new(seconds(start), seconds(end))
    }

    #[test]
    fn an_ended_stream_becomes_a_recording_with_its_final_end_and_the_proven_seek_range() {
        let publication = timeline_publication(
            HlsStreamEnd::Ended,
            PublishedHlsView::Live,
            seconds(130),
            range(100, 124),
        );
        let HlsTimelinePublication::Recording(recording) = publication else {
            panic!("an ended stream with a proven range is a recording: {publication:?}");
        };
        assert_eq!(recording.duration(), seconds(130));
        assert_eq!(Some(recording.available_range()), range(100, 124));
    }

    #[test]
    fn a_stream_on_air_stays_live_with_its_window() {
        let publication = timeline_publication(
            HlsStreamEnd::OnAir,
            PublishedHlsView::Live,
            seconds(130),
            range(100, 124),
        );
        let HlsTimelinePublication::Live(live) = publication else {
            panic!("a stream on air is live: {publication:?}");
        };
        assert_eq!(live.live_edge(), seconds(130));
        assert_eq!(live.seekable_range(), range(100, 124));
    }

    #[test]
    fn an_ended_stream_without_proven_packets_stays_live_until_something_is_proven() {
        let publication = timeline_publication(
            HlsStreamEnd::Ended,
            PublishedHlsView::Live,
            seconds(130),
            None,
        );
        assert_eq!(
            publication,
            HlsTimelinePublication::Live(DynamicMediaTimelineState::without_dvr(seconds(130)))
        );
    }

    #[test]
    fn a_published_recording_never_turns_back_into_live() {
        for (stream_end, proven_range) in [
            (HlsStreamEnd::Ended, None),
            (HlsStreamEnd::OnAir, range(100, 124)),
            (HlsStreamEnd::OnAir, None),
        ] {
            assert_eq!(
                timeline_publication(
                    stream_end,
                    PublishedHlsView::Recording,
                    seconds(130),
                    proven_range,
                ),
                HlsTimelinePublication::KeepPublishedRecording,
                "{stream_end:?} with {proven_range:?}"
            );
        }
    }

    #[test]
    fn a_proven_range_past_the_edge_is_not_offered_for_seeking() {
        let publication = timeline_publication(
            HlsStreamEnd::Ended,
            PublishedHlsView::Live,
            seconds(120),
            range(100, 124),
        );
        assert_eq!(
            publication,
            HlsTimelinePublication::Live(DynamicMediaTimelineState::without_dvr(seconds(120)))
        );
    }
}
