//! UI-сторона: очередь готовых картинок и выбор картинки для отправляемого кадра.
//!
//! # Что делает
//! Поток-рисовальщик присылает готовые [`PreparedOverlay`]; UI кладёт их в [`PreparedQueue`] и на
//! каждом кадре спрашивает [`PreparedQueue::select`]. Выбор — O(очередь) без растеризации и без
//! аллокаций на точном пути.
//!
//! # Три исхода выбора ([`Selection`])
//! - `Exact` — есть картинка той же идентичности, чей интервал содержит lookup-время кадра.
//! - `StaleWithoutEnded` — точной нет (рисовальщик не успел): берём последнюю ПОКАЗАННУЮ
//!   картинку того же источника (даже если холст или ревизия входов другие) и убираем из неё
//!   реплики, которых уже нет среди активных на кадре. Закончившаяся реплика не «залипает»,
//!   новая появится, когда придёт точная картинка.
//! - `Nothing` — показывать нечего.
//!
//! # Инварианты
//! - Фоллбэк НИКОГДА не берёт картинку другого источника (`CueSourceId`): другая дорожка/media
//!   не просачивается ни на кадр.
//! - «Показанная» картинка — та, что вернул последний `Exact`; фоллбэк её не заменяет (держим
//!   оригинал: следующий кадр отфильтрует его по своему набору активных реплик).
//! - Вытеснение: время ушло дальше `valid_until`; идентичность не совпала с запрошенной (входы
//!   сменились — картинка не пригодится); переполнение лимита (первыми самые старые).

use std::collections::VecDeque;
use std::num::NonZeroUsize;

use subtitle_core::{SubtitleEventId, SubtitleTime};
use subtitle_raster::{CanvasSize, CueSourceId, SubtitleOverlay};

use super::validity::{TimeSpan, ValidityInterval};

/// Ревизия входов рисования: растёт при смене стиля, настроек ASS, шрифтов, замене дорожки.
/// Картинки разных ревизий не взаимозаменяемы для точного выбора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::state::subtitles) struct RenderInputsRevision(u64);

impl RenderInputsRevision {
    /// Начальная ревизия.
    pub(in crate::state::subtitles) const INITIAL: Self = Self(0);

    /// Следующая ревизия (на переполнении u64, недостижимом, остаётся на месте).
    pub(in crate::state::subtitles) fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// Чьи это картинки: источник реплик, холст и ревизия входов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::state::subtitles) struct OverlayIdentity {
    /// Источник реплик (дорожка и её поколение).
    pub(in crate::state::subtitles) source: CueSourceId,
    /// Холст, для которого рисовали.
    pub(in crate::state::subtitles) canvas: CanvasSize,
    /// Ревизия входов рисования.
    pub(in crate::state::subtitles) inputs: RenderInputsRevision,
}

/// Готовая картинка от рисовальщика со всем, что нужно для выбора.
#[derive(Debug, Clone)]
pub(in crate::state::subtitles) struct PreparedOverlay {
    identity: OverlayIdentity,
    interval: ValidityInterval,
    rendered_events: Vec<SubtitleEventId>,
    contains_ass: bool,
    overlay: SubtitleOverlay,
}

impl PreparedOverlay {
    /// Собирает готовую картинку; `rendered_events` — реплики, с которыми её рисовали.
    pub(in crate::state::subtitles) fn new(
        identity: OverlayIdentity,
        interval: ValidityInterval,
        rendered_events: Vec<SubtitleEventId>,
        contains_ass: bool,
        overlay: SubtitleOverlay,
    ) -> Self {
        Self {
            identity,
            interval,
            rendered_events,
            contains_ass,
            overlay,
        }
    }

    /// Идентичность входов.
    pub(in crate::state::subtitles) fn identity(&self) -> OverlayIdentity {
        self.identity
    }

    /// Срок годности.
    pub(in crate::state::subtitles) fn interval(&self) -> ValidityInterval {
        self.interval
    }

    /// Реплики, с которыми рисовали картинку.
    pub(in crate::state::subtitles) fn rendered_events(&self) -> &[SubtitleEventId] {
        &self.rendered_events
    }

    /// Есть ли среди реплик ASS (нужно показу: ASS и обычные реплики размещаются по-разному).
    pub(in crate::state::subtitles) fn contains_ass(&self) -> bool {
        self.contains_ass
    }

    /// Сама картинка.
    pub(in crate::state::subtitles) fn overlay(&self) -> &SubtitleOverlay {
        &self.overlay
    }
}

/// Результат выбора картинки для кадра.
#[derive(Debug)]
pub(in crate::state::subtitles) enum Selection<'queue> {
    /// Точная картинка из очереди (без копирования).
    Exact(&'queue SubtitleOverlay),
    /// Последняя показанная без закончившихся реплик (новый объект, маски общие).
    StaleWithoutEnded(SubtitleOverlay),
    /// Показывать нечего.
    Nothing,
}

/// Счётчики выбора: нужны диагностике («опаздывает ли рисовальщик»).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::state::subtitles) struct SelectionCounters {
    /// Точных попаданий.
    pub(in crate::state::subtitles) exact_hits: u64,
    /// Промахов, закрытых фоллбэком (в картинке осталась хотя бы одна реплика).
    pub(in crate::state::subtitles) stale_fallbacks: u64,
    /// Промахов, где показывать было нечего (фоллбэка нет или после отсечения закончившихся
    /// реплик ничего не осталось).
    pub(in crate::state::subtitles) misses_without_picture: u64,
}

/// Запись очереди: картинка и номер, по которому видно «это уже показанная».
#[derive(Debug)]
struct QueueEntry {
    serial: u64,
    prepared: PreparedOverlay,
}

/// Последняя показанная картинка (копия: запись очереди могла быть вытеснена).
#[derive(Debug)]
struct ShownOverlay {
    serial: u64,
    prepared: PreparedOverlay,
}

/// Очередь готовых картинок с лимитом и учётом показанного.
#[derive(Debug)]
pub(in crate::state::subtitles) struct PreparedQueue {
    entries: VecDeque<QueueEntry>,
    capacity: NonZeroUsize,
    next_serial: u64,
    shown: Option<ShownOverlay>,
    /// Содержит ли ASS то, что отдал последний `select` (для `Nothing` — нет).
    displayed_contains_ass: bool,
    counters: SelectionCounters,
    /// Опорное время планирования последних «часов»: от него считается «время ушло».
    planning_anchor: Option<SubtitleTime>,
}

impl PreparedQueue {
    /// Пустая очередь на `capacity` картинок (конфиг `prepared_overlays`).
    pub(in crate::state::subtitles) fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: VecDeque::with_capacity(capacity.get()),
            capacity,
            next_serial: 0,
            shown: None,
            displayed_contains_ass: false,
            counters: SelectionCounters::default(),
            planning_anchor: None,
        }
    }

    /// Запоминает опорное время планирования (`PresentationClock::planning_anchor`): записи,
    /// чей срок вышел к нему, выбрасываются при выборе. Без этого при seek/scrub назад
    /// выбор от времени СТАРОГО показанного кадра выбросил бы свежую картинку цели, а поток
    /// считал бы её выданной.
    pub(in crate::state::subtitles) fn note_planning_anchor(&mut self, anchor: SubtitleTime) {
        self.planning_anchor = Some(anchor);
    }

    /// Кладёт готовую картинку; при переполнении вытесняет самые старые.
    pub(in crate::state::subtitles) fn push(&mut self, prepared: PreparedOverlay) {
        while self.entries.len() >= self.capacity.get() {
            self.entries.pop_front();
        }
        self.next_serial = self.next_serial.saturating_add(1);
        self.entries.push_back(QueueEntry {
            serial: self.next_serial,
            prepared,
        });
    }

    /// Выбирает картинку для кадра с идентичностью `identity` и lookup-временем `lookup_time`;
    /// `active_event_ids` — реплики, активные на этом времени (нужны фоллбэку).
    pub(in crate::state::subtitles) fn select(
        &mut self,
        identity: OverlayIdentity,
        lookup_time: SubtitleTime,
        active_event_ids: &[SubtitleEventId],
    ) -> Selection<'_> {
        let anchor = self.planning_anchor.unwrap_or(lookup_time);
        self.evict_obsolete(identity, anchor);
        if let Some(index) = self.find_exact(identity, lookup_time) {
            self.mark_exact_as_shown(index);
            self.counters.exact_hits += 1;
            return match self.entries.get(index) {
                Some(entry) => Selection::Exact(entry.prepared.overlay()),
                None => Selection::Nothing,
            };
        }
        self.select_fallback(identity, active_event_ids)
    }

    /// Содержит ли ASS картинка, выданная последним `select` (`Nothing` -> `false`).
    /// Для фоллбэка значение консервативно: берётся у исходной показанной картинки.
    pub(in crate::state::subtitles) fn shown_contains_ass(&self) -> bool {
        self.displayed_contains_ass
    }

    /// Счётчики выбора.
    #[cfg(test)]
    pub(in crate::state::subtitles) fn counters(&self) -> SelectionCounters {
        self.counters
    }

    /// Сколько картинок в очереди.
    #[cfg(test)]
    pub(in crate::state::subtitles) fn prepared_count(&self) -> usize {
        self.entries.len()
    }

    /// Выбрасывает картинки источника `source`, чей срок годности задевает `span`: в этот
    /// отрезок дописали реплики, и прежний `valid_until` мог стать неверным. Показанная
    /// картинка (опора фоллбэка) остаётся: фоллбэк сам отсекает закончившиеся реплики.
    /// Возвращает, сколько записей выброшено.
    pub(in crate::state::subtitles) fn invalidate_overlapping(
        &mut self,
        source: CueSourceId,
        span: TimeSpan,
    ) -> usize {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.prepared.identity().source != source || !entry.prepared.interval().overlaps(span)
        });
        before - self.entries.len()
    }

    /// Забывает всё: очередь и показанное (смена дорожки или media).
    pub(in crate::state::subtitles) fn clear(&mut self) {
        self.entries.clear();
        self.shown = None;
        self.displayed_contains_ass = false;
    }

    /// Выбрасывает записи, которые уже не пригодятся: другая идентичность (входы сменились) и
    /// те, чей срок вышел к `lookup_time`.
    fn evict_obsolete(&mut self, identity: OverlayIdentity, anchor: SubtitleTime) {
        self.entries.retain(|entry| {
            entry.prepared.identity() == identity && !entry.prepared.interval().is_over_at(anchor)
        });
    }

    /// Индекс точной записи; при нескольких берётся нарисованная позже всех.
    fn find_exact(&self, identity: OverlayIdentity, lookup_time: SubtitleTime) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.prepared.identity() == identity
                    && entry.prepared.interval().contains(lookup_time)
            })
            .max_by_key(|(_, entry)| entry.prepared.interval().rendered_at())
            .map(|(index, _)| index)
    }

    /// Запоминает точную запись как показанную; копия делается только при смене картинки.
    fn mark_exact_as_shown(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        self.displayed_contains_ass = entry.prepared.contains_ass();
        let already_shown = self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.serial == entry.serial);
        if !already_shown {
            self.shown = Some(ShownOverlay {
                serial: entry.serial,
                prepared: entry.prepared.clone(),
            });
        }
    }

    /// Промах: последняя показанная того же источника без закончившихся реплик.
    fn select_fallback(
        &mut self,
        identity: OverlayIdentity,
        active_event_ids: &[SubtitleEventId],
    ) -> Selection<'_> {
        let same_source = self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.prepared.identity().source == identity.source);
        if !same_source {
            // Чужой источник не показываем и не храним.
            self.shown = None;
            self.displayed_contains_ass = false;
            self.counters.misses_without_picture += 1;
            return Selection::Nothing;
        }
        let Some(shown) = self.shown.as_ref() else {
            return Selection::Nothing;
        };
        let source = identity.source;
        let still_active = shown
            .prepared
            .overlay()
            .retain_cues(|key| key.source() == source && active_event_ids.contains(&key.event()));
        if still_active.is_empty() {
            self.displayed_contains_ass = false;
            self.counters.misses_without_picture += 1;
            return Selection::Nothing;
        }
        self.displayed_contains_ass = shown.prepared.contains_ass();
        self.counters.stale_fallbacks += 1;
        Selection::StaleWithoutEnded(still_active)
    }
}

#[cfg(test)]
mod tests;
