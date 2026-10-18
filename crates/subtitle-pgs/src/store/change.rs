//! Типизированные итоги изменений хранилища композиций для инкрементального потребителя.
//!
//! Потребитель (приложение) ведёт временную шкалу, которая умеет только ДОПИСЫВАТЬ события
//! и УКОРАЧИВАТЬ конец существующего. Каждое изменение хранилища описано в этих терминах;
//! всё, что так выразить нельзя, — [`StoreChange::RequiresRebuild`] (полная пересборка
//! шкалы под новым источником).

use subtitle_core::{SubtitleEvent, SubtitleTime};

/// Стабильный номер композиции в пределах жизни одного хранилища.
///
/// Выдаётся по возрастающему счётчику и НИКОГДА не переиспользуется: композиция, вытесненная
/// или замещённая другим содержимым в тот же момент, теряет свой номер навсегда, а новая
/// получает свежий. Поэтому потребитель может хранить `CompositionHandle -> id событий
/// шкалы` без риска перепутать композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CompositionHandle(u64);

impl CompositionHandle {
    pub(super) const fn from_sequence(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Порядковый номер выдачи (для диагностики).
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

/// Композиция, дописанная в хвост: её начало позже начал всех хранимых композиций.
#[derive(Debug, Clone, PartialEq)]
pub struct TailAppend {
    /// Номер новой композиции.
    pub composition: CompositionHandle,
    /// События для шкалы по одному на реплику (слой = порядок объекта). Пусто у «очистки».
    /// Конец — явный (если задан блоком) либо [`super::PgsCompositionStore::OPEN_END`].
    pub events: Vec<SubtitleEvent>,
    /// Предыдущая композиция, чей показ из-за этой оборвался раньше: её события нужно
    /// укоротить до `new_end` (= начало новой композиции). `None` — предшественник не менялся.
    pub predecessor_closed: Option<PredecessorClosed>,
}

/// Укорочение показа предыдущей композиции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PredecessorClosed {
    pub composition: CompositionHandle,
    pub new_end: SubtitleTime,
}

/// Что изменила вставка композиции.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreChange {
    /// Такая композиция уже хранится (повторное чтение после seek), показ не изменился.
    Duplicate,
    /// Новая композиция в хвосте: шкала меняется только дописыванием и укорочением.
    AppendedAtTail(TailAppend),
    /// Изменение нельзя выразить дописыванием/укорочением (вставка между хранимыми,
    /// замена содержимого в тот же момент, изменение показа уже известной композиции):
    /// шкалу надо пересобрать целиком из [`super::PgsCompositionStore::events_by_composition`].
    RequiresRebuild,
}

/// Итог [`super::PgsCompositionStore::note_discontinuity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiscontinuityOutcome {
    /// Сколько открытых композиций выброшено (включая «очистки»).
    pub dropped_open_compositions: usize,
    /// Из них несущих картинки: именно их события могли попасть на шкалу потребителя.
    pub dropped_with_pictures: usize,
}

impl DiscontinuityOutcome {
    /// Шкалу надо пересобрать: на ней остались события выброшенных композиций.
    /// «Очистка» событий не даёт, её выброс шкалу не затрагивает.
    pub const fn requires_rebuild(self) -> bool {
        self.dropped_with_pictures > 0
    }
}
