//! Одно лицо (face) шрифта в библиотеке: откуда взяты байты и как их получить.

use std::path::PathBuf;
use std::sync::Arc;

use skrifa::FontRef;

use super::counters::LibraryCounters;
use super::coverage::GlyphCoverage;
use super::face_language::FaceLanguages;
use super::shared_bytes::SharedBytesCache;
use super::style::{FontStyle, FontWeightClass};

/// Стабильный идентификатор лица внутри одной [`super::SubtitleFontLibrary`].
///
/// Идентификаторы только добавляются и не переиспользуются, поэтому сохранённый `FaceId`
/// (например, в кэше глифов) остаётся валидным всё время жизни библиотеки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaceId(pub(super) u32);

impl FaceId {
    /// Порядковый номер (для диагностики).
    pub fn index(&self) -> u32 {
        self.0
    }
}

/// Откуда взят шрифт; определяет приоритет при поиске по имени.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaceOrigin {
    /// Вложенный в контейнер (MKV) шрифт: самый высокий приоритет.
    Attached,
    /// Установленный в системе.
    System,
    /// Встроенный Noto.
    Embedded,
}

/// Байты шрифтового файла (вся коллекция, не одно лицо). Дёшево клонируется.
#[derive(Debug, Clone)]
pub enum FaceBytes {
    /// Встроенные данные из бинарника.
    Static(&'static [u8]),
    /// Разделяемые данные (вложенные или считанные системные).
    Shared(Arc<[u8]>),
}

impl FaceBytes {
    /// Содержимое файла.
    pub fn as_slice(&self) -> &[u8] {
        match self {
            Self::Static(bytes) => bytes,
            Self::Shared(bytes) => bytes,
        }
    }
}

/// Идентификатор файла системного шрифта: общий для всех лиц одной коллекции `.ttc`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum SystemFileKey {
    /// Файл на диске (так `fontdb` описывает системные шрифты).
    Path(PathBuf),
    /// Источник без пути (не встречается при сканировании каталогов): не делится с другими.
    Face(fontdb::ID),
}

/// Хранилище байтов лица.
pub(super) enum FaceSource {
    /// Встроенный шрифт.
    Static(&'static [u8]),
    /// Вложенный шрифт: байты уже в памяти.
    Attached(Arc<[u8]>),
    /// Снятое вложенное лицо: байты освобождены, а запись осталась, чтобы `FaceId` не
    /// переиспользовался (см. инвариант [`FaceId`]). Не выбирается и не отдаёт данных.
    Detached,
    /// Системный шрифт: лежит на диске. Покрытие кодпоинтов посчитано при сканировании, так что
    /// поиск символа файл не открывает; файл читается целиком только когда лицо реально
    /// понадобилось для отрисовки ([`LoadedFace::materialize_bytes`]), причём байты общие на файл.
    System {
        /// Идентификатор в базе `fontdb`.
        database_id: fontdb::ID,
        /// Файл, из которого взято лицо (ключ общих байт).
        file_key: SystemFileKey,
        /// Какие кодпоинты есть в `cmap` лица.
        coverage: GlyphCoverage,
        /// CJK-языки лица (`meta`/`dlng`, иначе эвристика по имени), посчитанные при
        /// сканировании: по ним fallback выбирает региональную форму иероглифов.
        languages: FaceLanguages,
    },
}

/// Лицо шрифта в библиотеке.
pub(super) struct LoadedFace {
    /// Откуда взято лицо.
    pub(super) origin: FaceOrigin,
    /// Байты.
    pub(super) source: FaceSource,
    /// Индекс лица в коллекции (для `.ttc`; для обычных файлов 0).
    pub(super) collection_index: u32,
    /// Основное имя семейства (для диагностики).
    pub(super) family: String,
    /// Насыщенность (класс: решает синтетику).
    pub(super) weight: FontWeightClass,
    /// Точный вес OS/2 (`usWeightClass`, 100..900): нужен, чтобы среди системных лиц с
    /// одинаковым покрытием выбрать ближайшее к запрошенному весу, а не первое попавшееся.
    pub(super) os2_weight: u16,
    /// Наклон.
    pub(super) style: FontStyle,
    /// Лицо признано непригодным для рисования (не загрузилось); больше не выбирается.
    pub(super) unusable: bool,
}

/// Всё, что нужно лицу для доступа к своим байтам: общие хранилища библиотеки.
#[derive(Clone, Copy)]
pub(super) struct FaceDataAccess<'a> {
    /// База системных шрифтов.
    pub(super) database: &'a fontdb::Database,
    /// Счётчики чтения.
    pub(super) counters: &'a LibraryCounters,
    /// Общие байты файлов системных шрифтов (слабые ссылки).
    pub(super) shared_files: &'a SharedBytesCache<SystemFileKey>,
}

impl LoadedFace {
    /// Лицо снято (вложенный шрифт убран при смене media).
    pub(super) fn is_detached(&self) -> bool {
        matches!(self.source, FaceSource::Detached)
    }

    /// Превращает лицо в «надгробие»: освобождает байты и навсегда исключает из выбора.
    pub(super) fn detach(&mut self) {
        self.source = FaceSource::Detached;
        self.family = String::new();
        self.unusable = true;
    }

    /// Выполняет `inspect` над разобранным `FontRef`, не копируя данные системных шрифтов.
    ///
    /// `None` — данные недоступны (файл исчез) или не разбираются как шрифт (второе считается
    /// в `unreadable_faces`).
    pub(super) fn with_font_ref<T>(
        &self,
        access: FaceDataAccess<'_>,
        inspect: impl FnOnce(&FontRef<'_>) -> T,
    ) -> Option<T> {
        let counters = access.counters;
        match &self.source {
            FaceSource::Detached => None,
            FaceSource::Static(bytes) => {
                parse_and_inspect(bytes, self.collection_index, counters, inspect)
            }
            FaceSource::Attached(bytes) => {
                parse_and_inspect(bytes, self.collection_index, counters, inspect)
            }
            FaceSource::System { database_id, .. } => {
                counters.note_system_file_open();
                access
                    .database
                    .with_face_data(*database_id, |bytes, index| {
                        parse_and_inspect(bytes, index, counters, inspect)
                    })
                    .flatten()
            }
        }
    }

    /// Байты всего файла для долгоживущего использования (растеризация).
    ///
    /// Для системных шрифтов байты общие на файл: все лица одной коллекции получают один
    /// `Arc`, а библиотека держит лишь слабую ссылку — память освобождается, когда лицо
    /// вытеснено из кэша подготовленных лиц. `None` — файл не читается (причина в `tracing`).
    pub(super) fn materialize_bytes(&self, access: FaceDataAccess<'_>) -> Option<FaceBytes> {
        match &self.source {
            FaceSource::Detached => None,
            FaceSource::Static(bytes) => Some(FaceBytes::Static(bytes)),
            FaceSource::Attached(bytes) => Some(FaceBytes::Shared(Arc::clone(bytes))),
            FaceSource::System {
                database_id,
                file_key,
                ..
            } => access
                .shared_files
                .get_or_load(file_key.clone(), || {
                    access.counters.note_system_file_open();
                    let loaded = access
                        .database
                        .with_face_data(*database_id, |bytes, _| Arc::<[u8]>::from(bytes));
                    if loaded.is_none() {
                        tracing::debug!(face = %self.family, "system font data is unavailable");
                    }
                    loaded
                })
                .map(FaceBytes::Shared),
        }
    }
}

/// Разбирает лицо `index` и вызывает `inspect`; ошибку разбора считает и превращает в `None`.
pub(super) fn parse_and_inspect<T>(
    bytes: &[u8],
    index: u32,
    counters: &LibraryCounters,
    inspect: impl FnOnce(&FontRef<'_>) -> T,
) -> Option<T> {
    match FontRef::from_index(bytes, index) {
        Ok(font) => Some(inspect(&font)),
        Err(error) => {
            counters.note_unreadable_face();
            tracing::debug!(%error, index, "font face cannot be parsed");
            None
        }
    }
}
