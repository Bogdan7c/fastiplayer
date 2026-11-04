//! Посадка перемотки на кадр, с которого можно декодировать.
//!
//! Полка находит сегмент с целью по времени показа, но декодер начинает не с любого
//! кадра. Обычный фрагмент начинается с ключевого, и разницы нет. У однокадровых
//! фрагментов (Twitch низкой задержки: один кадр на `moof`) сегмент с целью — почти всегда
//! середина группы кадров: подача с него не даёт ни одного кадра, демуксер полосы
//! выбрасывает всё до следующего ключевого. А тот лежит на целую группу дальше или вообще
//! ещё не пришёл: у начала записи Twitch группа длиннее 7 с, и плеер стоял на нулевом кадре
//! вечно, хотя байты цели лежали на полке (живая проверка 09b2).
//!
//! Поэтому старт отступает к ближайшему сегменту с точкой доступа: плеер декодирует вперёд и
//! прячет кадры до цели, как у любой посадки раньше цели. Такая посадка ничего не меняет для
//! страницы и для вопроса «сядет ли с полки»: сегмент с целью на полке есть в любом случае.

use super::{LaneShelf, ShelfRules, later};
use crate::framing::SegmentRandomAccess;

impl LaneShelf {
    /// Откуда начать подачу, чтобы сегмент на месте `containing` (с целью) декодировался.
    ///
    /// Отступать можно только по непрерывному куску сегментов, которые страница держит.
    /// Дыра или стёртое по дороге — ключевого кадра у полки нет, и остаётся сегмент с целью.
    /// То же у контейнера, который точки доступа не сообщает
    /// ([`SegmentRandomAccess::Unrecorded`]): там отступать не с чего.
    pub(super) fn decodable_landing(&self, containing: usize, rules: &ShelfRules) -> usize {
        let mut landing = containing;
        while self.entries[landing].random_access == SegmentRandomAccess::NoPoints {
            let Some(previous) = landing.checked_sub(1) else {
                return containing;
            };
            let (earlier_entry, later_entry) = (&self.entries[previous], &self.entries[landing]);
            let contiguous = later_entry.span().start()
                <= later(earlier_entry.reach(rules), rules.max_contiguous_gap_micros);
            if !earlier_entry.page_holds() || !contiguous {
                return containing;
            }
            landing = previous;
        }
        landing
    }
}
