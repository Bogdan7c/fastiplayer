//! Read-only порт «timestamp origin» эпох HLS: как сырое время контейнера превращается во время плеера.
//!
//! Демуксер HLS переводит пакеты видео/аудио на общую шкалу плеера формулой
//! `player = raw - media_origin + timeline_start`, где `media_origin` - сырая метка времени
//! первого пакета эпохи, а `timeline_start` - начало эпохи на шкале плеера. Субтитры
//! (WebVTT-сегменты со своей картой `X-TIMESTAMP-MAP`) обязаны попасть на ту же шкалу
//! ровно с теми же числами, иначе реплики разойдутся с картинкой.
//!
//! Владелец истины - демуксер: он публикует пару в тот самый момент, когда фиксирует/использует
//! origin для пересчёта пакетов (в том числе origin, восстановленный из seek-индекса после seek).
//! Потребитель (субтитры) получает только [`HlsTimestampOrigins`] - клонируемый read-only
//! обработчик. Писать в него может лишь внутренний [`HlsTimestampOriginsWriter`].
//!
//! Ключ - номер discontinuity-эпохи (`EXT-X-DISCONTINUITY-SEQUENCE` + число разрывов): сегмент
//! субтитров вычисляет его так же, как сегмент видео.
//!
//! VOD: у эпохи ровно одна пара. Live: демуксер сегментный, origin фиксируется заново на каждом
//! сегменте, поэтому у эпохи набор пар (по одной на сегмент, отсортирован по `media_origin`), а
//! читатель выбирает ближайшую пару не позже нужного сырого времени.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Сколько последних discontinuity-эпох хранится (структурная граница памяти на бесконечном live).
/// Самые старые по номеру эпохи вытесняются первыми.
const MAXIMUM_RETAINED_EPOCHS: usize = 256;

/// Сколько пар `(media_origin, timeline_start)` хранится в одной эпохе (live: по паре на сегмент).
/// При переполнении вытесняется пара, опубликованная ДАВНЬШЕ всех (по возрасту вставки, а не по
/// значению `media_origin`: 33-битный счётчик оборачивается, и после оборота «самое маленькое»
/// сырое время принадлежит самой свежей паре). Читатель тогда использует самую раннюю из
/// оставшихся - ошибка ограничена дрейфом между сегментами, а не разрывом шкалы.
const MAXIMUM_ORIGINS_PER_EPOCH: usize = 512;

/// Пара «сырое время начала эпохи/сегмента» -> «время плеера», которой демуксер реально пользовался.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlsEpochTimestampOrigin {
    media_origin: Duration,
    timeline_start: Duration,
}

impl HlsEpochTimestampOrigin {
    pub(crate) const fn new(media_origin: Duration, timeline_start: Duration) -> Self {
        Self {
            media_origin,
            timeline_start,
        }
    }

    /// Сырое время контейнера (PTS/DTS в секундах), вычитаемое демуксером из каждого пакета.
    #[must_use]
    pub const fn media_origin(self) -> Duration {
        self.media_origin
    }

    /// Время плеера, которое получает пакет с сырым временем `media_origin`.
    #[must_use]
    pub const fn timeline_start(self) -> Duration {
        self.timeline_start
    }
}

/// Пары одной эпохи: отсортированы по `media_origin` для поиска, возраст хранится отдельно.
#[derive(Default)]
struct EpochOrigins {
    /// Пары по возрастанию сырого `media_origin` (двоичный поиск читателя).
    sorted: Vec<HlsEpochTimestampOrigin>,
    /// `media_origin` пар от самой давней публикации к самой свежей (очередь вытеснения).
    insertion_order: VecDeque<Duration>,
}

impl EpochOrigins {
    fn publish(&mut self, origin: HlsEpochTimestampOrigin) {
        match self
            .sorted
            .binary_search_by_key(&origin.media_origin, |known| known.media_origin)
        {
            // Тот же сырой origin: актуальна последняя опубликованная пара, а возраст обновляется
            // (повторная публикация после seek назад делает пару снова «свежей»).
            Ok(position) => {
                self.sorted[position] = origin;
                if let Some(age_position) = self
                    .insertion_order
                    .iter()
                    .position(|known| *known == origin.media_origin)
                {
                    self.insertion_order.remove(age_position);
                }
            }
            Err(position) => self.sorted.insert(position, origin),
        }
        self.insertion_order.push_back(origin.media_origin);
        while self.sorted.len() > MAXIMUM_ORIGINS_PER_EPOCH {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            if let Ok(position) = self
                .sorted
                .binary_search_by_key(&oldest, |known| known.media_origin)
            {
                self.sorted.remove(position);
            }
        }
    }
}

/// Общее состояние: эпохи по возрастанию номера.
#[derive(Default)]
struct OriginsState {
    epochs: BTreeMap<u64, EpochOrigins>,
    /// Сколько раз состояние реально писали (проверка, что демуксер не публикует впустую).
    #[cfg(test)]
    publish_count: u64,
}

impl OriginsState {
    fn publish(&mut self, discontinuity_sequence: u64, origin: HlsEpochTimestampOrigin) {
        #[cfg(test)]
        {
            self.publish_count += 1;
        }
        self.epochs
            .entry(discontinuity_sequence)
            .or_default()
            .publish(origin);
        while self.epochs.len() > MAXIMUM_RETAINED_EPOCHS {
            self.epochs.pop_first();
        }
    }
}

type SharedOrigins = Arc<Mutex<OriginsState>>;

fn lock(shared: &SharedOrigins) -> MutexGuard<'_, OriginsState> {
    // Poison не скрывает данные: состояние - простая карта, любая её версия валидна.
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read-only читатель origin-ов эпох; дёшево клонируется и безопасен между потоками.
#[derive(Clone)]
pub struct HlsTimestampOrigins {
    shared: SharedOrigins,
}

impl HlsTimestampOrigins {
    /// Создаёт пару «читатель + писатель» одного открытого HLS-источника.
    pub(crate) fn new_pair() -> (Self, HlsTimestampOriginsWriter) {
        let shared = SharedOrigins::default();
        (
            Self {
                shared: Arc::clone(&shared),
            },
            HlsTimestampOriginsWriter {
                shared: Some(shared),
                last_published: None,
            },
        )
    }

    /// Первая (самая ранняя из сохранённых) пара эпохи; для VOD это единственная пара эпохи.
    /// `None`, пока демуксер ещё не дошёл до первого пакета эпохи.
    #[must_use]
    pub fn epoch_origin(&self, discontinuity_sequence: u64) -> Option<HlsEpochTimestampOrigin> {
        lock(&self.shared)
            .epochs
            .get(&discontinuity_sequence)
            .and_then(|origins| origins.sorted.first().copied())
    }

    /// Пара, действующая для сырого времени `media_time`: последняя с `media_origin <= media_time`,
    /// а если такой нет - самая ранняя. Для VOD совпадает с [`Self::epoch_origin`].
    #[must_use]
    pub fn origin_for_media_time(
        &self,
        discontinuity_sequence: u64,
        media_time: Duration,
    ) -> Option<HlsEpochTimestampOrigin> {
        let state = lock(&self.shared);
        let origins = &state.epochs.get(&discontinuity_sequence)?.sorted;
        let not_after = origins.partition_point(|known| known.media_origin <= media_time);
        origins
            .get(not_after.saturating_sub(1))
            .or_else(|| origins.first())
            .copied()
    }

    /// Сколько эпох сейчас известно (для диагностики и тестов границы памяти).
    #[must_use]
    pub fn known_epoch_count(&self) -> usize {
        lock(&self.shared).epochs.len()
    }
}

impl std::fmt::Debug for HlsTimestampOrigins {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HlsTimestampOrigins")
            .field("known_epochs", &self.known_epoch_count())
            .finish()
    }
}

/// Последняя пара, которую писатель отправил в общее состояние (ключ эпохи + сама пара).
type PublishedPair = (u64, HlsEpochTimestampOrigin);

/// Писатель для демуксеров; `disabled()` ничего не публикует (компоненты без субтитров).
#[derive(Default)]
pub(crate) struct HlsTimestampOriginsWriter {
    shared: Option<SharedOrigins>,
    /// Что писатель опубликовал последним: повтор той же пары (каждый пакет VOD) пропускается.
    last_published: Option<PublishedPair>,
}

impl Clone for HlsTimestampOriginsWriter {
    /// Копия делит состояние, но НЕ кэш последней публикации: кэш верен только пока другой
    /// писатель не менял те же эпохи, а копия этого не гарантирует.
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            last_published: None,
        }
    }
}

impl std::fmt::Debug for HlsTimestampOriginsWriter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HlsTimestampOriginsWriter")
            .field("enabled", &self.shared.is_some())
            .finish()
    }
}

impl HlsTimestampOriginsWriter {
    /// Писатель-пустышка: используется alternate audio и тестовыми индексами.
    pub(crate) fn disabled() -> Self {
        Self::default()
    }

    /// Публикует пару, только если она новая или изменилась с прошлой публикации этого писателя.
    ///
    /// Нужна VOD-демуксеру, который видит пару на каждом пакете: без неё каждый пакет брал бы
    /// общий замок. Кэш - собственное поле писателя, поэтому лишнего замка на пакет нет.
    pub(crate) fn publish_if_changed(
        &mut self,
        discontinuity_sequence: u64,
        media_origin: Duration,
        timeline_start: Duration,
    ) {
        let pair = (
            discontinuity_sequence,
            HlsEpochTimestampOrigin::new(media_origin, timeline_start),
        );
        if self.last_published == Some(pair) {
            return;
        }
        self.publish(discontinuity_sequence, media_origin, timeline_start);
        if self.shared.is_some() {
            self.last_published = Some(pair);
        }
    }

    /// Публикует пару, которой демуксер только что воспользовался для эпохи.
    pub(crate) fn publish(
        &self,
        discontinuity_sequence: u64,
        media_origin: Duration,
        timeline_start: Duration,
    ) {
        if let Some(shared) = &self.shared {
            lock(shared).publish(
                discontinuity_sequence,
                HlsEpochTimestampOrigin::new(media_origin, timeline_start),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn disabled_writer_publishes_nothing_and_reader_is_empty() {
        let (reader, _writer) = HlsTimestampOrigins::new_pair();
        HlsTimestampOriginsWriter::disabled().publish(0, secs(1), secs(2));
        assert_eq!(reader.epoch_origin(0), None);
        assert_eq!(reader.known_epoch_count(), 0);
    }

    #[test]
    fn published_pair_is_visible_through_every_reader_clone() {
        let (reader, writer) = HlsTimestampOrigins::new_pair();
        let clone = reader.clone();
        writer.publish(3, secs(10), secs(0));
        let expected = HlsEpochTimestampOrigin::new(secs(10), secs(0));
        assert_eq!(reader.epoch_origin(3), Some(expected));
        assert_eq!(clone.epoch_origin(3), Some(expected));
        assert_eq!(clone.epoch_origin(4), None);
    }

    #[test]
    fn repeated_publish_of_same_origin_does_not_grow_the_epoch() {
        let (reader, writer) = HlsTimestampOrigins::new_pair();
        for _ in 0..1_000 {
            writer.publish(0, secs(10), secs(0));
        }
        assert_eq!(reader.known_epoch_count(), 1);
        assert_eq!(
            reader.origin_for_media_time(0, secs(500)),
            Some(HlsEpochTimestampOrigin::new(secs(10), secs(0)))
        );
    }

    #[test]
    fn live_epoch_picks_the_latest_pair_not_after_media_time() {
        let (reader, writer) = HlsTimestampOrigins::new_pair();
        // Публикация не по порядку (seek назад) не ломает сортировку.
        writer.publish(1, secs(26), secs(16));
        writer.publish(1, secs(10), secs(0));
        writer.publish(1, secs(18), secs(8));
        let at = |media| {
            reader
                .origin_for_media_time(1, media)
                .map(|o| o.media_origin())
        };
        assert_eq!(at(secs(11)), Some(secs(10)));
        assert_eq!(at(secs(18)), Some(secs(18)));
        assert_eq!(at(secs(100)), Some(secs(26)));
        // До самой ранней пары берётся самая ранняя.
        assert_eq!(at(secs(1)), Some(secs(10)));
        assert_eq!(
            reader.epoch_origin(1).map(|o| o.media_origin()),
            Some(secs(10))
        );
    }

    #[test]
    fn retained_epochs_are_bounded_and_keep_the_most_recent() {
        let (reader, writer) = HlsTimestampOrigins::new_pair();
        let total = u64::try_from(MAXIMUM_RETAINED_EPOCHS).unwrap_or(u64::MAX) + 10;
        for epoch in 0..total {
            writer.publish(epoch, secs(epoch), secs(epoch));
        }
        assert_eq!(reader.known_epoch_count(), MAXIMUM_RETAINED_EPOCHS);
        assert_eq!(reader.epoch_origin(0), None);
        assert!(reader.epoch_origin(total - 1).is_some());
    }

    #[test]
    fn eviction_goes_by_insertion_age_even_when_the_raw_clock_wraps() {
        let (reader, writer) = HlsTimestampOrigins::new_pair();
        // Сырые origin-ы растут к границе 2^33 тиков (~95443 с), затем оборот: новые пары
        // имеют МАЛЕНЬКОЕ сырое значение, но они самые свежие.
        let wrap_secs = 95_443;
        let before_wrap = u64::try_from(MAXIMUM_ORIGINS_PER_EPOCH).unwrap_or(u64::MAX);
        for segment in 0..before_wrap {
            writer.publish(0, secs(wrap_secs - before_wrap + segment), secs(segment));
        }
        // Первая пара после оборота вытесняет самую давнюю (а не самую маленькую по значению).
        writer.publish(0, secs(3), secs(before_wrap));
        let oldest_gone = reader.origin_for_media_time(0, secs(wrap_secs - before_wrap));
        // Самая давняя пара (wrap_secs - before_wrap) ушла: ближайшая «не позже» - свежая пара оборота.
        assert_eq!(
            oldest_gone.map(|origin| origin.media_origin()),
            Some(secs(3)),
            "the wrapped (smallest raw) pair must survive eviction"
        );
        assert_eq!(
            reader.epoch_origin(0).map(|origin| origin.media_origin()),
            Some(secs(3))
        );
        assert_eq!(
            reader
                .origin_for_media_time(0, secs(wrap_secs))
                .map(|origin| origin.media_origin()),
            Some(secs(wrap_secs - 1))
        );
    }

    #[test]
    fn republishing_an_identical_pair_does_not_touch_the_shared_state() {
        let (reader, mut writer) = HlsTimestampOrigins::new_pair();
        writer.publish_if_changed(0, secs(10), secs(0));
        let revision = || lock(&reader.shared).publish_count;
        assert_eq!(revision(), 1);
        for _ in 0..100 {
            writer.publish_if_changed(0, secs(10), secs(0));
        }
        assert_eq!(revision(), 1, "identical pairs are skipped");
        // Изменённая пара (другое начало на шкале плеера) обязана дойти до читателя.
        writer.publish_if_changed(0, secs(10), secs(5));
        assert_eq!(revision(), 2);
        assert_eq!(
            reader.epoch_origin(0),
            Some(HlsEpochTimestampOrigin::new(secs(10), secs(5)))
        );
        // Другая эпоха - тоже новая пара.
        writer.publish_if_changed(1, secs(10), secs(5));
        assert_eq!(revision(), 3);
    }

    #[test]
    fn origins_per_epoch_are_bounded_and_drop_the_oldest() {
        let (reader, writer) = HlsTimestampOrigins::new_pair();
        let total = u64::try_from(MAXIMUM_ORIGINS_PER_EPOCH).unwrap_or(u64::MAX) + 5;
        for segment in 0..total {
            writer.publish(0, secs(segment), secs(segment));
        }
        assert_eq!(
            reader.epoch_origin(0).map(|o| o.media_origin()),
            Some(secs(5))
        );
        assert_eq!(
            reader
                .origin_for_media_time(0, secs(total))
                .map(|o| o.media_origin()),
            Some(secs(total - 1))
        );
    }
}
