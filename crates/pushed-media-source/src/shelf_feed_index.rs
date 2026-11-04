//! Указатель подачи полки: ждущие сегменты в порядке времени декодирования.
//!
//! Зачем. Выбор следующего сегмента ([`LaneShelf::next_to_feed`]) раньше перебирал
//! ВСЮ полку при каждом вызове, а вызывается он на каждый принятый сегмент. У
//! однокадровых фрагментов (низкая задержка Twitch, сессия 09b2) журнал страницы —
//! тысячи записей на полке, и повтор журнала стоил квадратично: на живой проверке
//! 09b2 очередь сеанса переполнилась за 214 мс, пока полка ещё разбирала первую
//! половину журнала. Здесь ждущие сегменты лежат в дереве по времени декодирования,
//! а выбор — O(log n) плюс сегменты, которые отпадают навсегда.
//!
//! Правило выбора — прежнее (описание у [`LaneShelf::next_to_feed`]); меняется только
//! способ перебора:
//! - любой годный сегмент начинает декодироваться не позже `досягаемость курсора +
//!   наибольшая дыра` (продолжение — по определению; «перешагивающий» — потому что
//!   уходит назад от курсора), поэтому дерево смотрится только до этой границы, а
//!   первый годный ключ в нём и есть самый ранний по декодированию;
//! - сегмент внутри границы, который НЕ годится, ушёл назад от курсора и кончился до
//!   него. Курсор ходит только вперёд (`LaneFeed::advance_cursor`: концы и дорожки
//!   только копятся), поэтому такой сегмент не станет годным, пока курсор не
//!   переставят, — его ключ снимается («отсев»). Исключение: курсор пока без дорожек,
//!   а сегмент с ними — правило может смениться на подорожечное, ключ остаётся.
//!
//! Инварианты дерева:
//! - ключ есть у КАЖДОГО ждущего сегмента, который ещё может быть подан при текущем
//!   курсоре; лишние ключи (сегмент снят или уже подан) безвредны — отбрасываются при
//!   встрече, а пути снятия чистят их сразу, чтобы дерево не росло;
//! - перестановка курсора (перемотка — [`LaneShelf::note_seek`], повторная подача —
//!   `restart_feeding_from`, запрос без курсора после отсева) собирает дерево заново из
//!   всех ждущих сегментов.

use std::collections::BTreeSet;

use super::{Delivery, FeedChoice, LaneShelf, ShelfEntry, ShelfRules, later};
use crate::feed::{EpisodeContinuity, FeedCursor};
use crate::framing::MediaTime;

/// Ключ ждущего сегмента: порядок — как у прежнего перебора (`min_by_key` по началу
/// декодирования, при равенстве — порядок полки: по началу показа, затем по приходу).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct CandidateKey {
    /// Начало на оси декодирования.
    decode_start: MediaTime,
    /// Начало показа (порядок полки).
    start: MediaTime,
    /// Номер прихода на полку (порядок полки при равном начале показа).
    arrival_number: u64,
}

impl CandidateKey {
    /// Ключ сегмента.
    pub(super) fn of(entry: &ShelfEntry) -> Self {
        Self {
            decode_start: entry.decode_start(),
            start: entry.span().start(),
            arrival_number: entry.arrival_number,
        }
    }

    /// Верхняя граница перебора: все ключи с началом декодирования не позже `limit`.
    const fn upper_bound(limit: MediaTime) -> Self {
        Self {
            decode_start: limit,
            start: MediaTime::from_micros(i64::MAX),
            arrival_number: u64::MAX,
        }
    }
}

/// Дерево ждущих сегментов (см. описание модуля).
#[derive(Debug, Default)]
pub(super) struct FeedCandidates {
    /// Ключи по возрастанию.
    keys: BTreeSet<CandidateKey>,
    /// Были ли ключи сняты отсевом относительно курсора: без курсора такие сегменты
    /// снова годятся, и дерево надо собрать заново.
    pruned_against_cursor: bool,
}

impl FeedCandidates {
    /// Сегмент лёг на полку и ждёт подачи.
    pub(super) fn add(&mut self, entry: &ShelfEntry) {
        self.keys.insert(CandidateKey::of(entry));
    }

    /// Сегмент подан или снят с полки.
    pub(super) fn remove(&mut self, entry: &ShelfEntry) {
        self.keys.remove(&CandidateKey::of(entry));
    }

    /// Полка опустела.
    pub(super) fn clear(&mut self) {
        self.keys.clear();
        self.pruned_against_cursor = false;
    }

    /// Собирает дерево заново из всех ждущих сегментов полки.
    pub(super) fn rebuild<'entries>(
        &mut self,
        entries: impl Iterator<Item = &'entries ShelfEntry>,
    ) {
        self.keys = entries
            .filter(|entry| entry.delivery == Delivery::Waiting)
            .map(CandidateKey::of)
            .collect();
        self.pruned_against_cursor = false;
    }
}

/// Решение по одному ключу при переборе.
enum Verdict {
    /// Сегмент подаётся.
    Feed(FeedChoice),
    /// Ключ лишний: сегмента на полке нет или он уже подан.
    Stale,
    /// Сегмент отпал навсегда при этом курсоре (отсев, см. описание модуля).
    Prune,
    /// Не годится сейчас, но может пригодиться при том же курсоре (см. описание модуля).
    Keep,
}

impl LaneShelf {
    /// Какой сегмент продолжает поданное до курсора `cursor` (правило — в описании
    /// [`Self::next_to_feed`] модуля полки; здесь — перебор по указателю).
    pub(crate) fn next_to_feed(
        &mut self,
        cursor: Option<&FeedCursor>,
        rules: &ShelfRules,
    ) -> Option<FeedChoice> {
        let Some(cursor) = cursor else {
            return self.earliest_waiting();
        };
        let latest_continuation = later(cursor.reach(), rules.max_contiguous_gap_micros);
        let straddle_end = later(cursor.certain_end(), rules.overlap_tolerance_micros);
        let mut dropped = Vec::new();
        let mut choice = None;
        for key in self
            .candidates
            .keys
            .range(..=CandidateKey::upper_bound(latest_continuation))
        {
            let verdict = match self.locate(*key) {
                Some(index) => Self::judge(
                    &self.entries[index],
                    index,
                    cursor,
                    rules,
                    latest_continuation,
                    straddle_end,
                ),
                None => Verdict::Stale,
            };
            match verdict {
                Verdict::Feed(found) => {
                    choice = Some(found);
                    break;
                }
                Verdict::Stale => dropped.push(*key),
                Verdict::Prune => {
                    dropped.push(*key);
                    self.candidates.pruned_against_cursor = true;
                }
                Verdict::Keep => {}
            }
        }
        for key in dropped {
            self.candidates.keys.remove(&key);
        }
        choice
    }

    /// Без курсора: самый ранний ждущий по декодированию (первый сегмент полосы задаёт
    /// курсор). После отсева дерево неполно — сначала собирается заново.
    fn earliest_waiting(&mut self) -> Option<FeedChoice> {
        if self.candidates.pruned_against_cursor {
            self.candidates.rebuild(self.entries.iter());
        }
        loop {
            let key = *self.candidates.keys.first()?;
            match self.locate(key) {
                Some(index) if self.entries[index].delivery == Delivery::Waiting => {
                    return Some(FeedChoice {
                        index,
                        continuity: EpisodeContinuity::MayContinue,
                    });
                }
                // Лишний ключ: сегмент снят или подан.
                _ => {
                    self.candidates.keys.remove(&key);
                }
            }
        }
    }

    /// Решение по сегменту `entry` на месте `index` при курсоре `cursor`.
    fn judge(
        entry: &ShelfEntry,
        index: usize,
        cursor: &FeedCursor,
        rules: &ShelfRules,
        latest_continuation: MediaTime,
        straddle_end: MediaTime,
    ) -> Verdict {
        if entry.delivery != Delivery::Waiting {
            return Verdict::Stale;
        }
        let goes_back = rules.goes_back(cursor, entry);
        let decode_span = entry.decode.timing().span();
        if !goes_back && decode_span.start() <= latest_continuation {
            return Verdict::Feed(FeedChoice {
                index,
                continuity: EpisodeContinuity::MayContinue,
            });
        }
        if goes_back && decode_span.end() > straddle_end {
            return Verdict::Feed(FeedChoice {
                index,
                continuity: EpisodeContinuity::MustRestart,
            });
        }
        // Ушёл назад и кончился до курсора. Навсегда ли — зависит от того, может ли
        // правило «назад» смениться с объединённого отрезка на подорожечное: это
        // случается только когда курсор ещё без дорожек, а у сегмента они есть.
        let rule_may_change = entry.decode.tracks().is_known() && !cursor.tracks().is_known();
        if rule_may_change {
            Verdict::Keep
        } else {
            Verdict::Prune
        }
    }

    /// Место сегмента с ключом `key` на полке: двоичный поиск по началу показа, среди
    /// равных начал — по номеру прихода (они лежат в порядке прихода).
    fn locate(&self, key: CandidateKey) -> Option<usize> {
        let first = self
            .entries
            .partition_point(|entry| entry.span().start() < key.start);
        self.entries[first..]
            .iter()
            .take_while(|entry| entry.span().start() == key.start)
            .position(|entry| entry.arrival_number == key.arrival_number)
            .map(|offset| first + offset)
    }
}

#[cfg(test)]
#[path = "shelf_feed_index_tests.rs"]
mod tests;
