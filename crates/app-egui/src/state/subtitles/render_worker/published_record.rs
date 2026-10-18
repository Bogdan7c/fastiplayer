//! Память потока о том, что он уже отдал UI для текущей идентичности.
//!
//! Поток планирует работу по интервалам уже выданных картинок (`plan_next_render`): если время
//! покрыто, второй раз его не рисуют. Поэтому запись обязана совпадать с тем, что реально лежит
//! у UI в очереди, иначе возможна беда в обе стороны:
//! - поток считает «выдано», а у UI нет -> вечный промах;
//! - поток считает «нет», а у UI есть -> лишняя растеризация (безвредно).
//!
//! Правила, удерживающие запись не «богаче» очереди UI:
//! - тот же лимит числа записей и вытеснение самой старой (пустые интервалы без реплик, которые
//!   UI не получает, только ускоряют вытеснение);
//! - записи, не пересекающие окно планирования (от опорного времени), выбрасываются; очередь
//!   UI выбрасывает ушедшее время от того же опорного времени;
//! - смена идентичности (дорожка, холст, ревизия входов) обнуляет запись целиком.

use std::collections::VecDeque;

use subtitle_core::SubtitleTime;

use super::prepared_queue::OverlayIdentity;
use super::validity::{ShorteningEffect, TimeSpan, ValidityInterval};

/// Интервалы выданных картинок для одной идентичности.
#[derive(Debug)]
pub(super) struct PublishedRecord {
    identity: Option<OverlayIdentity>,
    intervals: VecDeque<ValidityInterval>,
    capacity: usize,
}

impl PublishedRecord {
    /// Пустая запись на `capacity` интервалов (не меньше одного).
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            identity: None,
            intervals: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Привязывает запись к идентичности; другая идентичность обнуляет интервалы.
    pub(super) fn align_to(&mut self, identity: OverlayIdentity) {
        if self.identity != Some(identity) {
            self.identity = Some(identity);
            self.intervals.clear();
        }
    }

    /// Оставляет только интервалы, пересекающие окно планирования `[start, end]`. Остальные
    /// планировщику не нужны, а держать их значит давать им вытеснять нужные (зацикливание).
    /// Окно считается от опорного времени планирования (цель в seek/scrub), не от показанного
    /// кадра: иначе при перемотке назад свежая картинка цели считалась бы устаревшей.
    pub(super) fn retain_window(&mut self, start: SubtitleTime, end: SubtitleTime) {
        self.intervals
            .retain(|interval| interval.intersects_window(start, end));
    }

    /// Запоминает выданный интервал; при переполнении вытесняет самый старый.
    pub(super) fn push(&mut self, interval: ValidityInterval) {
        while self.intervals.len() >= self.capacity {
            self.intervals.pop_front();
        }
        self.intervals.push_back(interval);
    }

    /// Выбрасывает интервалы, задетые `span` (дописаны реплики). Сколько выброшено.
    pub(super) fn invalidate_overlapping(&mut self, span: TimeSpan) -> usize {
        let before = self.intervals.len();
        self.intervals.retain(|interval| !interval.overlaps(span));
        before - self.intervals.len()
    }

    /// Событие укорочено, `lost` - отрезок, который оно потеряло. Интервалы, нарисованные до
    /// `lost`, обрезаются по его началу (та же картинка остаётся верной, перерисовывать её не
    /// надо), нарисованные внутри `lost` выбрасываются. Возвращает (обрезано, выброшено).
    pub(super) fn apply_shortening(&mut self, lost: TimeSpan) -> (usize, usize) {
        let mut narrowed = 0;
        let mut dropped = 0;
        self.intervals
            .retain_mut(|interval| match interval.after_shortening(lost) {
                ShorteningEffect::Unaffected => true,
                ShorteningEffect::Narrowed(shorter) => {
                    *interval = shorter;
                    narrowed += 1;
                    true
                }
                ShorteningEffect::Invalid => {
                    dropped += 1;
                    false
                }
            });
        (narrowed, dropped)
    }

    /// Обнуляет интервалы, сохраняя идентичность.
    pub(super) fn clear(&mut self) {
        self.intervals.clear();
    }

    /// Интервалы для планировщика.
    pub(super) fn intervals(&mut self) -> &[ValidityInterval] {
        self.intervals.make_contiguous()
    }

    /// Максимум интервалов (горизонт рисования обязан быть меньше, иначе планировщик
    /// вытеснял бы собственные результаты и крутился вхолостую).
    pub(super) fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::subtitles::render_worker::prepared_queue::RenderInputsRevision;
    use subtitle_raster::{CanvasSize, CueSourceId};

    fn ms(value: i64) -> SubtitleTime {
        SubtitleTime::from_millis(value)
    }

    fn identity(inputs: RenderInputsRevision) -> OverlayIdentity {
        OverlayIdentity {
            source: CueSourceId::new(1),
            canvas: CanvasSize::new(100, 50).expect("canvas"),
            inputs,
        }
    }

    #[test]
    fn identity_change_forgets_everything_and_the_same_identity_keeps_it() {
        let mut record = PublishedRecord::new(4);
        let first = identity(RenderInputsRevision::INITIAL);
        record.align_to(first);
        record.push(ValidityInterval::new(ms(0), Some(ms(10))));
        record.align_to(first);
        assert_eq!(record.intervals().len(), 1);
        record.align_to(identity(RenderInputsRevision::INITIAL.next()));
        assert!(record.intervals().is_empty());
    }

    #[test]
    fn oldest_interval_is_evicted_at_capacity() {
        let mut record = PublishedRecord::new(2);
        record.push(ValidityInterval::new(ms(0), Some(ms(1))));
        record.push(ValidityInterval::new(ms(1), Some(ms(2))));
        record.push(ValidityInterval::new(ms(2), Some(ms(3))));
        let starts: Vec<_> = record
            .intervals()
            .iter()
            .map(ValidityInterval::rendered_at)
            .collect();
        assert_eq!(starts, vec![ms(1), ms(2)]);
    }

    #[test]
    fn shortening_narrows_earlier_intervals_and_drops_those_inside_the_lost_span() {
        let mut record = PublishedRecord::new(8);
        record.push(ValidityInterval::new(ms(0), None));
        record.push(ValidityInterval::new(ms(700), Some(ms(900))));
        record.push(ValidityInterval::new(ms(5_000), Some(ms(6_000))));
        let (narrowed, dropped) = record.apply_shortening(TimeSpan::new(ms(500), ms(10_000)));
        assert_eq!((narrowed, dropped), (1, 2));
        let intervals = record.intervals();
        assert_eq!(intervals.len(), 1);
        assert!(intervals[0].contains(ms(499)));
        assert!(!intervals[0].contains(ms(500)));
    }

    #[test]
    fn expired_and_overlapped_intervals_are_dropped() {
        let mut record = PublishedRecord::new(8);
        record.push(ValidityInterval::new(ms(0), Some(ms(10))));
        record.push(ValidityInterval::new(ms(10), Some(ms(20))));
        record.push(ValidityInterval::new(ms(20), None));
        record.retain_window(ms(10), ms(25));
        assert_eq!(record.intervals().len(), 2, "[0,10) is over at 10");
        let dropped = record.invalidate_overlapping(TimeSpan::new(ms(12), ms(14)));
        assert_eq!(dropped, 1);
        assert_eq!(record.intervals().len(), 1);
    }
}
