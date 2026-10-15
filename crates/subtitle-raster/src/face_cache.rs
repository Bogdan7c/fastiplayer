//! Лица, подготовленные для отрисовки реплик: байты, масштаб и данные шейпера.
//!
//! Конвейер текста обращается к одному и тому же лицу из нескольких стадий (шейпинг, контуры,
//! метрики). [`FaceCache`] собирает всё нужное по [`FaceId`] один раз, чтобы стадии не ходили
//! в [`SubtitleFontLibrary`] повторно и не знали, откуда взялись байты (встроенный шрифт,
//! вложенный или системный).
//!
//! Подготовка лица дорогая (`ShaperData::new` разбирает таблицы GSUB/GPOS/GDEF, хинтованные
//! продвижения считаются по всем глифам), поэтому кэш живёт дольше одной реплики и ограничен
//! числом лиц. Вытеснение ([`FaceCache::trim_to`]) вызывает владелец МЕЖДУ репликами: внутри
//! реплики стадии держат ссылки на лица, и убрать лицо посреди отрисовки нельзя.

use std::collections::HashMap;

use harfrust::ShaperData;
use skrifa::FontRef;

use crate::fonts::{FaceBytes, FaceId, SubtitleFontLibrary};
use crate::glyph_render::{FaceScale, HintedAdvances};

/// Почему лицо не удалось подготовить.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FaceLoadError {
    /// Лица с таким идентификатором нет в библиотеке или его байты недоступны.
    #[error("font face {face:?} is not available")]
    Unavailable {
        /// Идентификатор лица.
        face: FaceId,
    },
    /// Байты лица не разбираются как шрифт.
    #[error("font face {face:?} cannot be parsed")]
    Unparsable {
        /// Идентификатор лица.
        face: FaceId,
    },
    /// Метрики лица вырождены (нулевая высота ячейки): масштаб не определён.
    #[error("font face {face:?} has degenerate vertical metrics")]
    DegenerateMetrics {
        /// Идентификатор лица.
        face: FaceId,
    },
}

/// Всё, что нужно стадиям конвейера о лице.
pub(crate) struct FaceEntry {
    face: FaceId,
    bytes: FaceBytes,
    collection_index: u32,
    scale: FaceScale,
    shaper_data: ShaperData,
    advances: HintedAdvances,
}

impl FaceEntry {
    /// Разобранный шрифт (дёшево: разбирается только заголовок таблиц).
    pub(crate) fn font_ref(&self) -> Result<FontRef<'_>, FaceLoadError> {
        FontRef::from_index(self.bytes.as_slice(), self.collection_index)
            .map_err(|_| FaceLoadError::Unparsable { face: self.face })
    }

    /// Масштаб лица при размере 256 px.
    pub(crate) fn scale(&self) -> &FaceScale {
        &self.scale
    }

    /// Хинтованные продвижения глифов (метрики, которыми libass кормит шейпер).
    pub(crate) fn advances(&self) -> &HintedAdvances {
        &self.advances
    }

    /// Кэши таблиц `harfrust` для этого лица.
    pub(crate) fn shaper_data(&self) -> &ShaperData {
        &self.shaper_data
    }
}

/// Счётчики кэша лиц.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FaceCacheStatistics {
    /// Лицо уже было подготовлено.
    pub hits: u64,
    /// Лицо пришлось готовить.
    pub misses: u64,
    /// Лицо вытеснено из-за лимита.
    pub evictions: u64,
    /// Сколько лиц подготовлено сейчас.
    pub cached_faces: usize,
}

/// Лицо и момент последнего обращения (для вытеснения давно не использованных).
struct CachedFace {
    entry: FaceEntry,
    last_used: u64,
}

/// Подготовленные лица.
#[derive(Default)]
pub(crate) struct FaceCache {
    entries: HashMap<FaceId, CachedFace>,
    /// Счётчик обращений: монотонно растёт, заменяет часы.
    use_clock: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl FaceCache {
    /// Пустой кэш.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Готовит лицо, если оно ещё не подготовлено.
    pub(crate) fn ensure_loaded(
        &mut self,
        fonts: &SubtitleFontLibrary,
        face: FaceId,
    ) -> Result<(), FaceLoadError> {
        self.use_clock = self.use_clock.wrapping_add(1);
        if let Some(cached) = self.entries.get_mut(&face) {
            cached.last_used = self.use_clock;
            self.hits += 1;
            return Ok(());
        }
        self.misses += 1;
        let bytes = fonts
            .face_bytes(face)
            .ok_or(FaceLoadError::Unavailable { face })?;
        let collection_index = fonts
            .describe_face(face)
            .ok_or(FaceLoadError::Unavailable { face })?
            .collection_index;
        let metrics = fonts
            .face_metrics(face)
            .ok_or(FaceLoadError::Unavailable { face })?;
        let scale =
            FaceScale::from_metrics(&metrics).ok_or(FaceLoadError::DegenerateMetrics { face })?;
        let font = FontRef::from_index(bytes.as_slice(), collection_index)
            .map_err(|_| FaceLoadError::Unparsable { face })?;
        let shaper_data = ShaperData::new(&font);
        let advances = HintedAdvances::new(&font, &scale);
        self.entries.insert(
            face,
            CachedFace {
                entry: FaceEntry {
                    face,
                    bytes,
                    collection_index,
                    scale,
                    shaper_data,
                    advances,
                },
                last_used: self.use_clock,
            },
        );
        Ok(())
    }

    /// Вытесняет давно не использованные лица, пока их не станет не больше `max_faces`.
    ///
    /// Вызывать только между репликами (см. документацию модуля).
    pub(crate) fn trim_to(&mut self, max_faces: usize) {
        while self.entries.len() > max_faces {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(face, _)| *face)
            else {
                return;
            };
            self.entries.remove(&oldest);
            self.evictions += 1;
        }
    }

    /// Забывает все лица (шрифты библиотеки изменились).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Счётчики кэша.
    pub(crate) fn statistics(&self) -> FaceCacheStatistics {
        FaceCacheStatistics {
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
            cached_faces: self.entries.len(),
        }
    }

    /// Подготовленное лицо (после [`Self::ensure_loaded`]).
    pub(crate) fn get(&self, face: FaceId) -> Result<&FaceEntry, FaceLoadError> {
        self.entries
            .get(&face)
            .map(|cached| &cached.entry)
            .ok_or(FaceLoadError::Unavailable { face })
    }
}
