//! Стирание страницей глазами полки одной полосы (правило — `crate::page_removal`).
//!
//! Полка отпускает сегмент, только если выполнены ВСЕ три условия:
//! 1. **Целиком в стёртом диапазоне** — с допуском наложения соседей на обоих
//!    краях ([`ShelfRules`] `overlap_tolerance`): границы диапазона страница
//!    берёт из своего `buffered` в секундах с плавающей точкой, и на переводе в
//!    микросекунды край расходится с краем сегмента на округление. Без допуска
//!    крайний сегмент оставался бы на полке после каждого стирания. Сегмент, лишь
//!    задетый диапазоном (часть его времени у страницы осталась), остаётся.
//! 2. **Уже сыгран** — кончился не позже того, что полоса сыграла (решает
//!    владелец буфера, [`PlayedSoFar`]). Стирание впереди плееру ничего не даёт:
//!    страница всё равно пришлёт замену новым `appendBuffer`, а снятый сегмент мог
//!    оказаться нужен подаче раньше, чем придёт замена.
//! 3. **Байты только у полки** — копии нет в трубе эпизода (демуксер ещё не
//!    дочитал): такой сегмент не трогаем, как и у жёсткого потолка.

use super::{LaneShelf, ShelfEntry, ShelfRules, earlier, later};
use crate::framing::MediaTime;
use crate::page_removal::{PageRemovalReport, PageRemovedRange, PageRemovedRangeEnd};

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
}

impl PlayedSoFar {
    /// Сыгран ли сегмент, кончившийся в `end`.
    fn covers(self, end: MediaTime) -> bool {
        match self {
            Self::Nothing => false,
            Self::Until(limit) => end <= limit,
        }
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
    /// (три условия — в описании модуля); остальные остаются на своих местах.
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
        // Порядок полки (по началу) сохраняется: остающиеся идут в том же порядке.
        for entry in std::mem::take(&mut self.entries) {
            let span = entry.span();
            let touched = span.start() < removal.end && span.end() > removal.start;
            let inside = touched && span.start() >= earliest_start && span.end() <= latest_end;
            if !inside {
                if touched {
                    report.kept_partially_removed += 1;
                }
                kept.push(entry);
            } else if !played.covers(span.end()) {
                report.kept_not_played += 1;
                kept.push(entry);
            } else if entry.media.is_shared() {
                report.kept_in_pipe += 1;
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
}
