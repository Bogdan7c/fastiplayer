//! Время, которое полоса сама забыла: выселено предохранителем памяти или
//! отпущено по стиранию страницей.
//!
//! Зачем: правило позднего вступления (правило 5 `stream_buffer::seek`) пускает
//! полосу без данных у цели перемотки с её первого сегмента после цели — так
//! играется медиа, где звук начинается позже видео. Но пустота у цели бывает и
//! дырой, которую сделали мы: предохранитель («зеркало браузера») выкинул
//! сыгранное видео, а звук оставил. Без памяти о забытом такая дыра выглядит
//! как «видео начинается позже»: перемотка садилась бы с полки без просьбы
//! странице, звук играл бы с цели, а картинка стояла до следующего сегмента на
//! полке. Забытое время — это дыра посреди полосы: байты с цели должна прислать
//! страница (у неё их тоже нет — полка держит не меньше браузера).
//!
//! Спаны хранятся сливаясь; их число ограничено: при переполнении сливаются
//! ближайшие соседи. Слияние только расширяет «забытое» — это осторожно:
//! перемотка лишний раз спросит страницу, но не сядет мимо дыры.

use crate::framing::{MediaTime, MediaTimeSpan};

/// Сколько отдельных забытых промежутков помнит полоса. Выселение идёт с краёв
/// (раннее прошлое, далёкое будущее), поэтому промежутки сливаются и на деле их
/// единицы; предел — защита от патологической страницы.
const MAX_FORGOTTEN_SPANS: usize = 64;

/// Забытые полосой промежутки времени: по возрастанию начала, без наложений.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ForgottenSpans {
    /// Промежутки по возрастанию начала, не касаются друг друга.
    spans: Vec<MediaTimeSpan>,
}

impl ForgottenSpans {
    /// Запоминает забытый промежуток.
    pub(crate) fn note(&mut self, forgotten: MediaTimeSpan) {
        if forgotten.is_empty() {
            return;
        }
        self.spans.push(forgotten);
        self.spans.sort_by_key(|span| span.start());
        self.merge_touching();
        while self.spans.len() > MAX_FORGOTTEN_SPANS {
            self.merge_closest_neighbours();
        }
    }

    /// Забыто ли что-нибудь строго между `after` и `before`.
    pub(crate) fn any_between(&self, after: MediaTime, before: MediaTime) -> bool {
        self.spans
            .iter()
            .any(|span| span.end() > after && span.start() < before)
    }

    /// Сливает касающиеся и наложенные соседние промежутки.
    fn merge_touching(&mut self) {
        let mut merged: Vec<MediaTimeSpan> = Vec::with_capacity(self.spans.len());
        for span in self.spans.drain(..) {
            match merged.last_mut() {
                Some(last) if span.start() <= last.end() => *last = last.union(span),
                _ => merged.push(span),
            }
        }
        self.spans = merged;
    }

    /// Сливает двух соседей с самым маленьким промежутком между ними.
    fn merge_closest_neighbours(&mut self) {
        let closest = self
            .spans
            .windows(2)
            .enumerate()
            .min_by_key(|(_, pair)| {
                pair[1]
                    .start()
                    .as_micros()
                    .saturating_sub(pair[0].end().as_micros())
            })
            .map(|(index, _)| index);
        if let Some(index) = closest {
            let next = self.spans.remove(index + 1);
            self.spans[index] = self.spans[index].union(next);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start_ms: i64, end_ms: i64) -> MediaTimeSpan {
        MediaTimeSpan::new(
            MediaTime::from_micros(start_ms * 1000),
            MediaTime::from_micros(end_ms * 1000),
        )
        .expect("span")
    }

    fn at(ms: i64) -> MediaTime {
        MediaTime::from_micros(ms * 1000)
    }

    #[test]
    fn forgotten_time_between_is_found_and_touching_spans_merge() {
        let mut forgotten = ForgottenSpans::default();
        assert!(!forgotten.any_between(at(0), at(10_000)));
        forgotten.note(span(400, 800));
        forgotten.note(span(0, 400));
        assert_eq!(forgotten.spans, vec![span(0, 800)]);
        // Строго между: забытое кончается ровно на `after` — не считается.
        assert!(!forgotten.any_between(at(800), at(2_000)));
        assert!(forgotten.any_between(at(200), at(2_400)));
        assert!(!forgotten.any_between(at(1_000), at(2_400)));
    }

    #[test]
    fn overflow_merges_the_closest_neighbours_and_only_widens_the_forgotten_time() {
        let mut forgotten = ForgottenSpans::default();
        for index in 0..=i64::try_from(MAX_FORGOTTEN_SPANS).expect("fits") {
            // Промежутки по 10 мс через 100 мс, а у последних двух — дыра 1 мс.
            let start = index * 100;
            forgotten.note(span(start, start + 10));
        }
        forgotten.note(span(100_000, 100_010));
        forgotten.note(span(100_011, 100_020));
        assert!(forgotten.spans.len() <= MAX_FORGOTTEN_SPANS);
        // Ничего забытое не потеряно: каждое место по-прежнему забыто.
        assert!(forgotten.any_between(at(0), at(5)));
        assert!(forgotten.any_between(at(100_012), at(100_015)));
    }
}
