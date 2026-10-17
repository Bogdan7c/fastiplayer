//! Учёт опозданий картинок: нужная картинка кадра ещё не была готова.
//!
//! Поток-рисовальщик работает наперёд, поэтому в штатной игре опозданий нет. Если они есть
//! (редкие — тяжёлая реплика, частые — медленная машина), это диагностика, а не авария: UI не
//! ждёт, кадр показывает последнюю картинку без закончившихся реплик. В лог на уровне `debug`
//! пишется каждое опоздание, на уровне `warn` — только систематическое: слишком много
//! опозданий в скользящем окне кадров (и не чаще раза в окно).

/// Размер окна наблюдения в кадрах с репликами.
const WINDOW_FRAMES: u32 = 240;

/// Сколько опозданий в окне считается систематическим (десятая часть кадров).
const SYSTEMATIC_LATE_FRAMES: u32 = 24;

/// Скользящее окно опозданий.
#[derive(Debug, Default)]
pub(in crate::state::subtitles) struct LatenessMonitor {
    frames_in_window: u32,
    late_in_window: u32,
    total_late: u64,
}

impl LatenessMonitor {
    /// Учитывает кадр с репликами. `true` — окно закрылось и опоздания были систематическими
    /// (вызывающий пишет `warn`).
    pub(in crate::state::subtitles) fn record_frame(&mut self, was_late: bool) -> bool {
        self.frames_in_window += 1;
        if was_late {
            self.late_in_window += 1;
            self.total_late += 1;
        }
        if self.frames_in_window < WINDOW_FRAMES {
            return false;
        }
        let systematic = self.late_in_window >= SYSTEMATIC_LATE_FRAMES;
        self.frames_in_window = 0;
        self.late_in_window = 0;
        systematic
    }

    /// Всего опозданий за жизнь монитора.
    pub(in crate::state::subtitles) fn total_late(&self) -> u64 {
        self.total_late
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rare_lateness_never_warns_and_systematic_lateness_warns_once_per_window() {
        let mut rare = LatenessMonitor::default();
        let warned_rare = (0..WINDOW_FRAMES * 3)
            .map(|frame| rare.record_frame(frame % 100 == 0))
            .filter(|&warned| warned)
            .count();
        assert_eq!(warned_rare, 0);

        let mut systematic = LatenessMonitor::default();
        let warned = (0..WINDOW_FRAMES * 2)
            .map(|frame| systematic.record_frame(frame % 4 == 0))
            .filter(|&warned| warned)
            .count();
        assert_eq!(warned, 2, "one warning per full window");
        assert_eq!(systematic.total_late(), u64::from(WINDOW_FRAMES / 2));
    }
}
