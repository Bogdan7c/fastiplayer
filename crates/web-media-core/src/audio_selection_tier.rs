//! Уровень audio codec family при выборе web-кандидата «по умолчанию».
//!
//! AC-3/E-AC-3 стали воспроизводимыми позже остальных audio families (2026-10-16).
//! До этого web-кандидаты с ними отбрасывались ещё до выбора, поэтому прежние
//! default-решения (HLS master, DASH native default, yt-dlp audio fallback) никогда
//! их не видели. Чтобы расширение набора не поменяло молча уже существующий выбор
//! (например, stereo AAC → 5.1 E-AC-3), новые families получают уровень
//! [`DefaultAudioSelectionTier::FallbackOnly`]: они выбираются по умолчанию, только
//! когда среди кандидатов того же уровня сравнения нет ни одного `Primary`.
//! Явный выбор пользователя (exact/semantic selection) этот уровень не затрагивает.
//!
//! Это единственный владелец политики: решение владельца «предпочитать многоканальный
//! E-AC-3» меняется здесь одной строкой, а не в каждом provider-е.

use crate::{CodecFamily, CodecKind};

/// Уровень audio codec-а для default-выбора; больший уровень предпочтительнее.
///
/// Порядок вариантов значим: derive `Ord` делает `Primary > FallbackOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DefaultAudioSelectionTier {
    /// Family выбирается по умолчанию только при отсутствии `Primary` альтернативы.
    FallbackOnly,
    /// Family участвует в default-выборе на прежних правах.
    Primary,
}

impl DefaultAudioSelectionTier {
    /// Возвращает уровень для нормализованного codec-а.
    ///
    /// `Absent`/`Unknown` остаются `Primary`: для них прежнее поведение не меняется
    /// (их судьбу решают capability/admission layers, а не этот tie-break).
    #[must_use]
    pub const fn for_codec(kind: CodecKind) -> Self {
        match kind {
            CodecKind::Known(CodecFamily::Ac3 | CodecFamily::Eac3) => Self::FallbackOnly,
            CodecKind::Known(_) | CodecKind::Absent | CodecKind::Unknown => Self::Primary,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC-3 и E-AC-3 уступают любой прежней audio family, но не исчезают из выбора.
    #[test]
    fn dolby_families_are_fallback_only_and_rank_below_primary() {
        for family in [CodecFamily::Ac3, CodecFamily::Eac3] {
            assert_eq!(
                DefaultAudioSelectionTier::for_codec(CodecKind::Known(family)),
                DefaultAudioSelectionTier::FallbackOnly
            );
        }
        for family in [
            CodecFamily::Aac,
            CodecFamily::Opus,
            CodecFamily::Mp3,
            CodecFamily::Flac,
        ] {
            assert_eq!(
                DefaultAudioSelectionTier::for_codec(CodecKind::Known(family)),
                DefaultAudioSelectionTier::Primary
            );
        }
        assert_eq!(
            DefaultAudioSelectionTier::for_codec(CodecKind::Unknown),
            DefaultAudioSelectionTier::Primary
        );
        assert!(DefaultAudioSelectionTier::Primary > DefaultAudioSelectionTier::FallbackOnly);
    }
}
