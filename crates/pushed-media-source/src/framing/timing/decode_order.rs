//! Время сегмента на оси ДЕКОДИРОВАНИЯ — порядок, в котором его кадры нужны декодеру.
//!
//! Время показа и время декодирования у видео с B-кадрами расходятся: опорный P
//! декодируется раньше своих B, а показывается после них. Пока сегмент несёт целые
//! группы кадров (GOP или хотя бы целую «пирамиду» B-кадров), сегменты по началу
//! показа идут в том же порядке, что и по декодированию. Но низкая задержка
//! (chunked CMAF эфира Twitch, сессия 09b2 моста) кладёт по ОДНОМУ кадру на фрагмент:
//! фрагмент P начинается по показу позже фрагментов своих B. Подача таких фрагментов
//! по времени показа переставляет кадры, и декодер теряет цепочку опорных кадров.
//!
//! MSE хранит буфер дорожки в порядке декодирования (кадры приходят и подаются в
//! декодер по DTS), а время показа использует для `buffered`, замены и удаления.
//! Здесь — то же разделение: [`DecodeOrder`] от разборщика говорит, записано ли время
//! декодирования, а [`DecodeTiming`] — время, по которому полка решает, что подать
//! следующим и продолжает ли сегмент поданное.

use super::{MediaTimeSpan, TimedSpan, TrackSpans};
use crate::time::TimestampOffset;

/// Время декодирования сегмента в том виде, в каком его знает контейнер (сырое, до
/// `timestampOffset`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecodeOrder {
    /// Контейнер записывает время декодирования: fMP4 — `tfdt` и длительности кадров.
    Recorded {
        /// Отрезок декодирования кадров всех дорожек сегмента.
        span: MediaTimeSpan,
        /// Отрезок декодирования каждой дорожки с кадрами.
        tracks: TrackSpans,
    },
    /// Времени декодирования в контейнере нет, сегменты декодируются в порядке показа:
    /// WebM (блоки лежат в порядке декодирования, а у кодеков MSE WebM — VP8/VP9/AV1,
    /// Opus/Vorbis — перестановки кадров нет), фрагмент fMP4 без кадров, MPEG-TS.
    FollowsPresentation,
}

impl DecodeOrder {
    /// Время на оси подачи после `timestampOffset` полосы: записанное время
    /// декодирования или, если его нет, время показа (`presentation` и
    /// `presentation_tracks` — уже со смещением). `None` — переполнение i64.
    pub(crate) fn shifted_timing(
        &self,
        offset: TimestampOffset,
        presentation: TimedSpan,
        presentation_tracks: &TrackSpans,
    ) -> Option<DecodeTiming> {
        match self {
            Self::Recorded { span, tracks } => Some(DecodeTiming {
                // Время декодирования fMP4 известно точно: `tfdt` плюс длительности.
                timing: TimedSpan::exact(span.shifted_by(offset)?),
                tracks: tracks.shifted_by(offset)?,
            }),
            Self::FollowsPresentation => Some(DecodeTiming::follows_presentation(
                presentation,
                presentation_tracks.clone(),
            )),
        }
    }
}

/// Время сегмента на оси подачи (декодирования), уже со смещением полосы.
///
/// Полка кладёт сегменты по времени показа (`buffered`, замена, перемотка), а подаёт
/// демуксеру — в порядке этого времени и продолжение поданного решает по нему же.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodeTiming {
    /// Отрезок декодирования всех дорожек и точность его конца.
    timing: TimedSpan,
    /// Отрезок декодирования каждой дорожки с кадрами (пусто — по дорожкам неизвестно).
    tracks: TrackSpans,
}

impl DecodeTiming {
    /// Время декодирования совпадает со временем показа (см. [`DecodeOrder::FollowsPresentation`]).
    pub(crate) const fn follows_presentation(timing: TimedSpan, tracks: TrackSpans) -> Self {
        Self { timing, tracks }
    }

    /// Отрезок декодирования и точность его конца.
    pub(crate) const fn timing(&self) -> TimedSpan {
        self.timing
    }

    /// Отрезок декодирования каждой дорожки.
    pub(crate) const fn tracks(&self) -> &TrackSpans {
        &self.tracks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::timing::{MediaTime, TrackMedia, TrackSlotAssigner};

    fn span_ms(start: i64, end: i64) -> MediaTimeSpan {
        MediaTimeSpan::new(
            MediaTime::from_micros(start * 1_000),
            MediaTime::from_micros(end * 1_000),
        )
        .expect("span")
    }

    fn video_track(span: MediaTimeSpan) -> TrackSpans {
        let mut tracks = TrackSpans::default();
        tracks.include(TrackSlotAssigner::default().next(TrackMedia::Video), span);
        tracks
    }

    #[test]
    fn recorded_decode_time_is_shifted_and_ignores_presentation() {
        // Кадр P: декодируется на 40 мс, показывается на 120 мс.
        let order = DecodeOrder::Recorded {
            span: span_ms(40, 80),
            tracks: video_track(span_ms(40, 80)),
        };
        let presentation = TimedSpan::exact(span_ms(1_120, 1_160));

        let decode = order
            .shifted_timing(
                TimestampOffset::from_micros(1_000_000),
                presentation,
                &video_track(presentation.span()),
            )
            .expect("no overflow");

        assert_eq!(decode.timing(), TimedSpan::exact(span_ms(1_040, 1_080)));
        assert_eq!(decode.tracks(), &video_track(span_ms(1_040, 1_080)));
    }

    #[test]
    fn without_recorded_decode_time_the_presentation_time_is_the_feed_order() {
        let presentation = TimedSpan::exact(span_ms(500, 750));
        let tracks = video_track(presentation.span());

        let decode = DecodeOrder::FollowsPresentation
            .shifted_timing(TimestampOffset::from_micros(0), presentation, &tracks)
            .expect("no overflow");

        assert_eq!(decode.timing(), presentation);
        assert_eq!(decode.tracks(), &tracks);
    }

    #[test]
    fn an_overflowing_offset_is_reported() {
        let order = DecodeOrder::Recorded {
            span: span_ms(0, 40),
            tracks: video_track(span_ms(0, 40)),
        };
        let presentation = TimedSpan::exact(span_ms(0, 40));
        assert_eq!(
            order.shifted_timing(
                TimestampOffset::from_micros(i64::MAX),
                presentation,
                &video_track(presentation.span()),
            ),
            None
        );
    }
}
