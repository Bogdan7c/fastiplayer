//! Ожидание устоявшейся панели: решение «можно кликать по значку» без снимков и таймеров.
//!
//! Снимки делает [`super::ScreenCapture`], а здесь только разбор их череды. Так правило
//! проверяется тестами без браузера и невидимого экрана.

use super::{IconMatch, ScreenError};

/// Сколько снимков подряд значок обязан стоять на одном месте, чтобы по нему кликать.
const SETTLED_READINGS: usize = 2;

/// Что делать после очередного снимка.
#[derive(Debug)]
pub(super) enum SettleStep {
    /// Значок устоялся: кликать сюда.
    Settled(IconMatch),
    /// Панель ещё двигается: снять ещё раз.
    KeepWatching,
    /// Снимок не даёт ждать дальше: значка нет или снимок не сделан.
    Failed(ScreenError),
}

/// Память о предыдущих снимках.
#[derive(Debug, Default)]
pub(super) struct IconSettling {
    /// Значок прошлого снимка; `None`, если в прошлом снимке одного значка не было.
    previous: Option<IconMatch>,
    /// Сколько снимков подряд значок стоит на месте `previous`.
    agreeing: usize,
    /// Неоднозначность последнего снимка: станет ошибкой, если попытки кончатся на ней.
    last_ambiguity: Option<ScreenError>,
}

impl IconSettling {
    /// Учитывает очередной снимок.
    ///
    /// Два места — не приговор, а признак неустоявшейся панели: закрываемая вкладка
    /// экстеншена ещё показывает ту же иконку в полосе вкладок. Клик наугад при этом всё
    /// равно запрещён: такой снимок обнуляет счёт, и кликнуть можно только после двух чистых.
    pub(super) fn observe(&mut self, reading: Result<IconMatch, ScreenError>) -> SettleStep {
        match reading {
            Ok(found) => {
                let same_place = self
                    .previous
                    .is_some_and(|before| before.top_left == found.top_left);
                self.agreeing = if same_place { self.agreeing + 1 } else { 1 };
                self.previous = Some(found);
                self.last_ambiguity = None;
                if self.agreeing >= SETTLED_READINGS {
                    SettleStep::Settled(found)
                } else {
                    SettleStep::KeepWatching
                }
            }
            Err(ambiguity @ ScreenError::IconAmbiguous { .. }) => {
                self.previous = None;
                self.agreeing = 0;
                self.last_ambiguity = Some(ambiguity);
                SettleStep::KeepWatching
            }
            Err(error) => SettleStep::Failed(error),
        }
    }

    /// Ошибка на случай, когда попытки кончились: стойкая неоднозначность остаётся собой
    /// (со снимком для разбора), а двигающийся значок называется двигающимся.
    pub(super) fn give_up(self) -> ScreenError {
        match self.last_ambiguity {
            Some(ambiguity) => ambiguity,
            None => ScreenError::IconNotSettled {
                last: self.previous.map(|found| found.top_left),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::desktop_input::Point;

    use super::*;

    fn icon_at(x: u32, y: u32) -> IconMatch {
        IconMatch {
            top_left: Point::new(x, y),
            size: 16,
            mean_difference: 0.0,
        }
    }

    fn ambiguous() -> ScreenError {
        ScreenError::IconAmbiguous {
            first: Point::new(1223, 86),
            second: Point::new(1027, 43),
            screenshot: Some(PathBuf::from("screen-00.png")),
        }
    }

    fn settled_at(step: SettleStep) -> Point {
        match step {
            SettleStep::Settled(found) => found.top_left,
            other => panic!("the icon must have settled, got {other:?}"),
        }
    }

    fn assert_keeps_watching(step: SettleStep) {
        assert!(matches!(step, SettleStep::KeepWatching), "got {step:?}");
    }

    #[test]
    fn two_equal_readings_settle_the_icon() {
        let mut settling = IconSettling::default();
        assert_keeps_watching(settling.observe(Ok(icon_at(10, 20))));
        assert_eq!(
            settled_at(settling.observe(Ok(icon_at(10, 20)))),
            Point::new(10, 20)
        );
    }

    #[test]
    fn a_moved_icon_restarts_the_count() {
        let mut settling = IconSettling::default();
        assert_keeps_watching(settling.observe(Ok(icon_at(10, 20))));
        assert_keeps_watching(settling.observe(Ok(icon_at(30, 20))));
        assert_eq!(
            settled_at(settling.observe(Ok(icon_at(30, 20)))),
            Point::new(30, 20)
        );
    }

    /// Случай со стенда: первый снимок поймал закрывающуюся вкладку с той же иконкой.
    #[test]
    fn an_ambiguity_that_goes_away_only_delays_the_click() {
        let mut settling = IconSettling::default();
        assert_keeps_watching(settling.observe(Err(ambiguous())));
        assert_keeps_watching(settling.observe(Ok(icon_at(1223, 86))));
        assert_eq!(
            settled_at(settling.observe(Ok(icon_at(1223, 86)))),
            Point::new(1223, 86)
        );
    }

    /// Неоднозначность между двумя чистыми снимками обнуляет счёт: клика по старому месту нет.
    #[test]
    fn an_ambiguity_between_clean_readings_resets_the_count() {
        let mut settling = IconSettling::default();
        assert_keeps_watching(settling.observe(Ok(icon_at(1223, 86))));
        assert_keeps_watching(settling.observe(Err(ambiguous())));
        assert_keeps_watching(settling.observe(Ok(icon_at(1223, 86))));
    }

    #[test]
    fn a_lasting_ambiguity_keeps_its_own_error_with_the_screenshot() {
        let mut settling = IconSettling::default();
        assert_keeps_watching(settling.observe(Err(ambiguous())));
        assert_keeps_watching(settling.observe(Err(ambiguous())));
        assert!(matches!(
            settling.give_up(),
            ScreenError::IconAmbiguous {
                screenshot: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn an_ambiguity_that_went_away_does_not_outlive_a_clean_reading() {
        let mut settling = IconSettling::default();
        assert_keeps_watching(settling.observe(Err(ambiguous())));
        assert_keeps_watching(settling.observe(Ok(icon_at(5, 6))));
        assert!(matches!(
            settling.give_up(),
            ScreenError::IconNotSettled {
                last: Some(Point { x: 5, y: 6 })
            }
        ));
    }

    #[test]
    fn a_missing_icon_fails_at_once() {
        let mut settling = IconSettling::default();
        let step = settling.observe(Err(ScreenError::IconNotFound {
            closest_difference: 99.0,
            screenshot: None,
        }));
        assert!(matches!(
            step,
            SettleStep::Failed(ScreenError::IconNotFound { .. })
        ));
    }
}
