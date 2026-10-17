//! Stable audio preservation/fallback policy без source-order tie-break.

use std::cmp::Ordering;

use web_media_core::{DefaultAudioSelectionTier, SemanticIdentity};

/// Audio-relevant subset pinned from yt-dlp sorting semantics.
///
/// Каждое known значение предпочтительнее missing; большее значение лучше.
/// Порядок полей = порядок сравнения (derive `Ord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioFallbackRank {
    /// Сравнивается первым: `FallbackOnly` family (AC-3/E-AC-3) не вытесняет прежние
    /// families, даже если у неё больше каналов. Политика — `mem:media-services/ac3-eac3-web-candidates`.
    codec_tier: DefaultAudioSelectionTier,
    preference: Option<i64>,
    language_preference: Option<i64>,
    quality: Option<i64>,
    channels: Option<u16>,
    codec: i16,
    bitrate: Option<u64>,
    sample_rate: Option<u32>,
    source_preference: Option<i64>,
}

impl AudioFallbackRank {
    /// Создаёт fully named rank; caller уже нормализовал provider numeric hints.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        preference: Option<i64>,
        language_preference: Option<i64>,
        quality: Option<i64>,
        channels: Option<u16>,
        codec: i16,
        bitrate: Option<u64>,
        sample_rate: Option<u32>,
        source_preference: Option<i64>,
    ) -> Self {
        Self {
            // По умолчанию прежнее поведение; caller с известным codec-ом уточняет уровень.
            codec_tier: DefaultAudioSelectionTier::Primary,
            preference,
            language_preference,
            quality,
            channels,
            codec,
            bitrate,
            sample_rate,
            source_preference,
        }
    }

    /// Задаёт уровень codec family для default-выбора (см. [`DefaultAudioSelectionTier`]).
    #[must_use]
    pub const fn with_codec_selection_tier(mut self, tier: DefaultAudioSelectionTier) -> Self {
        self.codec_tier = tier;
        self
    }
}

/// Сравнивает playable audio rows: current semantic identity сохраняется первой,
/// затем применяется stable rank и только потом semantic identity.
///
/// `Ordering::Less` означает, что `left` предпочтительнее `right`.
pub fn compare_audio_fallback(
    current: Option<&SemanticIdentity>,
    left_identity: &SemanticIdentity,
    left_rank: AudioFallbackRank,
    right_identity: &SemanticIdentity,
    right_rank: AudioFallbackRank,
) -> Ordering {
    let left_is_current = current == Some(left_identity);
    let right_is_current = current == Some(right_identity);
    right_is_current
        .cmp(&left_is_current)
        .then_with(|| right_rank.cmp(&left_rank))
        .then_with(|| left_identity.cmp(right_identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use web_media_core::SourceIdentity;

    fn semantic(key: &str) -> SemanticIdentity {
        SemanticIdentity::new(SourceIdentity::new(1), key).unwrap()
    }

    fn rank(bitrate: Option<u64>) -> AudioFallbackRank {
        AudioFallbackRank::new(None, None, None, Some(2), 30, bitrate, Some(48_000), None)
    }

    #[test]
    fn current_semantic_audio_wins_before_quality_and_fallback_is_order_independent() {
        let current = semantic("current");
        let better = semantic("better");
        assert_eq!(
            compare_audio_fallback(
                Some(&current),
                &current,
                rank(Some(64_000)),
                &better,
                rank(Some(320_000)),
            ),
            Ordering::Less
        );
        assert_eq!(
            compare_audio_fallback(
                None,
                &current,
                rank(Some(64_000)),
                &better,
                rank(Some(320_000)),
            ),
            Ordering::Greater
        );
        assert_eq!(
            compare_audio_fallback(
                None,
                &better,
                rank(Some(320_000)),
                &current,
                rank(Some(64_000)),
            ),
            Ordering::Less
        );
    }

    #[test]
    fn known_metadata_wins_over_missing_without_array_position() {
        let known = semantic("known");
        let missing = semantic("missing");
        assert_eq!(
            compare_audio_fallback(None, &known, rank(Some(1)), &missing, rank(None),),
            Ordering::Less
        );
    }

    /// Fallback-only family проигрывает даже при большем числе каналов, но остаётся
    /// лучшей, если других кандидатов нет; current identity по-прежнему сильнее уровня.
    #[test]
    fn fallback_only_codec_tier_yields_to_primary_before_channel_count() {
        let stereo_aac = semantic("aac-2ch");
        let surround_eac3 = semantic("eac3-6ch");
        let aac_rank = rank(Some(128_000));
        let eac3_rank = AudioFallbackRank::new(
            None,
            None,
            None,
            Some(6),
            25,
            Some(384_000),
            Some(48_000),
            None,
        )
        .with_codec_selection_tier(DefaultAudioSelectionTier::FallbackOnly);

        assert_eq!(
            compare_audio_fallback(None, &stereo_aac, aac_rank, &surround_eac3, eac3_rank),
            Ordering::Less
        );
        assert_eq!(
            compare_audio_fallback(
                Some(&surround_eac3),
                &stereo_aac,
                aac_rank,
                &surround_eac3,
                eac3_rank
            ),
            Ordering::Greater
        );
        // Без tier-а 6 каналов выиграли бы: доказываем, что именно tier меняет исход.
        let untiered_eac3 = eac3_rank.with_codec_selection_tier(DefaultAudioSelectionTier::Primary);
        assert_eq!(
            compare_audio_fallback(None, &stereo_aac, aac_rank, &surround_eac3, untiered_eac3),
            Ordering::Greater
        );
    }
}
