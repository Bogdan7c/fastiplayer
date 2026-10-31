//! Полка полосы: уже принятые медиасегменты, разложенные по времени.
//!
//! Как `buffered` в MSE, только с точностью до целого сегмента:
//! - сегмент ложится на своё место во времени (по началу), а не в порядке прихода;
//! - сегмент, который заметно накрывает уже лежащие, заменяет их (MSE «coded frame
//!   removal», но целыми сегментами) — повтор того же времени не играется дважды;
//! - каждый сегмент помнит, нужен ли он ещё демуксеру ([`Delivery`]) и с каким
//!   init/смещением его играть ([`EpisodeKey`]).
//!
//! Конец сегмента бывает известен неточно ([`SegmentEnd`]: у WebM без длительности
//! последнего кадра). Поэтому у сегмента три границы конца:
//! - **точный конец** (`certain_end`) — докуда время известно наверняка; по нему
//!   решается замена (оценка не должна выбить соседа с полки);
//! - **лучший конец** (`span.end`) — точный или оценённый; по нему считаются
//!   «буферизовано», выброс старого и цель перемотки;
//! - **досягаемость** (`reach`) — докуда сегмент может тянуться: лучший конец, а
//!   у открытого конца — ещё [`ShelfRules`] «открытый хвост». Следующий сегмент,
//!   начавшийся до досягаемости (плюс наибольшая дыра), — продолжение.
//!
//! Полка — чистые данные без потоков и замков: что подавать демуксеру и когда,
//! решает владелец (`stream_buffer`) по правилам [`ShelfRules`].

use std::time::Duration;

use crate::buffer_window::BufferWindow;
use crate::feed::{EpisodeContinuity, EpisodeKey, FeedCursor};
use crate::framing::{MediaTime, MediaTimeSpan, SegmentEnd, TimedSpan};
use crate::intake::ChargedSegment;

#[path = "shelf_late_join.rs"]
mod late_join;
#[path = "shelf_removal.rs"]
mod removal;

pub(crate) use late_join::{LateJoinCandidates, LateJoinWindow};
pub(crate) use removal::{PlayedSoFar, ShelfRemoval};

/// Микросекунды длительности со знаком (насыщение — для гигантских настроек).
fn signed_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

/// Момент на `micros` позже (с насыщением).
fn later(moment: MediaTime, micros: i64) -> MediaTime {
    MediaTime::from_micros(moment.as_micros().saturating_add(micros))
}

/// Момент на `micros` раньше (с насыщением).
fn earlier(moment: MediaTime, micros: i64) -> MediaTime {
    MediaTime::from_micros(moment.as_micros().saturating_sub(micros))
}

/// Правила полки: окно и допуски времени (из [`crate::PushedSourcePolicy`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ShelfRules {
    /// Окно буфера.
    window: BufferWindow,
    /// Наложение соседей, которое ещё не наложение, мкс.
    overlap_tolerance_micros: i64,
    /// Наибольшая дыра внутри непрерывного куска, мкс.
    max_contiguous_gap_micros: i64,
    /// Насколько сегмент с открытым концом может тянуться за свой точный конец, мкс.
    open_tail_reach_micros: i64,
}

impl ShelfRules {
    /// Правила из окна и допусков.
    pub(crate) fn new(
        window: BufferWindow,
        overlap_tolerance: Duration,
        max_contiguous_gap: Duration,
        open_tail_reach: Duration,
    ) -> Self {
        Self {
            window,
            overlap_tolerance_micros: signed_micros(overlap_tolerance),
            max_contiguous_gap_micros: signed_micros(max_contiguous_gap),
            open_tail_reach_micros: signed_micros(open_tail_reach),
        }
    }

    /// Граница «далеко впереди»: сегменты, начавшиеся позже (и не продолжающие
    /// кусок у позиции), предохранитель памяти снимает после старого прошлого.
    pub(crate) fn ahead_limit_from(&self, position: MediaTime) -> MediaTime {
        later(position, signed_micros(self.window.ahead))
    }

    /// Запас вперёд, при котором источник «полон».
    pub(crate) const fn ahead_limit(&self) -> Duration {
        self.window.ahead
    }

    /// Граница гарантированного прошлого: сегменты, кончившиеся позже, держатся
    /// даже под предохранителем памяти (решение «помним, пока помнит страница»).
    pub(crate) fn behind_limit(&self, position: MediaTime) -> MediaTime {
        earlier(position, signed_micros(self.window.behind))
    }

    /// Накрывают ли отрезки друг друга настолько, что новый заменяет старый.
    ///
    /// Сравниваются ТОЧНО известные отрезки ([`ShelfEntry::certain_extent`]):
    /// оценённый конец не должен выбить соседа. Наложение до допуска — это
    /// округление соседей, а не повтор. Исключение: короткий сегмент, целиком
    /// лежащий внутри другого, — повтор при любой длине.
    fn overlaps(&self, first: MediaTimeSpan, second: MediaTimeSpan) -> bool {
        let shared_start = first.start().max(second.start()).as_micros();
        let shared_end = first.end().min(second.end()).as_micros();
        let shared = shared_end.saturating_sub(shared_start);
        if shared <= 0 {
            return false;
        }
        let duration = |span: MediaTimeSpan| {
            span.end()
                .as_micros()
                .saturating_sub(span.start().as_micros())
        };
        shared > self.overlap_tolerance_micros
            || shared == duration(first)
            || shared == duration(second)
    }
}

/// Нужен ли сегмент ещё демуксеру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// Лежит и ждёт своей очереди (дыра перед ним или он из прошлого).
    Waiting,
    /// Уже отдан в трубу эпизода.
    Fed,
}

/// Будет ли ещё вход у полосы (решает, можно ли «сесть» за конец полки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IncomingMedia {
    /// Страница ещё может прислать сегменты.
    MayArrive,
    /// Вход окончен (`endOfStream`): за концом полки ничего не будет.
    Ended,
}

/// Пришёл ли сегмент после последней перемотки (правило позднего вступления,
/// модуль `late_join`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Arrival {
    /// Лежал на полке до последней перемотки.
    BeforeLastSeek,
    /// Принят после неё (у перемотки вне полки — ответ страницы).
    SinceLastSeek,
}

/// Сегмент на полке.
#[derive(Debug)]
pub(crate) struct ShelfEntry {
    /// Время показа после смещения полосы и точность конца.
    timing: TimedSpan,
    /// Байты (общие с трубой эпизода).
    media: ChargedSegment,
    /// С каким init, контейнером и смещением его играть.
    key: EpisodeKey,
    /// Отдан ли демуксеру.
    delivery: Delivery,
    /// Пришёл ли после последней перемотки.
    arrival: Arrival,
}

impl ShelfEntry {
    /// Новый, ещё не поданный сегмент.
    pub(crate) fn waiting(timing: TimedSpan, media: ChargedSegment, key: EpisodeKey) -> Self {
        Self {
            timing,
            media,
            key,
            delivery: Delivery::Waiting,
            arrival: Arrival::SinceLastSeek,
        }
    }

    /// Сколько байтов держит сегмент (столько отпустит учёт, если копия последняя).
    pub(crate) fn media_len(&self) -> usize {
        self.media.len()
    }

    /// Отрезок (конец — лучший известный).
    const fn span(&self) -> MediaTimeSpan {
        self.timing.span()
    }

    /// Точно известный отрезок: от начала до точного конца. Сегмент с кадрами,
    /// у которого точно известно только начало последнего кадра, занимает хотя
    /// бы 1 мкс — иначе его повтор не считался бы наложением.
    fn certain_extent(&self) -> MediaTimeSpan {
        certain_extent_of(self.timing)
    }

    /// Досягаемость: докуда сегмент может тянуться (см. описание модуля).
    fn reach(&self, rules: &ShelfRules) -> MediaTime {
        reach_of(self.timing, rules)
    }

    /// Курсор подачи, который оставляет за собой этот сегмент.
    fn fed_to(&self, rules: &ShelfRules) -> FeedCursor {
        FeedCursor::new(self.timing.certain_end(), self.reach(rules))
    }
}

/// Точно известный отрезок времени сегмента (правило — в [`ShelfEntry::certain_extent`]).
fn certain_extent_of(timing: TimedSpan) -> MediaTimeSpan {
    let start = timing.span().start();
    let certain_end = match timing.end_kind() {
        SegmentEnd::Exact => timing.certain_end(),
        SegmentEnd::Estimated { .. } | SegmentEnd::Open { .. } => {
            timing.certain_end().max(later(start, 1))
        }
    };
    MediaTimeSpan::new(start, certain_end).unwrap_or_else(|| MediaTimeSpan::empty_at(start))
}

/// Досягаемость отрезка: лучший конец, у открытого — плюс «открытый хвост».
fn reach_of(timing: TimedSpan, rules: &ShelfRules) -> MediaTime {
    match timing.end_kind() {
        SegmentEnd::Exact | SegmentEnd::Estimated { .. } => timing.span().end(),
        SegmentEnd::Open { certain_end } => {
            later(certain_end, rules.open_tail_reach_micros).max(timing.span().end())
        }
    }
}

/// Какой сегмент подать следующим и можно ли продолжить им текущий эпизод.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeedChoice {
    /// Место сегмента на полке.
    index: usize,
    /// Продолжать ли текущий эпизод.
    continuity: EpisodeContinuity,
}

impl FeedChoice {
    /// Можно ли продолжить им текущий эпизод.
    pub(crate) const fn continuity(self) -> EpisodeContinuity {
        self.continuity
    }
}

/// Сегмент, только что отмеченный как поданный: что отдать в трубу.
#[derive(Debug)]
pub(crate) struct FedSegment {
    /// Байты.
    pub(crate) media: ChargedSegment,
    /// Ключ эпизода.
    pub(crate) key: EpisodeKey,
    /// Время (конец — лучший известный).
    pub(crate) span: MediaTimeSpan,
    /// Курсор подачи после этого сегмента.
    pub(crate) fed_to: FeedCursor,
    /// Продолжать ли текущий эпизод.
    pub(crate) continuity: EpisodeContinuity,
}

/// Полка одной полосы: сегменты по возрастанию начала.
#[derive(Debug, Default)]
pub(crate) struct LaneShelf {
    /// Сегменты по возрастанию `span.start`; при равных — в порядке прихода.
    entries: Vec<ShelfEntry>,
}

impl LaneShelf {
    /// Снимает с полки сегменты, которые накрывает `span` (заменит новый).
    ///
    /// Возвращает снятые: их байты освободятся, когда вызывающий их отпустит
    /// (и труба дочитает, если сегмент уже был подан).
    pub(crate) fn take_overlapped_by(
        &mut self,
        timing: TimedSpan,
        rules: &ShelfRules,
    ) -> Vec<ShelfEntry> {
        let incoming = certain_extent_of(timing);
        let (overlapped, kept) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|entry| rules.overlaps(entry.certain_extent(), incoming));
        self.entries = kept;
        overlapped
    }

    /// Кладёт сегмент на его место во времени.
    pub(crate) fn insert(&mut self, entry: ShelfEntry) {
        let place = self
            .entries
            .partition_point(|existing| existing.span().start() <= entry.span().start());
        self.entries.insert(place, entry);
    }

    /// Какой сегмент продолжает поданное до курсора `cursor`.
    ///
    /// - курсора ещё нет — первый ждущий (первый сегмент полосы задаёт курсор);
    /// - начало в пределах `[точный конец − допуск, досягаемость + наибольшая
    ///   дыра]` — продолжение, текущий эпизод может его принять (у точного конца
    ///   досягаемость равна ему самому — это прежнее правило «у курсора»);
    /// - начало раньше точного конца, а конец заметно позже — сегмент
    ///   «перешагивает» курсор (другая нарезка того же времени, например при
    ///   смене качества): подаётся, но в новом эпизоде, чтобы демуксер не получил
    ///   время назад (повтор уже выданного отсекает демуксер полосы — «шов»);
    /// - иначе сегмент из прошлого (кончился до курсора) или за дырой — ждёт.
    pub(crate) fn next_to_feed(
        &self,
        cursor: Option<FeedCursor>,
        rules: &ShelfRules,
    ) -> Option<FeedChoice> {
        let waiting = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.delivery == Delivery::Waiting);
        let Some(cursor) = cursor else {
            return waiting
                .map(|(index, _)| FeedChoice {
                    index,
                    continuity: EpisodeContinuity::MayContinue,
                })
                .next();
        };
        let earliest_continuation = earlier(cursor.certain_end(), rules.overlap_tolerance_micros);
        let latest_continuation = later(cursor.reach(), rules.max_contiguous_gap_micros);
        let straddle_end = later(cursor.certain_end(), rules.overlap_tolerance_micros);
        waiting
            .filter_map(|(index, entry)| {
                let start = entry.span().start();
                if start >= earliest_continuation && start <= latest_continuation {
                    Some(FeedChoice {
                        index,
                        continuity: EpisodeContinuity::MayContinue,
                    })
                } else if start < earliest_continuation && entry.span().end() > straddle_end {
                    Some(FeedChoice {
                        index,
                        continuity: EpisodeContinuity::MustRestart,
                    })
                } else {
                    None
                }
            })
            .next()
    }

    /// Ключ эпизода выбранного сегмента — чтобы до подачи решить, нужен ли новый
    /// эпизод (и есть ли под него место в ящике).
    pub(crate) fn key_of(&self, choice: FeedChoice) -> Option<&EpisodeKey> {
        self.entries.get(choice.index).map(|entry| &entry.key)
    }

    /// Место сегмента, с которого перемотка к `target` начинает подачу заново.
    ///
    /// 1. Сегмент, внутри которого лежит цель (начало ≤ цель < конец; у открытого
    ///    конца — до досягаемости, но не дальше начала следующего сегмента), — его
    ///    начало и есть точка безопасного старта не позже цели.
    /// 2. Цель попала в маленькую дыру (не больше наибольшей дыры) перед
    ///    сегментом — этот сегмент: плеер перешагивает такие дыры.
    /// 3. Вход окончен, а цель — за концом полки (перемотка на длительность или в
    ///    последний кадр, конец которого не записан) — последний сегмент: плеер
    ///    доиграет его и получит конец потока, а не будет вечно ждать байтов,
    ///    которых страница не пришлёт.
    ///
    /// `None` — цели на полке нет.
    fn restart_index(
        &self,
        target: MediaTime,
        rules: &ShelfRules,
        incoming: IncomingMedia,
    ) -> Option<usize> {
        let containing = (0..self.entries.len()).rev().find(|&index| {
            let entry = &self.entries[index];
            entry.span().start() <= target && target < self.containment_end(index, rules)
        });
        containing
            .or_else(|| {
                let latest_start = later(target, rules.max_contiguous_gap_micros);
                self.entries.iter().position(|entry| {
                    entry.span().start() > target && entry.span().start() <= latest_start
                })
            })
            .or_else(|| self.final_landing_index(target, incoming))
    }

    /// До какого момента сегмент на месте `index` «держит» цель перемотки.
    fn containment_end(&self, index: usize, rules: &ShelfRules) -> MediaTime {
        let entry = &self.entries[index];
        match entry.timing.end_kind() {
            SegmentEnd::Exact | SegmentEnd::Estimated { .. } => entry.span().end(),
            SegmentEnd::Open { .. } => {
                let next_start = self.entries[index + 1..]
                    .iter()
                    .map(ShelfEntry::span)
                    .map(MediaTimeSpan::start)
                    .find(|start| *start > entry.span().start());
                let reach = entry.reach(rules);
                next_start.map_or(reach, |next| next.min(reach))
            }
        }
    }

    /// Правило 3 [`Self::restart_index`]: вход окончен, цель не раньше конца
    /// самого позднего сегмента — посадка на него.
    fn final_landing_index(&self, target: MediaTime, incoming: IncomingMedia) -> Option<usize> {
        if incoming != IncomingMedia::Ended {
            return None;
        }
        let last = self.entries.len().checked_sub(1)?;
        (target >= self.entries[last].span().start()).then_some(last)
    }

    /// Есть ли цель перемотки на полке (правило — в [`Self::restart_index`]).
    pub(crate) fn holds(
        &self,
        target: MediaTime,
        rules: &ShelfRules,
        incoming: IncomingMedia,
    ) -> bool {
        self.restart_index(target, rules, incoming).is_some()
    }

    /// Готовит перемотку к `target`: сегмент с целью и всё после него снова
    /// ждут подачи, а сам сегмент подаётся первым и обязательно в НОВОМ эпизоде.
    /// `None` — цели на полке нет, ничего не меняется.
    pub(crate) fn prepare_restart(
        &mut self,
        target: MediaTime,
        rules: &ShelfRules,
        incoming: IncomingMedia,
    ) -> Option<FeedChoice> {
        let index = self.restart_index(target, rules, incoming)?;
        Some(self.restart_feeding_from(index))
    }

    /// Сегмент на месте `index` и всё после него снова ждут подачи; сам сегмент
    /// подаётся первым и обязательно в НОВОМ эпизоде.
    fn restart_feeding_from(&mut self, index: usize) -> FeedChoice {
        for entry in &mut self.entries[index..] {
            entry.delivery = Delivery::Waiting;
        }
        FeedChoice {
            index,
            continuity: EpisodeContinuity::MustRestart,
        }
    }

    /// Отмечает выбранный сегмент поданным и отдаёт то, что нужно трубе.
    pub(crate) fn take_for_feeding(
        &mut self,
        choice: FeedChoice,
        rules: &ShelfRules,
    ) -> Option<FedSegment> {
        let entry = self.entries.get_mut(choice.index)?;
        entry.delivery = Delivery::Fed;
        Some(FedSegment {
            media: entry.media.clone(),
            key: entry.key.clone(),
            span: entry.span(),
            fed_to: entry.fed_to(rules),
            continuity: choice.continuity,
        })
    }

    /// Самый давний сегмент, кончившийся не позже `limit`, чьи байты держит
    /// ТОЛЬКО полка: (место, конец).
    ///
    /// Сегмент, копия которого ещё в трубе эпизода (демуксер не дочитал), снимать
    /// с полки у потолка бессмысленно: память не освободится, а перемотка назад
    /// потеряет его.
    pub(crate) fn oldest_unshared_ended_by(&self, limit: MediaTime) -> Option<(usize, MediaTime)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.span().end() <= limit && !entry.media.is_shared())
            .map(|(index, entry)| (index, entry.span().end()))
            .min_by_key(|(_, end)| *end)
    }

    /// Самый поздний сегмент, начавшийся позже `limit`, чьи байты держит только
    /// полка: (место, начало).
    pub(crate) fn latest_unshared_started_after(
        &self,
        limit: MediaTime,
    ) -> Option<(usize, MediaTime)> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.span().start() > limit && !entry.media.is_shared())
            .map(|(index, entry)| (index, entry.span().start()))
            .max_by_key(|(_, start)| *start)
    }

    /// Снимает сегмент с полки.
    pub(crate) fn remove(&mut self, index: usize) -> Option<ShelfEntry> {
        (index < self.entries.len()).then(|| self.entries.remove(index))
    }

    /// Снимает с полки всё, что ещё не подано (полоса стала полосой без времени:
    /// эти сегменты больше никто не подаст). Поданные снимаются тоже: их байты
    /// дочитает труба эпизода, полка их больше не держит.
    pub(crate) fn take_all(&mut self) -> Vec<ShelfEntry> {
        std::mem::take(&mut self.entries)
    }

    /// Начало самого раннего сегмента на полке (`None` — полка пуста).
    pub(crate) fn earliest_start(&self) -> Option<MediaTime> {
        // Сегменты лежат по возрастанию начала: первый и есть самый ранний.
        self.entries.first().map(|entry| entry.span().start())
    }

    /// Конец непрерывного куска полки, покрывающего `position`.
    pub(crate) fn covered_run_end(
        &self,
        position: MediaTime,
        rules: &ShelfRules,
    ) -> Option<MediaTime> {
        self.covered_run(position, rules).map(MediaTimeSpan::end)
    }

    /// Непрерывный кусок полки, покрывающий `position`.
    ///
    /// Кусок — сегменты подряд: следующий начинается не дальше наибольшей дыры
    /// от досягаемости предыдущего. Он покрывает позицию, если
    /// `начало − дыра ≤ position < конец` (дыру плеер перешагивает; конец —
    /// лучший известный, без открытого хвоста: «буферизовано» не преувеличивается).
    pub(crate) fn covered_run(
        &self,
        position: MediaTime,
        rules: &ShelfRules,
    ) -> Option<MediaTimeSpan> {
        let gap = rules.max_contiguous_gap_micros;
        let covers = |run: &CoveredRun| earlier(run.start, gap) <= position && position < run.end;
        let mut run: Option<CoveredRun> = None;
        for entry in &self.entries {
            let (start, end, reach) =
                (entry.span().start(), entry.span().end(), entry.reach(rules));
            run = match run {
                Some(current) if start <= later(current.reach, gap) => {
                    Some(current.extended(end, reach))
                }
                Some(current) if covers(&current) => {
                    return MediaTimeSpan::new(current.start, current.end);
                }
                _ => Some(CoveredRun { start, end, reach }),
            };
        }
        run.filter(covers)
            .and_then(|run| MediaTimeSpan::new(run.start, run.end))
    }

    /// Выбрасывает всё (плеер закрыл поток).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Времена сегментов и подан ли каждый — для тестов.
    #[cfg(test)]
    pub(crate) fn layout(&self) -> Vec<(i64, i64, Delivery)> {
        self.entries
            .iter()
            .map(|entry| {
                (
                    entry.span().start().as_micros(),
                    entry.span().end().as_micros(),
                    entry.delivery,
                )
            })
            .collect()
    }
}

/// Непрерывный кусок полки, собираемый по порядку сегментов.
#[derive(Debug, Clone, Copy)]
struct CoveredRun {
    /// Начало куска.
    start: MediaTime,
    /// Лучший известный конец куска.
    end: MediaTime,
    /// Досягаемость куска — досягаемость сегмента с самым поздним концом.
    reach: MediaTime,
}

impl CoveredRun {
    /// Кусок, продолженный сегментом с концом `end` и досягаемостью `reach`.
    /// Досягаемость берётся у сегмента, кончающегося позже: открытый хвост
    /// раннего сегмента не должен «дотягивать» кусок через дыру после позднего.
    fn extended(self, end: MediaTime, reach: MediaTime) -> Self {
        if end >= self.end {
            Self {
                start: self.start,
                end,
                reach,
            }
        } else {
            self
        }
    }
}

#[cfg(test)]
#[path = "shelf_tests.rs"]
mod tests;
