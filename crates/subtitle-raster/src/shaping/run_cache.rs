// Rust-часть шейпера, © Fastiplayer contributors.

//! Кэш результатов шейпинга участков текста: один на растеризатор, с бюджетом в байтах.
//!
//! Результат шейпинга участка - чистая функция лица, текста участка и [`PlanSelector`]
//! (направление, письменность, кернинг, лигатуры): буфер `harfrust` не видит соседних участков,
//! а метрики шрифта зависят только от лица. Реплики же в основном повторяются: анимированная
//! надпись или бегущая строка каждый кадр заново шейпит тот же текст, а сам шейпинг (применение
//! таблиц GSUB/GPOS) - заметная доля кадра.
//!
//! # Владение и бюджет
//!
//! Кэш принадлежит [`crate::face_cache::FaceCache`] (владельцу всех данных лиц растеризатора) и
//! ОДИН на все лица: ключ содержит [`FaceId`], а бюджет (`shaping_cache_mb` в конфиге) общий.
//! Кэш на каждое лицо дал бы бюджет, умноженный на число лиц, и не позволял бы вытеснять давние
//! участки одного лица ради свежих другого. Вытеснение - LRU по байтам ([`ByteLruCache`]):
//! переполнение отбрасывает самые давние участки, а не весь кэш разом (раньше был жёсткий сброс
//! на 2048 участках, из-за которого сцена чуть больше лимита шейпилась заново каждый кадр).
//! Лица кэш лиц вытесняет независимо: результат участка не зависит от жизни `FaceEntry`, так что
//! записи вытесненного лица просто стареют и уходят по LRU. Смена набора шрифтов
//! ([`ShapedRunCache::clear`]) сбрасывает всё: тот же `FaceId` мог получить другие байты.
//!
//! # Замок и аллокации
//!
//! Замок держится только на поиске и на вставке, но НЕ на шейпинге: шейпинг идёт без замка
//! (его делает вызывающий через замыкание). Поиск идёт по заимствованному ключу
//! ([`RunKeyView`]): текст участка собирается в переиспользуемый буфер внутри замка, поэтому
//! попадание не выделяет память под `String`; владеющий ключ строится только на промахе.

use std::borrow::Borrow;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::plan_cache::PlanSelector;
use crate::cache::{ByteLruCache, CacheStatistics};
use crate::fonts::FaceId;

/// Служебные расходы одной записи сверх глифов и текста (ячейка LRU, `Arc`, `Vec`, ключ).
const ENTRY_OVERHEAD_BYTES: usize = 160;

/// Один глиф результата шейпинга (то, что `harfrust` вернул для участка).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ShapedGlyph {
    /// Номер символа участка, которому принадлежит глиф (кластер).
    pub(super) cluster: u32,
    /// Номер глифа в лице.
    pub(super) glyph_id: u32,
    /// Продвижение по X при размере 256 px, 1/64 пикселя.
    pub(super) x_advance: i32,
    /// Продвижение по Y.
    pub(super) y_advance: i32,
    /// Смещение по X.
    pub(super) x_offset: i32,
    /// Смещение по Y.
    pub(super) y_offset: i32,
}

/// Общий вид ключа участка: владеющего ([`RunKey`]) и заимствованного ([`RunKeyRef`]).
///
/// Хэш и равенство определены через этот вид, поэтому оба ключа хэшируются одинаково, и
/// `HashMap` ищет владеющий ключ по заимствованному без построения `String`.
trait RunKeyView {
    fn face(&self) -> FaceId;
    fn selector(&self) -> PlanSelector;
    fn text(&self) -> &str;
}

impl Hash for dyn RunKeyView + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.face().hash(state);
        self.selector().hash(state);
        self.text().hash(state);
    }
}

impl PartialEq for dyn RunKeyView + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.face() == other.face()
            && self.selector() == other.selector()
            && self.text() == other.text()
    }
}

impl Eq for dyn RunKeyView + '_ {}

/// Владеющий ключ участка: лицо, свойства шейпинга и текст.
///
/// Текст в `Arc<str>`: кэш хранит ключ дважды (индекс и ячейка), и клон не копирует буфер, так
/// что память текста считается один раз.
#[derive(Debug, Clone)]
struct RunKey {
    face: FaceId,
    selector: PlanSelector,
    text: Arc<str>,
}

impl RunKeyView for RunKey {
    fn face(&self) -> FaceId {
        self.face
    }
    fn selector(&self) -> PlanSelector {
        self.selector
    }
    fn text(&self) -> &str {
        &self.text
    }
}

impl Hash for RunKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Тем же путём, что и заимствованный ключ: иначе поиск по `dyn RunKeyView` промахнулся бы.
        <dyn RunKeyView as Hash>::hash(self, state);
    }
}

impl PartialEq for RunKey {
    fn eq(&self, other: &Self) -> bool {
        <dyn RunKeyView as PartialEq>::eq(self, other)
    }
}

impl Eq for RunKey {}

impl<'view> Borrow<dyn RunKeyView + 'view> for RunKey {
    fn borrow(&self) -> &(dyn RunKeyView + 'view) {
        self
    }
}

/// Заимствованный ключ для поиска: текст лежит в буфере кэша, ничего не копируется.
struct RunKeyRef<'text> {
    face: FaceId,
    selector: PlanSelector,
    text: &'text str,
}

impl RunKeyView for RunKeyRef<'_> {
    fn face(&self) -> FaceId {
        self.face
    }
    fn selector(&self) -> PlanSelector {
        self.selector
    }
    fn text(&self) -> &str {
        self.text
    }
}

/// Изменяемое состояние кэша под одним замком.
struct Inner {
    runs: ByteLruCache<RunKey, Vec<ShapedGlyph>>,
    /// Буфер, в который собирается текст участка для поиска. Живёт между вызовами, поэтому
    /// попадание не выделяет память; растёт до самого длинного участка (он ограничен
    /// `max_chars_per_cue`).
    text_scratch: String,
}

/// Результаты шейпинга участков всех лиц растеризатора.
pub(crate) struct ShapedRunCache {
    inner: Mutex<Inner>,
}

impl ShapedRunCache {
    /// Кэш с бюджетом `budget_bytes` (0 - ничего не кэшируется, каждый участок шейпится заново).
    pub(crate) fn new(budget_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                runs: ByteLruCache::new(budget_bytes),
                text_scratch: String::new(),
            }),
        }
    }

    /// Замок кэша. Отравленный замок не опасен: внутри только готовые результаты.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Глифы участка `symbols` лица `face`; при отсутствии вызывается `shape(text)` БЕЗ замка и
    /// результат запоминается. Ошибка шейпинга не запоминается и возвращается как есть.
    pub(super) fn glyphs_for<E>(
        &self,
        face: FaceId,
        selector: PlanSelector,
        symbols: impl IntoIterator<Item = char>,
        shape: impl FnOnce(&str) -> Result<Vec<ShapedGlyph>, E>,
    ) -> Result<Arc<Vec<ShapedGlyph>>, E> {
        let text: Arc<str> = {
            let mut guard = self.lock();
            let Inner { runs, text_scratch } = &mut *guard;
            text_scratch.clear();
            text_scratch.extend(symbols);
            let probe = RunKeyRef {
                face,
                selector,
                text: text_scratch,
            };
            if let Some(found) = runs.get(&probe as &dyn RunKeyView) {
                return Ok(found);
            }
            Arc::from(text_scratch.as_str())
        };
        // Шейпинг без замка: он дорогой, а соседний поток мог бы ждать на поиске.
        let shaped = Arc::new(shape(&text)?);
        let bytes = ENTRY_OVERHEAD_BYTES
            .saturating_add(text.len())
            .saturating_add(shaped.len().saturating_mul(size_of::<ShapedGlyph>()));
        let key = RunKey {
            face,
            selector,
            text,
        };
        self.lock().runs.insert(key, Arc::clone(&shaped), bytes);
        Ok(shaped)
    }

    /// Забывает все участки (набор шрифтов изменился).
    pub(crate) fn clear(&self) {
        self.lock().runs.clear();
    }

    /// Счётчики кэша.
    pub(crate) fn statistics(&self) -> CacheStatistics {
        self.lock().runs.statistics()
    }
}
