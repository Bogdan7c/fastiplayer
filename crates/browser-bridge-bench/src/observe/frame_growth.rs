//! «Показ кадров идёт прямо сейчас»: правило роста кадров за время и его судья.
//!
//! Раньше стенд доказывал показ так: первая новая сводка — точка отсчёта, следующая —
//! «кадров стало на N больше». Между сводками плеер ждал 10 с, и правило молча значило
//! «N кадров за 10–20 с». С коротким шагом сводки (сессия 16b3c2) смысл задаётся явно:
//! **после события нашлись две сводки, между которыми по часам плеера прошло не больше T,
//! а показ вырос не меньше чем на N кадров.** Шаг сводки в правило больше не входит: любой
//! шаг короче T даёт тот же приговор.
//!
//! Время между сводками — по отметкам плеера в начале строк ([`parse_log_time`]): так
//! опрос файла стендом и нагрузка параллельных строк приговор не сдвигают.

use std::time::Duration;

use super::log_time::parse_log_time;
use super::player_log::FrameEvidence;

/// Окно «показ идёт» по умолчанию для судей стенда: N кадров (30–100, то есть 1–4 с
/// показа при 25 к/с) должны прийти не дольше чем за 10 с по часам плеера. Это прежнее
/// неявное правило «между двумя сводками по 10 с», записанное явно: показ медленнее
/// ~40 % реального времени — уже не «играет», а застревает.
pub const PLAYBACK_GROWTH_WINDOW: Duration = Duration::from_secs(10);

/// Две сводки, между которыми показ кадров вырос на нужную величину.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameGrowth {
    /// Сводка-точка отсчёта.
    pub before: FrameEvidence,
    /// Сводка, в которой кадров уже достаточно больше.
    pub after: FrameEvidence,
}

impl FrameGrowth {
    /// На сколько выросло число показанных кадров.
    pub fn frames_gained(&self) -> u64 {
        self.after
            .presented_frames
            .saturating_sub(self.before.presented_frames)
    }

    /// Сколько прошло между двумя сводками по часам плеера; `None`, если у строки нет
    /// отметки времени или порядок строк обратный.
    pub fn player_time(&self) -> Option<Duration> {
        let before = parse_log_time(&self.before.summary_line)?;
        let after = parse_log_time(&self.after.summary_line)?;
        after.since(before)
    }
}

/// Правило «показ идёт»: не меньше `frames` кадров не дольше чем за `within`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameGrowthRule {
    frames: u64,
    within: Duration,
}

impl FrameGrowthRule {
    /// `frames` кадров за `within` по часам плеера.
    pub const fn frames_within(frames: u64, within: Duration) -> Self {
        Self { frames, within }
    }

    /// Сколько кадров требуется.
    pub fn frames(self) -> u64 {
        self.frames
    }

    /// За какое время.
    pub fn within(self) -> Duration {
        self.within
    }

    /// Выполнено ли правило для этой пары сводок. Пара без времени плеера правило не
    /// выполняет: «быстро ли» тогда не проверить.
    pub fn is_met_by(self, growth: &FrameGrowth) -> bool {
        growth.frames_gained() >= self.frames
            && growth
                .player_time()
                .is_some_and(|between| between <= self.within)
    }

    /// Человеческое описание правила для сообщений о провале.
    pub fn describe(self) -> String {
        format!(
            "presented_frames to grow by at least {} within {:.1} s of player time",
            self.frames,
            self.within.as_secs_f64()
        )
    }
}

/// Ищет среди сводок после события (по порядку журнала) первую пару, выполняющую
/// правило: для каждой сводки «после» берётся самая ранняя сводка «до» не старше `within` —
/// при накопительном счётчике это лучшая пара для неё. `None` — правило пока не выполнено.
pub fn find_frame_growth(
    summaries: &[FrameEvidence],
    rule: FrameGrowthRule,
) -> Option<FrameGrowth> {
    summaries
        .iter()
        .enumerate()
        .find_map(|(after_index, after)| {
            summaries[..after_index].iter().find_map(|before| {
                let growth = FrameGrowth {
                    before: before.clone(),
                    after: after.clone(),
                };
                rule.is_met_by(&growth).then_some(growth)
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Сводка плеера с отметкой `seconds` после 16:17:00 и `frames` кадрами.
    fn summary_at(seconds: f64, frames: u64) -> FrameEvidence {
        let whole = seconds.trunc() as u64;
        let micros = ((seconds.fract()) * 1_000_000.0).round() as u64;
        let line = format!(
            "2026-11-20T16:17:{whole:02}.{micros:06}Z  INFO Browser playback summary kind=periodic \
             presented_frames={frames} position_regressions=0"
        );
        super::super::parse_summary(&line).expect("a summary line")
    }

    const SIXTY_IN_TEN: FrameGrowthRule =
        FrameGrowthRule::frames_within(60, Duration::from_secs(10));

    #[test]
    fn enough_frames_soon_enough_meet_the_rule() {
        let summaries = [
            summary_at(1.0, 10),
            summary_at(2.0, 40),
            summary_at(3.5, 75),
        ];
        let growth = find_frame_growth(&summaries, SIXTY_IN_TEN).expect("growth");
        assert_eq!(growth.before.presented_frames, 10);
        assert_eq!(growth.after.presented_frames, 75);
        assert_eq!(growth.frames_gained(), 65);
        assert_eq!(growth.player_time(), Some(Duration::from_millis(2_500)));
    }

    #[test]
    fn too_few_frames_do_not_meet_the_rule() {
        let summaries = [
            summary_at(1.0, 10),
            summary_at(2.0, 40),
            summary_at(3.0, 69),
        ];
        assert_eq!(find_frame_growth(&summaries, SIXTY_IN_TEN), None);
    }

    #[test]
    fn enough_frames_too_slowly_do_not_meet_the_rule() {
        // 60 кадров, но за 12 с: показ шёл урывками, а не «прямо сейчас».
        let summaries = [summary_at(1.0, 0), summary_at(13.0, 60)];
        assert_eq!(find_frame_growth(&summaries, SIXTY_IN_TEN), None);
    }

    #[test]
    fn the_window_slides_past_a_stall_after_the_event() {
        // Застой 0–20 с после события, потом показ пошёл: правило выполняет поздняя пара.
        let summaries = [
            summary_at(0.0, 5),
            summary_at(20.0, 5),
            summary_at(21.0, 35),
            summary_at(22.0, 66),
        ];
        let growth = find_frame_growth(&summaries, SIXTY_IN_TEN).expect("growth");
        assert_eq!(growth.before.presented_frames, 5);
        assert_eq!(growth.after.presented_frames, 66);
        assert_eq!(growth.player_time(), Some(Duration::from_secs(2)));
    }

    #[test]
    fn a_single_summary_is_never_growth() {
        // Одна накопительная сводка могла вобрать кадры, показанные до события.
        assert_eq!(
            find_frame_growth(&[summary_at(1.0, 500)], SIXTY_IN_TEN),
            None
        );
    }

    #[test]
    fn summaries_without_player_time_cannot_prove_speed() {
        let unstamped = |frames: u64| {
            super::super::parse_summary(&format!(
                "Browser playback summary kind=periodic presented_frames={frames} position_regressions=0"
            ))
            .expect("a summary line")
        };
        assert_eq!(
            find_frame_growth(&[unstamped(0), unstamped(500)], SIXTY_IN_TEN),
            None
        );
    }

    #[test]
    fn the_rule_names_its_frames_and_window() {
        assert_eq!(
            SIXTY_IN_TEN.describe(),
            "presented_frames to grow by at least 60 within 10.0 s of player time"
        );
    }
}
