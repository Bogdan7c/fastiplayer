//! Выбор лица по запросу (имя семейства + начертание).

use super::face::{FaceId, FaceOrigin};
use super::library::SubtitleFontLibrary;
use super::style::{FontRequest, FontStyle, FontWeightClass, SyntheticBold, SyntheticItalic};

/// Найдено ли запрошенное семейство.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamilyMatch {
    /// Семейство найдено по имени.
    Requested,
    /// Имени не было или оно не найдено нигде: взят встроенный Noto Sans.
    DefaultFallback,
}

/// Результат выбора лица. Синтетика сообщается явно: решение «утолщить/наклонить» принимает
/// растеризатор, библиотека лишь говорит, что у лица нужного начертания нет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceMatch {
    /// Выбранное лицо.
    pub face: FaceId,
    /// Источник лица.
    pub origin: FaceOrigin,
    /// Найдено ли запрошенное семейство.
    pub family_match: FamilyMatch,
    /// Нужна ли синтетическая жирность.
    pub synthetic_bold: SyntheticBold,
    /// Нужен ли синтетический наклон.
    pub synthetic_italic: SyntheticItalic,
}

impl SubtitleFontLibrary {
    /// Выбирает лицо: по имени — вложенные, затем системные, затем встроенные;
    /// `None` или ненайденное имя — встроенный Noto Sans (Bold, если он запрошен).
    pub fn resolve_family(&self, request: &FontRequest<'_>) -> FaceMatch {
        if let Some(name) = request.family {
            let sources = [
                &self.attached_names,
                &self.system_names,
                &self.embedded_names,
            ];
            for index in sources {
                if let Some(face) = self.best_candidate(index.faces_of(name), request) {
                    return self.build_match(face, FamilyMatch::Requested, request);
                }
            }
            tracing::debug!(
                family = name,
                "font family not found, using default Noto Sans"
            );
        }
        let default_face = match request.weight {
            FontWeightClass::Bold => self.default_bold,
            FontWeightClass::Regular => self.default_regular,
        };
        self.build_match(default_face, FamilyMatch::DefaultFallback, request)
    }

    /// Лучшее лицо семейства: сначала совпадение наклона, затем насыщенности; при равенстве — первое.
    fn best_candidate(&self, candidates: &[FaceId], request: &FontRequest<'_>) -> Option<FaceId> {
        let mut best: Option<(u8, FaceId)> = None;
        for &candidate in candidates {
            // Непригодные лица (не загрузились при рисовании) не выбираются: иначе реплика
            // снова упёрлась бы в то же лицо.
            let Some(face) = self.face(candidate).filter(|face| !face.unusable) else {
                continue;
            };
            let score =
                u8::from(face.style == request.style) * 2 + u8::from(face.weight == request.weight);
            if best.is_none_or(|(best_score, _)| score > best_score) {
                best = Some((score, candidate));
            }
        }
        best.map(|(_, face)| face)
    }

    fn build_match(
        &self,
        face: FaceId,
        family_match: FamilyMatch,
        request: &FontRequest<'_>,
    ) -> FaceMatch {
        let (origin, weight, style) = self.face(face).map_or(
            (
                FaceOrigin::Embedded,
                FontWeightClass::Regular,
                FontStyle::Normal,
            ),
            |loaded| (loaded.origin, loaded.weight, loaded.style),
        );
        FaceMatch {
            face,
            origin,
            family_match,
            synthetic_bold: if request.weight.is_bold() && !weight.is_bold() {
                SyntheticBold::Needed
            } else {
                SyntheticBold::NotNeeded
            },
            synthetic_italic: if request.style == FontStyle::Italic && style == FontStyle::Normal {
                SyntheticItalic::Needed
            } else {
                SyntheticItalic::NotNeeded
            },
        }
    }
}
