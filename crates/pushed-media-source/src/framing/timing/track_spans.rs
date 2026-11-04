//! Время сегмента ПО ДОРОЖКАМ — как отдельные «буферы дорожек» MSE.
//!
//! Один SourceBuffer страницы может нести несколько дорожек (fMP4 «видео + звук» в
//! одном фрагменте, WebM с видео и звуком в одном кластере). Браузер держит буфер
//! каждой дорожки отдельно и удаляет пересечения («coded frame removal») тоже по
//! каждой дорожке. Объединённый отрезок сегмента для этого не годится: звук фрагмента
//! обычно кончается на 5–21 мс позже начала видео следующего (кадр AAC — 21 мс, его
//! граница не совпадает с границей видео), и соседи «накрывали» друг друга, хотя ни
//! у одной дорожки по отдельности пересечения нет (сессия 09a моста: стенд вставал в
//! Buffering, потому что полка выбрасывала фрагмент, которого у плеера ещё не было).
//!
//! Здесь — чистые данные: какой дорожке какой ТОЧНО известный отрезок принадлежит.
//! Правила замены и подачи по ним — у полки (`shelf`).
//!
//! **Какая дорожка «та же».** Номера дорожек принадлежат init-сегменту, а новый init
//! (смена качества) может их сменить. MSE сопоставляет дорожки нового init со старыми
//! по типу (и по номеру — только если дорожек одного типа несколько). Поэтому ключ —
//! [`TrackSlot`]: тип дорожки и её порядковый номер среди дорожек того же типа в init.

use super::{MediaTime, MediaTimeSpan, TrackMedia};
use crate::time::TimestampOffset;

/// Место дорожки в полосе, общее для всех init этой полосы.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct TrackSlot {
    /// Тип дорожки по её заголовку в init.
    kind: TrackSlotKind,
    /// Порядковый номер среди дорожек того же типа (с нуля, в порядке init).
    ordinal: u32,
}

/// Тип дорожки для сопоставления между init-сегментами.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum TrackSlotKind {
    /// Видео.
    Video,
    /// Указанный тип, не видео (звук, субтитры).
    NotVideo,
    /// Тип не указан.
    Unknown,
}

impl From<TrackMedia> for TrackSlotKind {
    fn from(media: TrackMedia) -> Self {
        match media {
            TrackMedia::Video => Self::Video,
            TrackMedia::NotVideo => Self::NotVideo,
            TrackMedia::Unknown => Self::Unknown,
        }
    }
}

/// Раздаёт места дорожкам init по порядку их объявления.
#[derive(Debug, Default)]
pub(crate) struct TrackSlotAssigner {
    /// Сколько дорожек каждого типа уже получили место.
    video: u32,
    /// То же для «не видео».
    not_video: u32,
    /// То же для дорожек без типа.
    unknown: u32,
}

impl TrackSlotAssigner {
    /// Место следующей дорожки init с типом `media`.
    pub(crate) fn next(&mut self, media: TrackMedia) -> TrackSlot {
        let kind = TrackSlotKind::from(media);
        let counter = match kind {
            TrackSlotKind::Video => &mut self.video,
            TrackSlotKind::NotVideo => &mut self.not_video,
            TrackSlotKind::Unknown => &mut self.unknown,
        };
        let ordinal = *counter;
        // Дорожек в init не больше, чем боксов/элементов в нём; насыщение — на всякий случай.
        *counter = counter.saturating_add(1);
        TrackSlot { kind, ordinal }
    }
}

/// Точно известные отрезки дорожек сегмента (или всего поданного полосой).
///
/// Дорожка без кадров в сегменте не записывается. Пустой набор — «по дорожкам
/// неизвестно» (сегмент без кадров): правила полки тогда работают по объединённому
/// отрезку, как раньше. Размер набора ограничен числом дорожек init, а не числами
/// страницы.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TrackSpans {
    /// Отрезки по возрастанию места, по одному на место.
    spans: Vec<(TrackSlot, MediaTimeSpan)>,
}

impl TrackSpans {
    /// Учитывает кадры дорожки `slot`, занимающие `span` (с уже записанным — объединение).
    ///
    /// Кадры есть, значит, время у дорожки есть: отрезок короче 1 мкс (кадры нулевой
    /// длительности, единственный блок WebM без длительности) расширяется до 1 мкс —
    /// иначе повтор такого сегмента не считался бы пересечением.
    pub(crate) fn include(&mut self, slot: TrackSlot, span: MediaTimeSpan) {
        let span = at_least_one_micro(span);
        match self.spans.binary_search_by_key(&slot, |(known, _)| *known) {
            Ok(index) => self.spans[index].1 = self.spans[index].1.union(span),
            Err(index) => self.spans.insert(index, (slot, span)),
        }
    }

    /// Известно ли время хоть одной дорожки.
    pub(crate) fn is_known(&self) -> bool {
        !self.spans.is_empty()
    }

    /// Отрезки всех дорожек с кадрами (порядок — по месту дорожки).
    pub(crate) fn spans(&self) -> impl Iterator<Item = MediaTimeSpan> + '_ {
        self.spans.iter().map(|(_, span)| *span)
    }

    /// То же после `timestampOffset` полосы; `None` — переполнение i64.
    pub(crate) fn shifted_by(&self, offset: TimestampOffset) -> Option<Self> {
        let spans = self
            .spans
            .iter()
            .map(|(slot, span)| span.shifted_by(offset).map(|span| (*slot, span)))
            .collect::<Option<Vec<_>>>()?;
        Some(Self { spans })
    }

    /// Пары отрезков дорожек, которые есть в обоих наборах: (этот, другой).
    pub(crate) fn shared_with<'a>(
        &'a self,
        other: &'a Self,
    ) -> impl Iterator<Item = (MediaTimeSpan, MediaTimeSpan)> + 'a {
        self.spans.iter().filter_map(|(slot, span)| {
            other
                .spans
                .binary_search_by_key(slot, |(known, _)| *known)
                .ok()
                .map(|index| (*span, other.spans[index].1))
        })
    }

    /// Добавляет время `other` к этому (по каждой дорожке — объединение): так курсор
    /// подачи помнит, докуда подана каждая дорожка.
    pub(crate) fn absorb(&mut self, other: &Self) {
        for (slot, span) in &other.spans {
            self.include(*slot, *span);
        }
    }
}

/// Непустой отрезок: пустой расширяется до 1 мкс (см. [`TrackSpans::include`]).
fn at_least_one_micro(span: MediaTimeSpan) -> MediaTimeSpan {
    if !span.is_empty() {
        return span;
    }
    let start = span.start();
    let end = MediaTime::from_micros(start.as_micros().saturating_add(1));
    MediaTimeSpan::new(start, end).unwrap_or(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_ms(start: i64, end: i64) -> MediaTimeSpan {
        MediaTimeSpan::new(
            MediaTime::from_micros(start * 1_000),
            MediaTime::from_micros(end * 1_000),
        )
        .expect("span")
    }

    fn video_and_audio_slots() -> (TrackSlot, TrackSlot) {
        let mut assigner = TrackSlotAssigner::default();
        let video = assigner.next(TrackMedia::Video);
        let audio = assigner.next(TrackMedia::NotVideo);
        (video, audio)
    }

    #[test]
    fn slots_match_by_kind_and_order_whatever_the_track_numbers() {
        // Init A: видео, потом звук; init B той же полосы: звук, потом видео.
        let mut first = TrackSlotAssigner::default();
        let video_a = first.next(TrackMedia::Video);
        let audio_a = first.next(TrackMedia::NotVideo);
        let mut second = TrackSlotAssigner::default();
        let audio_b = second.next(TrackMedia::NotVideo);
        let video_b = second.next(TrackMedia::Video);
        let second_video_b = second.next(TrackMedia::Video);

        assert_eq!(video_a, video_b);
        assert_eq!(audio_a, audio_b);
        assert_ne!(video_b, second_video_b, "two videos keep separate slots");
    }

    #[test]
    fn shared_tracks_pair_up_and_foreign_ones_are_skipped() {
        let (video, audio) = video_and_audio_slots();
        let mut muxed = TrackSpans::default();
        muxed.include(audio, span_ms(0, 832));
        muxed.include(video, span_ms(0, 821));
        let mut video_only = TrackSpans::default();
        video_only.include(video, span_ms(821, 1_621));

        let pairs: Vec<_> = muxed.shared_with(&video_only).collect();

        assert_eq!(pairs, [(span_ms(0, 821), span_ms(821, 1_621))]);
        assert!(muxed.is_known());
        assert!(!TrackSpans::default().is_known());
    }

    #[test]
    fn including_the_same_track_twice_unites_and_absorb_extends_every_track() {
        let (video, audio) = video_and_audio_slots();
        let mut fed = TrackSpans::default();
        fed.include(video, span_ms(0, 400));
        fed.include(video, span_ms(400, 800));
        let mut next = TrackSpans::default();
        next.include(audio, span_ms(0, 810));
        fed.absorb(&next);

        let mut expected = TrackSpans::default();
        expected.include(audio, span_ms(0, 810));
        expected.include(video, span_ms(0, 800));
        assert_eq!(fed, expected);
    }

    #[test]
    fn a_track_with_frames_never_has_an_empty_span() {
        let (video, _) = video_and_audio_slots();
        let mut spans = TrackSpans::default();
        spans.include(video, span_ms(500, 500));

        let pairs: Vec<_> = spans.shared_with(&spans).collect();
        assert_eq!(pairs[0].0.end().as_micros(), 500_001);
    }

    #[test]
    fn shifting_moves_every_track_and_reports_overflow() {
        let (video, _) = video_and_audio_slots();
        let mut spans = TrackSpans::default();
        spans.include(video, span_ms(0, 40));

        let shifted = spans
            .shifted_by(TimestampOffset::from_micros(1_000_000))
            .expect("shift");
        assert_eq!(
            shifted.shared_with(&shifted).next().map(|(span, _)| span),
            Some(span_ms(1_000, 1_040))
        );
        assert_eq!(
            spans.shifted_by(TimestampOffset::from_micros(i64::MAX)),
            None
        );
    }
}
