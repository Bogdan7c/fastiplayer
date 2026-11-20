//! Строки «края ленты» (сессия 16b3): что настоящие сайты делают помимо спокойного листания, наблюдение
//! без моста 19 ноября 2026 (Firefox 160; Instagram с входом, TikTok и Shorts без входа). Страница —
//! `site/pages/feed-edges.html`, звенья робота — `edge_steps`, приговор — `edge_judge`.
//!
//! | Строка | Что делает страница | Что обязано случиться |
//! |---|---|---|
//! | `feed-burst-*` | после спокойной прокрутки три прокрутки подряд с промежутком ~200 мс, пять роликов | окно кончает на ПОСЛЕДНЕМ ролике, поколения подряд, кадры растут |
//! | `feed-removed-mid-move-*` | прежний элемент удалён сразу после `play()` следующего, посреди переезда | сеанс жив, окно на следующем ролике |
//! | `feed-removed-after-move-*` | элемент, на который переехало окно, удалён через 0,3 с, адрес тот же | сеанс жив, окно идёт к следующему ролику |
//! | `feed-leave-section-*` | уход в другой раздел: адрес, удаление/снятие источника, играющее тихое превью | честный конец «Source tab closed», окно на паузе (решение 370), превью не принято; Shorts (S4) — конец не позже [`SHORTS_LEAVE_END_DEADLINE`] после адреса (решение 381) |
//! | `feed-leave-preview-same-element-moved` | уход Shorts на главную, превью — новый MediaSource на ТОМ ЖЕ элементе внутри отсрочки | то же; поколение превью окно не берёт (16b3c) |
//! | `feed-leave-section-serial` | как S4, но адрес шага — сериал `/serial/123/<n>` (не из списка лент) | прежний конец 306: окно доигрывает и закрывается — сериал лентой не стал (решение 374) |
//! | `feed-shorts-reel-dropped-waits` | Shorts: после шага страница сняла источник ролика, адрес тот же | окно ждёт следующий ролик без предела (решение 382): ни конца, ни нового поколения |
//! | `feed-leave-before-step-*` | ролик отдан, прокруток нет, сразу уход в другой раздел | честный конец, окно на паузе, превью не принято |
//! | `feed-leave-while-waiting` | конец ленты (окно ждёт), потом уход в раздел с играющим превью | честный конец, окно на паузе, превью не принято |
//! | `feed-end-waits` | конец ленты без ухода (решение 356) | 15 с ожидания: ни конца, ни нового поколения, окно на месте |

use std::time::Duration;

use super::edge_judge::{EndAfterAddressLeave, WindowAfterLeave};
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
        /// Каким хуком страница уходит из ленты.
        hook: LeaveHook,
        /// Метки журнала страницы, которыми она доказывает, что уход был целиком (адрес, удаление, превью).
        /// Первая метка — смена адреса: от неё отсчитывается [`EndAfterAddressLeave`].
        page_marks: &'static [&'static str],
        /// Каким окно остаётся после конца: на паузе (решение 370) или доигрывает (сериал, 306).
        window: WindowAfterLeave,
        /// Как скоро после смены адреса окно обязано кончить сеанс.
        end_deadline: EndAfterAddressLeave,
    },
    /// Ролик 1 отдан, прокруток НЕТ вовсе, затем уход в другой раздел сценарием формы (сеанс ещё не делал
    /// шага ленты).
    LeaveBeforeStep {
        /// Метки журнала страницы, которыми она доказывает, что уход был целиком.
        page_marks: &'static [&'static str],
    },
    /// Конец ленты (окно ждёт), затем уход в другой раздел с играющим превью.
    LeaveWhileWaiting,
    /// Конец ленты без ухода: окно ждёт.
    WaitAtFeedEnd,
    /// Shorts (решение 382): спокойная прокрутка, затем страница снимает источник текущего ролика, адрес тот же.
    /// Окно ждёт следующий ролик, как на конце ленты.
    ReelDroppedWaits,
}

/// Каким хуком страница уходит из ленты.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeaveHook {
    /// Сценарий ухода формы (I6, T5, S4): `leave-section`.
    SectionScenario,
    /// Shorts: адрес главной и превью новым MediaSource на том же элементе (16b3c).
    SameElementPreview,
}

impl LeaveHook {
    /// Команда странице.
    pub(super) const fn page_command(self) -> &'static str {
        match self {
            Self::SectionScenario => "hook:leave-section",
            Self::SameElementPreview => "hook:leave-with-same-element-preview",
        }
    }
}

/// Предел от смены адреса до сообщения о конце для ухода из Shorts (решение 381). Фон кончает сеанс по адресу
/// через отсрочку `FEED_SECTION_LEAVE_GRACE_MS` (1 с) плюс путь новости `tabs.onUpdated`, сообщения плееру и
/// опрос журналов роботом; прежний путь через тень — снятый источник (1,45 с) и её допуск 5 с, ~6,5 с. Три
/// секунды — с запасом на занятую машину и всё ещё отделяют один путь от другого.
pub(super) const SHORTS_LEAVE_END_DEADLINE: Duration = Duration::from_secs(3);

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
        EdgeScenario::LeaveSection {
            window,
            end_deadline,
            ..
        } => ExpectedWindow::EndsHonestly {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=1],
            window,
            end_deadline,
        },
        EdgeScenario::LeaveBeforeStep { .. } => ExpectedWindow::EndsHonestly {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=0],
            window: WindowAfterLeave::StandsPaused,
            end_deadline: EndAfterAddressLeave::NotMeasured,
        },
        EdgeScenario::LeaveWhileWaiting => ExpectedWindow::EndsHonestly {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=2],
            window: WindowAfterLeave::StandsPaused,
            end_deadline: EndAfterAddressLeave::NotMeasured,
        },
        EdgeScenario::WaitAtFeedEnd => ExpectedWindow::WaitsAtFeedEnd {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=2],
        },
        // Ролик 2 пропал, окно ждёт на нём: поколения и ширины — роликов 1 и 2.
        EdgeScenario::ReelDroppedWaits => ExpectedWindow::WaitsAtFeedEnd {
            reel_stream_widths: &EDGE_REEL_STREAM_WIDTHS[..=1],
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
pub(super) const MARKS_LEAVE_POOL: &[&str] = &[
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
/// Метки ухода Shorts с превью на том же элементе (16b3c): адрес, новый MediaSource превью, превью играет.
const MARKS_LEAVE_SAME_ELEMENT_PREVIEW: &[&str] = &[
    "leave-url-pushed path=/",
    "leave-same-element-preview n=2",
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
            hook: LeaveHook::SectionScenario,
            page_marks: MARKS_LEAVE_MANY,
            window: WindowAfterLeave::StandsPaused,
            end_deadline: EndAfterAddressLeave::NotMeasured,
        },
    ),
    edge_row(
        "feed-leave-section-pool",
        "&mode=handoff&shape=pool&reels=3",
        EdgeScenario::LeaveSection {
            hook: LeaveHook::SectionScenario,
            page_marks: MARKS_LEAVE_POOL,
            window: WindowAfterLeave::StandsPaused,
            end_deadline: EndAfterAddressLeave::NotMeasured,
        },
    ),
    // Shorts — лента по списку адресов (решение 374): уход по адресу, окно на паузе (381), а не досмотр 306.
    edge_row(
        "feed-leave-section-moved",
        "&mode=handoff&shape=moved&reels=3",
        EdgeScenario::LeaveSection {
            hook: LeaveHook::SectionScenario,
            page_marks: MARKS_LEAVE_MOVED,
            window: WindowAfterLeave::StandsPaused,
            end_deadline: EndAfterAddressLeave::Within(SHORTS_LEAVE_END_DEADLINE),
        },
    ),
    // Превью главной — новое поколение ТОГО ЖЕ элемента Shorts, внутри отсрочки конца: окно его не берёт.
    edge_row(
        "feed-leave-preview-same-element-moved",
        "&mode=handoff&shape=moved&reels=3",
        EdgeScenario::LeaveSection {
            hook: LeaveHook::SameElementPreview,
            page_marks: MARKS_LEAVE_SAME_ELEMENT_PREVIEW,
            window: WindowAfterLeave::StandsPaused,
            end_deadline: EndAfterAddressLeave::Within(SHORTS_LEAVE_END_DEADLINE),
        },
    ),
    // Сериал с номером серии в адресе: то же устройство, что Shorts, но не из списка — прежний конец 306.
    edge_row(
        "feed-leave-section-serial",
        "&mode=handoff&shape=moved&reels=3&address=serial",
        EdgeScenario::LeaveSection {
            hook: LeaveHook::SectionScenario,
            page_marks: MARKS_LEAVE_MOVED,
            window: WindowAfterLeave::PlaysOut,
            end_deadline: EndAfterAddressLeave::NotMeasured,
        },
    ),
    // Shorts: страница сняла источник ролика, адрес в ленте — окно ждёт (решение 382), а не 4009.
    edge_row(
        "feed-shorts-reel-dropped-waits",
        "&mode=handoff&shape=moved&reels=3",
        EdgeScenario::ReelDroppedWaits,
    ),
    // Уход до первого шага ленты (находка: сеанс без единой прокрутки).
    edge_row(
        "feed-leave-before-step-many",
        "&mode=handoff&shape=many&reels=3",
        EdgeScenario::LeaveBeforeStep {
            page_marks: MARKS_LEAVE_MANY,
        },
    ),
    edge_row(
        "feed-leave-before-step-pool",
        "&mode=handoff&shape=pool&reels=3",
        EdgeScenario::LeaveBeforeStep {
            page_marks: MARKS_LEAVE_POOL,
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
