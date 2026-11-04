//! Стирание страницей глазами полки одной полосы (правило — `crate::page_removal`).
//!
//! Полка отпускает сегмент, только если выполнены ВСЕ три условия:
//! 1. **Страницы он больше нет** — как считает сам MSE (W3C, «Coded Frame Removal
//!    Algorithm»): стираются кадры, НАЧИНАЮЩИЕСЯ в `[start, конец)`, где конец сдвинут
//!    вперёд до ближайшей точки произвольного доступа не раньше `end`: кадры до неё
//!    зависят от стёртого. Страница этого хвоста не называет: `remove(0,07; 0,56)` при
//!    группе кадров на 2 с стирает всю группу, а следующий `remove` Twitch начинается
//!    уже с 2,05 с. Если точки сегмента неизвестны ([`SegmentRandomAccess::Unrecorded`]),
//!    сегмент должен лежать в диапазоне целиком. Начало сравнивается с допуском
//!    наложения соседей ([`ShelfRules`] `overlap_tolerance`): границы диапазона страница
//!    берёт из своего `buffered` в секундах с плавающей точкой, и на переводе в
//!    микросекунды край расходится с краем сегмента на округление. Сегмент с точкой
//!    доступа за концом диапазона остаётся: его кадры у страницы остались.
//! 2. **Уже сыгран** — кончился не позже того, что полоса сыграла (решает
//!    владелец буфера, [`PlayedSoFar`]). Плеер отстаёт от страницы, и то, что она
//!    считает прошлым, ему может быть ещё нужно, пока не придёт замена.
//! 3. **Байты только у полки** — копии нет в трубе эпизода (демуксер ещё не
//!    дочитал): такой сегмент не трогаем, как и у жёсткого потолка.
//!
//! **Стёртое, но ещё нужное** (сессия 09b2, решение 194). Сегмент, не отпущенный по
//! условиям 2–3, остаётся ради плеера, но помечается: страницы его больше нет
//! ([`PageCopy::Erased`]). Первая же перемотка снимает все такие сегменты
//! ([`LaneShelf::take_erased_by_page`]): плеер ушёл с места, ради которого они лежали, а
//! для шкалы их не существует. Иначе полка перестаёт быть `buffered` страницы: страница
//! перезапускает эфир в том же `MediaSource` от нуля (Twitch на паузе: `remove` всего,
//! перемотка на 0, новые байты с 0,09 с) — и новая шкала ложится вперемешку с хвостами
//! старой. Перемотка «вернула в очередь» всё после цели, включая старые хвосты: подача
//! уходила в них, курсор вставал на 56 с, а новые байты оказывались «в прошлом». Если
//! плеер уже ушёл (полоса ждёт цель, [`PlayedSoFar::PlayerLeft`]), новой перемотки не
//! будет, и стёртое отпускается сразу.

use super::{LaneShelf, PageCopy, ShelfEntry, ShelfRules, earlier, later};
use crate::framing::{MediaTime, SegmentRandomAccess};
use crate::page_removal::{PageRemovalReport, PageRemovedRange, PageRemovedRangeEnd};

/// Насколько точка доступа может быть раньше конца диапазона и всё равно считаться
/// стоящей на нём, мкс: страница часто стирает ровно до начала сегмента, а её секунды с
/// плавающей точкой при переводе расходятся с его началом на округление. Точка на
/// конце диапазона не стирается; допуск наложения соседей здесь не годится: он больше
/// кадра звука и оставлял бы на полке настоящий стёртый кадр.
const REMOVAL_END_ROUNDING_MICROS: i64 = 2;

/// Стёртый диапазон на шкале полки: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShelfRemoval {
    /// Начало (включительно).
    start: MediaTime,
    /// Конец (не включительно); «до конца шкалы» — самый поздний момент шкалы.
    end: MediaTime,
}

impl ShelfRemoval {
    /// Диапазон страницы на шкале полки (та же шкала показа после смещения).
    pub(crate) fn from_page(range: PageRemovedRange) -> Self {
        let end = match range.end() {
            PageRemovedRangeEnd::At(end) => MediaTime::from_player_position(end),
            // Ни один сегмент не кончается позже самого позднего момента шкалы.
            PageRemovedRangeEnd::EndOfTimeline => MediaTime::from_micros(i64::MAX),
        };
        Self {
            start: MediaTime::from_player_position(range.start()),
            end,
        }
    }

    /// Пустой или перевёрнутый диапазон ничего не стирает.
    fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

/// Докуда полоса уже сыграла (решает владелец буфера по позиции и курсору).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayedSoFar {
    /// Полоса ещё ничего не сыграла (нет позиции или курсора подачи).
    Nothing,
    /// Сыграно всё, что кончилось не позже этого момента.
    Until(MediaTime),
    /// Плеер уже перемотался и ждёт цель: стёртое страницей ему не понадобится.
    PlayerLeft,
}

impl PlayedSoFar {
    /// Сыгран ли сегмент, кончившийся в `end`.
    fn covers(self, end: MediaTime) -> bool {
        match self {
            Self::Nothing => false,
            Self::Until(limit) => end <= limit,
            Self::PlayerLeft => true,
        }
    }

    /// Трубу эпизода, который плеер бросил, ждать незачем: её байты уйдут вместе с ней.
    const fn abandoned_pipes(self) -> bool {
        matches!(self, Self::PlayerLeft)
    }
}

/// Проход по полке в порядке времени: дошли ли до конца стирания MSE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemovalWalk {
    /// Ещё нет: зависимые кадры за концом диапазона тоже стёрты.
    Erasing,
    /// Встретилась точка доступа не раньше конца диапазона: дальше всё цело.
    ReachedRandomAccess,
}

impl ShelfRemoval {
    /// Стёрт ли страницей сегмент `entry` (условие 1 в описании модуля). `walk` —
    /// состояние прохода в порядке времени.
    fn erases(
        self,
        entry: &ShelfEntry,
        walk: &mut RemovalWalk,
        earliest_start: MediaTime,
        latest_end: MediaTime,
    ) -> bool {
        let span = entry.span();
        let touched = span.start() < self.end && span.end() > self.start;
        let lies_inside = touched && span.end() <= latest_end;
        let keeps_random_access_past_the_end = !lies_inside
            && match entry.random_access {
                // Неизвестно: сегмент считается имеющим точку в самом конце — стирается,
                // только если лежит в диапазоне целиком.
                SegmentRandomAccess::Unrecorded => span.end() > self.end,
                SegmentRandomAccess::NoPoints => false,
                SegmentRandomAccess::LatestPoint(point) => {
                    point >= earlier(self.end, REMOVAL_END_ROUNDING_MICROS)
                }
            };
        if keeps_random_access_past_the_end {
            *walk = RemovalWalk::ReachedRandomAccess;
        }
        span.start() >= earliest_start && (lies_inside || *walk == RemovalWalk::Erasing)
    }
}

/// Что полка отпустила по стиранию страницы.
#[derive(Debug)]
pub(crate) struct ShelfRemovalResult {
    /// Снятые сегменты: их байты освободятся, когда вызывающий их отпустит.
    pub(crate) released: Vec<ShelfEntry>,
    /// Счёт для журнала и тестов.
    pub(crate) report: PageRemovalReport,
}

impl LaneShelf {
    /// Отпускает сегменты, которые страница стёрла у себя и которые уже сыграны
    /// (три условия — в описании модуля); стёртые, но ещё нужные плееру, помечаются;
    /// остальные остаются на своих местах.
    pub(crate) fn release_removed_by_page(
        &mut self,
        removal: ShelfRemoval,
        played: PlayedSoFar,
        rules: &ShelfRules,
    ) -> ShelfRemovalResult {
        let mut report = PageRemovalReport::default();
        let mut released = Vec::new();
        if removal.is_empty() {
            return ShelfRemovalResult { released, report };
        }
        let earliest_start = earlier(removal.start, rules.overlap_tolerance_micros);
        let latest_end = later(removal.end, rules.overlap_tolerance_micros);
        let mut kept = Vec::with_capacity(self.entries.len());
        let mut walk = RemovalWalk::Erasing;
        // Порядок полки (по началу) сохраняется: остающиеся идут в том же порядке.
        for mut entry in std::mem::take(&mut self.entries) {
            let span = entry.span();
            let touched = span.start() < removal.end && span.end() > removal.start;
            if !removal.erases(&entry, &mut walk, earliest_start, latest_end) {
                if touched {
                    report.kept_partially_removed += 1;
                }
                kept.push(entry);
            } else if !played.covers(span.end()) {
                report.kept_not_played += 1;
                entry.page_copy = PageCopy::Erased;
                kept.push(entry);
            } else if entry.media.is_shared() && !played.abandoned_pipes() {
                report.kept_in_pipe += 1;
                entry.page_copy = PageCopy::Erased;
                kept.push(entry);
            } else {
                report.released_segments += 1;
                report.released_bytes += entry.media.len();
                self.forgotten.note(span);
                released.push(entry);
            }
        }
        self.entries = kept;
        ShelfRemovalResult { released, report }
    }

    /// Перемотка: снимает всё, что страница уже стёрла, а полка держала ради плеера
    /// (см. описание модуля). Возвращает снятое: байты освободятся, когда вызывающий их
    /// отпустит и труба брошенного эпизода уйдёт. Снятое время запоминается как забытое.
    pub(crate) fn take_erased_by_page(&mut self) -> Vec<ShelfEntry> {
        let (erased, kept): (Vec<ShelfEntry>, Vec<ShelfEntry>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| entry.page_copy == PageCopy::Erased);
        self.entries = kept;
        for entry in &erased {
            self.forgotten.note(entry.span());
        }
        erased
    }
}
