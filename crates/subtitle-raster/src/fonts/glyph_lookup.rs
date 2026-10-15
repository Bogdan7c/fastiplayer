//! Поиск глифа символа с fallback по шрифтам.
//!
//! Порядок (решение владельца): основное лицо -> встроенные (в порядке списка) -> вложенные ->
//! системные по индексу покрытия. Если глифа нет нигде — `.notdef` основного лица, `tracing::debug` и счётчик.

use skrifa::{GlyphId, MetadataProvider};

use super::face::{FaceId, FaceSource};
use super::language::TextLanguagePreference;
use super::library::SubtitleFontLibrary;
use super::lookup_cache::CachedLookup;

/// Результат поиска глифа для символа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphLookup {
    /// Глиф найден в `face` (не обязательно в основном лице).
    Found {
        /// Лицо, содержащее глиф.
        face: FaceId,
        /// Номер глифа (не `.notdef`).
        glyph: GlyphId,
    },
    /// Управляющий символ (< 0x20): как в libass глиф 0 основного лица, не считается пропущенным.
    ControlCharacter {
        /// Основное лицо.
        face: FaceId,
    },
    /// Глифа нет ни в одном шрифте: рисуем `.notdef` основного лица.
    Missing {
        /// Основное лицо.
        face: FaceId,
        /// `.notdef` (глиф 0).
        notdef: GlyphId,
    },
}

impl SubtitleFontLibrary {
    /// Находит лицо и глиф для символа `ch` при основном лице `primary`
    /// (libass 0.17.5: `ass_font.c:ass_font_get_index`).
    ///
    /// `language` влияет только на выбор СИСТЕМНОГО fallback-лица среди равных по покрытию
    /// (региональная форма CJK); основное, встроенные и вложенные лица он не меняет.
    pub fn glyph_for_char(
        &mut self,
        primary: FaceId,
        ch: char,
        language: TextLanguagePreference,
    ) -> GlyphLookup {
        // libass: управляющие символы не ищутся, это глиф 0 первого лица.
        if u32::from(ch) < 0x20 {
            return GlyphLookup::ControlCharacter { face: primary };
        }
        let cached = self.lookup_cache.get(primary, ch, language);
        let outcome = match cached {
            Some(hit) => {
                self.lookup_cache_hits += 1;
                hit
            }
            None => {
                self.lookup_cache_misses += 1;
                let SearchOutcome { lookup, unreadable } = self.search_glyph(primary, ch, language);
                // Лицо, которое не читается, исключаем из выбора до записи в кэш: иначе кэш
                // мог бы хранить ссылки на лицо, которое мы только что отключили.
                for face in unreadable {
                    self.mark_face_unusable(face);
                }
                self.lookup_cache.insert((primary, ch, language), lookup);
                lookup
            }
        };
        match outcome {
            CachedLookup::Found { face, glyph } => GlyphLookup::Found { face, glyph },
            CachedLookup::Missing => {
                self.missing_glyph_lookups += 1;
                if cached.is_none() {
                    tracing::debug!(
                        codepoint = u32::from(ch),
                        "no font has a glyph for the character"
                    );
                }
                GlyphLookup::Missing {
                    face: primary,
                    notdef: GlyphId::NOTDEF,
                }
            }
        }
    }

    /// Полный поиск без кэша.
    ///
    /// Порядок: основное лицо -> встроенные -> вложенные -> системные. Вложенные идут раньше
    /// системных, потому что шрифты MKV часто несут нужные письменности (например, CJK) именно
    /// для этого фильма. Непригодные лица пропускаются; системные проверяются по индексу
    /// покрытия в памяти, файл открывается только у лица-кандидата для получения номера глифа.
    fn search_glyph(
        &self,
        primary: FaceId,
        ch: char,
        language: TextLanguagePreference,
    ) -> SearchOutcome {
        let embedded = (0..self.embedded_face_count)
            .map(|index| FaceId(u32::try_from(index).unwrap_or(u32::MAX)));
        // Системные лица идут не по порядку индекса, а от самого похожего на основное лицо
        // по наклону и весу (см. `system_fallback`). Список строится лениво: только когда
        // перебор дошёл до системных, то есть в основном, встроенных и вложенных символа нет.
        let system = std::iter::once(()).flat_map(|()| {
            let preference = self.fallback_preference(primary, language);
            self.ranked_system_candidates(ch, preference)
        });
        let candidates = std::iter::once(primary)
            .chain(embedded)
            .chain(self.attached.all_faces())
            .chain(system)
            .enumerate()
            // Основное лицо уже стоит первым, повторно его не проверяем.
            .filter(|(position, face)| *position == 0 || *face != primary);
        let mut unreadable = Vec::new();
        for (_, candidate) in candidates {
            if !self.is_face_usable(candidate) {
                continue;
            }
            match self.glyph_in_face(candidate, ch) {
                FaceProbe::Glyph(glyph) => {
                    return SearchOutcome {
                        lookup: CachedLookup::Found {
                            face: candidate,
                            glyph,
                        },
                        unreadable,
                    };
                }
                FaceProbe::Absent => {}
                FaceProbe::Unreadable => unreadable.push(candidate),
            }
        }
        SearchOutcome {
            lookup: CachedLookup::Missing,
            unreadable,
        }
    }

    /// Глиф символа в одном из уже использованных системных лиц (непрерывность участка).
    ///
    /// libass (`ass_font_get_index`) перед запросом нового fallback перебирает лица, уже
    /// добавленные в шрифт; иначе соседние символы одной строки расползались бы по разным
    /// лицам. Берётся первое из `used` (в порядке использования), где символ есть. Лица не
    /// системные, непригодные и нечитаемые пропускаются: решение о них принимает основной поиск.
    pub(crate) fn glyph_in_used_system_faces(
        &self,
        used: &[FaceId],
        ch: char,
    ) -> Option<(FaceId, GlyphId)> {
        used.iter().find_map(|&face| {
            if !self.is_system_face(face) || !self.is_face_usable(face) {
                return None;
            }
            match self.glyph_in_face(face, ch) {
                FaceProbe::Glyph(glyph) => Some((face, glyph)),
                FaceProbe::Absent | FaceProbe::Unreadable => None,
            }
        })
    }

    /// Системное ли лицо (только такие участвуют в непрерывности fallback).
    pub(crate) fn is_system_face(&self, face: FaceId) -> bool {
        self.face(face)
            .is_some_and(|loaded| matches!(loaded.source, FaceSource::System { .. }))
    }

    /// Есть ли в лице глиф символа.
    fn glyph_in_face(&self, face: FaceId, ch: char) -> FaceProbe {
        let Some(loaded) = self.face(face) else {
            return FaceProbe::Absent;
        };
        // Системное лицо: сначала индекс в памяти, файл не трогаем, если символа там нет.
        if let FaceSource::System { coverage, .. } = &loaded.source
            && !coverage.contains(u32::from(ch))
        {
            return FaceProbe::Absent;
        }
        match loaded.with_font_ref(self.data_access(), |font| font.charmap().map(ch)) {
            Some(Some(glyph)) if glyph != GlyphId::NOTDEF => FaceProbe::Glyph(glyph),
            Some(_) => FaceProbe::Absent,
            None => FaceProbe::Unreadable,
        }
    }
}

/// Результат полного поиска: ответ и лица, которые не удалось прочитать по пути.
struct SearchOutcome {
    lookup: CachedLookup,
    /// Лица, которые должны были быть проверены, но файл не открылся/не разобрался:
    /// владелец помечает их непригодными.
    unreadable: Vec<FaceId>,
}

/// Итог проверки одного лица.
enum FaceProbe {
    /// Глиф есть.
    Glyph(GlyphId),
    /// Глифа нет (`.notdef` не считается).
    Absent,
    /// Лицо не прочиталось.
    Unreadable,
}
