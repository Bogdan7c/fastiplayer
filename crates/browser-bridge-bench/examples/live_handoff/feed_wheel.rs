//! Листать ленту настоящим колесом после передачи (`--wheel-at <x>,<y> --wheel-after <с>`, 16b2).
//!
//! Живая проверка лент коротких видео (Instagram, TikTok, Shorts): человек отдал ролик и крутит колесо
//! над ним. Помощник делает то же мышью невидимого экрана: подводит курсор в точку и крутит колесо вниз.
//! `--wheel-after` можно повторять — каждое значение даёт один рывок колеса через столько секунд после
//! передачи; `--wheel-notches <n>` — сколько щелчков в рывке. Точку находят по снимку `--screenshots`.
//! Только `--display virtual`: на настоящем экране мышь владельца помощник не трогает.

use std::time::{Duration, Instant};

use browser_bridge_bench::desktop_input::{DesktopInput, Point, WheelDirection};

/// Сколько щелчков колеса в рывке, если `--wheel-notches` не задан: столько человек делает одним
/// движением пальца, и ленте с прилипанием этого хватает, чтобы уйти на следующий ролик.
pub(crate) const DEFAULT_WHEEL_NOTCHES: u32 = 3;

/// Что просили в командной строке: где крутить, когда и по сколько щелчков.
#[derive(Clone)]
pub(crate) struct FeedWheelRequest {
    /// Точка невидимого экрана (`--wheel-at`).
    pub(crate) point: Point,
    /// Через сколько после передачи делать рывки (`--wheel-after`, по порядку командной строки).
    pub(crate) after: Vec<Duration>,
    /// Щелчков в одном рывке (`--wheel-notches`).
    pub(crate) notches: u32,
}

impl FeedWheelRequest {
    /// Собирает просьбу из разобранных опций. Рывки без точки или точка без рывков — ошибка вызова, а не
    /// тихий пропуск: иначе прогон «без прокрутки» выглядел бы как «прокрутка не подействовала».
    pub(crate) fn from_options(
        point: Option<Point>,
        after: Vec<Duration>,
        notches: Option<u32>,
    ) -> Result<Option<Self>, String> {
        match (point, after.is_empty()) {
            (None, true) if notches.is_none() => Ok(None),
            (Some(point), false) => {
                let notches = notches.unwrap_or(DEFAULT_WHEEL_NOTCHES);
                if notches == 0 {
                    return Err("--wheel-notches must be at least 1".to_owned());
                }
                Ok(Some(Self {
                    point,
                    after,
                    notches,
                }))
            }
            _ => Err(
                "--wheel-at, --wheel-after (and optional --wheel-notches) go together".to_owned(),
            ),
        }
    }
}

/// Рывки колеса, которые ещё предстоит сделать после передачи.
pub(crate) struct FeedWheel {
    point: Point,
    notches: u32,
    /// Моменты рывков по возрастанию; сделанные убираются с начала.
    pending: Vec<Instant>,
}

impl FeedWheel {
    /// План от момента передачи.
    pub(crate) fn planned(handed_off_at: Instant, request: &FeedWheelRequest) -> Self {
        let mut pending: Vec<Instant> = request
            .after
            .iter()
            .map(|after| handed_off_at + *after)
            .collect();
        pending.sort();
        Self {
            point: request.point,
            notches: request.notches,
            pending,
        }
    }

    /// Пора — один рывок колеса вниз в точке. Сбой печатается, прогон не рвётся: это помощник человека,
    /// а несостоявшийся рывок виден в выводе. За один вызов делается не больше одного рывка.
    pub(crate) fn poll(&mut self, input: &DesktopInput, now: Instant, handed_off_at: Instant) {
        if self.pending.first().is_none_or(|at| now < *at) {
            return;
        }
        self.pending.remove(0);
        let since = handed_off_at.elapsed().as_secs_f64();
        match input
            .mouse()
            .scroll_at(self.point, WheelDirection::Down, self.notches)
        {
            Ok(()) => println!(
                "[{since:>7.1} s] feed wheel: {} notch(es) down at {},{}",
                self.notches, self.point.x, self.point.y
            ),
            Err(error) => println!("[{since:>7.1} s] feed wheel: failed: {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_asked_is_no_request() {
        assert!(
            FeedWheelRequest::from_options(None, Vec::new(), None)
                .expect("no options is fine")
                .is_none()
        );
    }

    #[test]
    fn a_point_with_bursts_takes_the_default_notches() {
        let request = FeedWheelRequest::from_options(
            Some(Point::new(10, 20)),
            vec![Duration::from_secs(5)],
            None,
        )
        .expect("a full request")
        .expect("a request");
        assert_eq!(request.notches, DEFAULT_WHEEL_NOTCHES);
        assert_eq!(request.after, [Duration::from_secs(5)]);
    }

    #[test]
    fn half_a_request_is_refused() {
        assert!(FeedWheelRequest::from_options(Some(Point::new(1, 1)), Vec::new(), None).is_err());
        assert!(FeedWheelRequest::from_options(None, vec![Duration::from_secs(1)], None).is_err());
        assert!(FeedWheelRequest::from_options(None, Vec::new(), Some(2)).is_err());
        assert!(
            FeedWheelRequest::from_options(
                Some(Point::new(1, 1)),
                vec![Duration::from_secs(1)],
                Some(0)
            )
            .is_err()
        );
    }
}
