use std::cmp::Ordering;
use std::fmt;
use std::num::NonZeroU32;

use crate::CandidateIdentity;

/// Верхняя граница neutral video height.
///
/// 16K оставляет запас над текущими 8K web ladders, но блокирует ошибочные
/// extractor числа до allocation/config persistence. Расширение этого bound
/// должно быть отдельным compatibility решением с decode/render evidence.
pub const MAX_VIDEO_HEIGHT: u32 = 16_384;

/// Проверенная ненулевая высота video representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VideoHeight(NonZeroU32);

impl VideoHeight {
    /// Проверяет диапазон `1..=MAX_VIDEO_HEIGHT`.
    pub fn new(pixels: u32) -> Result<Self, VideoHeightError> {
        let non_zero = NonZeroU32::new(pixels).ok_or(VideoHeightError::Zero)?;
        if pixels > MAX_VIDEO_HEIGHT {
            return Err(VideoHeightError::TooLarge {
                provided_pixels: pixels,
                maximum_pixels: MAX_VIDEO_HEIGHT,
            });
        }

        Ok(Self(non_zero))
    }

    /// Возвращает высоту в pixels.
    pub const fn pixels(self) -> u32 {
        self.0.get()
    }
}

/// Ошибка checked video height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoHeightError {
    /// Нулевая высота не описывает video representation.
    Zero,
    /// Значение выше named compatibility bound.
    TooLarge {
        /// Полученное значение.
        provided_pixels: u32,
        /// Разрешённое значение.
        maximum_pixels: u32,
    },
}

impl fmt::Display for VideoHeightError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("высота видео должна быть больше нуля"),
            Self::TooLarge {
                provided_pixels,
                maximum_pixels,
            } => write!(
                formatter,
                "высота {provided_pixels}px превышает лимит {maximum_pixels}px"
            ),
        }
    }
}

impl std::error::Error for VideoHeightError {}

/// Global/app-provided preference, отделённая от фактической высоты candidate-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreferredVideoHeight(VideoHeight);

impl PreferredVideoHeight {
    /// Валидирует preferred height тем же neutral bound.
    pub fn new(pixels: u32) -> Result<Self, VideoHeightError> {
        VideoHeight::new(pixels).map(Self)
    }

    /// Возвращает желаемую высоту.
    pub const fn height(self) -> VideoHeight {
        self.0
    }
}

/// Чистая height policy после полного playability/HDR filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreferredHeightPolicy {
    /// Height не влияет на quality ordering.
    NoPreference,
    /// Exact → closest lower → closest higher → missing.
    Prefer(PreferredVideoHeight),
}

impl PreferredHeightPolicy {
    /// Строит total-order rank для candidate height.
    pub const fn rank(self, candidate: Option<VideoHeight>) -> PreferredHeightRank {
        match (self, candidate) {
            (Self::NoPreference, _) => PreferredHeightRank::Unranked,
            (Self::Prefer(_), None) => PreferredHeightRank::Missing,
            (Self::Prefer(preferred), Some(actual))
                if actual.pixels() == preferred.height().pixels() =>
            {
                PreferredHeightRank::Exact
            }
            (Self::Prefer(preferred), Some(actual))
                if actual.pixels() < preferred.height().pixels() =>
            {
                PreferredHeightRank::Lower {
                    distance_pixels: preferred.height().pixels() - actual.pixels(),
                }
            }
            (Self::Prefer(preferred), Some(actual)) => PreferredHeightRank::Higher {
                distance_pixels: actual.pixels() - preferred.height().pixels(),
            },
        }
    }

    /// Сравнивает только height policy; quality/semantic tie-break остаётся caller-у.
    pub fn compare(self, left: Option<VideoHeight>, right: Option<VideoHeight>) -> Ordering {
        self.rank(left).cmp(&self.rank(right))
    }
}

/// Rank, где меньшее значение предпочтительнее.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PreferredHeightRank {
    /// Policy отключена; все heights равны на этом уровне.
    Unranked,
    /// Exact match.
    Exact,
    /// Ниже preference; меньший gap означает более высокую доступную высоту.
    Lower {
        /// Абсолютная разница.
        distance_pixels: u32,
    },
    /// Выше preference; меньший gap означает самую низкую fallback-высоту.
    Higher {
        /// Абсолютная разница.
        distance_pixels: u32,
    },
    /// Video height отсутствует.
    Missing,
}

/// Намерение выбора candidate-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionRequest {
    /// Выбрать лучший полностью playable candidate.
    BestPlayable,
    /// Выбрать exact candidate только в matching extraction snapshot.
    Exact(CandidateIdentity),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Policy сортирует exact, lower и higher buckets строго по D09.
    #[test]
    fn preferred_height_order_is_deterministic() {
        let policy =
            PreferredHeightPolicy::Prefer(PreferredVideoHeight::new(2160).expect("2160 валидно"));
        let height = |pixels| Some(VideoHeight::new(pixels).expect("test height валидна"));
        let mut candidates = [
            height(4320),
            None,
            height(720),
            height(2160),
            height(1440),
            height(1080),
            height(2880),
        ];

        candidates.sort_by_key(|candidate| policy.rank(*candidate));

        assert_eq!(
            candidates.map(|candidate| candidate.map(VideoHeight::pixels)),
            [
                Some(2160),
                Some(1440),
                Some(1080),
                Some(720),
                Some(2880),
                Some(4320),
                None,
            ]
        );
    }

    /// Disabled policy не вмешивается в следующий quality comparator.
    #[test]
    fn no_preference_leaves_all_heights_unranked() {
        let policy = PreferredHeightPolicy::NoPreference;
        let low = Some(VideoHeight::new(360).expect("height валидна"));
        let high = Some(VideoHeight::new(4320).expect("height валидна"));

        assert_eq!(policy.compare(low, high), Ordering::Equal);
        assert_eq!(policy.rank(None), PreferredHeightRank::Unranked);
    }

    /// Named bounds исключают zero и явно чрезмерные значения.
    #[test]
    fn video_height_rejects_values_outside_named_bounds() {
        assert_eq!(VideoHeight::new(0), Err(VideoHeightError::Zero));
        assert_eq!(
            PreferredVideoHeight::new(MAX_VIDEO_HEIGHT + 1),
            Err(VideoHeightError::TooLarge {
                provided_pixels: MAX_VIDEO_HEIGHT + 1,
                maximum_pixels: MAX_VIDEO_HEIGHT,
            })
        );
    }
}
