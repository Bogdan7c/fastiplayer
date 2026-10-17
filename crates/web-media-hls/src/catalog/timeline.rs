//! Доказательство «общей временной шкалы» независимо нарезанных HLS child playlist-ов.
//!
//! Видео и каждая версия звука (AAC, E-AC-3, ...) режутся упаковщиком независимо: границы
//! сегментов ложатся на границы своих access unit-ов (кадр AAC ≈ 21,3 мс, кадр E-AC-3 = 32 мс
//! при 48 кГц), поэтому число сегментов и сумма `EXTINF` у версий законно расходятся. Runtime
//! HLS сопоставляет компоненты по времени (seek index по PTS, длительность = максимум из
//! компонентов), а не по номеру сегмента, поэтому каталогу достаточно доказать, что версии
//! покрывают один и тот же интервал презентации.
//!
//! Допуск — одна целевая длительность сегмента (`EXT-X-TARGETDURATION`) большей из двух
//! шкал. Обоснование: RFC 8216 гарантирует только то, что каждый сегмент не длиннее target
//! duration; при независимой нарезке концы двух шкал могут разойтись не больше чем на один
//! (последний, неполный) сегмент. Расхождение больше — это уже другой монтаж/другой файл,
//! и такая версия честно несовместима.

use std::sync::Arc;
use std::time::Duration;

/// Измеренная по manifest-у временная шкала одного child playlist-а.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsCatalogTimeline {
    /// Сумма `EXTINF` всех сегментов.
    pub total_duration: Duration,
    /// `EXT-X-TARGETDURATION`: верхняя граница длительности любого сегмента этой шкалы.
    pub target_segment_duration: Duration,
    /// Момент (сумма `EXTINF` до сегмента), с которого начинается каждый новый
    /// discontinuity-участок, по порядку. Первый участок (с нуля) не записывается.
    pub discontinuity_starts: Vec<Duration>,
}

/// Snapshot-local proof того, какой интервал презентации покрывает child.
///
/// Proof намеренно не требует равного числа сегментов и точного равенства `EXTINF`:
/// совместимость двух proofs проверяет [`HlsCatalogAlignmentProof::compatibility_with`]
/// с допуском в один сегмент (см. документацию модуля).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsCatalogAlignmentProof {
    total_duration: Duration,
    target_segment_duration: Duration,
    /// `Arc`: proof копируется в pending rows каталога, а список разрывов не меняется.
    discontinuity_starts: Arc<[Duration]>,
}

/// Результат сверки двух временных шкал; варианты несовместимости названы точно, чтобы
/// ошибка/диагностика говорила, что именно не сошлось.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsTimelineCompatibility {
    /// Шкалы покрывают один интервал с точностью до одного сегмента.
    Compatible,
    /// Суммарные длительности расходятся больше чем на один сегмент.
    TotalDurationMismatch,
    /// Разное число discontinuity-участков или они начинаются в разные моменты.
    DiscontinuityLayoutMismatch,
}

impl HlsCatalogAlignmentProof {
    /// Фиксирует измеренную шкалу как неизменяемый proof.
    #[must_use]
    pub fn from_timeline(timeline: HlsCatalogTimeline) -> Self {
        Self {
            total_duration: timeline.total_duration,
            target_segment_duration: timeline.target_segment_duration,
            discontinuity_starts: timeline.discontinuity_starts.into(),
        }
    }

    /// Сверяет две шкалы с допуском в одну target duration большей из них.
    ///
    /// Отношение симметрично, но не транзитивно (A≈B и B≈C не дают A≈C), поэтому каталог
    /// сверяет каждую пару video/audio отдельно, а не группирует proofs по классам.
    #[must_use]
    pub fn compatibility_with(&self, other: &Self) -> HlsTimelineCompatibility {
        let tolerance = self
            .target_segment_duration
            .max(other.target_segment_duration);
        if self.discontinuity_starts.len() != other.discontinuity_starts.len()
            || self
                .discontinuity_starts
                .iter()
                .zip(other.discontinuity_starts.iter())
                .any(|(left, right)| left.abs_diff(*right) > tolerance)
        {
            return HlsTimelineCompatibility::DiscontinuityLayoutMismatch;
        }
        if self.total_duration.abs_diff(other.total_duration) > tolerance {
            return HlsTimelineCompatibility::TotalDurationMismatch;
        }
        HlsTimelineCompatibility::Compatible
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(
        total_ms: u64,
        target_secs: u64,
        discontinuity_ms: &[u64],
    ) -> HlsCatalogAlignmentProof {
        HlsCatalogAlignmentProof::from_timeline(HlsCatalogTimeline {
            total_duration: Duration::from_millis(total_ms),
            target_segment_duration: Duration::from_secs(target_secs),
            discontinuity_starts: discontinuity_ms
                .iter()
                .copied()
                .map(Duration::from_millis)
                .collect(),
        })
    }

    #[test]
    fn frame_rounded_audio_cut_matches_video_within_one_segment() {
        // Живой случай D6: видео 20,000 с, E-AC-3 резан кадрами 32 мс → 20,032 с.
        let video = proof(20_000, 2, &[]);
        let eac3 = proof(20_032, 2, &[]);
        assert_eq!(
            video.compatibility_with(&eac3),
            HlsTimelineCompatibility::Compatible
        );
        assert_eq!(
            eac3.compatibility_with(&video),
            HlsTimelineCompatibility::Compatible,
            "relation is symmetric"
        );
    }

    #[test]
    fn tolerance_is_exactly_one_target_duration_of_the_coarser_timeline() {
        let video = proof(20_000, 2, &[]);
        assert_eq!(
            video.compatibility_with(&proof(22_000, 2, &[])),
            HlsTimelineCompatibility::Compatible,
            "one full segment is still tolerated"
        );
        assert_eq!(
            video.compatibility_with(&proof(22_001, 2, &[])),
            HlsTimelineCompatibility::TotalDurationMismatch
        );
        assert_eq!(
            video.compatibility_with(&proof(25_000, 6, &[])),
            HlsTimelineCompatibility::Compatible,
            "the coarser timeline's segment bound applies"
        );
    }

    #[test]
    fn discontinuity_layout_is_compared_by_time_not_segment_index() {
        let video = proof(30_000, 4, &[10_000]);
        assert_eq!(
            video.compatibility_with(&proof(30_010, 4, &[10_016])),
            HlsTimelineCompatibility::Compatible
        );
        assert_eq!(
            video.compatibility_with(&proof(30_000, 4, &[])),
            HlsTimelineCompatibility::DiscontinuityLayoutMismatch,
            "missing discontinuity"
        );
        assert_eq!(
            video.compatibility_with(&proof(30_000, 4, &[20_000])),
            HlsTimelineCompatibility::DiscontinuityLayoutMismatch,
            "discontinuity at another moment"
        );
    }
}
