//! Звенья строк краёв ленты (16b3): быстрая серия настоящего колеса, команды странице (удалить элемент,
//! уйти из ленты), ожидание честного конца и тихого окна. Всё, что здесь измеряется, возвращается уликой
//! для чистого приговора (`edge_judge`), а не решается тут.

use std::time::{Duration, Instant};

use super::super::super::context::ScenarioError;
use super::super::super::entries::return_hand::ReturnHand;
use super::edge_judge::{EdgeRecord, EndAfterAddressLeave, EndRecord, WindowAfterLeave};
use super::edges::{EDGE_REEL_STREAM_WIDTHS, EDGE_WHEEL, EdgeScenario, LeaveHook};
use super::judge::{FrameGrowthCheck, ScrollRecord};
use super::steps::{
    BURST_SETTLE, NOTCHES_PER_BURST, POLL_STEP, RELOCATE_TIMEOUT, frame_growth_from_now,
    loaded_generations, page_lines, scroll_to_reel, scroll_to_reel_then, shown_stream_widths,
    wheel_point, wheel_until_page_logs,
};
use crate::desktop_input::WheelDirection;
use crate::observe::EndNotice;
use crate::player_control::PlayerControl;

/// Сколько прокруток подряд в быстрой серии (после спокойной прокрутки на ролик 2).
const BURST_SCROLLS: u32 = 3;
/// Сколько ещё одиночных рывков разрешено, если серия не домотала до последнего ролика (ролик мог
/// проскочить): серия остаётся быстрой, потолок не даёт листать бесконечно.
const MAX_CATCH_UP_SCROLLS: u32 = 3;
/// Пауза между щелчками колеса внутри прокрутки серии: живая серия — 3-6 прокруток за 0,25-0,5 с (I4),
/// то есть прокрутка занимает ~100 мс, а не 200 мс, как при спокойном колесе (40 мс на щелчок).
const BURST_NOTCH_PAUSE: Duration = Duration::from_millis(20);
/// Пауза между прокрутками серии: вместе с прокруткой даёт промежуток ~160 мс между шагами ленты.
const BURST_SCROLL_GAP: Duration = Duration::from_millis(60);
/// Сколько ждём, что страница дошла до последнего ролика, прежде чем докрутить ещё одним рывком.
const CATCH_UP_WAIT: Duration = Duration::from_millis(1500);
/// Пауза после подвода курсора перед серией: браузер должен увидеть мышь над страницей.
const POINTER_SETTLE: Duration = Duration::from_millis(300);
/// Через сколько после переезда окна страница убирает элемент, на который оно переехало (находка F2).
const REMOVE_AFTER_MOVE: Duration = Duration::from_millis(300);
/// Сколько окно ждёт на конце ленты, пока ничего не происходит (решение 356): дольше любой отсрочки конца.
const FEED_END_QUIET_PERIOD: Duration = Duration::from_secs(15);
/// Сколько окно ждёт в разделе без видео перед уходом из него (отсрочка F5 короче).
const WAIT_BEFORE_LEAVING: Duration = Duration::from_secs(3);
/// Сколько ждём сообщение о конце: честный конец сначала доигрывает скачанное (ролик до 42 с).
const END_NOTICE_TIMEOUT: Duration = Duration::from_secs(90);
/// Сколько ждём закрытия окна после сообщения.
const WINDOW_GONE_TIMEOUT: Duration = Duration::from_secs(60);
/// Сколько после конца ждём ещё: принятое с опозданием превью дало бы лишнюю строку журнала.
const AFTER_END_GRACE: Duration = Duration::from_secs(3);
/// Сколько ждём, что окно после ожидания на конце ленты отвечает.
const WINDOW_PRESENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Что собрал робот в строке краёв: спокойные прокрутки и запись края.
pub(super) struct EdgeRun {
    pub scrolls: Vec<ScrollRecord>,
    pub edge: EdgeRecord,
}

/// Исполняет сценарий строки краёв после передачи ролика 1.
pub(super) fn run_edge(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
    scenario: EdgeScenario,
) -> Result<EdgeRun, ScenarioError> {
    match scenario {
        EdgeScenario::BurstToLastReel => burst_to_last_reel(hand),
        EdgeScenario::OldRemovedDuringMove => old_removed_during_move(hand),
        EdgeScenario::CurrentRemovedAfterMove => current_removed_after_move(hand),
        EdgeScenario::LeaveSection {
            hook,
            page_marks,
            window,
            end_deadline,
        } => leave_section(
            hand,
            control,
            LeavePlan {
                hook,
                page_marks,
                calm_scrolls: 1,
                window,
                end_deadline,
            },
        ),
        EdgeScenario::LeaveBeforeStep { page_marks } => leave_section(
            hand,
            control,
            LeavePlan {
                hook: LeaveHook::SectionScenario,
                page_marks,
                calm_scrolls: 0,
                window: WindowAfterLeave::StandsPaused,
                end_deadline: EndAfterAddressLeave::NotMeasured,
            },
        ),
        EdgeScenario::LeaveWhileWaiting => leave_while_waiting(hand, control),
        EdgeScenario::WaitAtFeedEnd => wait_at_feed_end(hand, control),
        EdgeScenario::ReelDroppedWaits => reel_dropped_waits(hand, control),
    }
}

/// Запись края с одним раздражителем; остальные поля заполняет сценарий.
fn edge_with_stimulus(stimulus: Result<(), String>) -> EdgeRecord {
    EdgeRecord {
        stimulus,
        window_reached_last_reel: None,
        frame_growth: None,
        generations_before_edge: None,
        end: None,
        window_still_present: None,
        end_after_address_leave: None,
    }
}

/// Ждёт в журнале страницы все `marks`; `Err` называет, чего не дождались.
fn page_logged_all(hand: &ReturnHand<'_, '_>, marks: &[&str]) -> Result<(), String> {
    super::steps::wait_for_page_facts(hand, marks)
}

/// Быстрая серия (I4 / T3 / S3): спокойная прокрутка на ролик 2, затем несколько прокруток подряд.
fn burst_to_last_reel(hand: &mut ReturnHand<'_, '_>) -> Result<EdgeRun, ScenarioError> {
    let last_reel = EDGE_REEL_STREAM_WIDTHS.len() as u32;
    let calm = scroll_to_reel(hand, 2, EDGE_WHEEL)?;
    let stimulus = scroll_burst(hand, last_reel)?;
    let window_reached_last_reel = wait_window_shows_reel(hand, last_reel);
    let frame_growth = match &window_reached_last_reel {
        Ok(()) => frame_growth_from_now(hand),
        Err(_) => Err("not awaited: the window never reached the last reel".to_owned()),
    };
    Ok(EdgeRun {
        scrolls: vec![calm],
        edge: EdgeRecord {
            window_reached_last_reel: Some(window_reached_last_reel),
            frame_growth: Some(frame_growth),
            ..edge_with_stimulus(stimulus)
        },
    })
}

/// Настоящее колесо подряд: курсор ставится один раз, дальше `BURST_SCROLLS` прокруток без пауз между
/// ними (кроме пауз между щелчками). Если ролик проскочил, докручиваем одиночными рывками. Раздражитель
/// состоялся, когда страница записала запуск последнего ролика.
fn scroll_burst(
    hand: &mut ReturnHand<'_, '_>,
    last_reel: u32,
) -> Result<Result<(), String>, ScenarioError> {
    let run = hand.run_ref().run.clone();
    let point = wheel_point(hand, &run, EDGE_WHEEL)?;
    hand.move_pointer(point)?;
    std::thread::sleep(POINTER_SETTLE);
    let wheel_before = page_lines(hand, "wheel trusted=true").len();
    for _ in 0..BURST_SCROLLS {
        hand.scroll_wheel_in_place(WheelDirection::Down, NOTCHES_PER_BURST, BURST_NOTCH_PAUSE)?;
        std::thread::sleep(BURST_SCROLL_GAP);
    }
    // Браузер мог докрутить не до последнего ролика (Firefox листает плавно и медленнее): докручиваем
    // одиночными рывками, пока страница не дойдёт до последнего ролика, но не бесконечно.
    let reached_mark = format!("reel-step n={last_reel} ");
    for _ in 0..MAX_CATCH_UP_SCROLLS {
        if hand
            .run_ref()
            .wait_page_log_line(&reached_mark, CATCH_UP_WAIT)
            .is_ok()
        {
            break;
        }
        hand.scroll_wheel_in_place(WheelDirection::Down, NOTCHES_PER_BURST, BURST_NOTCH_PAUSE)?;
    }
    if page_lines(hand, "wheel trusted=true").len() == wheel_before {
        return Ok(Err(
            "no real wheel event reached the page during the burst".to_owned()
        ));
    }
    Ok(hand
        .run_ref()
        .wait_page_log_line(&format!("reel-started n={last_reel}"), BURST_SETTLE)
        .map(|_| ())
        .map_err(|message| format!("the page never started the last reel of the burst: {message}")))
}

/// Ждёт, пока окно покажет ролик номер `reel` (последняя строка «пул поверхностей перестроен» — его ширина).
fn wait_window_shows_reel(hand: &mut ReturnHand<'_, '_>, reel: u32) -> Result<(), String> {
    let wanted = EDGE_REEL_STREAM_WIDTHS[reel as usize - 1];
    let deadline = Instant::now() + RELOCATE_TIMEOUT;
    loop {
        let widths = shown_stream_widths(hand).map_err(|error| error.to_string())?;
        if widths.last() == Some(&wanted) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the window showed widths {widths:?} within {RELOCATE_TIMEOUT:?}, never ending on reel {reel} (width {wanted})"
            ));
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Страница убирает прежний элемент посреди переезда (причуда страницы в адресе): одна прокрутка.
fn old_removed_during_move(hand: &mut ReturnHand<'_, '_>) -> Result<EdgeRun, ScenarioError> {
    let scroll = scroll_to_reel(hand, 2, EDGE_WHEEL)?;
    let stimulus = page_logged_all(hand, &["stimulus old-removed-during-move n=1"]);
    Ok(EdgeRun {
        scrolls: vec![scroll],
        edge: edge_with_stimulus(stimulus),
    })
}

/// Окно переехало на ролик 2; через 0,3 с страница убирает этот элемент (адрес тот же); затем играет ролик 3.
fn current_removed_after_move(hand: &mut ReturnHand<'_, '_>) -> Result<EdgeRun, ScenarioError> {
    let first = scroll_to_reel_then(hand, 2, EDGE_WHEEL, FrameGrowthCheck::NotRequired, |hand| {
        std::thread::sleep(REMOVE_AFTER_MOVE);
        hand.run().tell_page("hook:remove-current-reel");
        Ok(())
    })?;
    let stimulus = page_logged_all(hand, &["stimulus current-reel-removed n=2"]);
    let second = scroll_to_reel(hand, 3, EDGE_WHEEL)?;
    Ok(EdgeRun {
        scrolls: vec![first, second],
        edge: edge_with_stimulus(stimulus),
    })
}

/// Как строка уходит из ленты: хук страницы, её метки, прокрутки до ухода и чего ждать от окна.
struct LeavePlan {
    hook: LeaveHook,
    page_marks: &'static [&'static str],
    /// Спокойных прокруток до ухода: одна (на ролик 2) или ни одной.
    calm_scrolls: usize,
    window: WindowAfterLeave,
    end_deadline: EndAfterAddressLeave,
}

/// Уход из ленты после одной спокойной прокрутки: страница исполняет сценарий формы, окно кончает честно.
fn leave_section(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
    plan: LeavePlan,
) -> Result<EdgeRun, ScenarioError> {
    // Спокойная прокрутка на ролик 2 или (`calm_scrolls == 0`) ни одной.
    let mut scrolls = Vec::new();
    if plan.calm_scrolls > 0 {
        scrolls.push(scroll_to_reel(hand, 2, EDGE_WHEEL)?);
    }
    let generations_before_edge = loaded_generations(hand)?;
    hand.run().tell_page(plan.hook.page_command());
    let (stimulus, end, end_after_address_leave) = match plan.end_deadline {
        EndAfterAddressLeave::NotMeasured => {
            let stimulus = page_logged_all(hand, plan.page_marks);
            (
                stimulus,
                observe_honest_end(hand, control, plan.window),
                None,
            )
        }
        EndAfterAddressLeave::Within(_) => {
            let timed = timed_end_after_address_leave(hand, plan.page_marks);
            let stimulus = page_logged_all(hand, plan.page_marks);
            let end = EndRecord {
                notice: timed.notice,
                window_after_end: window_after_end(control, plan.window),
            };
            (stimulus, end, Some(timed.elapsed))
        }
    };
    Ok(EdgeRun {
        scrolls,
        edge: EdgeRecord {
            generations_before_edge: Some(generations_before_edge),
            end: Some(end),
            end_after_address_leave,
            ..edge_with_stimulus(stimulus)
        },
    })
}

/// Сообщение о конце и сколько оно шло от смены адреса страницей.
struct TimedEnd {
    notice: Result<EndNotice, String>,
    elapsed: Result<Duration, String>,
}

/// Засекает смену адреса (первая метка ухода) и ждёт сообщение о конце СРАЗУ, не дожидаясь остальных меток:
/// превью страница пишет позже (~1,9 с), а конец по адресу приходит раньше. Оба мига — когда робот увидел
/// строку журнала, поэтому погрешность — шаг опроса журналов в обе стороны: поздно увиденный адрес укорачивает
/// промежуток, поздно увиденный конец удлиняет. Предел строки с запасом много больше шага опроса.
fn timed_end_after_address_leave(hand: &mut ReturnHand<'_, '_>, page_marks: &[&str]) -> TimedEnd {
    let Some(address_mark) = page_marks.first() else {
        let missing = "the row has no address mark to time the end from".to_owned();
        return TimedEnd {
            notice: Err(missing.clone()),
            elapsed: Err(missing),
        };
    };
    let address_left_at = page_logged_all(hand, &[address_mark]).map(|()| Instant::now());
    let notice = wait_end_notice(hand);
    let elapsed = match (&address_left_at, &notice) {
        (Ok(at), Ok(_)) => Ok(at.elapsed()),
        (Err(message), _) => Err(message.clone()),
        (Ok(_), Err(message)) => Err(message.clone()),
    };
    TimedEnd { notice, elapsed }
}

/// Shorts (решение 382): спокойная прокрутка на ролик 2, страница снимает источник ролика без смены адреса,
/// окно всё `FEED_END_QUIET_PERIOD` ждёт (дольше допуска тени 5 с: прежний код за это время кончил бы 4009).
fn reel_dropped_waits(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
) -> Result<EdgeRun, ScenarioError> {
    let scroll = scroll_to_reel(hand, 2, EDGE_WHEEL)?;
    let generations_before_edge = loaded_generations(hand)?;
    hand.run().tell_page("hook:drop-current-source");
    let stimulus = page_logged_all(hand, &["stimulus current-source-dropped n=2"]);
    std::thread::sleep(FEED_END_QUIET_PERIOD);
    let window_still_present = control
        .wait_until_controllable(WINDOW_PRESENT_TIMEOUT)
        .map_err(|error| error.to_string());
    Ok(EdgeRun {
        scrolls: vec![scroll],
        edge: EdgeRecord {
            generations_before_edge: Some(generations_before_edge),
            window_still_present: Some(window_still_present),
            ..edge_with_stimulus(stimulus)
        },
    })
}

/// Колесом до конца ленты: ролики 2 и 3, затем раздел без видео. Возвращает спокойные прокрутки и
/// раздражитель «страница ушла за последний ролик и остановила его».
fn scroll_to_feed_end(
    hand: &mut ReturnHand<'_, '_>,
) -> Result<(Vec<ScrollRecord>, Result<(), String>), ScenarioError> {
    let second = scroll_to_reel(hand, 2, EDGE_WHEEL)?;
    let third = scroll_to_reel(hand, 3, EDGE_WHEEL)?;
    let tail = wheel_until_page_logs(hand, "feed-tail-reached", EDGE_WHEEL)?;
    let stimulus = tail.reached.and_then(|()| {
        if tail.trusted_wheel_events == 0 {
            return Err(
                "no real wheel event reached the page on the way to the feed end".to_owned(),
            );
        }
        page_logged_all(hand, &["tail-reel-paused n=3"])
    });
    Ok((vec![second, third], stimulus))
}

/// Конец ленты, окно ждёт (отсрочка), потом уход в раздел с играющим превью (F5): честный конец.
fn leave_while_waiting(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
) -> Result<EdgeRun, ScenarioError> {
    let (scrolls, tail_stimulus) = scroll_to_feed_end(hand)?;
    std::thread::sleep(WAIT_BEFORE_LEAVING);
    let generations_before_edge = loaded_generations(hand)?;
    hand.run().tell_page("hook:leave-from-tail");
    let stimulus = tail_stimulus.and_then(|()| {
        page_logged_all(
            hand,
            &[
                "leave-url-pushed path=/explore/",
                "leave-preview-playing kind=file muted=true visible=true",
            ],
        )
    });
    let end = observe_honest_end(hand, control, WindowAfterLeave::StandsPaused);
    Ok(EdgeRun {
        scrolls,
        edge: EdgeRecord {
            generations_before_edge: Some(generations_before_edge),
            end: Some(end),
            ..edge_with_stimulus(stimulus)
        },
    })
}

/// Конец ленты без ухода: окно ждёт всё `FEED_END_QUIET_PERIOD`, затем проверяем, что оно на месте.
fn wait_at_feed_end(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
) -> Result<EdgeRun, ScenarioError> {
    let (scrolls, stimulus) = scroll_to_feed_end(hand)?;
    let generations_before_edge = loaded_generations(hand)?;
    std::thread::sleep(FEED_END_QUIET_PERIOD);
    let window_still_present = control
        .wait_until_controllable(WINDOW_PRESENT_TIMEOUT)
        .map_err(|error| error.to_string());
    Ok(EdgeRun {
        scrolls,
        edge: EdgeRecord {
            generations_before_edge: Some(generations_before_edge),
            window_still_present: Some(window_still_present),
            ..edge_with_stimulus(stimulus)
        },
    })
}

/// Честный конец: сообщение окна о конце сеанса, затем окно — на месте после короткого ожидания (решение 370:
/// стоит на паузе; за это же ожидание видно, не примет ли окно превью с опозданием) или закрылось (доиграло).
pub(super) fn observe_honest_end(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
    window: WindowAfterLeave,
) -> EndRecord {
    let notice = wait_end_notice(hand);
    EndRecord {
        notice,
        window_after_end: window_after_end(control, window),
    }
}

/// Сообщение окна о конце сеанса (журнал плеера).
fn wait_end_notice(hand: &mut ReturnHand<'_, '_>) -> Result<EndNotice, String> {
    hand.run()
        .player_log
        .wait_for_end_notice(END_NOTICE_TIMEOUT)
        .map_err(|error| format!("no end notice from the player: {error}"))
}

/// Окно после сообщения о конце: на месте после короткого ожидания или закрылось ([`WindowAfterLeave`]).
fn window_after_end(control: &PlayerControl, window: WindowAfterLeave) -> Result<(), String> {
    match window {
        WindowAfterLeave::StandsPaused => {
            std::thread::sleep(AFTER_END_GRACE);
            control
                .wait_until_controllable(WINDOW_PRESENT_TIMEOUT)
                .map_err(|error| format!("the window should stand paused, but it is gone: {error}"))
        }
        WindowAfterLeave::PlaysOut => {
            let closed = control
                .wait_until_gone(WINDOW_GONE_TIMEOUT)
                .map_err(|error| {
                    format!("the window should play out and close, but it stayed: {error}")
                });
            std::thread::sleep(AFTER_END_GRACE);
            closed
        }
    }
}
