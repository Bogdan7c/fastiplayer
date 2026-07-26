//! Stable audio preservation/fallback policy без source-order tie-break.

use std::cmp::Ordering;

use web_media_core::SemanticIdentity;

/// Audio-relevant subset pinned from yt-dlp sorting semantics.
///
/// Каждое known значение предпочтительнее missing; большее значение лучше.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AudioFallbackRank {
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
}
