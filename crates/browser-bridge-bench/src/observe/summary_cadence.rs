//! Шаг сводки журнала плеера на стенде (сессия 16b3c2, решение 386).
//!
//! Настоящий плеер пишет сводку раз в 10 с, и строки стенда ждали её целиком. Стенд
//! просит плеер (тестовой переменной процесса, как `FASTIPLAYER_TEST_BROWSER_AUDIO`)
//! писать сводку раз в [`PLAYER_SUMMARY_INTERVAL`]. Судьи от шага не зависят — они меряют
//! рост кадров за время ([`super::FrameGrowthRule`]); шаг лишь решает, как скоро стенд
//! увидит то, что уже случилось.

use std::time::Duration;

/// Переменная процесса плеера с шагом сводки в миллисекундах (тот же текст, что в
/// `app-egui` `browser_session/summary_interval_override.rs`).
pub const PLAYER_SUMMARY_INTERVAL_VARIABLE: &str = "FASTIPLAYER_TEST_BROWSER_SUMMARY_INTERVAL_MS";

/// Шаг сводки на стенде: 1 с — в десять раз меньше настоящего, но в четыре раза длиннее
/// тика часов плеера (250 мс, короче плеер не принимает), и журнал строки не распухает.
pub const PLAYER_SUMMARY_INTERVAL: Duration = Duration::from_secs(1);

/// Значение переменной для плеера: шаг в целых миллисекундах.
pub fn player_summary_interval_value() -> String {
    PLAYER_SUMMARY_INTERVAL.as_millis().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_player_gets_whole_milliseconds() {
        assert_eq!(player_summary_interval_value(), "1000");
    }

    #[test]
    fn the_bench_interval_is_within_what_the_player_accepts() {
        // Пределы плеера: от тика часов 250 мс до настоящего шага 10 с — вне их плеер
        // молча оставил бы 10 с, и стенд снова ждал бы по 10 с.
        assert!(PLAYER_SUMMARY_INTERVAL >= Duration::from_millis(250));
        assert!(PLAYER_SUMMARY_INTERVAL <= Duration::from_secs(10));
    }
}
