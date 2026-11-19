//! Звенья строк «прокрутка назад» (16b3b): колесо ВВЕРХ после прокрутки вперёд, нагрузка памятью,
//! ожидание показа вернувшегося поколения. Всё, что здесь измеряется, возвращается уликой для чистого
//! приговора (`back_judge`), а не решается тут.

use std::time::{Duration, Instant};

use super::super::super::context::ScenarioError;
use super::super::super::entries::return_hand::ReturnHand;
use super::back::BackScenario;
use super::back_judge::{BackRecord, BackStart, parse_paused_at_ms};
use super::edges::EDGE_WHEEL;
use super::judge::{FrameGrowthCheck, ScrollRecord};
use super::steps::{
    DETACH_PASSED_REELS_AND_PRELOAD_FAR_AHEAD, POLL_STEP, WheelOutcome, frame_growth_from_now,
    loaded_generations, page_lines, run_page_load, scroll_to_reel, shown_stream_widths,
    wait_for_new_generation, wait_for_page_facts, wheel_until_page_logs_toward,
};
use crate::desktop_input::WheelDirection;
use crate::observe::{
    STREAM_GENERATION_SHOWN_MARKER, ShownGeneration, parse_stream_generation_shown,
};
use crate::player_control::PlayerControl;

/// Сколько ролик 1 играет до ухода вперёд: секунда его паузы обязана быть заметно больше нуля, иначе
/// «продолжил с места» неотличимо от «начал с нуля».
const WATCH_FIRST_REEL: Duration = Duration::from_millis(3500);
/// Сколько ждём, пока окно ПОКАЖЕТ вернувшееся поколение. Больше срока ожидания данных у цели в плеере
/// (строка-замер ждёт его целиком) и запаса на подготовку.
const SHOWN_TIMEOUT: Duration = Duration::from_secs(45);
/// Метка журнала страницы: секунда, на которой страница остановила ролик 1.
const PAUSED_MARK: &str = "reel-paused n=1 at_ms=";
/// Метка журнала страницы: страница ПОЗВАЛА `play()` ролика 1 при возврате (раздражитель строки).
const PLAY_CALLED_MARK: &str = "reel-back-play-called n=1";
/// Метка журнала страницы: ролик 1 после возврата ИГРАЕТ (часы элемента пошли).
const PLAYING_MARK: &str = "reel-back-started n=1";
/// Метка журнала страницы: `play()` отклонён.
const PLAY_REJECTED_MARK: &str = "play-rejected";
/// Сколько ждём отклика окна после возврата.
const WINDOW_RESPONDS_TIMEOUT: Duration = Duration::from_secs(5);
/// Сколько дальний возврат ждёт, пока окно покажет ролик 1 (промежуточный ролик мог дать лишнее поколение).
const FAR_SETTLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Ширина кадра ролика 1.
const FIRST_REEL_WIDTH: u64 = 640;

/// Что собрал робот в строке возврата: прокрутка вперёд и запись возврата.
pub(super) struct BackRun {
    pub scrolls: Vec<ScrollRecord>,
    pub record: BackRecord,
}

/// Исполняет сценарий строки возврата после передачи ролика 1.
pub(super) fn run_back(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
    scenario: BackScenario,
) -> Result<BackRun, ScenarioError> {
    std::thread::sleep(WATCH_FIRST_REEL);
    // Вперёд до ролика `forward_reels` (две прокрутки у дальнего возврата, одна у остальных).
    let mut forward_scrolls = Vec::new();
    for target_reel in 2..=scenario.forward_reels() as u32 {
        forward_scrolls.push(scroll_to_reel(hand, target_reel, EDGE_WHEEL)?);
    }
    // Давление памяти приходит ПОСЛЕ переезда окна на ролик 2 и до возврата: ролик 1 при этом остаётся
    // живым в документе.
    let pressure = if scenario.presses_bridge_memory() {
        run_page_load(hand, DETACH_PASSED_REELS_AND_PRELOAD_FAR_AHEAD)
    } else {
        Ok(())
    };
    let scroll_back = scroll_back_to_first_reel(hand)?;
    if scenario.start() == BackStart::MeasuredFar {
        settle_far_return(hand, scenario.forward_reels());
    }
    let stimulus = pressure.and_then(|()| wait_for_page_facts(hand, &[scenario.started_mark()]));
    let paused_at_ms = parse_paused_at_ms(&page_lines(hand, PAUSED_MARK));
    let shown = shown_generations(hand)?;
    Ok(BackRun {
        scrolls: forward_scrolls,
        record: BackRecord {
            stimulus,
            forward_reels: scenario.forward_reels(),
            paused_at_ms,
            scroll_back,
            shown,
            page_saw_playing: !page_lines(hand, PLAYING_MARK).is_empty(),
            window_responds: control
                .wait_until_controllable(WINDOW_RESPONDS_TIMEOUT)
                .map_err(|error| error.to_string()),
            play_rejections: page_lines(hand, PLAY_REJECTED_MARK),
        },
    })
}

/// Дальний возврат: ждёт, пока окно покажет ролик 1 после пути вперёд (на пути назад промежуточный ролик 2
/// мог дать своё поколение). Не дождались за срок — это не ошибка робота: приговор назовёт исход сам.
fn settle_far_return(hand: &mut ReturnHand<'_, '_>, forward_reels: usize) {
    let deadline = Instant::now() + FAR_SETTLE_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(widths) = shown_stream_widths(hand) {
            let mut collapsed: Vec<u64> = Vec::new();
            for width in widths {
                if collapsed.last() != Some(&width) {
                    collapsed.push(width);
                }
            }
            if collapsed.len() > forward_reels && collapsed.last() == Some(&FIRST_REEL_WIDTH) {
                return;
            }
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Поколения, которые окно показало (строки `Browser stream generation shown`), по порядку журнала.
fn shown_generations(hand: &mut ReturnHand<'_, '_>) -> Result<Vec<ShownGeneration>, ScenarioError> {
    let lines = hand
        .run()
        .player_log
        .snapshot_lines_containing(STREAM_GENERATION_SHOWN_MARKER)?;
    Ok(lines
        .iter()
        .filter_map(|line| parse_stream_generation_shown(line))
        .collect())
}

/// Ждёт, пока число показанных поколений станет больше `before`.
fn wait_for_new_shown_generation(
    hand: &mut ReturnHand<'_, '_>,
    before: usize,
) -> Result<(), String> {
    let deadline = Instant::now() + SHOWN_TIMEOUT;
    loop {
        let seen = shown_generations(hand)
            .map_err(|error| error.to_string())?
            .len();
        if seen > before {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the window showed no new generation within {SHOWN_TIMEOUT:?} (still {seen} shown)"
            ));
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// Колесо ВВЕРХ до ролика 1: дошло ли колесо, вернулась ли страница, загрузил и ПОКАЗАЛ ли плеер новое
/// поколение, растут ли кадры.
fn scroll_back_to_first_reel(hand: &mut ReturnHand<'_, '_>) -> Result<ScrollRecord, ScenarioError> {
    let generations_before = loaded_generations(hand)?.len();
    let shown_before = shown_generations(hand)?.len();
    let WheelOutcome {
        trusted_wheel_events,
        reached: page_started_next_reel,
    } = wheel_until_page_logs_toward(hand, PLAY_CALLED_MARK, EDGE_WHEEL, WheelDirection::Up)?;
    let (player_loaded_new_generation, frame_growth) = if page_started_next_reel.is_ok() {
        let shown = wait_for_new_generation(hand, generations_before)
            .and_then(|()| wait_for_new_shown_generation(hand, shown_before));
        let growth = match &shown {
            Ok(()) => frame_growth_from_now(hand),
            Err(_) => Err("not awaited: the window did not show the returned reel".to_owned()),
        };
        (shown, growth)
    } else {
        let skipped = "not awaited: the page did not return to the reel".to_owned();
        (Err(skipped.clone()), Err(skipped))
    };
    Ok(ScrollRecord {
        trusted_wheel_events,
        page_started_next_reel,
        player_loaded_new_generation,
        frame_growth,
        frame_growth_check: FrameGrowthCheck::Required,
    })
}
