//! Нормы памяти полос для предохранителя: «зеркало браузера».
//!
//! Браузер держит буфер каждой дорожки отдельно и выкидывает лишнее только из
//! дорожки, перевалившей СВОЙ лимит (Chromium: видео 150 МиБ, звук 12 МиБ;
//! Firefox: 150 / 20 МиБ; замер YouTube 4K60, 1.11.2026). Поэтому лёгкий звук
//! лежит у браузера долго (минуты), а тяжёлое видео — меньше минуты, и при
//! возврате в пройденное место страница перекачивает только видео. Чтобы такая
//! перемотка доезжала, полка держит по каждой полосе не меньше браузера:
//! - у полосы без видео — резерв (24 МиБ, больше лимита звука любого браузера);
//! - полосам с видео поровну делится всё остальное место до порога «полон».
//!
//! Предохранитель выкидывает только у полосы, перевалившей свою норму.

use std::collections::BTreeMap;

use crate::framing::LaneContent;
use crate::lane::LaneId;

/// Нормы памяти полос с временем на один шаг предохранителя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LaneNorms {
    /// Полоса → сколько байтов она вправе держать.
    norms: BTreeMap<LaneId, usize>,
}

impl LaneNorms {
    /// Считает нормы.
    ///
    /// `full_at_held_bytes` — порог «полон», `without_video_reserve` — резерв
    /// полосы без видео. Если резервы сами не помещаются в порог (крошечный
    /// потолок из config), полосам с видео достаётся ноль, а резерв полосы без
    /// видео урезается до доли порога: норма не может быть больше порога.
    pub(super) fn compute(
        lanes: impl IntoIterator<Item = (LaneId, LaneContent)>,
        full_at_held_bytes: usize,
        without_video_reserve: usize,
    ) -> Self {
        let lanes: Vec<(LaneId, LaneContent)> = lanes.into_iter().collect();
        let without_video = lanes
            .iter()
            .filter(|(_, content)| *content == LaneContent::WithoutVideo)
            .count();
        let with_video = lanes.len() - without_video;
        // Резерв лёгкой полосы — не больше её равной доли порога.
        let reserve = match without_video {
            0 => 0,
            count => without_video_reserve.min(full_at_held_bytes / count),
        };
        let left_for_video =
            full_at_held_bytes.saturating_sub(reserve.saturating_mul(without_video));
        let video_norm = match with_video {
            0 => 0,
            count => left_for_video / count,
        };
        let norms = lanes
            .into_iter()
            .map(|(lane, content)| {
                let norm = match content {
                    LaneContent::WithoutVideo => reserve,
                    LaneContent::CarriesVideo => video_norm,
                };
                (lane, norm)
            })
            .collect();
        Self { norms }
    }

    /// Насколько полоса, держащая `held_bytes`, перевалила свою норму
    /// (отрицательное — сколько ещё до нормы). Неизвестная полоса — норма ноль.
    pub(super) fn excess_of(&self, lane: LaneId, held_bytes: usize) -> i128 {
        let norm = self.norms.get(&lane).copied().unwrap_or(0);
        i128::try_from(held_bytes).unwrap_or(i128::MAX) - i128::try_from(norm).unwrap_or(i128::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: usize = 1024 * 1024;
    const VIDEO: LaneId = LaneId::new(0);
    const AUDIO: LaneId = LaneId::new(1);

    #[test]
    fn audio_gets_its_reserve_and_video_gets_the_rest_of_the_threshold() {
        let norms = LaneNorms::compute(
            [
                (VIDEO, LaneContent::CarriesVideo),
                (AUDIO, LaneContent::WithoutVideo),
            ],
            240 * MIB,
            24 * MIB,
        );
        // Видео — 216 МиБ: держит 216 → на норме, 217 → перевалило на 1 МиБ.
        assert_eq!(norms.excess_of(VIDEO, 216 * MIB), 0);
        assert_eq!(
            norms.excess_of(VIDEO, 217 * MIB),
            i128::try_from(MIB).expect("fits")
        );
        // Звук — 24 МиБ.
        assert_eq!(
            norms.excess_of(AUDIO, 23 * MIB),
            -i128::try_from(MIB).expect("fits")
        );
        assert_eq!(norms.excess_of(AUDIO, 24 * MIB), 0);
    }

    #[test]
    fn a_single_muxed_lane_may_hold_the_whole_threshold() {
        let norms = LaneNorms::compute([(VIDEO, LaneContent::CarriesVideo)], 240 * MIB, 24 * MIB);
        assert_eq!(norms.excess_of(VIDEO, 240 * MIB), 0);
    }

    #[test]
    fn two_video_lanes_split_what_is_left_after_the_reserves() {
        let second_video = LaneId::new(2);
        let norms = LaneNorms::compute(
            [
                (VIDEO, LaneContent::CarriesVideo),
                (second_video, LaneContent::CarriesVideo),
                (AUDIO, LaneContent::WithoutVideo),
            ],
            240 * MIB,
            24 * MIB,
        );
        assert_eq!(norms.excess_of(VIDEO, 108 * MIB), 0);
        assert_eq!(norms.excess_of(second_video, 108 * MIB), 0);
    }

    #[test]
    fn a_tiny_threshold_never_gives_a_norm_above_the_threshold() {
        // Порог 10 МиБ меньше резерва: звук получает весь порог, видео — ноль.
        let norms = LaneNorms::compute(
            [
                (VIDEO, LaneContent::CarriesVideo),
                (AUDIO, LaneContent::WithoutVideo),
            ],
            10 * MIB,
            24 * MIB,
        );
        assert_eq!(norms.excess_of(AUDIO, 10 * MIB), 0);
        assert_eq!(norms.excess_of(VIDEO, 0), 0);
        // Неизвестная полоса — норма ноль.
        assert_eq!(norms.excess_of(LaneId::new(7), 5), 5);
    }
}
