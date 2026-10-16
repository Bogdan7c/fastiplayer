//! Вложенные шрифты (например, из вложений MKV): лимиты, ошибки и разбор.
//!
//! Извлечение шрифтов из контейнера — не здесь; библиотека принимает готовые байты.
//!
//! Жизненный цикл разделён на две фазы:
//! 1. ТЯЖЁЛАЯ ([`PreparedAttachedFont::prepare`], [`PreparedAttachedFonts::prepare`]): проверка
//!    лимита файла, разбор лиц через `fontdb` и `skrifa`. Не трогает библиотеку, результат
//!    `Send` — выполняется вне UI-потока.
//! 2. ДЕШЁВАЯ (`install_*` у библиотеки и растеризатора): дедупликация, суммарные лимиты и
//!    добавление уже разобранных лиц. Ничего не парсит.
//!
//! Снять все вложенные шрифты (смена видео) можно через `clear_attached_fonts`.

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

/// Исход снятия всех вложенных шрифтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachedFontsClearOutcome {
    /// Вложенные шрифты были и сняты; зависящие от набора шрифтов кэши надо сбросить.
    Cleared {
        /// Сколько лиц снято.
        faces: usize,
        /// Сколько байт файлов освобождено.
        bytes: usize,
    },
    /// Вложенных шрифтов не было: ничего не изменилось, кэши сбрасывать не нужно.
    NothingAttached,
}

/// Файл, отвергнутый на одной из фаз пакетной подготовки/установки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectedAttachedFont {
    /// Номер файла во входном списке [`PreparedAttachedFonts::prepare`].
    pub input_index: usize,
    /// Причина отказа.
    pub error: AttachedFontError,
}

/// Итог пакетной установки подготовленных шрифтов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedFontsInstallReport {
    /// Сколько файлов добавлено в библиотеку.
    pub registered: usize,
    /// Сколько файлов уже были зарегистрированы (библиотека от них не менялась).
    pub already_registered: usize,
    /// Отказы подготовки и установки, в порядке входных файлов.
    pub rejected: Vec<RejectedAttachedFont>,
}

/// Вложенный файл шрифта, разобранный вне библиотеки (тяжёлая фаза готова).
///
/// Содержит только байты и описания лиц, поэтому `Send + Sync`: готовится в фоне и
/// передаётся в UI-поток для дешёвой установки.
#[derive(Debug)]
pub struct PreparedAttachedFont {
    pub(super) bytes: Arc<[u8]>,
    pub(super) faces: Vec<ParsedAttachedFace>,
}

impl PreparedAttachedFont {
    /// Тяжёлая фаза: проверяет лимит одного файла и разбирает все лица. Суммарные лимиты и
    /// дубликаты проверяются при установке — только библиотека знает, что уже принято.
    pub fn prepare(
        font_bytes: Arc<[u8]>,
        limits: &AttachedFontLimits,
    ) -> Result<Self, AttachedFontError> {
        let faces = parse_attached_font(&font_bytes, limits)?;
        Ok(Self {
            bytes: font_bytes,
            faces,
        })
    }

    /// Размер файла в байтах.
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Сколько пригодных лиц нашлось в файле.
    pub fn face_count(&self) -> usize {
        self.faces.len()
    }
}

/// Подготовленный пакет вложений одного media: принятые файлы и отказы тяжёлой фазы.
#[derive(Debug, Default)]
pub struct PreparedAttachedFonts {
    /// Принятые файлы вместе с номером во входном списке.
    pub(super) fonts: Vec<(usize, PreparedAttachedFont)>,
    rejected: Vec<RejectedAttachedFont>,
}

impl PreparedAttachedFonts {
    /// Тяжёлая фаза для набора файлов; плохой файл не мешает остальным.
    pub fn prepare(
        files: impl IntoIterator<Item = Arc<[u8]>>,
        limits: &AttachedFontLimits,
    ) -> Self {
        let mut batch = Self::default();
        for (input_index, bytes) in files.into_iter().enumerate() {
            match PreparedAttachedFont::prepare(bytes, limits) {
                Ok(font) => batch.fonts.push((input_index, font)),
                Err(error) => batch
                    .rejected
                    .push(RejectedAttachedFont { input_index, error }),
            }
        }
        batch
    }

    /// Файлы, отвергнутые тяжёлой фазой.
    pub fn rejected(&self) -> &[RejectedAttachedFont] {
        &self.rejected
    }

    /// Сколько файлов подготовлено к установке.
    pub fn prepared_count(&self) -> usize {
        self.fonts.len()
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        Vec<(usize, PreparedAttachedFont)>,
        Vec<RejectedAttachedFont>,
    ) {
        (self.fonts, self.rejected)
    }
}

/// Лицо вложенного шрифта, разобранное до изменения библиотеки.
#[derive(Debug)]
pub(super) struct ParsedAttachedFace {
    pub(super) collection_index: u32,
    pub(super) family_names: Vec<String>,
    pub(super) weight: FontWeightClass,
    pub(super) os2_weight: u16,
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
            os2_weight: info.weight.0,
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

    /// Забывает все файлы и возвращает лица, которые надо снять с библиотеки.
    pub(super) fn take_all(&mut self) -> (Vec<FaceId>, usize) {
        let faces: Vec<FaceId> = self.all_faces().collect();
        let bytes = self.total_bytes;
        self.files.clear();
        self.total_bytes = 0;
        self.total_faces = 0;
        (faces, bytes)
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
