//! Окна, на которых судьи стенда верят счётчикам страницы.
//!
//! Страница меряет своими часами окно «под тенью» (настоящие кадры браузера после
//! «handed-off») и окно фильма после рекламы (часы фильма после `film-started`). На коротком
//! окне счётчик ничего не доказывает: и честный браузер, и «пропускающий» тень дают почти
//! ноль кадров, а часы фильма — почти ноль секунд. Пока сводка плеера шла раз в 10 с,
//! ожидание кадров само растягивало оба окна; с коротким шагом сводки (сессия 16b3c2) окно
//! держат явно: окно тени — сам разговор со страницей ([`super::handoff_run::HandoffRun`]
//! не просит итог раньше), окно фильма — сценарии рекламы
//! ([`JudgeWindow::hold_until_judgeable`]).

use std::time::{Duration, Instant};

/// Окно, которое судья требует от страницы, и запас строки сверх него.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct JudgeWindow {
    /// Самое короткое окно, которому верит судья.
    min: Duration,
}

/// Окно «под тенью»: судья счётчика браузера (`judge_capture`).
pub(super) const SHADOW_WINDOW: JudgeWindow = JudgeWindow {
    min: Duration::from_secs(6),
};

/// Окно фильма после рекламы: судья часов фильма (`judge_ad_hop`).
pub(super) const FILM_WINDOW: JudgeWindow = JudgeWindow {
    min: Duration::from_secs(8),
};

/// Запас сверх предела: команда «finish» доходит до страницы не мгновенно, а страница меряет
/// окно своими часами от своей отметки.
const WINDOW_MARGIN: Duration = Duration::from_millis(500);

impl JudgeWindow {
    /// Предел судьи в секундах (страница присылает окно числом секунд).
    pub(super) fn min_seconds(self) -> f64 {
        self.min.as_secs_f64()
    }

    /// Сколько ещё держать окно, если с его начала прошло `elapsed`.
    fn remaining(self, elapsed: Duration) -> Duration {
        (self.min + WINDOW_MARGIN).saturating_sub(elapsed)
    }

    /// Ждёт, пока окно с мига `started_at` станет пригодным для судьи; если строка и так
    /// шла дольше (перемотка, смена качества), не ждёт ничего.
    pub(super) fn hold_until_judgeable(self, started_at: Instant) {
        let remaining = self.remaining(started_at.elapsed());
        if !remaining.is_zero() {
            std::thread::sleep(remaining);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_window_waits_for_the_whole_minimum_with_margin() {
        assert_eq!(
            SHADOW_WINDOW.remaining(Duration::ZERO),
            Duration::from_millis(6_500)
        );
        assert_eq!(
            FILM_WINDOW.remaining(Duration::ZERO),
            Duration::from_millis(8_500)
        );
    }

    #[test]
    fn frames_that_came_early_wait_only_for_the_rest() {
        // Кадры плеера пришли через 2,4 с — держим ещё 4,1 с.
        assert_eq!(
            SHADOW_WINDOW.remaining(Duration::from_millis(2_400)),
            Duration::from_millis(4_100)
        );
    }

    #[test]
    fn a_long_row_waits_for_nothing() {
        assert_eq!(
            FILM_WINDOW.remaining(Duration::from_secs(30)),
            Duration::ZERO
        );
    }

    #[test]
    fn the_judges_see_the_same_minimum_in_seconds() {
        assert_eq!(SHADOW_WINDOW.min_seconds(), 6.0);
        assert_eq!(FILM_WINDOW.min_seconds(), 8.0);
    }
}
