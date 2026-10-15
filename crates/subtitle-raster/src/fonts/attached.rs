//! Вложенные шрифты (например, из вложений MKV): лимиты, ошибки и разбор.
//!
//! Извлечение шрифтов из контейнера — не здесь (сессия 06); библиотека принимает готовые байты.

use std::sync::Arc;

use skrifa::FontRef;

use super::face::FaceId;
use super::metrics::compute_face_metrics;
use super::style::{FontStyle, FontWeightClass};

/// Лимиты на вложенные шрифты. Значения задаёт вызывающий (из конфига), `Default` нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachedFontLimits {
    /// Максимальный размер одного вложенного файла шрифта в байтах.
    pub max_font_bytes: usize,
    /// Максимальный суммарный размер всех вложенных файлов в байтах (в памяти они живут всё
    /// время жизни библиотеки). Дубликаты уже зарегистрированных файлов не учитываются.
    pub max_total_bytes: usize,
    /// Максимальное суммарное число вложенных лиц (лица коллекции `.ttc` считаются по одному).
    pub max_faces: usize,
}

/// Почему вложенный шрифт не принят. Библиотека при любой ошибке остаётся без изменений.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AttachedFontError {
    /// Файл больше лимита.
    #[error("attached font is too large: {actual_bytes} bytes, limit is {max_bytes}")]
    TooLarge {
        /// Фактический размер.
        actual_bytes: usize,
        /// Лимит.
        max_bytes: usize,
    },
    /// Суммарный размер вложенных шрифтов превысил бы лимит.
    #[error(
        "attached fonts would exceed the total size limit: {current_bytes} registered + {adding_bytes} new > {max_bytes}"
    )]
    TotalBytesExceeded {
        /// Уже зарегистрировано байт.
        current_bytes: usize,
        /// Размер добавляемого файла.
        adding_bytes: usize,
        /// Лимит.
        max_bytes: usize,
    },
    /// Суммарное число вложенных лиц превысило бы лимит.
    #[error(
        "attached fonts would exceed the face limit: {current_faces} registered + {adding_faces} new > {max_faces}"
    )]
    TooManyFaces {
        /// Уже зарегистрировано лиц.
        current_faces: usize,
        /// Лиц в добавляемом файле.
        adding_faces: usize,
        /// Лимит.
        max_faces: usize,
    },
    /// Данные не разбираются как TrueType/OpenType/коллекция (или у лиц нет пригодных метрик).
    #[error("attached font data is not a usable font file")]
    Unparseable,
}

/// Что произошло с зарегистрированным файлом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachedFontStatus {
    /// Шрифт добавлен; библиотека изменилась (выбор шрифтов для реплик мог поменяться).
    Registered,
    /// Точно такие же байты уже были зарегистрированы: библиотека НЕ изменилась, кэши, зависящие
    /// от набора шрифтов, сбрасывать не нужно. В результате лежат лица прежней регистрации.
    AlreadyRegistered,
}

/// Результат успешной регистрации.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedFontRegistration {
    /// Новая ли это регистрация или повтор уже известного файла.
    pub status: AttachedFontStatus,
    /// Лица файла в библиотеке (для `.ttc` — все лица коллекции), не пусто.
    pub faces: Vec<FaceId>,
}

/// Лицо вложенного шрифта, разобранное до изменения библиотеки.
#[derive(Debug)]
pub(super) struct ParsedAttachedFace {
    pub(super) collection_index: u32,
    pub(super) family_names: Vec<String>,
    pub(super) weight: FontWeightClass,
    pub(super) style: FontStyle,
}

/// Лицо разбирается и у него есть метрики, по которым можно посчитать масштаб: иначе его
/// выбор заведомо роняет рисование реплики.
fn is_renderable_face(font_bytes: &[u8], index: u32) -> bool {
    FontRef::from_index(font_bytes, index)
        .ok()
        .and_then(|font| compute_face_metrics(&font))
        .is_some_and(|metrics| metrics.is_renderable())
}

/// Проверяет лимит одного файла и разбирает все лица файла. Ничего не меняет — только
/// возвращает результат. Суммарные лимиты проверяет [`AttachedRegistry`].
pub(super) fn parse_attached_font(
    font_bytes: &Arc<[u8]>,
    limits: &AttachedFontLimits,
) -> Result<Vec<ParsedAttachedFace>, AttachedFontError> {
    if font_bytes.len() > limits.max_font_bytes {
        return Err(AttachedFontError::TooLarge {
            actual_bytes: font_bytes.len(),
            max_bytes: limits.max_font_bytes,
        });
    }
    // Имена и начертания читает fontdb во временной базе (она же понимает .ttc);
    // пригодность лица для отрисовки подтверждаем собственным разбором skrifa.
    let mut scratch_database = fontdb::Database::new();
    let shared: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(Arc::clone(font_bytes));
    let face_ids = scratch_database.load_font_source(fontdb::Source::Binary(shared));
    let parsed: Vec<ParsedAttachedFace> = face_ids
        .iter()
        .filter_map(|id| scratch_database.face(*id))
        .filter(|info| is_renderable_face(font_bytes, info.index))
        .map(|info| ParsedAttachedFace {
            collection_index: info.index,
            family_names: info.families.iter().map(|(name, _)| name.clone()).collect(),
            weight: FontWeightClass::from_os2_weight(info.weight.0),
            style: match info.style {
                fontdb::Style::Normal => FontStyle::Normal,
                fontdb::Style::Italic | fontdb::Style::Oblique => FontStyle::Italic,
            },
        })
        .collect();
    if parsed.is_empty() {
        return Err(AttachedFontError::Unparseable);
    }
    Ok(parsed)
}

/// Уже зарегистрированный файл и его лица.
struct RegisteredFile {
    bytes: Arc<[u8]>,
    faces: Vec<FaceId>,
}

/// Реестр вложенных шрифтов: владеет учётом файлов, суммарных лимитов и дубликатов.
#[derive(Default)]
pub(super) struct AttachedRegistry {
    files: Vec<RegisteredFile>,
    total_bytes: usize,
    total_faces: usize,
}

impl AttachedRegistry {
    /// Лица ранее зарегистрированного файла с точно такими же байтами.
    pub(super) fn faces_of_duplicate(&self, bytes: &Arc<[u8]>) -> Option<&[FaceId]> {
        self.files
            .iter()
            .find(|known| Arc::ptr_eq(&known.bytes, bytes) || known.bytes[..] == bytes[..])
            .map(|known| known.faces.as_slice())
    }

    /// Проверяет суммарные лимиты для файла из `adding_faces` лиц. Ничего не меняет.
    pub(super) fn check_total_limits(
        &self,
        adding_bytes: usize,
        adding_faces: usize,
        limits: &AttachedFontLimits,
    ) -> Result<(), AttachedFontError> {
        if self.total_bytes.saturating_add(adding_bytes) > limits.max_total_bytes {
            return Err(AttachedFontError::TotalBytesExceeded {
                current_bytes: self.total_bytes,
                adding_bytes,
                max_bytes: limits.max_total_bytes,
            });
        }
        if self.total_faces.saturating_add(adding_faces) > limits.max_faces {
            return Err(AttachedFontError::TooManyFaces {
                current_faces: self.total_faces,
                adding_faces,
                max_faces: limits.max_faces,
            });
        }
        Ok(())
    }

    /// Запоминает принятый файл (после успешной проверки лимитов).
    pub(super) fn record(&mut self, bytes: Arc<[u8]>, faces: Vec<FaceId>) {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        self.total_faces = self.total_faces.saturating_add(faces.len());
        self.files.push(RegisteredFile { bytes, faces });
    }

    /// Суммарный размер зарегистрированных файлов.
    pub(super) fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Все вложенные лица в порядке регистрации.
    pub(super) fn all_faces(&self) -> impl Iterator<Item = FaceId> + '_ {
        self.files
            .iter()
            .flat_map(|file| file.faces.iter().copied())
    }
}
