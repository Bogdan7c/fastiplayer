//! Строка `feed-history-back-pool` (сессия 16b3b, решения 375-376): «Назад» в истории после ухода из ленты.
//! Живое наблюдение (16b3, TikTok): после ухода в «Смотреть» (`/explore`) «Назад» возвращает прежний элемент с
//! ТЕМ ЖЕ MediaSource, и он играет с начала. Страница — `site/pages/feed-edges.html` (`back=history`, хук
//! `history-back`, форма `lib/feed-edge-back.js`), приговор — `history_back_judge`.
//!
//! Ход строки: ролик 1 передан, спокойная прокрутка на ролик 2, уход (сценарий T5 формы `pool`), честный
//! конец (окно стоит на паузе, решение 370), через ~2 с «Назад» в истории: страница возвращает в документ
//! элемент ролика 2, ставит на 0 и играет. Фон подхватывает его новым сеансом в ТОМ ЖЕ окне.

use std::time::{Duration, Instant};

use super::super::super::context::ScenarioError;
use super::super::super::entries::return_hand::ReturnHand;
use super::edge_judge::WindowAfterLeave;
use super::edge_steps::observe_honest_end;
use super::edges::{EDGE_WHEEL, MARKS_LEAVE_POOL};
use super::history_back_judge::{HistoryBackRecord, SESSION_OPENED_MARKER, WINDOW_CREATED_MARKER};
use super::judge::{ExpectedWindow, ScrollRecord};
use super::steps::{
    POLL_STEP, frame_growth_from_now, page_lines, scroll_to_reel, wait_for_page_facts,
};
use super::{BeforeScrolling, FeedRow, FeedScenario};
use crate::observe::{
    STREAM_GENERATION_LOADED_MARKER, STREAM_GENERATION_SHOWN_MARKER,
    STREAM_SURFACE_RECONFIGURED_MARKER,
};
use crate::player_control::PlayerControl;

/// Пауза между честным концом и «Назад» в истории: человек не жмёт кнопку в ту же миллисекунду.
const HISTORY_BACK_DELAY: Duration = Duration::from_secs(2);
/// Сколько ждём второй сеанс (подхват) после «Назад»: запись «откуда ушли» держится 30 с (решение 375).
const PICKUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Метки журнала страницы: «Назад» сработал (`popstate`) и страница позвала `play()` вернувшегося ролика 2.
const HISTORY_BACK_MARKS: &[&str] = &[
    "history-back-popstate",
    "reel-back-play-called n=2 shape=pool element=same",
];
/// Метка журнала страницы: `play()` отклонён.
const PLAY_REJECTED_MARK: &str = "play-rejected";

/// Окно, которого ждёт приговор.
pub(super) const EXPECTED_WINDOW: ExpectedWindow = ExpectedWindow::PicksUpAfterHistoryBack;

/// Строка таблицы. Страница `feed-edges`, форма TikTok, возврат кнопкой «Назад».
pub(super) const HISTORY_BACK_ROWS: &[FeedRow] = &[FeedRow {
    step_id: "feed-history-back-pool",
    page_id: "feed-edges",
    query_suffix: "&mode=handoff&shape=pool&reels=3&back=history",
    scenario: FeedScenario::HistoryBack,
    before_scrolling: BeforeScrolling::Nothing,
}];

/// Что собрал робот в строке: прокрутка на ролик 2 и запись подхвата.
pub(super) struct HistoryBackRun {
    pub scrolls: Vec<ScrollRecord>,
    pub record: HistoryBackRecord,
}

/// Исполняет строку после передачи ролика 1.
pub(super) fn run_history_back(
    hand: &mut ReturnHand<'_, '_>,
    control: &PlayerControl,
) -> Result<HistoryBackRun, ScenarioError> {
    let scroll = scroll_to_reel(hand, 2, EDGE_WHEEL)?;
    hand.run().tell_page("hook:leave-section");
    let left = wait_for_page_facts(hand, MARKS_LEAVE_POOL);
    let end = observe_honest_end(hand, control, WindowAfterLeave::StandsPaused);
    std::thread::sleep(HISTORY_BACK_DELAY);
    hand.run().tell_page("hook:history-back");
    let returned = left.and_then(|()| wait_for_page_facts(hand, HISTORY_BACK_MARKS));
    let picked_up = wait_for_second_session(hand);
    let frame_growth = match &picked_up {
        Ok(()) => frame_growth_from_now(hand),
        Err(message) => Err(format!("not awaited: {message}")),
    };
    let player_lines = hand.run().player_log.snapshot_lines_containing_any(&[
        WINDOW_CREATED_MARKER,
        SESSION_OPENED_MARKER,
        STREAM_GENERATION_LOADED_MARKER,
        STREAM_GENERATION_SHOWN_MARKER,
        STREAM_SURFACE_RECONFIGURED_MARKER,
    ])?;
    Ok(HistoryBackRun {
        scrolls: vec![scroll],
        record: HistoryBackRecord {
            stimulus: returned,
            end,
            player_lines,
            frame_growth,
            play_rejections: page_lines(hand, PLAY_REJECTED_MARK),
        },
    })
}

/// Ждёт, пока плеер откроет второй сеанс (подхват); `Err` — не дождались за срок.
fn wait_for_second_session(hand: &mut ReturnHand<'_, '_>) -> Result<(), String> {
    let deadline = Instant::now() + PICKUP_TIMEOUT;
    loop {
        let opened = hand
            .run()
            .player_log
            .snapshot_lines_containing(SESSION_OPENED_MARKER)
            .map_err(|error| error.to_string())?
            .len();
        if opened >= 2 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the player opened no second session within {PICKUP_TIMEOUT:?} after the history back (still {opened})"
            ));
        }
        std::thread::sleep(POLL_STEP);
    }
}

#[cfg(test)]
mod tests;
