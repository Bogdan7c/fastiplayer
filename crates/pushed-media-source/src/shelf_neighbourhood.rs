//! Окрестность нового сегмента на полке: кого он может накрыть.
//!
//! Зачем. Замена накрытых сегментов ([`LaneShelf::take_overlapped_by`]) раньше
//! проверяла правило замены на ВСЕЙ полке при каждом принятом сегменте. На журнале
//! однокадровых фрагментов (Twitch, сессия 09b2) это тысячи проверок на каждый из
//! тысяч кусков — 78 % времени разбора журнала, квадратично (живая проверка 09b2:
//! очередь сеанса переполнилась за 214 мс). Здесь проверяется только окрестность.
//!
//! Правило замены ([`ShelfRules::replaces`]) сравнивает отрезки: точно известный
//! отрезок сегмента или отрезки его дорожек. Накрытие требует общего времени, то есть
//! лежащий сегмент начинается раньше конца нового и кончается позже начала нового —
//! по тем отрезкам, что участвуют в правиле. Полка лежит по возрастанию начала показа,
//! поэтому окрестность — срез полки:
//! - справа: сегменты, начинающиеся не позже самой поздней границы нового;
//! - слева: сегмент может кончаться не позже чем через «самую длинную протяжённость»
//!   после своего начала, а его дорожка — начинаться не раньше чем за «самое большое
//!   опережение» до него. Обе меры полка копит по всем положенным сегментам как
//!   верхние оценки (монотонно: снятый сегмент оценку не уменьшает — оценка только
//!   шире, проверка только дольше, результат тот же).
//!
//! Внутри среза правило применяется ровно как раньше; снаружи оно не может сработать,
//! поэтому ответ совпадает с перебором всей полки.

use crate::framing::{MediaTime, TimedSpan, TrackSpans};

use super::{LaneShelf, ShelfEntry, ShelfRules, certain_extent_of, earlier, later};

/// Самые дальние границы сегмента по всем отрезкам, которые участвуют в правиле замены.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Extent {
    /// Самое раннее начало: начала показа и дорожек.
    pub(super) earliest: MediaTime,
    /// Самый поздний конец: лучший конец, точный отрезок и концы дорожек.
    pub(super) latest: MediaTime,
}

impl Extent {
    /// Границы по времени показа и дорожкам сегмента.
    pub(super) fn of(timing: TimedSpan, tracks: &TrackSpans) -> Self {
        let certain = certain_extent_of(timing);
        let mut extent = Self {
            earliest: timing.span().start().min(certain.start()),
            latest: timing.span().end().max(certain.end()),
        };
        for span in tracks.spans() {
            extent.earliest = extent.earliest.min(span.start());
            extent.latest = extent.latest.max(span.end());
        }
        extent
    }
}

/// Верхние оценки формы сегментов полки (см. описание модуля).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ShapeBounds {
    /// На сколько микросекунд самый поздний конец сегмента может отстоять от его
    /// начала показа (по всем сегментам, когда-либо положенным на полку).
    longest_extent_micros: i64,
    /// На сколько микросекунд самое раннее начало сегмента может опережать его
    /// начало показа.
    widest_lead_micros: i64,
}

impl ShapeBounds {
    /// Расширяет оценки под сегмент с началом показа `start` и границами `extent`.
    pub(super) fn widen(&mut self, start: MediaTime, extent: Extent) {
        let extent_micros = extent.latest.as_micros().saturating_sub(start.as_micros());
        let lead_micros = start
            .as_micros()
            .saturating_sub(extent.earliest.as_micros());
        self.longest_extent_micros = self.longest_extent_micros.max(extent_micros);
        self.widest_lead_micros = self.widest_lead_micros.max(lead_micros);
    }
}

impl ShelfEntry {
    /// Самые дальние границы сегмента для правила замены.
    pub(super) fn extent(&self) -> Extent {
        Extent::of(self.timing, &self.tracks)
    }
}

impl LaneShelf {
    /// Снимает с полки сегменты, которые накрывает новый (`timing`, по дорожкам —
    /// `tracks`): его замена. Проверяется только окрестность нового (описание модуля).
    ///
    /// Возвращает снятые в порядке полки: их байты освободятся, когда вызывающий их
    /// отпустит (и труба дочитает, если сегмент уже был подан).
    pub(crate) fn take_overlapped_by(
        &mut self,
        timing: TimedSpan,
        tracks: &TrackSpans,
        rules: &ShelfRules,
    ) -> Vec<ShelfEntry> {
        let incoming = certain_extent_of(timing);
        let neighbourhood = self.neighbourhood_of(Extent::of(timing, tracks));
        let overlapped: Vec<usize> = neighbourhood
            .filter(|&index| rules.replaces(&self.entries[index], incoming, tracks))
            .collect();
        let mut taken = Vec::with_capacity(overlapped.len());
        // Снимаем с конца, чтобы места ещё не снятых не сдвигались.
        for &index in overlapped.iter().rev() {
            let entry = self.entries.remove(index);
            self.candidates.remove(&entry);
            taken.push(entry);
        }
        taken.reverse();
        taken
    }

    /// Места сегментов полки, которые могут иметь общее время с сегментом границ
    /// `incoming` (срез по началу показа, см. описание модуля).
    fn neighbourhood_of(&self, incoming: Extent) -> std::ops::Range<usize> {
        // Лежащий сегмент кончается не позже `начало + самая длинная протяжённость`,
        // а должен кончаться позже самого раннего начала нового.
        let earliest_start = earlier(incoming.earliest, self.shape.longest_extent_micros);
        // Лежащий сегмент (его дорожка) начинается не раньше `начало − самое большое
        // опережение`, а должен начинаться раньше самого позднего конца нового.
        let latest_start = later(incoming.latest, self.shape.widest_lead_micros);
        let first = self
            .entries
            .partition_point(|entry| entry.span().start() <= earliest_start);
        let end = self
            .entries
            .partition_point(|entry| entry.span().start() < latest_start);
        first..end.max(first)
    }

    /// Самые дальние границы сегмента относительно отрезка `span` (для тестов правила
    /// окрестности): помогает проверить, что срез не пропускает длинный сегмент.
    #[cfg(test)]
    pub(crate) fn neighbourhood_len_for_tests(&self, span: crate::framing::MediaTimeSpan) -> usize {
        self.neighbourhood_of(Extent {
            earliest: span.start(),
            latest: span.end(),
        })
        .len()
    }
}

#[cfg(test)]
#[path = "shelf_neighbourhood_tests.rs"]
mod tests;
