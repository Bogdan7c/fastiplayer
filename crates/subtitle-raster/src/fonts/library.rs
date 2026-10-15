//! Библиотека шрифтов субтитров: владелец всех лиц (встроенных, системных, вложенных).
//!
//! Порядок источников при запросе по имени: вложенные -> системные -> встроенные
//! (решение владельца). Ввод-вывод (сканирование системных шрифтов) — отдельный явный шаг
//! [`SubtitleFontLibrary::load_system_fonts`]; остальная логика работает на уже загруженном.

use std::sync::Arc;
use std::time::Duration;

use super::attached::{
    AttachedFontError, AttachedFontLimits, AttachedFontRegistration, AttachedFontStatus,
    AttachedRegistry, parse_attached_font,
};
use super::counters::LibraryCounters;
use super::embedded::{EMBEDDED_FONTS, EmbeddedFont, EmbeddedFontPurpose};
use super::face::{
    FaceBytes, FaceDataAccess, FaceId, FaceOrigin, FaceSource, LoadedFace, SystemFileKey,
};
use super::family_index::FamilyIndex;
use super::lookup_cache::{FontLookupCacheLimits, LookupCache};
use super::metrics::{FaceMetrics, compute_face_metrics};
use super::shared_bytes::SharedBytesCache;
use super::style::{FontStyle, FontWeightClass};
use skrifa::FontRef;

/// Ошибка создания библиотеки: встроенные данные повреждены (ловится тестом сборки).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FontLibraryError {
    /// Встроенный шрифт не разбирается.
    #[error("embedded font `{family}` cannot be parsed")]
    EmbeddedFontUnparseable {
        /// Семейство повреждённого шрифта.
        family: &'static str,
    },
    /// В списке встроенных нет основного Noto Sans нужного начертания.
    #[error("embedded primary font is missing: {weight:?}")]
    PrimaryFontMissing {
        /// Отсутствующее начертание.
        weight: FontWeightClass,
    },
}

/// Выполнялось ли сканирование.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemFontScanStatus {
    /// Сканирование выполнено в этом вызове.
    Scanned,
    /// Системные шрифты уже загружены раньше; повторного сканирования не было.
    AlreadyScanned,
}

/// Отчёт о сканировании системных шрифтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemFontScanReport {
    /// Выполнялось ли сканирование.
    pub status: SystemFontScanStatus,
    /// Сколько системных лиц зарегистрировано (всего в библиотеке).
    pub faces_registered: usize,
    /// Сколько различных имён семейств.
    pub family_names: usize,
    /// Системных лиц, пропущенных при сканировании как непригодные (не читаются или без метрик).
    pub faces_skipped_unusable: usize,
    /// Память индекса покрытия кодпоинтов всех системных лиц.
    pub coverage_index_bytes: usize,
    /// Сколько длилось сканирование (`Duration::ZERO` при повторном вызове).
    pub elapsed: Duration,
}

/// Описание лица (для диагностики и тестов).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceDescription<'a> {
    /// Источник.
    pub origin: FaceOrigin,
    /// Основное имя семейства.
    pub family: &'a str,
    /// Насыщенность.
    pub weight: FontWeightClass,
    /// Точный вес OS/2 (100..900).
    pub os2_weight: u16,
    /// Наклон.
    pub style: FontStyle,
    /// Индекс в коллекции `.ttc` (0 для обычных файлов).
    pub collection_index: u32,
}

/// Счётчики библиотеки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontLibraryStatistics {
    /// Встроенных лиц.
    pub embedded_faces: usize,
    /// Системных лиц.
    pub system_faces: usize,
    /// Вложенных лиц.
    pub attached_faces: usize,
    /// Записей в кэше поиска символов.
    pub cached_lookups: usize,
    /// Попаданий в кэш.
    pub lookup_cache_hits: u64,
    /// Промахов кэша (полный поиск).
    pub lookup_cache_misses: u64,
    /// Поисков, не нашедших глиф ни в одном шрифте (включая повторы из кэша).
    pub missing_glyph_lookups: u64,
    /// Лиц, признанных непригодными для рисования (при сканировании или при неудачной загрузке);
    /// они больше не выбираются ни по имени, ни как запасные для глифа.
    pub unusable_faces: u64,
    /// Сколько раз лицо не удалось разобрать как шрифт.
    pub unreadable_faces: u64,
    /// Сколько раз открывался файл системного шрифта (скан, метрики, материализация байт).
    pub system_font_file_opens: u64,
    /// Файлов системных шрифтов, чьи байты сейчас удерживаются кэшем подготовленных лиц.
    pub resident_system_files: usize,
    /// Суммарный размер этих байт.
    pub resident_system_file_bytes: usize,
    /// Память индекса покрытия системных лиц.
    pub coverage_index_bytes: usize,
    /// Суммарный размер зарегистрированных вложенных шрифтов.
    pub attached_font_bytes: usize,
}

/// Библиотека шрифтов. Все поля приватны: снаружи только методы-намерения.
pub struct SubtitleFontLibrary {
    pub(super) faces: Vec<LoadedFace>,
    /// База системных шрифтов; вложенные и встроенные в неё не попадают.
    pub(super) system_database: fontdb::Database,
    pub(super) system_scanned: bool,
    pub(super) embedded_face_count: usize,
    pub(super) system_face_ids: Vec<FaceId>,
    pub(super) attached: AttachedRegistry,
    pub(super) attached_names: FamilyIndex,
    pub(super) system_names: FamilyIndex,
    pub(super) embedded_names: FamilyIndex,
    pub(super) default_regular: FaceId,
    pub(super) default_bold: FaceId,
    pub(super) lookup_cache: LookupCache,
    pub(super) lookup_cache_hits: u64,
    pub(super) lookup_cache_misses: u64,
    pub(super) missing_glyph_lookups: u64,
    pub(super) unusable_faces: u64,
    pub(super) coverage_index_bytes: usize,
    pub(super) counters: LibraryCounters,
    /// Байты файлов системных шрифтов: только слабые ссылки (сильные держит `FaceCache`).
    pub(super) shared_files: SharedBytesCache<SystemFileKey>,
}

impl SubtitleFontLibrary {
    /// Создаёт библиотеку только со встроенными шрифтами. Ошибка — если встроенные данные
    /// повреждены (защищено тестом; паники в production нет).
    pub fn with_embedded_fonts(
        cache_limits: FontLookupCacheLimits,
    ) -> Result<Self, FontLibraryError> {
        let mut faces = Vec::with_capacity(EMBEDDED_FONTS.len());
        let mut embedded_names = FamilyIndex::default();
        let mut default_regular = None;
        let mut default_bold = None;
        for font in &EMBEDDED_FONTS {
            if FontRef::from_index(font.data, 0).is_err() {
                return Err(FontLibraryError::EmbeddedFontUnparseable {
                    family: font.family,
                });
            }
            let face_id = FaceId(u32::try_from(faces.len()).unwrap_or(u32::MAX));
            faces.push(embedded_face(font));
            embedded_names.add(font.family, face_id);
            if font.purpose == EmbeddedFontPurpose::PrimaryLatinGreekCyrillic {
                match font.weight {
                    FontWeightClass::Regular => default_regular.get_or_insert(face_id),
                    FontWeightClass::Bold => default_bold.get_or_insert(face_id),
                };
            }
        }
        let default_regular = default_regular.ok_or(FontLibraryError::PrimaryFontMissing {
            weight: FontWeightClass::Regular,
        })?;
        let default_bold = default_bold.ok_or(FontLibraryError::PrimaryFontMissing {
            weight: FontWeightClass::Bold,
        })?;
        Ok(Self {
            embedded_face_count: faces.len(),
            faces,
            system_database: fontdb::Database::new(),
            system_scanned: false,
            system_face_ids: Vec::new(),
            attached: AttachedRegistry::default(),
            attached_names: FamilyIndex::default(),
            system_names: FamilyIndex::default(),
            embedded_names,
            default_regular,
            default_bold,
            lookup_cache: LookupCache::new(cache_limits),
            lookup_cache_hits: 0,
            lookup_cache_misses: 0,
            missing_glyph_lookups: 0,
            unusable_faces: 0,
            coverage_index_bytes: 0,
            counters: LibraryCounters::default(),
            shared_files: SharedBytesCache::default(),
        })
    }

    /// Регистрирует вложенный шрифт (все лица коллекции). При ошибке библиотека не меняется.
    ///
    /// Точно такие же байты, уже зарегистрированные раньше, дают
    /// [`AttachedFontStatus::AlreadyRegistered`] и не меняют библиотеку (в том числе кэш
    /// поиска глифов): для вызывающего это сигнал, что сбрасывать свои кэши не нужно.
    pub fn register_attached_font(
        &mut self,
        font_bytes: Arc<[u8]>,
        limits: &AttachedFontLimits,
    ) -> Result<AttachedFontRegistration, AttachedFontError> {
        if let Some(known) = self.attached.faces_of_duplicate(&font_bytes) {
            return Ok(AttachedFontRegistration {
                status: AttachedFontStatus::AlreadyRegistered,
                faces: known.to_vec(),
            });
        }
        let parsed_faces = parse_attached_font(&font_bytes, limits)?;
        self.attached
            .check_total_limits(font_bytes.len(), parsed_faces.len(), limits)?;
        let mut registered = Vec::with_capacity(parsed_faces.len());
        for parsed in parsed_faces {
            let face_id = self.next_face_id();
            self.faces.push(LoadedFace {
                origin: FaceOrigin::Attached,
                source: FaceSource::Attached(Arc::clone(&font_bytes)),
                collection_index: parsed.collection_index,
                family: parsed.family_names.first().cloned().unwrap_or_default(),
                weight: parsed.weight,
                os2_weight: parsed.os2_weight,
                style: parsed.style,
                unusable: false,
            });
            for name in &parsed.family_names {
                self.attached_names.add(name, face_id);
            }
            registered.push(face_id);
        }
        self.attached.record(font_bytes, registered.clone());
        // Вложенные шрифты входят в цепочку fallback по глифу: прежние результаты поиска
        // (особенно «нигде нет») могли устареть.
        self.lookup_cache.clear();
        Ok(AttachedFontRegistration {
            status: AttachedFontStatus::Registered,
            faces: registered,
        })
    }

    /// Описание лица или `None`, если такого `FaceId` нет в этой библиотеке.
    pub fn describe_face(&self, face: FaceId) -> Option<FaceDescription<'_>> {
        let loaded = self.face(face)?;
        Some(FaceDescription {
            origin: loaded.origin,
            family: &loaded.family,
            weight: loaded.weight,
            os2_weight: loaded.os2_weight,
            style: loaded.style,
            collection_index: loaded.collection_index,
        })
    }

    /// Вертикальные метрики лица в font units; `None` — лица нет или шрифт не читается.
    pub fn face_metrics(&self, face: FaceId) -> Option<FaceMetrics> {
        self.face(face)?
            .with_font_ref(self.data_access(), compute_face_metrics)
            .flatten()
    }

    /// Байты файла лица для долгоживущего использования (растеризация).
    ///
    /// Для системных шрифтов файл читается с диска, если его байты сейчас никто не держит;
    /// библиотека хранит лишь слабую ссылку, поэтому память освобождается, когда все
    /// держатели (кэш подготовленных лиц) отпустили `FaceBytes`. Лица одного `.ttc` делят байты.
    pub fn face_bytes(&self, face: FaceId) -> Option<FaceBytes> {
        self.face(face)?.materialize_bytes(self.data_access())
    }

    /// Счётчики и размеры.
    pub fn statistics(&self) -> FontLibraryStatistics {
        let resident = self.shared_files.resident();
        FontLibraryStatistics {
            embedded_faces: self.embedded_face_count,
            system_faces: self.system_face_ids.len(),
            attached_faces: self.attached.all_faces().count(),
            cached_lookups: self.lookup_cache.len(),
            lookup_cache_hits: self.lookup_cache_hits,
            lookup_cache_misses: self.lookup_cache_misses,
            missing_glyph_lookups: self.missing_glyph_lookups,
            unusable_faces: self.unusable_faces,
            unreadable_faces: self.counters.unreadable_faces(),
            system_font_file_opens: self.counters.system_file_opens(),
            resident_system_files: resident.files,
            resident_system_file_bytes: resident.bytes,
            coverage_index_bytes: self.coverage_index_bytes,
            attached_font_bytes: self.attached.total_bytes(),
        }
    }

    /// Число встроенных лиц: первые `FaceId` библиотеки (для тестов, строящих `FaceId` вручную).
    #[cfg(test)]
    pub(super) fn embedded_face_count_for_test(&self) -> u32 {
        u32::try_from(self.embedded_face_count).unwrap_or(u32::MAX)
    }

    /// Лицо годится для выбора (не помечено непригодным).
    pub(super) fn is_face_usable(&self, face: FaceId) -> bool {
        self.face(face).is_some_and(|loaded| !loaded.unusable)
    }

    /// Помечает лицо непригодным: оно не загрузилось при рисовании. Больше не выбирается
    /// (ни по имени, ни как запасное), закэшированные результаты поиска на него забываются.
    ///
    /// `true` — лицо помечено сейчас; `false` — лица нет, оно уже помечено или встроенное
    /// (встроенные шрифты проверены при создании библиотеки и не отключаются).
    pub(crate) fn mark_face_unusable(&mut self, face: FaceId) -> bool {
        let Some(loaded) = usize::try_from(face.0)
            .ok()
            .and_then(|index| self.faces.get_mut(index))
        else {
            return false;
        };
        if loaded.unusable || loaded.origin == FaceOrigin::Embedded {
            return false;
        }
        loaded.unusable = true;
        self.unusable_faces += 1;
        self.lookup_cache.forget_face(face);
        tracing::warn!(
            face = face.index(),
            "font face failed to load and is excluded from selection"
        );
        true
    }

    /// Доступ лиц к общим хранилищам библиотеки.
    pub(super) fn data_access(&self) -> FaceDataAccess<'_> {
        FaceDataAccess {
            database: &self.system_database,
            counters: &self.counters,
            shared_files: &self.shared_files,
        }
    }

    pub(super) fn face(&self, face: FaceId) -> Option<&LoadedFace> {
        self.faces.get(usize::try_from(face.0).ok()?)
    }

    pub(super) fn next_face_id(&self) -> FaceId {
        FaceId(u32::try_from(self.faces.len()).unwrap_or(u32::MAX))
    }
}

/// Лицо из статического описания.
fn embedded_face(font: &EmbeddedFont) -> LoadedFace {
    LoadedFace {
        origin: FaceOrigin::Embedded,
        source: FaceSource::Static(font.data),
        collection_index: 0,
        family: font.family.to_owned(),
        weight: font.weight,
        os2_weight: font.weight.nominal_os2_weight(),
        style: font.style,
        unusable: false,
    }
}
