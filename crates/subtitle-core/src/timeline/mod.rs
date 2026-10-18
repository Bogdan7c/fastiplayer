//! Временная шкала: быстрый поиск событий, активных в заданный момент.
//!
//! Владелец инвариантов: события хранятся в порядке вставки (их индекс — стабильный
//! [`SubtitleEventId`]), рядом поддерживается порядок по началу и дерево максимумов окончаний.
//! Внешний код видит только намерения (`insert`, `collect_active_at`, `next_change_after`).

mod max_end_tree;
mod reveal;
mod shorten;
#[cfg(test)]
mod tests;

use max_end_tree::MaxEndTree;
pub use shorten::{EndShortening, ShortenError, ShortenOutcome, ShorteningHistoryLost};

use crate::event::SubtitleEvent;
use crate::time::SubtitleTime;

/// Верхняя граница числа событий. Степень двойки: помещается в `u32` для id
/// и даёт безопасный размер дерева даже на 32-битных платформах.
const MAX_TIMELINE_EVENTS: usize = 1 << 30;

/// Стабильный идентификатор события = порядковый номер вставки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubtitleEventId(u32);

impl SubtitleEventId {
    /// Числовое значение (порядок вставки).
    pub const fn get(self) -> u32 {
        self.0
    }

    fn index(self) -> usize {
        // u32 -> usize без потерь на всех поддерживаемых платформах (>= 32 бит).
        self.0 as usize
    }
}

/// Результат вставки одного события.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineInsertOutcome {
    /// Событие добавлено под новым id.
    Inserted(SubtitleEventId),
    /// Идентичное событие уже есть (демуксер перечитал пакеты после seek); вернулся id существующего.
    DuplicateIgnored(SubtitleEventId),
    /// Достигнут предел числа событий; событие отброшено.
    CapacityExceeded,
}

/// Итоги пакетной загрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimelineBuildReport {
    pub inserted: usize,
    pub duplicates_ignored: usize,
    pub rejected_over_capacity: usize,
}

/// Таймлайн событий одной дорожки.
#[derive(Debug, Clone, Default)]
pub struct SubtitleTimeline {
    /// События в порядке вставки; индекс = `SubtitleEventId`.
    events: Vec<SubtitleEvent>,
    /// Id, отсортированные по `(start, id)`.
    order_by_start: Vec<SubtitleEventId>,
    /// Максимумы `end` над `order_by_start`.
    max_end_tree: MaxEndTree,
    /// Счётчик изменений набора/интервалов событий, см. [`SubtitleTimeline::revision`].
    revision: u64,
    /// Журнал недавних укорочений для зеркал, см. [`SubtitleTimeline::shortenings_since`].
    shorten_log: shorten::ShortenLog,
}

impl SubtitleTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Строит таймлайн из набора событий: сортировка и построение дерева выполняются один раз.
    pub fn from_events(
        events: impl IntoIterator<Item = SubtitleEvent>,
    ) -> (Self, TimelineBuildReport) {
        let mut timeline = Self::new();
        let report = timeline.extend(events);
        (timeline, report)
    }

    /// Добавляет одно событие.
    ///
    /// Перестраивает дерево за O(n), поэтому подходит для потоковых вставок (единицы в секунду).
    /// Массовые загрузки должны идти через [`Self::from_events`] или [`Self::extend`].
    pub fn insert(&mut self, event: SubtitleEvent) -> TimelineInsertOutcome {
        if let Some(existing) = self.find_duplicate(&event) {
            return TimelineInsertOutcome::DuplicateIgnored(existing);
        }
        let Some(id) = self.next_id() else {
            return TimelineInsertOutcome::CapacityExceeded;
        };
        let start = event.start();
        self.events.push(event);
        // Новый id максимален, поэтому среди равных начал он встаёт последним.
        let insert_at = self
            .order_by_start
            .partition_point(|&other| self.start_of(other) <= start);
        self.order_by_start.insert(insert_at, id);
        self.rebuild_tree();
        self.bump_revision();
        TimelineInsertOutcome::Inserted(id)
    }

    /// Пакетное добавление с одним перестроением дерева. Дубликаты (и среди новых, и
    /// относительно уже имеющихся) отбрасываются и считаются в отчёте.
    pub fn extend(
        &mut self,
        events: impl IntoIterator<Item = SubtitleEvent>,
    ) -> TimelineBuildReport {
        let candidates: Vec<SubtitleEvent> = events.into_iter().collect();
        let mut report = TimelineBuildReport::default();

        // Кандидаты группируются по началу (стабильная сортировка сохраняет порядок поступления),
        // чтобы сравнивать на дубликат только события с одинаковым start.
        let mut by_start: Vec<usize> = (0..candidates.len()).collect();
        by_start.sort_by_key(|&candidate| candidate_start(&candidates, candidate));
        let mut keep = vec![true; candidates.len()];
        for group in by_start
            .chunk_by(|a, b| candidate_start(&candidates, *a) == candidate_start(&candidates, *b))
        {
            for (position, &candidate) in group.iter().enumerate() {
                let duplicate_of_earlier_candidate = group.iter().take(position).any(|&earlier| {
                    keep.get(earlier).copied().unwrap_or(false)
                        && candidates.get(earlier) == candidates.get(candidate)
                });
                let duplicate_of_existing = candidates
                    .get(candidate)
                    .is_some_and(|event| self.find_duplicate(event).is_some());
                if (duplicate_of_earlier_candidate || duplicate_of_existing)
                    && let Some(flag) = keep.get_mut(candidate)
                {
                    *flag = false;
                }
            }
        }

        for (event, keep_event) in candidates.into_iter().zip(keep) {
            if !keep_event {
                report.duplicates_ignored += 1;
            } else if self.next_id().is_none() {
                report.rejected_over_capacity += 1;
            } else {
                self.events.push(event);
                report.inserted += 1;
            }
        }
        self.rebuild_order_and_tree();
        if report.inserted > 0 {
            self.bump_revision();
        }
        report
    }

    /// Очищает `output` и заполняет его id событий, активных в момент `time`,
    /// в порядке `(layer, start, id)` — то есть снизу вверх по слоям, внутри слоя по появлению.
    ///
    /// Буфер принадлежит вызывающему и переиспользуется: если ёмкости хватает,
    /// аллокаций нет. Цена — O((k + 1) · log n): бинарный поиск границы `start <= time`
    /// и спуск по дереву только в поддеревья с `max_end > time`.
    pub fn collect_active_at(&self, time: SubtitleTime, output: &mut Vec<SubtitleEventId>) {
        output.clear();
        let started = self
            .order_by_start
            .partition_point(|&id| self.start_of(id) <= time);
        self.max_end_tree
            .visit_positions_ending_after(time, started, &mut |position| {
                if let Some(&id) = self.order_by_start.get(position) {
                    output.push(id);
                }
            });
        // id уникальны, поэтому нестабильная сортировка (без аллокации) детерминирована.
        output.sort_unstable_by_key(|&id| self.paint_order_key(id));
    }

    /// Ближайший момент строго после `time`, в который набор активных событий или видимые слова
    /// активного события меняются (начало или конец какого-то события, `TextSpan::reveal_at`
    /// слова активного события). Нужен, чтобы перерисовывать только при изменении.
    ///
    /// Ближайшее начало — бинарный поиск. Ближайшее окончание достаточно искать среди
    /// событий, уже начавшихся к `time` (активных): любое ещё не начавшееся событие
    /// заканчивается позже своего начала, а начало уже учтено. Цена — O((k + 1) · log n).
    pub fn next_change_after(&self, time: SubtitleTime) -> Option<SubtitleTime> {
        let started = self
            .order_by_start
            .partition_point(|&id| self.start_of(id) <= time);
        let next_start = self
            .order_by_start
            .get(started)
            .map(|&id| self.start_of(id));
        let mut next_end: Option<SubtitleTime> = None;
        self.max_end_tree
            .visit_positions_ending_after(time, started, &mut |position| {
                let Some(event) = self
                    .order_by_start
                    .get(position)
                    .and_then(|&id| self.event(id))
                else {
                    return;
                };
                // Конец события и появление его слов — одинаково «следующее изменение».
                let next_reveal = reveal::reveal_times_within(event)
                    .filter(|&reveal_at| reveal_at > time)
                    .min();
                let next_event_change =
                    next_reveal.map_or(event.end(), |reveal_at| reveal_at.min(event.end()));
                next_end = Some(
                    next_end.map_or(next_event_change, |current| current.min(next_event_change)),
                );
            });
        match (next_start, next_end) {
            (Some(start), Some(end)) => Some(start.min(end)),
            (start, end) => start.or(end),
        }
    }

    /// Последний момент `b`, `after < b <= up_to`, в который набор активных событий или видимые
    /// слова менялись (начало, конец события или `TextSpan::reveal_at` слова). `None` — в этом окне изменений не было.
    ///
    /// Нужен рисовальщику анимированных сцен: картинка, нарисованная на `up_to`, не годится
    /// для времени ДО этого момента (там реплика ещё не началась или уже была другой).
    /// Начала берутся бинарным поиском, концы — среди уже начавшихся событий через дерево
    /// максимумов, цена O((k + 1) · log n).
    pub fn latest_change_within(
        &self,
        after: SubtitleTime,
        up_to: SubtitleTime,
    ) -> Option<SubtitleTime> {
        let started = self
            .order_by_start
            .partition_point(|&id| self.start_of(id) <= up_to);
        // Начала отсортированы: последнее начавшееся событие даёт самое позднее начало.
        let latest_start = started
            .checked_sub(1)
            .and_then(|position| self.order_by_start.get(position))
            .map(|&id| self.start_of(id))
            .filter(|&start| start > after);
        let mut latest_end: Option<SubtitleTime> = None;
        self.max_end_tree
            .visit_positions_ending_after(after, started, &mut |position| {
                let Some(event) = self
                    .order_by_start
                    .get(position)
                    .and_then(|&id| self.event(id))
                else {
                    return;
                };
                // Конец события и появление его слов — изменения; берём самое позднее в окне.
                let changes = std::iter::once(event.end())
                    .chain(reveal::reveal_times_within(event).filter(|&at| at > after))
                    .filter(|&at| at <= up_to);
                for change in changes {
                    latest_end = Some(latest_end.map_or(change, |current| current.max(change)));
                }
            });
        match (latest_start, latest_end) {
            (Some(start), Some(end)) => Some(start.max(end)),
            (start, end) => start.or(end),
        }
    }

    /// Счётчик изменений: растёт при каждой вставке/пакете, реально добавившем события,
    /// и при каждом эффективном укорочении ([`Self::shorten_event_end`]). Дубликаты, отказы
    /// по ёмкости и no-op укорочения его НЕ меняют. Потребитель, который зеркалирует
    /// таймлайн, по этому числу узнаёт, что менялось не только «хвост» событий:
    /// если `revision` вырос, а `len` нет — было укорочение.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    pub fn event(&self, id: SubtitleEventId) -> Option<&SubtitleEvent> {
        self.events.get(id.index())
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// События с порядковым номером вставки `>= first_index`, в порядке id (то есть в порядке
    /// вставки), вместе с их id. Стоит O(число возвращённых событий): id = индекс в векторе
    /// событий, поэтому всю дорожку обходить не нужно (так UI берёт только дописанное).
    pub fn events_from_index(
        &self,
        first_index: usize,
    ) -> impl Iterator<Item = (SubtitleEventId, &SubtitleEvent)> {
        self.events
            .iter()
            .enumerate()
            .skip(first_index)
            .filter_map(|(index, event)| {
                u32::try_from(index)
                    .ok()
                    .map(|raw| (SubtitleEventId(raw), event))
            })
    }

    /// События в порядке возрастания `(start, id)` вместе с их id.
    pub fn iter(&self) -> impl Iterator<Item = (SubtitleEventId, &SubtitleEvent)> {
        self.order_by_start
            .iter()
            .filter_map(|&id| self.event(id).map(|event| (id, event)))
    }

    /// Идентичное событие, уже лежащее в таймлайне: кандидаты ищутся бинарным поиском по start.
    fn find_duplicate(&self, event: &SubtitleEvent) -> Option<SubtitleEventId> {
        let first_with_start = self
            .order_by_start
            .partition_point(|&id| self.start_of(id) < event.start());
        self.order_by_start
            .iter()
            .skip(first_with_start)
            .take_while(|&&id| self.start_of(id) == event.start())
            .copied()
            .find(|&id| self.event(id) == Some(event))
    }

    /// Id для следующей вставки или `None`, если предел достигнут.
    fn next_id(&self) -> Option<SubtitleEventId> {
        if self.events.len() >= MAX_TIMELINE_EVENTS {
            return None;
        }
        u32::try_from(self.events.len()).ok().map(SubtitleEventId)
    }

    fn start_of(&self, id: SubtitleEventId) -> SubtitleTime {
        // Id выдаёт только этот модуль, промах невозможен; запасное значение лишь заменяет panic.
        self.event(id)
            .map_or(SubtitleTime::from_micros(i64::MIN), SubtitleEvent::start)
    }

    /// Ключ порядка отрисовки: слой, затем начало, затем порядок вставки.
    fn paint_order_key(&self, id: SubtitleEventId) -> (i32, SubtitleTime, SubtitleEventId) {
        let layer = self.event(id).map_or(i32::MIN, |event| event.layer().get());
        (layer, self.start_of(id), id)
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn rebuild_order_and_tree(&mut self) {
        let mut order: Vec<SubtitleEventId> = (0..self.events.len())
            .filter_map(|index| u32::try_from(index).ok().map(SubtitleEventId))
            .collect();
        order.sort_unstable_by_key(|&id| (self.start_of(id), id));
        self.order_by_start = order;
        self.rebuild_tree();
    }

    fn rebuild_tree(&mut self) {
        let ends = self
            .order_by_start
            .iter()
            .map(|&id| {
                self.event(id)
                    .map_or(SubtitleTime::from_micros(i64::MIN), SubtitleEvent::end)
            })
            .collect::<Vec<_>>();
        self.max_end_tree = MaxEndTree::build(ends.into_iter());
    }
}

fn candidate_start(candidates: &[SubtitleEvent], index: usize) -> SubtitleTime {
    candidates
        .get(index)
        .map_or(SubtitleTime::from_micros(i64::MIN), SubtitleEvent::start)
}
