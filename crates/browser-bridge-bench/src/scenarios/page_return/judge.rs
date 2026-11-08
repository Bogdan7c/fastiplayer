//! Чистые судьи сценария `page-return`: на входе только собранные факты, на выходе — список провалов
//! (пусто — прошло). Каждый допуск назван и объяснён здесь, а не размазан по строкам.

use std::time::Duration;

use super::facts::{PlayerEndFacts, ReturnFacts};
use crate::observe::EndNotice;
use crate::scenarios::entries::return_hand::ClientBox;

/// Допуск секунды возврата (задание 11a: «играет с N ± 1 с»), с.
pub(super) const RETURN_POSITION_TOLERANCE_S: f64 = 1.0;
/// Самый большой допустимый скачок часов страницы на возврате, с: тот же допуск, что у секунды.
/// Скачок больше значит, что видео вернулось не туда (чужая шкала, «старая секунда»).
pub(super) const RETURN_MAX_JUMP_S: f64 = 1.0;
/// «Играет сама» за наблюдение (3 с): часы ушли хотя бы на столько, с…
pub(super) const PLAYING_MIN_ADVANCE_S: f64 = 1.5;
/// …и браузер декодировал хотя бы столько настоящих кадров (25 к/с × 3 с ≈ 75; запас на медленную машину).
pub(super) const PLAYING_MIN_FRAMES: u64 = 20;
/// «Стоит на паузе»: за наблюдение часы ушли не дальше этого, с.
pub(super) const PAUSED_MAX_DRIFT_S: f64 = 0.3;
/// Заглушка над видео: допуск каждой стороны, px (округление и дробные пиксели вёрстки).
pub(super) const PANEL_TOLERANCE_PX: f64 = 2.0;
/// Прокрутка страницы обязана сдвинуть видео хотя бы на столько, px (страница крутит на 120 px):
/// иначе проверка «заглушка поехала за видео» ничего не доказывает.
pub(super) const SCROLL_MIN_SHIFT_PX: f64 = 60.0;
/// Сколько раз `paused` может смениться в гонке «Return + закрытие»: один возврат — одна смена
/// (игра → пауза у победившего закрытия, у победившего «Return» — ни одной).
pub(super) const RACE_MAX_PAUSE_FLIPS: u32 = 1;
/// Причина конца в журнале плеера, когда сломался плеер страницы (решение 237).
pub(super) const PAGE_PLAYER_FAILED_CAUSE: &str = "page_player_failed";
/// Сторож экстеншена ждёт 5 с (`PAGE_PLAYER_FAILURE_GRACE_MS`), прежде чем объявить плеер мёртвым;
/// вариант «ошибка, потом снятие источника» ждёт допуск пустого источника (тоже 5 с). Раньше этого
/// срока сообщение — значит, продукт не выдержал допуск. Запас 1 с — на опрос часов тени.
pub(super) const FAILURE_NOTICE_EARLIEST: Duration = Duration::from_secs(4);

/// Чем кончился возврат на странице к концу наблюдения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReturnOutcome {
    /// Браузер снова декодирует сам, часы идут.
    Playing,
    /// Элемент стоит на паузе, часы не идут.
    Paused,
    /// Ни то ни другое (поворота не было, часы идут без кадров, пауза дрейфует…).
    Unsettled,
}

/// Чем кончилось, по фактам страницы.
pub(super) fn classify(facts: &ReturnFacts) -> ReturnOutcome {
    let advance = facts.final_s - facts.turn_s;
    if !facts.turned {
        return ReturnOutcome::Unsettled;
    }
    if !facts.final_paused
        && advance >= PLAYING_MIN_ADVANCE_S
        && facts.settle_frames >= PLAYING_MIN_FRAMES
    {
        ReturnOutcome::Playing
    } else if facts.final_paused && advance.abs() <= PAUSED_MAX_DRIFT_S {
        ReturnOutcome::Paused
    } else {
        ReturnOutcome::Unsettled
    }
}

/// Короткая сводка фактов для отчёта.
pub(super) fn describe(facts: &ReturnFacts) -> String {
    format!(
        "mark {:.2} s -> turn {:.2} s after {:.2} s -> {:.2} s (paused={}), frames {}, flips {}, jump {:.2} s, events play/pause/seeking {}/{}/{}",
        facts.mark_s,
        facts.turn_s,
        facts.elapsed_s,
        facts.final_s,
        facts.final_paused,
        facts.settle_frames,
        facts.paused_flips,
        facts.max_jump_s,
        facts.play_events,
        facts.pause_events,
        facts.seeking_events
    )
}

/// Отклонение секунды поворота от «страница всё это время играла»: часы шли от отметки вместе со
/// стенными, значит, при честном возврате на секунде тени в миг поворота они у `mark + elapsed`.
fn playing_position_error_s(facts: &ReturnFacts) -> f64 {
    facts.turn_s - (facts.mark_s + facts.elapsed_s)
}

/// «Return» (и любой возврат с игрой): страница играет сама дальше со своей секунды N ± 1 с,
/// без скачка и без паузы посередине.
pub(super) fn judge_returned_playing(facts: &ReturnFacts) -> Vec<String> {
    let mut failures = Vec::new();
    let outcome = classify(facts);
    if outcome != ReturnOutcome::Playing {
        failures.push(format!(
            "the page does not play by itself after the return ({outcome:?}): {}",
            describe(facts)
        ));
        return failures;
    }
    failures.extend(playing_position_failures(facts));
    // По выборкам `paused`, а не по событиям: возврат файла перезагружает `src`, и элемент шлёт
    // `pause`/`play`, не меняя видимого состояния (события остаются в сводке).
    if facts.paused_flips != 0 {
        failures.push(format!(
            "the page paused on the way back although Return must keep it playing: {} pause flips",
            facts.paused_flips
        ));
    }
    failures
}

/// Секунда возврата с игрой и отсутствие скачка.
fn playing_position_failures(facts: &ReturnFacts) -> Vec<String> {
    let mut failures = Vec::new();
    let error = playing_position_error_s(facts);
    if error.abs() > RETURN_POSITION_TOLERANCE_S {
        failures.push(format!(
            "the page came back at {:.2} s, expected {:.2} s (mark {:.2} s + {:.2} s) ± {RETURN_POSITION_TOLERANCE_S} s",
            facts.turn_s,
            facts.mark_s + facts.elapsed_s,
            facts.mark_s,
            facts.elapsed_s
        ));
    }
    if facts.max_jump_s > RETURN_MAX_JUMP_S {
        failures.push(format!(
            "the page clock jumped by {:.2} s on the return (limit {RETURN_MAX_JUMP_S} s)",
            facts.max_jump_s
        ));
    }
    failures
}

/// Возврат на паузе (закрытие окна, замена из другой вкладки): элемент стоит в окне
/// `[earliest_s, latest_s]` секунд страницы (± допуск) — там, где он был к мигу конца.
pub(super) fn judge_returned_paused_between(
    facts: &ReturnFacts,
    earliest_s: f64,
    latest_s: f64,
) -> Vec<String> {
    let outcome = classify(facts);
    if outcome != ReturnOutcome::Paused {
        return vec![format!(
            "the page is not paused after the end ({outcome:?}): {}",
            describe(facts)
        )];
    }
    let low = earliest_s - RETURN_POSITION_TOLERANCE_S;
    let high = latest_s + RETURN_POSITION_TOLERANCE_S;
    if facts.final_s < low || facts.final_s > high {
        return vec![format!(
            "the page is paused at {:.2} s, expected between {low:.2} and {high:.2} s",
            facts.final_s
        )];
    }
    Vec::new()
}

/// «Return» и закрытие окна разом: побеждает кто-то один, экстеншен возвращает видео ровно один раз.
/// Возвращает исход и провалы. Смены паузы считаются по выборкам `paused`, а не по событиям: возврат
/// файла перезагружает `src`, и элемент честно шлёт `pause`/`play`, не меняя видимого состояния.
///
/// Страница гонки — с родными кнопками `<video>`: опоздавший клик «Return», пропусти его щит заглушки,
/// родными кнопками переключил бы игру — здесь это видно как лишняя смена паузы (в Firefox такой клик
/// странице даже не виден, остаётся только этот суд).
pub(super) fn judge_race(facts: &ReturnFacts) -> (ReturnOutcome, Vec<String>) {
    let outcome = classify(facts);
    let mut failures = match outcome {
        ReturnOutcome::Playing => playing_position_failures(facts),
        // Закрытие победило: пауза там, где страница была между отметкой и поворотом.
        ReturnOutcome::Paused => {
            judge_returned_paused_between(facts, facts.mark_s, facts.mark_s + facts.elapsed_s)
        }
        ReturnOutcome::Unsettled => vec![format!(
            "the page never settled after Return and the window close at once: {}",
            describe(facts)
        )],
    };
    // Один возврат — одна смена паузы самое большее (игра → пауза у победившего закрытия).
    if facts.paused_flips > RACE_MAX_PAUSE_FLIPS {
        failures.push(format!(
            "the page flapped between play and pause: {} pause flips (one return allows at most {RACE_MAX_PAUSE_FLIPS})",
            facts.paused_flips
        ));
    }
    if outcome == ReturnOutcome::Playing && facts.paused_flips != 0 {
        failures.push(format!(
            "Return won, yet the page paused on the way: {} pause flips",
            facts.paused_flips
        ));
    }
    (outcome, failures)
}

/// Отклонение сторон двух прямоугольников больше допуска (имя стороны и величина).
fn edge_mismatches(panel: ClientBox, video: ClientBox) -> Vec<String> {
    [
        ("left", panel.left, video.left),
        ("top", panel.top, video.top),
        ("width", panel.width, video.width),
        ("height", panel.height, video.height),
    ]
    .into_iter()
    .filter(|(_, panel_edge, video_edge)| (panel_edge - video_edge).abs() > PANEL_TOLERANCE_PX)
    .map(|(name, panel_edge, video_edge)| format!("{name} {panel_edge:.1} vs {video_edge:.1}"))
    .collect()
}

/// Заглушка ровно над видео (± допуск каждой стороны). `when` — имя момента для текста провала.
pub(super) fn judge_panel_over_video(
    when: &str,
    panel: Option<ClientBox>,
    video: Option<ClientBox>,
) -> Vec<String> {
    match (panel, video) {
        (None, _) => vec![format!("{when}: the placeholder panel is not shown")],
        (_, None) => vec![format!("{when}: the page did not report the video rect")],
        (Some(panel), Some(video)) => {
            let mismatches = edge_mismatches(panel, video);
            if mismatches.is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "{when}: the placeholder is not over the video (± {PANEL_TOLERANCE_PX} px): {}",
                    mismatches.join(", ")
                )]
            }
        }
    }
}

/// Прокрутка действительно сдвинула видео (иначе «заглушка поехала» ничего не доказывает).
pub(super) fn judge_scroll_moved_video(
    before: Option<ClientBox>,
    after: Option<ClientBox>,
) -> Vec<String> {
    match (before, after) {
        (Some(before), Some(after)) if before.top - after.top >= SCROLL_MIN_SHIFT_PX => Vec::new(),
        (before, after) => vec![format!(
            "the page scroll did not move the video up by {SCROLL_MIN_SHIFT_PX} px: {before:?} -> {after:?}"
        )],
    }
}

/// Точка внутри прямоугольника (клиентские координаты).
pub(super) fn box_contains(area: ClientBox, x: f64, y: f64) -> bool {
    x >= area.left && x <= area.left + area.width && y >= area.top && y <= area.top + area.height
}

/// Конец «Return»: плеер узнал код 4010 и закрылся молча — без доигрывания, прощания и сообщения.
pub(super) fn judge_silent_viewer_end(player: &PlayerEndFacts) -> Vec<String> {
    let mut failures = Vec::new();
    if !player.ended_by_viewer_return() {
        failures.push(format!(
            "the player did not end the session with 4010 viewer_returned_to_page: {:?}",
            player.ended_lines
        ));
    }
    if !player.silent_end_violations.is_empty() {
        failures.push(format!(
            "the window did not close silently after Return: {}",
            player.silent_end_violations.join(" | ")
        ));
    }
    failures
}

/// Сломанный плеер страницы (решение 237): причина `page_player_failed`, плеер доиграл скачанное,
/// сообщение не раньше допуска сторожа.
pub(super) fn judge_page_player_failed_end(
    notice: &EndNotice,
    after_break: Duration,
    drained: bool,
) -> Vec<String> {
    let mut failures = Vec::new();
    if notice.cause != PAGE_PLAYER_FAILED_CAUSE {
        failures.push(format!(
            "end notice cause={}, expected {PAGE_PLAYER_FAILED_CAUSE}: {}",
            notice.cause,
            notice.line.trim()
        ));
    }
    if after_break < FAILURE_NOTICE_EARLIEST {
        failures.push(format!(
            "the end notice came {:.1} s after the page player broke, before the {:.0} s grace",
            after_break.as_secs_f64(),
            FAILURE_NOTICE_EARLIEST.as_secs_f64()
        ));
    }
    if !drained {
        failures.push(
            "the player did not drain what was downloaded ('Playback drain finished' missing)"
                .to_owned(),
        );
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Честный «Return» с игрой: отметка 12.4 с, поворот через 0.8 с на 13.15 с, дальше игра 3 с.
    fn returned_playing() -> ReturnFacts {
        ReturnFacts {
            turned: true,
            mark_s: 12.4,
            turn_s: 13.15,
            elapsed_s: 0.8,
            final_s: 16.1,
            final_paused: false,
            settle_s: 3.0,
            settle_frames: 74,
            paused_flips: 0,
            max_jump_s: 0.1,
            play_events: 1,
            pause_events: 0,
            seeking_events: 1,
        }
    }

    /// Закрытие окна: пауза на 13.0 с, часы стоят.
    fn returned_paused() -> ReturnFacts {
        ReturnFacts {
            turn_s: 13.0,
            final_s: 13.0,
            final_paused: true,
            settle_frames: 2,
            paused_flips: 1,
            pause_events: 1,
            ..returned_playing()
        }
    }

    fn rect(left: f64, top: f64) -> ClientBox {
        ClientBox {
            left,
            top,
            width: 480.0,
            height: 270.0,
        }
    }

    #[test]
    fn an_honest_return_plays_from_the_page_second() {
        let facts = returned_playing();
        assert_eq!(classify(&facts), ReturnOutcome::Playing);
        assert!(judge_returned_playing(&facts).is_empty());
    }

    #[test]
    fn a_return_to_another_second_fails_even_when_it_plays() {
        // Чужая шкала: видео вернулось на секунду ролика (на 6 с раньше) — играет, но не там.
        let elsewhere = ReturnFacts {
            turn_s: 7.15,
            final_s: 10.1,
            max_jump_s: 6.0,
            ..returned_playing()
        };
        let failures = judge_returned_playing(&elsewhere);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("came back at 7.15")),
            "{failures:?}"
        );
        assert!(
            failures.iter().any(|failure| failure.contains("jumped")),
            "{failures:?}"
        );
        // Ровно на границе допуска ещё честно, чуть дальше — нет.
        let at_edge = ReturnFacts {
            turn_s: 12.4 + 0.8 + 1.0,
            final_s: 17.0,
            ..returned_playing()
        };
        assert!(judge_returned_playing(&at_edge).is_empty());
        let past_edge = ReturnFacts {
            turn_s: 12.4 + 0.8 + 1.05,
            final_s: 17.0,
            ..returned_playing()
        };
        assert!(!judge_returned_playing(&past_edge).is_empty());
    }

    #[test]
    fn a_return_that_left_the_page_paused_or_frozen_is_not_playing() {
        assert!(!judge_returned_playing(&returned_paused()).is_empty());
        // Часы идут, а браузер не декодирует: это всё ещё тень, а не возврат.
        let shadow_still = ReturnFacts {
            settle_frames: 0,
            ..returned_playing()
        };
        assert_eq!(classify(&shadow_still), ReturnOutcome::Unsettled);
        assert!(!judge_returned_playing(&shadow_still).is_empty());
        let never_turned = ReturnFacts {
            turned: false,
            ..returned_playing()
        };
        assert_eq!(classify(&never_turned), ReturnOutcome::Unsettled);
    }

    #[test]
    fn a_pause_on_the_way_back_fails_return() {
        let blinked = ReturnFacts {
            paused_flips: 2,
            pause_events: 1,
            ..returned_playing()
        };
        let failures = judge_returned_playing(&blinked);
        assert!(
            failures
                .iter()
                .any(|failure| failure.contains("paused on the way")),
            "{failures:?}"
        );
    }

    #[test]
    fn a_paused_return_must_stand_inside_its_window() {
        let facts = returned_paused();
        assert!(judge_returned_paused_between(&facts, 12.4, 13.2).is_empty());
        // Стоит на секунде другой вкладки (замена): провал с числами.
        let failures = judge_returned_paused_between(&facts, 20.0, 22.0);
        assert!(
            failures.iter().any(|failure| failure.contains("13.00")),
            "{failures:?}"
        );
        // Играет вместо паузы — тоже провал.
        assert!(!judge_returned_paused_between(&returned_playing(), 12.0, 14.0).is_empty());
        let drifting = ReturnFacts {
            final_s: 14.0,
            ..returned_paused()
        };
        assert_eq!(classify(&drifting), ReturnOutcome::Unsettled);
    }

    #[test]
    fn a_race_ends_either_way_but_never_flaps() {
        let (outcome, failures) = judge_race(&returned_playing());
        assert_eq!(outcome, ReturnOutcome::Playing);
        assert!(failures.is_empty(), "{failures:?}");
        let (outcome, failures) = judge_race(&returned_paused());
        assert_eq!(outcome, ReturnOutcome::Paused);
        assert!(failures.is_empty(), "{failures:?}");
        // Пауза закрытия, потом игра от второго возврата: на странице два возврата — провал.
        let paused_then_played = ReturnFacts {
            paused_flips: 2,
            pause_events: 1,
            ..returned_playing()
        };
        let (outcome, failures) = judge_race(&paused_then_played);
        assert_eq!(outcome, ReturnOutcome::Playing);
        assert!(
            failures.iter().any(|failure| failure.contains("flapped")),
            "{failures:?}"
        );
        // Пауза, игра и снова пауза — тоже.
        let flapping = ReturnFacts {
            paused_flips: 3,
            pause_events: 2,
            ..returned_paused()
        };
        let (_, failures) = judge_race(&flapping);
        assert!(
            failures.iter().any(|failure| failure.contains("flapped")),
            "{failures:?}"
        );
        let (outcome, failures) = judge_race(&ReturnFacts {
            turned: false,
            ..returned_playing()
        });
        assert_eq!(outcome, ReturnOutcome::Unsettled);
        assert!(!failures.is_empty());
    }

    #[test]
    fn the_panel_must_cover_the_video_within_two_pixels() {
        assert!(
            judge_panel_over_video("now", Some(rect(16.0, 60.0)), Some(rect(17.5, 58.5)))
                .is_empty()
        );
        let failures = judge_panel_over_video(
            "after scroll",
            Some(rect(16.0, 60.0)),
            Some(rect(16.0, -60.0)),
        );
        assert!(
            failures[0].contains("after scroll") && failures[0].contains("top"),
            "{failures:?}"
        );
        assert!(!judge_panel_over_video("now", None, Some(rect(0.0, 0.0))).is_empty());
        assert!(!judge_panel_over_video("now", Some(rect(0.0, 0.0)), None).is_empty());
    }

    #[test]
    fn a_scroll_counts_only_when_it_moved_the_video() {
        assert!(
            judge_scroll_moved_video(Some(rect(16.0, 60.0)), Some(rect(16.0, -60.0))).is_empty()
        );
        assert!(
            !judge_scroll_moved_video(Some(rect(16.0, 60.0)), Some(rect(16.0, 30.0))).is_empty()
        );
        assert!(!judge_scroll_moved_video(None, Some(rect(16.0, 0.0))).is_empty());
    }

    #[test]
    fn a_point_inside_and_outside_a_box() {
        let button = ClientBox {
            left: 10.0,
            top: 10.0,
            width: 20.0,
            height: 10.0,
        };
        assert!(box_contains(button, 20.0, 15.0));
        assert!(!box_contains(button, 31.0, 15.0));
        assert!(!box_contains(button, 20.0, 21.0));
    }

    #[test]
    fn a_silent_end_needs_4010_and_no_notice_drain_or_farewell() {
        let silent = PlayerEndFacts {
            ended_lines: vec![
                "Browser session ended reason=ClosedByPage { code: Some(4010) }".to_owned(),
            ],
            ..PlayerEndFacts::default()
        };
        assert!(judge_silent_viewer_end(&silent).is_empty());
        let noisy = PlayerEndFacts {
            silent_end_violations: vec!["Browser session end notice shown notice=x".to_owned()],
            ..silent.clone()
        };
        assert_eq!(judge_silent_viewer_end(&noisy).len(), 1);
        let other_end = PlayerEndFacts {
            ended_lines: vec![
                "Browser session ended reason=ClosedByPage { code: Some(4009) }".to_owned(),
            ],
            ..PlayerEndFacts::default()
        };
        assert_eq!(judge_silent_viewer_end(&other_end).len(), 1);
    }

    fn notice(cause: &str) -> EndNotice {
        EndNotice {
            notice: "finished".into(),
            cause: cause.into(),
            end: "played_to_end".into(),
            line: format!(
                "Browser session end notice shown notice=finished cause={cause} end=played_to_end"
            ),
        }
    }

    #[test]
    fn a_broken_page_player_ends_with_its_own_cause_after_the_grace_and_a_drain() {
        let ok = judge_page_player_failed_end(
            &notice("page_player_failed"),
            Duration::from_secs(12),
            true,
        );
        assert!(ok.is_empty(), "{ok:?}");
        // Брошенный после ошибки источник не должен читаться как «страница забрала видео».
        let took_back = judge_page_player_failed_end(
            &notice("page_took_video_back"),
            Duration::from_secs(12),
            true,
        );
        assert!(
            took_back[0].contains("page_took_video_back"),
            "{took_back:?}"
        );
        let early = judge_page_player_failed_end(
            &notice("page_player_failed"),
            Duration::from_secs(2),
            true,
        );
        assert!(early[0].contains("before the"), "{early:?}");
        let undrained = judge_page_player_failed_end(
            &notice("page_player_failed"),
            Duration::from_secs(12),
            false,
        );
        assert!(undrained[0].contains("drain"), "{undrained:?}");
    }
}
