//! Чистые приговоры строк предложения «Ask» (сессия 12b). Как и в [`super::judge`], ввода-вывода здесь
//! нет: каждое нарушение закрепляет юнит-тест. Строка «видео стоит» доказывается не отсутствием кадров
//! вообще, а набором фактов страницы: пауза, часы стоят, браузер почти не декодирует.

use serde_json::Value;

use super::super::capture::max_real_frames;
use super::super::entries::return_hand::{OfferChoice, ViewerOfferView};
use super::facts::WindowFacts;

/// Самое короткое окно, на котором «видео стоит» что-то доказывает, с (как у остальных судей).
const MIN_HELD_WINDOW_S: f64 = 5.0;
/// На сколько секунд может уйти вперёд часы замершего видео, с (дрожь паузы после запуска).
const MAX_HELD_CLOCK_ADVANCE_S: f64 = 0.5;

/// Предложение на экране целиком: панель видна, на ней все четыре кнопки, выбора ещё не было.
pub(super) fn offer_shown_failures(label: &str, view: &ViewerOfferView) -> Vec<String> {
    let missing: Vec<&str> = [
        (OfferChoice::Fastiplayer, "Play in Fastiplayer"),
        (OfferChoice::Here, "Play here"),
        (OfferChoice::Always, "Always for this site"),
        (OfferChoice::Never, "Never for this site"),
    ]
    .into_iter()
    .filter(|(choice, _)| view.button(*choice).is_none())
    .map(|(_, name)| name)
    .collect();
    let mut failures = Vec::new();
    if !view.offered || !view.visible || view.panel.is_none() {
        failures.push(format!(
            "{label}: the offer panel must be on screen, the probe says {view:?}"
        ));
    }
    if !missing.is_empty() {
        failures.push(format!("{label}: no button for {missing:?}"));
    }
    if view.answered {
        failures.push(format!("{label}: the offer is already answered"));
    }
    failures
}

/// Предложения на экране нет (человек выбрал или предложения и не должно было быть).
pub(super) fn offer_absent_failures(label: &str, view: &ViewerOfferView) -> Vec<String> {
    if view.visible || view.panel.is_some() {
        vec![format!(
            "{label}: the offer panel must not be on screen, the probe says {view:?}"
        )]
    } else {
        Vec::new()
    }
}

/// Видео держится под предложением: страница видит паузу, часы стоят, браузер почти не декодирует.
pub(super) fn held_by_offer_failures(facts: &WindowFacts) -> Vec<String> {
    if !facts.probe_available {
        return vec!["the page could not read the real frame counter".to_owned()];
    }
    if facts.window_s < MIN_HELD_WINDOW_S {
        return vec![format!(
            "the window was {:.1} s, too short to judge a frozen video (need {MIN_HELD_WINDOW_S} s)",
            facts.window_s
        )];
    }
    let mut failures = Vec::new();
    if !facts.paused {
        failures.push("the page must see the video paused while the offer hangs".to_owned());
    }
    if facts.clock_advance_s > MAX_HELD_CLOCK_ADVANCE_S {
        failures.push(format!(
            "the page clock advanced {:.2} s while the offer hangs (allowed {MAX_HELD_CLOCK_ADVANCE_S} s)",
            facts.clock_advance_s
        ));
    }
    let allowed = max_real_frames(facts.window_s);
    if (facts.real_frames as f64) > allowed {
        failures.push(format!(
            "the browser decoded {} real frames in {:.1} s while the offer hangs (allowed {allowed:.0})",
            facts.real_frames, facts.window_s
        ));
    }
    failures
}

/// Настройка режима 2 в хранилище равна ожидаемой целиком: правило сайта изменилось ровно так, как
/// обещал выбор, и больше ничего не тронуто.
pub(super) fn stored_settings_failures(
    label: &str,
    expected: &Value,
    stored: &Value,
) -> Vec<String> {
    if stored == expected {
        Vec::new()
    } else {
        vec![format!(
            "{label}: the extension storage holds {stored}, expected {expected}"
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts::PageDuration;
    use super::*;
    use crate::scenarios::entries::return_hand::ViewerOfferView;
    use serde_json::json;

    fn facts(paused: bool, real_frames: u64, clock_advance_s: f64) -> WindowFacts {
        WindowFacts {
            probe_available: true,
            window_s: 8.0,
            real_frames,
            clock_advance_s,
            start_s: 0.0,
            end_s: clock_advance_s,
            paused,
            muted: false,
            ended: false,
            looping: false,
            duration: PageDuration::Finite(60.0),
        }
    }

    #[test]
    fn a_frozen_paused_video_is_held() {
        assert!(held_by_offer_failures(&facts(true, 0, 0.0)).is_empty());
    }

    #[test]
    fn a_video_that_plays_is_not_held() {
        let failures = held_by_offer_failures(&facts(false, 200, 8.0));
        assert_eq!(failures.len(), 3, "{failures:?}");
    }

    #[test]
    fn a_dead_frame_counter_or_a_short_window_proves_nothing() {
        let mut dead = facts(true, 0, 0.0);
        dead.probe_available = false;
        assert_eq!(held_by_offer_failures(&dead).len(), 1);
        let mut short = facts(true, 0, 0.0);
        short.window_s = 2.0;
        assert_eq!(held_by_offer_failures(&short).len(), 1);
    }

    #[test]
    fn an_idle_probe_is_neither_a_shown_offer_nor_a_complete_panel() {
        let idle = ViewerOfferView::default();
        let failures = offer_shown_failures("idle", &idle);
        assert!(
            failures
                .iter()
                .any(|f| f.contains("panel must be on screen"))
        );
        assert!(failures.iter().any(|f| f.contains("Never for this site")));
        assert!(offer_absent_failures("idle", &idle).is_empty());
    }

    #[test]
    fn stored_settings_must_equal_the_expected_ones_exactly() {
        let expected = json!({"enabled": false, "site_rules": {"h": "always"}});
        assert!(stored_settings_failures("x", &expected, &expected.clone()).is_empty());
        let other = json!({"enabled": false, "site_rules": {"h": "ask"}});
        let failures = stored_settings_failures("x", &expected, &other);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("expected"));
        assert_eq!(
            stored_settings_failures("x", &expected, &Value::Null).len(),
            1
        );
    }
}
