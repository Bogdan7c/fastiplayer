//! Выбор системного fallback-лица по глифу: язык, наклон и вес основного лица.
//!
//! libass через fontconfig просит «лицо с символом, похожее на запрошенное» (вес, наклон), а не
//! «первое лицо с символом». Без этого у CJK-шрифтов с десятком весов (Thin..Black) японская
//! строка рисовалась бы первым по индексу весом (Black), а отдельные иероглифы брались бы
//! из шрифтов, где они есть «заодно» (например, Nushu Bold для U+4EBA).
//!
//! Региональная форма CJK выбирается по языку (дорожки, иначе системы): fontconfig ставит
//! язык выше веса, поэтому японская реплика берёт `Noto Sans CJK JP`, а не алфавитно первый HK.

use super::face::{FaceId, FaceSource, LoadedFace, SystemFileKey};
use super::face_language::LanguageMatch;
use super::language::TextLanguagePreference;
use super::library::SubtitleFontLibrary;
use super::style::FontStyle;

/// Какое начертание желательно для системного fallback: берётся у основного лица реплики.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FallbackPreference {
    /// Желаемый вес OS/2.
    pub(super) os2_weight: u16,
    /// Желаемый наклон.
    pub(super) style: FontStyle,
    /// Язык реплики (дорожки или системы) для выбора региональной формы CJK.
    pub(super) language: TextLanguagePreference,
}

/// Ключ сортировки кандидатов: меньше — лучше. Порядок полей = порядок важности (наклон,
/// совпадение языка, расстояние по весу); хвост (семейство, файл, индекс в коллекции) делает
/// выбор детерминированным и не зависящим от порядка обхода каталогов.
///
/// Совпадение языка `None` у всех кандидатов, если язык неизвестен: порядок тогда прежний.
type RankKey = (bool, Option<LanguageMatch>, u16, String, String, u32);

fn rank_key(face: &LoadedFace, preference: FallbackPreference) -> RankKey {
    let (file, language_match) = match &face.source {
        FaceSource::System {
            file_key,
            languages,
            ..
        } => {
            let file = match file_key {
                SystemFileKey::Path(path) => path.to_string_lossy().into_owned(),
                SystemFileKey::Face(_) => String::new(),
            };
            let language_match = preference
                .language
                .profile()
                .map(|wanted| languages.match_for(wanted));
            (file, language_match)
        }
        _ => (String::new(), None),
    };
    (
        face.style != preference.style,
        language_match,
        face.os2_weight.abs_diff(preference.os2_weight),
        face.family.to_lowercase(),
        file,
        face.collection_index,
    )
}

impl SubtitleFontLibrary {
    /// Предпочтение fallback для основного лица `primary` (его вес и наклон) и языка `language`.
    pub(super) fn fallback_preference(
        &self,
        primary: FaceId,
        language: TextLanguagePreference,
    ) -> FallbackPreference {
        self.face(primary).map_or(
            FallbackPreference {
                os2_weight: 400,
                style: FontStyle::Normal,
                language,
            },
            |face| FallbackPreference {
                os2_weight: face.os2_weight,
                style: face.style,
                language,
            },
        )
    }

    /// Системные лица, чьё покрытие содержит `ch`, от лучшего кандидата к худшему.
    pub(super) fn ranked_system_candidates(
        &self,
        ch: char,
        preference: FallbackPreference,
    ) -> Vec<FaceId> {
        let mut candidates: Vec<(RankKey, FaceId)> = self
            .system_face_ids
            .iter()
            .copied()
            .filter(|face| self.is_face_usable(*face))
            .filter_map(|face_id| {
                let face = self.face(face_id)?;
                match &face.source {
                    FaceSource::System { coverage, .. } if coverage.contains(u32::from(ch)) => {
                        Some((rank_key(face, preference), face_id))
                    }
                    _ => None,
                }
            })
            .collect();
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        candidates.into_iter().map(|(_, face)| face).collect()
    }
}
