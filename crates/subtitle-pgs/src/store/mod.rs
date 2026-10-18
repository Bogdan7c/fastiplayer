//! Хранилище композиций PGS одной дорожки: интервалы показа и политика памяти.
//!
//! Композиция = момент начала + реплики (пусто = «очистка»). Конец композиции — начало
//! СЛЕДУЮЩЕЙ, но только если следующая получена подряд в том же «прогоне чтения»; иначе
//! конец неизвестен (после seek мы не знаем, что было в пропущенном участке).
//!
//! Правила:
//! - последняя композиция ОТКРЫТА: показывается до следующего набора (в Matroska блоки PGS
//!   часто без длительности);
//! - явная длительность блока ([`CompositionEnd::ExplicitAt`]) задаёт конец, но следующий
//!   набор побеждает, если наступает раньше;
//! - [`PgsCompositionStore::note_discontinuity`] (seek) выбрасывает ОТКРЫТЫЕ композиции —
//!   показывать их дальше значило бы рисковать «залипшим» субтитром — и начинает новый
//!   прогон; закрытые сохраняются;
//! - повторное чтение уже известной композиции (seek назад) дубликатов не создаёт.
//!
//! Инкрементальность: [`PgsCompositionStore::insert`] возвращает [`StoreChange`] — что именно
//! изменилось в терминах «дописать в хвост + укоротить предшественника» либо «нужна
//! пересборка». Композиции имеют стабильные [`CompositionHandle`].
//!
//! Память: хранятся только сжатые данные (RLE + палитра); см. [`PgsRetention`].

mod change;
mod ledger;
#[cfg(test)]
mod tests;

pub use self::change::{
    CompositionHandle, DiscontinuityOutcome, PredecessorClosed, StoreChange, TailAppend,
};

use std::collections::BTreeMap;
use std::ops::Bound;
use subtitle_core::{
    PicturePalette, SubtitleContent, SubtitleEvent, SubtitleLayer, SubtitlePictureCue, SubtitleTime,
};

use self::ledger::SharedBytesLedger;
use crate::decoder::PgsComposition;

/// Известен ли конец показа, пришедший вместе с блоком.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositionEnd {
    /// Блок без длительности: показ до следующего набора.
    UntilNextDisplaySet,
    /// Блок с ненулевой длительностью: показ до указанного момента (если раньше не придёт
    /// следующий набор).
    ExplicitAt(SubtitleTime),
}

/// Политика удержания данных в памяти.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgsRetention {
    /// Показываемая дорожка: общий бюджет памяти; вытесняются композиции, самые далёкие от
    /// позиции воспроизведения. Композиция, активная в позиции, не вытесняется никогда.
    Shown { budget_bytes: usize },
    /// Фоновая дорожка: хвост заданной длины до позиции воспроизведения (плюс открытая композиция).
    Background { trailing_window: SubtitleTime },
}

/// Итог одного вызова [`PgsCompositionStore::enforce_retention`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionOutcome {
    pub evicted_compositions: usize,
}

impl RetentionOutcome {
    /// Вытеснение убирает события, а шкала потребителя убирать их не умеет: если что-то
    /// вытеснено, шкалу надо пересобрать.
    pub const fn requires_rebuild(self) -> bool {
        self.evicted_compositions > 0
    }
}

/// Накопленная статистика хранилища.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PgsStoreStatistics {
    /// Вытеснено окном фоновой дорожки.
    pub evicted_by_trailing_window: u64,
    /// Вытеснено бюджетом памяти показываемой дорожки.
    pub evicted_by_budget: u64,
    /// Открытых композиций, выброшенных при разрыве потока.
    pub open_dropped_on_discontinuity: u64,
    /// Повторно прочитанных композиций, совпавших с уже хранимыми.
    pub duplicate_compositions_ignored: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndState {
    Open,
    Closed(SubtitleTime),
}

/// Итог `store_composition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreResult {
    /// Точно такая же композиция уже хранилась.
    Duplicate,
    /// Новая композиция в свободный момент.
    Added(CompositionHandle),
    /// Содержимое в занятый момент заменено другим.
    Replaced,
}

/// Предшественник новой композиции и его конец показа до вставки.
struct PredecessorDisplay {
    start: SubtitleTime,
    handle: CompositionHandle,
    display_end: SubtitleTime,
}

struct StoredComposition {
    handle: CompositionHandle,
    cues: Vec<SubtitlePictureCue>,
    end: EndState,
    /// В каком прогоне чтения получена.
    run: u64,
}

/// Накладные расходы на композицию и реплику сверх данных картинок (оценка).
const COMPOSITION_OVERHEAD_BYTES: usize = 64;
const CUE_OVERHEAD_BYTES: usize = 96;

/// Хранилище композиций дорожки PGS.
pub struct PgsCompositionStore {
    compositions: BTreeMap<SubtitleTime, StoredComposition>,
    current_run: u64,
    revision: u64,
    /// Следующий номер для [`CompositionHandle`]; только растёт.
    next_handle_sequence: u64,
    pictures: SharedBytesLedger,
    palettes: SharedBytesLedger,
    overhead_bytes: usize,
    stats: PgsStoreStatistics,
}

impl Default for PgsCompositionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PgsCompositionStore {
    /// Конец открытой композиции в [`Self::events`], если известной следующей нет:
    /// «до следующего набора». Запас до `i64::MAX` оставлен под сдвиг синхронизации.
    pub const OPEN_END: SubtitleTime = SubtitleTime::from_micros(i64::MAX / 2);

    pub fn new() -> Self {
        Self {
            compositions: BTreeMap::new(),
            current_run: 0,
            revision: 0,
            next_handle_sequence: 0,
            pictures: SharedBytesLedger::default(),
            palettes: SharedBytesLedger::default(),
            overhead_bytes: 0,
            stats: PgsStoreStatistics::default(),
        }
    }

    /// Счётчик изменений: растёт при любом изменении набора композиций и НЕ растёт при
    /// no-op (повторное чтение того же). По нему приложение понимает, что пора пересобрать
    /// временную шкалу.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn statistics(&self) -> &PgsStoreStatistics {
        &self.stats
    }

    pub fn len(&self) -> usize {
        self.compositions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.compositions.is_empty()
    }

    /// Оценка занимаемой памяти (общие картинки и палитры считаются один раз).
    pub fn estimated_bytes(&self) -> usize {
        self.pictures
            .total_bytes()
            .saturating_add(self.palettes.total_bytes())
            .saturating_add(self.overhead_bytes)
    }

    /// Разрыв потока (seek): открытые композиции выбрасываются, следующий приём — новый прогон.
    /// Итог говорит, нужно ли потребителю пересобрать шкалу (выброшены композиции с картинками).
    pub fn note_discontinuity(&mut self) -> DiscontinuityOutcome {
        let open_starts: Vec<SubtitleTime> = self
            .compositions
            .iter()
            .filter(|(_, stored)| stored.end == EndState::Open)
            .map(|(start, _)| *start)
            .collect();
        let dropped_with_pictures = open_starts
            .iter()
            .filter(|start| {
                self.compositions
                    .get(start)
                    .is_some_and(|stored| !stored.cues.is_empty())
            })
            .count();
        for start in &open_starts {
            self.remove_composition(*start);
        }
        self.stats.open_dropped_on_discontinuity += open_starts.len() as u64;
        self.current_run += 1;
        if !open_starts.is_empty() {
            self.revision += 1;
        }
        DiscontinuityOutcome {
            dropped_open_compositions: open_starts.len(),
            dropped_with_pictures,
        }
    }

    /// Принимает композицию из декодера и описывает изменение для инкрементального
    /// потребителя (см. [`StoreChange`]).
    pub fn insert(&mut self, composition: PgsComposition, end: CompositionEnd) -> StoreChange {
        let PgsComposition { start, cues } = composition;
        let explicit_end = match end {
            CompositionEnd::ExplicitAt(at) if at > start => EndState::Closed(at),
            CompositionEnd::ExplicitAt(_) | CompositionEnd::UntilNextDisplaySet => EndState::Open,
        };
        // Хвост определяется ДО вставки: «позже всех хранимых начал».
        let is_tail = self
            .compositions
            .range((Bound::Excluded(start), Bound::Unbounded))
            .next()
            .is_none();
        let predecessor_before = self.predecessor_display(start);
        let stored = self.store_composition(start, cues, explicit_end);
        let changed_by_close = self.close_predecessor(start);
        if stored != StoreResult::Duplicate || changed_by_close {
            self.revision += 1;
        }
        // Изменился ли ПОКАЗ предшественника (а не только внутреннее состояние конца).
        let predecessor_closed = predecessor_before.and_then(|before| {
            let after = self.display_end_of(before.start)?;
            (after != before.display_end).then_some(PredecessorClosed {
                composition: before.handle,
                new_end: after,
            })
        });
        match stored {
            StoreResult::Duplicate if predecessor_closed.is_none() => StoreChange::Duplicate,
            StoreResult::Added(handle) if is_tail => StoreChange::AppendedAtTail(TailAppend {
                composition: handle,
                events: self.events_at(start),
                predecessor_closed,
            }),
            StoreResult::Duplicate | StoreResult::Added(_) | StoreResult::Replaced => {
                StoreChange::RequiresRebuild
            }
        }
    }

    /// Предыдущая по времени композиция и её текущий конец показа (до вставки новой).
    fn predecessor_display(&self, start: SubtitleTime) -> Option<PredecessorDisplay> {
        let (key, stored) = self.compositions.range(..start).next_back()?;
        Some(PredecessorDisplay {
            start: *key,
            handle: stored.handle,
            display_end: self.display_end(*key, stored),
        })
    }

    fn display_end_of(&self, start: SubtitleTime) -> Option<SubtitleTime> {
        self.compositions
            .get(&start)
            .map(|stored| self.display_end(start, stored))
    }

    fn issue_handle(&mut self) -> CompositionHandle {
        let handle = CompositionHandle::from_sequence(self.next_handle_sequence);
        self.next_handle_sequence += 1;
        handle
    }

    /// Кладёт саму композицию.
    fn store_composition(
        &mut self,
        start: SubtitleTime,
        cues: Vec<SubtitlePictureCue>,
        explicit_end: EndState,
    ) -> StoreResult {
        let Some(existing) = self.compositions.get(&start) else {
            self.account_added(&cues);
            let handle = self.issue_handle();
            self.compositions.insert(
                start,
                StoredComposition {
                    handle,
                    cues,
                    end: explicit_end,
                    run: self.current_run,
                },
            );
            return StoreResult::Added(handle);
        };
        if existing.cues == cues {
            self.stats.duplicate_compositions_ignored += 1;
            return StoreResult::Duplicate;
        }
        // Другое содержимое в тот же момент: побеждает последнее прочитанное. Известный конец
        // сохраняется — он определялся реальным следующим набором, а не содержимым.
        let kept_end = match existing.end {
            EndState::Closed(_) => existing.end,
            EndState::Open => explicit_end,
        };
        self.remove_composition(start);
        self.account_added(&cues);
        // Новое содержимое = новая композиция: старый номер не переиспользуется.
        let handle = self.issue_handle();
        self.compositions.insert(
            start,
            StoredComposition {
                handle,
                cues,
                end: kept_end,
                run: self.current_run,
            },
        );
        StoreResult::Replaced
    }

    /// Предыдущая композиция заканчивается там, где началась эта: так как это реальный
    /// следующий набор потока, он побеждает любой более поздний конец.
    fn close_predecessor(&mut self, start: SubtitleTime) -> bool {
        let current_run = self.current_run;
        let Some((_, predecessor)) = self.compositions.range_mut(..start).next_back() else {
            return false;
        };
        match predecessor.end {
            EndState::Open if predecessor.run == current_run => {
                predecessor.end = EndState::Closed(start);
                true
            }
            EndState::Closed(end) if end > start => {
                predecessor.end = EndState::Closed(start);
                true
            }
            EndState::Open | EndState::Closed(_) => false,
        }
    }

    fn account_added(&mut self, cues: &[SubtitlePictureCue]) {
        self.overhead_bytes += composition_overhead(cues);
        for cue in cues {
            self.pictures
                .add(cue.picture(), cue.picture().encoded_len());
            self.palettes.add(cue.palette(), PicturePalette::BYTE_LEN);
        }
    }

    fn remove_composition(&mut self, start: SubtitleTime) {
        if let Some(stored) = self.compositions.remove(&start) {
            self.overhead_bytes = self
                .overhead_bytes
                .saturating_sub(composition_overhead(&stored.cues));
            for cue in &stored.cues {
                self.pictures.remove(cue.picture());
                self.palettes.remove(cue.palette());
            }
        }
    }

    /// Конец композиции для показа: известный; иначе начало следующей хранимой (чтобы не
    /// накладывались две композиции); иначе [`Self::OPEN_END`].
    fn display_end(&self, start: SubtitleTime, stored: &StoredComposition) -> SubtitleTime {
        match stored.end {
            EndState::Closed(end) => end,
            EndState::Open => self
                .compositions
                .range((Bound::Excluded(start), Bound::Unbounded))
                .next()
                .map_or(Self::OPEN_END, |(next_start, _)| *next_start),
        }
    }

    /// События для временной шкалы: по одному на реплику, слой = порядок объекта в композиции.
    /// «Очистки» событий не дают.
    pub fn events(&self) -> Vec<SubtitleEvent> {
        self.events_by_composition()
            .into_iter()
            .flat_map(|(_, events)| events)
            .collect()
    }

    /// То же, что [`Self::events`], но сгруппировано по композициям (в порядке времени):
    /// потребитель при пересборке шкалы заново строит соответствие «номер -> события».
    /// У «очистки» список пуст.
    pub fn events_by_composition(&self) -> Vec<(CompositionHandle, Vec<SubtitleEvent>)> {
        self.compositions
            .iter()
            .map(|(start, stored)| (stored.handle, self.events_of(*start, stored)))
            .collect()
    }

    fn events_at(&self, start: SubtitleTime) -> Vec<SubtitleEvent> {
        self.compositions
            .get(&start)
            .map(|stored| self.events_of(start, stored))
            .unwrap_or_default()
    }

    fn events_of(&self, start: SubtitleTime, stored: &StoredComposition) -> Vec<SubtitleEvent> {
        let end = self.display_end(start, stored);
        let mut events = Vec::with_capacity(stored.cues.len());
        for (layer, cue) in stored.cues.iter().enumerate() {
            let layer = SubtitleLayer::new(i32::try_from(layer).unwrap_or(i32::MAX));
            // end > start гарантировано построением (ключи уникальны и упорядочены,
            // явный конец принимается только после начала); Err недостижим.
            if let Ok(event) =
                SubtitleEvent::new(start, end, layer, SubtitleContent::Picture(cue.clone()))
            {
                events.push(event);
            }
        }
        events
    }

    /// Применяет политику памяти относительно позиции воспроизведения.
    pub fn enforce_retention(
        &mut self,
        playback_position: SubtitleTime,
        policy: PgsRetention,
    ) -> RetentionOutcome {
        let evicted = match policy {
            PgsRetention::Background { trailing_window } => {
                self.evict_before_window(playback_position, trailing_window)
            }
            PgsRetention::Shown { budget_bytes } => {
                self.evict_to_budget(playback_position, budget_bytes)
            }
        };
        if evicted > 0 {
            self.revision += 1;
        }
        RetentionOutcome {
            evicted_compositions: evicted,
        }
    }

    /// Фон: остаются композиции, чей интервал пересекает `[position - window, +inf)`.
    fn evict_before_window(&mut self, position: SubtitleTime, window: SubtitleTime) -> usize {
        let cutoff = SubtitleTime::from_micros(
            position
                .as_micros()
                .saturating_sub(window.as_micros().max(0)),
        );
        let expired: Vec<SubtitleTime> = self
            .compositions
            .iter()
            .filter(|(_, stored)| matches!(stored.end, EndState::Closed(end) if end <= cutoff))
            .map(|(start, _)| *start)
            .collect();
        for start in &expired {
            self.remove_composition(*start);
        }
        self.stats.evicted_by_trailing_window += expired.len() as u64;
        expired.len()
    }

    /// Показываемая дорожка: вытесняем самые далёкие от позиции, пока не влезем в бюджет.
    ///
    /// Расстояния считаются ОДИН раз и сортируются (O(n log n)): вытеснение композиции не
    /// меняет показ остальных (см. [`Self::remove_without_stretching_predecessor`]), поэтому
    /// порядок не нужно пересчитывать после каждого удаления. Порядок: дальше — раньше;
    /// при равенстве расстояний раньше уходит более поздняя по времени.
    fn evict_to_budget(&mut self, position: SubtitleTime, budget_bytes: usize) -> usize {
        if self.estimated_bytes() <= budget_bytes {
            return 0;
        }
        // Активные в позиции композиции в список не попадают — их не трогаем никогда.
        let mut candidates: Vec<(i64, SubtitleTime)> = self
            .compositions
            .iter()
            .filter_map(|(start, stored)| {
                let end = self.display_end(*start, stored);
                distance_from(position, *start, end).map(|distance| (distance, *start))
            })
            .collect();
        candidates.sort_unstable_by(|left, right| right.cmp(left));
        let mut evicted = 0;
        for (_, start) in candidates {
            if self.estimated_bytes() <= budget_bytes {
                break;
            }
            self.remove_without_stretching_predecessor(start);
            evicted += 1;
        }
        self.stats.evicted_by_budget += evicted as u64;
        evicted
    }

    /// Удаляет композицию, не меняя показ её предшественника.
    ///
    /// Конец ОТКРЫТОЙ композиции вычисляется как начало следующей хранимой. Если просто
    /// удалить эту следующую, предшественник растянулся бы до [`Self::OPEN_END`] и закрыл бы
    /// собой всё, что шло после (залипший субтитр). Поэтому перед удалением конец
    /// открытого предшественника фиксируется на текущем значении (начале удаляемой).
    fn remove_without_stretching_predecessor(&mut self, start: SubtitleTime) {
        if let Some((_, predecessor)) = self.compositions.range_mut(..start).next_back()
            && predecessor.end == EndState::Open
        {
            predecessor.end = EndState::Closed(start);
        }
        self.remove_composition(start);
    }
}

/// Расстояние композиции `[start, end)` до позиции; `None`, если она активна в позиции
/// (такую вытеснять нельзя).
fn distance_from(position: SubtitleTime, start: SubtitleTime, end: SubtitleTime) -> Option<i64> {
    if start <= position && position < end {
        None
    } else if start > position {
        Some(start.as_micros().saturating_sub(position.as_micros()))
    } else {
        Some(position.as_micros().saturating_sub(end.as_micros()))
    }
}

fn composition_overhead(cues: &[SubtitlePictureCue]) -> usize {
    COMPOSITION_OVERHEAD_BYTES + cues.len() * CUE_OVERHEAD_BYTES
}
