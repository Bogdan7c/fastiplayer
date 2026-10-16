//! Жизненный цикл вложенных шрифтов в библиотеке: регистрация, установка подготовленных
//! шрифтов (дешёвая фаза) и снятие всех вложений при смене media.
//!
//! Тяжёлый разбор живёт в [`super::attached`]; здесь только изменение состояния библиотеки.

use std::sync::Arc;

use super::attached::{
    AttachedFontError, AttachedFontLimits, AttachedFontRegistration, AttachedFontStatus,
    AttachedFontsClearOutcome, AttachedFontsInstallReport, PreparedAttachedFont,
    PreparedAttachedFonts, RejectedAttachedFont,
};
use super::face::{FaceId, FaceOrigin, FaceSource, LoadedFace};
use super::library::SubtitleFontLibrary;

impl SubtitleFontLibrary {
    /// Регистрирует вложенный шрифт (все лица коллекции): подготовка и установка подряд.
    /// При ошибке библиотека не меняется.
    ///
    /// Точно такие же байты, уже зарегистрированные раньше, дают
    /// [`AttachedFontStatus::AlreadyRegistered`] и не меняют библиотеку (в том числе кэш
    /// поиска глифов): для вызывающего это сигнал, что сбрасывать свои кэши не нужно.
    /// Для фонового разбора используйте [`PreparedAttachedFont::prepare`] +
    /// [`Self::install_prepared_attached_font`].
    pub fn register_attached_font(
        &mut self,
        font_bytes: Arc<[u8]>,
        limits: &AttachedFontLimits,
    ) -> Result<AttachedFontRegistration, AttachedFontError> {
        // Дубликат определяем до тяжёлого разбора: повтор должен оставаться дешёвым.
        if let Some(known) = self.attached.faces_of_duplicate(&font_bytes) {
            return Ok(AttachedFontRegistration {
                status: AttachedFontStatus::AlreadyRegistered,
                faces: known.to_vec(),
            });
        }
        let prepared = PreparedAttachedFont::prepare(font_bytes, limits)?;
        self.install_prepared_attached_font(prepared, limits)
    }

    /// Дешёвая фаза: добавляет уже разобранный шрифт. Проверяет дубликат и суммарные лимиты
    /// (`limits.max_total_bytes`, `limits.max_faces`); при ошибке библиотека не меняется.
    pub fn install_prepared_attached_font(
        &mut self,
        prepared: PreparedAttachedFont,
        limits: &AttachedFontLimits,
    ) -> Result<AttachedFontRegistration, AttachedFontError> {
        let PreparedAttachedFont {
            bytes: font_bytes,
            faces: parsed_faces,
        } = prepared;
        if let Some(known) = self.attached.faces_of_duplicate(&font_bytes) {
            return Ok(AttachedFontRegistration {
                status: AttachedFontStatus::AlreadyRegistered,
                faces: known.to_vec(),
            });
        }
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

    /// Дешёвая фаза для пакета: устанавливает каждый подготовленный файл по очереди.
    /// Отказ одного файла (суммарный лимит) не мешает остальным; отказы тяжёлой фазы
    /// переносятся в отчёт, чтобы вызывающий видел все отказы в одном месте.
    pub fn install_prepared_attached_fonts(
        &mut self,
        prepared: PreparedAttachedFonts,
        limits: &AttachedFontLimits,
    ) -> AttachedFontsInstallReport {
        let (fonts, mut rejected) = prepared.into_parts();
        let mut registered = 0;
        let mut already_registered = 0;
        for (input_index, font) in fonts {
            match self.install_prepared_attached_font(font, limits) {
                Ok(registration) => match registration.status {
                    AttachedFontStatus::Registered => registered += 1,
                    AttachedFontStatus::AlreadyRegistered => already_registered += 1,
                },
                Err(error) => rejected.push(RejectedAttachedFont { input_index, error }),
            }
        }
        rejected.sort_by_key(|rejection| rejection.input_index);
        AttachedFontsInstallReport {
            registered,
            already_registered,
            rejected,
        }
    }

    /// Снимает ВСЕ вложенные шрифты (смена media). Встроенные и системные не затрагиваются.
    ///
    /// `FaceId` снятых лиц не переиспользуются: записи остаются «надгробиями» без данных
    /// (сотня байт на лицо), поэтому устаревший `FaceId` никогда не укажет на чужой шрифт.
    /// Кэш поиска глифов сбрасывается, только если что-то было снято.
    pub fn clear_attached_fonts(&mut self) -> AttachedFontsClearOutcome {
        let (face_ids, bytes) = self.attached.take_all();
        if face_ids.is_empty() {
            return AttachedFontsClearOutcome::NothingAttached;
        }
        let faces = face_ids.len();
        for face_id in face_ids {
            self.detach_face(face_id);
        }
        self.attached_names.clear();
        self.lookup_cache.clear();
        AttachedFontsClearOutcome::Cleared { faces, bytes }
    }

    fn detach_face(&mut self, face: FaceId) {
        if let Some(loaded) = usize::try_from(face.0)
            .ok()
            .and_then(|index| self.faces.get_mut(index))
        {
            loaded.detach();
        }
    }
}
