//! Строки «края ленты» (сессия 16b3): что настоящие сайты делают помимо спокойного листания, наблюдение
//! без моста 19 ноября 2026 (Firefox 160; Instagram с входом, TikTok и Shorts без входа). Страница —
//! `site/pages/feed-edges.html`, звенья робота — `edge_steps`, приговор — `edge_judge`.
//!
//! | Строка | Что делает страница | Что обязано случиться |
//! |---|---|---|
//! | `feed-burst-*` | после спокойной прокрутки три прокрутки подряд с промежутком ~200 мс, пять роликов | окно кончает на ПОСЛЕДНЕМ ролике, поколения подряд, кадры растут |
//! | `feed-removed-mid-move-*` | прежний элемент удалён сразу после `play()` следующего, посреди переезда | сеанс жив, окно на следующем ролике |
//! | `feed-removed-after-move-*` | элемент, на который переехало окно, удалён через 0,3 с, адрес тот же | сеанс жив, окно идёт к следующему ролику |
//! | `feed-leave-section-*` | уход в другой раздел: адрес, удаление/снятие источника, играющее тихое превью | честный конец «Source tab closed», превью не принято |
//! | `feed-leave-while-waiting` | конец ленты (окно ждёт), потом уход в раздел с играющим превью | честный конец, превью не принято |
//! | `feed-end-waits` | конец ленты без ухода (решение 356) | 15 с ожидания: ни конца, ни нового поколения, окно на месте |

use super::judge::ExpectedWindow;
use super::steps::WheelPlace;
use super::{BeforeScrolling, FeedRow, FeedScenario};

/// Ширины кадра пяти роликов ленты краёв (ассеты `reel-1..5`; 4 и 5 — для быстрой серии).
pub(super) const EDGE_REEL_STREAM_WIDTHS: &[u64] = &[640, 512, 384, 320, 448];

/// Что делает строка краёв.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EdgeScenario {
    /// Спокойная прокрутка и быстрая серия до последнего из пяти роликов.
    BurstToLastReel,
    /// Страница убирает прежний элемент во время переезда окна на ролик 2.
    OldRemovedDuringMove,
    /// Окно переехало на ролик 2, страница через 0,3 с убирает этот элемент (адрес тот же), затем ролик 3.
    CurrentRemovedAfterMove,
    /// Спокойная прокрутка (ролик 2), затем уход в другой раздел сценарием формы.
    LeaveSection {
        /// Метки журнала страницы, которыми она доказывает, что уход был целиком (адрес, удаление, превью).
        page_marks: &'static [&'static str],
    },
    /// Конец ленты (окно ждёт), затем уход в другой раздел с играющим превью.
    LeaveWhileWaiting,
    /// Конец ленты без ухода: окно ждёт.
    WaitAtFeedEnd,
}

/// Окно, которого ждёт приговор для сценария строки.
pub(super) fn expected_window(scenario: EdgeScenario) -> ExpectedWindow {
    match scenario {
        EdgeScenario::BurstToLastReel => ExpectedWindow::EndsOnLastReel {
            reel_stream_widths: EDGE_REEL_STREAM_WIDTHS,
        },
        EdgeScenario::OldRemovedDuringMove => ExpectedWindow::FollowsFeed {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=1],
        },
        EdgeScenario::CurrentRemovedAfterMove => ExpectedWindow::FollowsFeed {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=2],
        },
        EdgeScenario::LeaveSection { .. } => ExpectedWindow::EndsHonestly {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=1],
        },
        EdgeScenario::LeaveWhileWaiting => ExpectedWindow::EndsHonestly {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=2],
        },
        EdgeScenario::WaitAtFeedEnd => ExpectedWindow::WaitsAtFeedEnd {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=2],
        },
    }
}

/// Метки ухода Instagram (I6): адрес, все элементы удалены, пауза уже удалённого, тихий ролик-файл.
const MARKS_LEAVE_MANY: &[&str] = &[
    "leave-url-pushed path=/explore/",
    "leave-elements-removed all=true",
    "leave-paused-after-removal",
    "leave-preview-playing kind=file muted=true visible=true",
];
/// Метки ухода TikTok (T5): адрес, наш элемент удалён и на паузе, второй элемент играет тихое превью.
const MARKS_LEAVE_POOL: &[&str] = &[
    "leave-url-pushed path=/explore",
    "leave-paused-after-removal",
    "leave-preview-playing kind=mse muted=true visible=true",
];
/// Метки ухода Shorts (S4): адрес, перестановка, снятие источника, тихий MSE-ролик.
const MARKS_LEAVE_MOVED: &[&str] = &[
    "leave-url-pushed path=/",
    "leave-element-moved",
    "leave-source-removed",
    "leave-preview-playing kind=mse muted=true visible=true",
];

const fn edge_row(
    step_id: &'static str,
    query_suffix: &'static str,
    scenario: EdgeScenario,
) -> FeedRow {
    FeedRow {
        step_id,
        page_id: "feed-edges",
        query_suffix,
        scenario: FeedScenario::Edge(scenario),
        before_scrolling: BeforeScrolling::Nothing,
    }
}

/// Строки краёв. Каждая — отдельное прогонное окружение (свой плеер и браузер).
pub(super) const EDGE_ROWS: &[FeedRow] = &[
    // Быстрая серия: Instagram (I4), TikTok (T3), Shorts (S3).
    edge_row(
        "feed-burst-many",
        "&mode=handoff&shape=many&reels=5&paging=wheel",
        EdgeScenario::BurstToLastReel,
    ),
    edge_row(
        "feed-burst-pool",
        "&mode=handoff&shape=pool&reels=5&paging=wheel",
        EdgeScenario::BurstToLastReel,
    ),
    edge_row(
        "feed-burst-moved",
        "&mode=handoff&shape=moved&reels=5&paging=wheel",
        EdgeScenario::BurstToLastReel,
    ),
    // Страница убирает элемент посреди переезда окна.
    edge_row(
        "feed-removed-mid-move-many",
        "&mode=handoff&shape=many&quirk=remove-old-during-move",
        EdgeScenario::OldRemovedDuringMove,
    ),
    edge_row(
        "feed-removed-mid-move-pool",
        "&mode=handoff&shape=pool&quirk=remove-old-during-move",
        EdgeScenario::OldRemovedDuringMove,
    ),
    // Страница убирает элемент, на который окно только что переехало (адрес тот же), потом играет следующий.
    edge_row(
        "feed-removed-after-move-many",
        "&mode=handoff&shape=many&reels=3",
        EdgeScenario::CurrentRemovedAfterMove,
    ),
    edge_row(
        "feed-removed-after-move-pool",
        "&mode=handoff&shape=pool&reels=3",
        EdgeScenario::CurrentRemovedAfterMove,
    ),
    // Уход из ленты в другой раздел: честный конец.
    edge_row(
        "feed-leave-section-many",
        "&mode=handoff&shape=many&reels=3",
        EdgeScenario::LeaveSection {
            page_marks: MARKS_LEAVE_MANY,
        },
    ),
    edge_row(
        "feed-leave-section-pool",
        "&mode=handoff&shape=pool&reels=3",
        EdgeScenario::LeaveSection {
            page_marks: MARKS_LEAVE_POOL,
        },
    ),
    edge_row(
        "feed-leave-section-moved",
        "&mode=handoff&shape=moved&reels=3",
        EdgeScenario::LeaveSection {
            page_marks: MARKS_LEAVE_MOVED,
        },
    ),
    // Конец ленты: окно ждёт; уход из раздела ожидания — честный конец.
    edge_row(
        "feed-leave-while-waiting",
        "&mode=handoff&shape=many&reels=3&tail=comments",
        EdgeScenario::LeaveWhileWaiting,
    ),
    edge_row(
        "feed-end-waits",
        "&mode=handoff&shape=many&reels=3&tail=comments",
        EdgeScenario::WaitAtFeedEnd,
    ),
];

/// Где на странице стоит курсор в строках краёв: над роликом, как у человека.
pub(super) const EDGE_WHEEL: WheelPlace = WheelPlace::OverReel;

#[cfg(test)]
mod tests;
