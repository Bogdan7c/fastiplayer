//! Строки «прокрутка ленты назад» (сессия 16b3b). Наблюдение живых лент (16b3): назад почти всегда новый
//! элемент с новым MediaSource и с нуля; если элемент Instagram ещё жив (назад сразу после вперёд), он
//! продолжает с секунды, где страница поставила его на паузу. Страница — `site/pages/feed-edges.html`
//! (параметр адреса `back`, формы возврата — `site/lib/feed-edge-back.js`), звенья робота — `back_steps`,
//! приговор — `back_judge`.
//!
//! | Строка | Что делает страница при колесе ВВЕРХ | Что обязано случиться |
//! |---|---|---|
//! | `feed-back-live-many` | ролик 2 на паузу, адрес ролика 1, `play()` ТОГО ЖЕ элемента ролика 1 | поколения `1, 2, 3`, ширины `640, 512, 640`, поколение 3 идёт с секунды паузы страницы (заметно больше 0) |
//! | `feed-back-live-many-pressure` | то же, но страница перед этим качает дальние ролики сверх потолка памяти моста (64 МиБ) | замер решения 369: секунда старта, ожидание данных, исход `immediate / waited / never`; проваливает только `never` |
//! | `feed-back-live-many-far` | назад на ДВА ролика под тем же давлением: 1 → 2 → 3, давление, затем колесом вверх к ролику 1 | замер решения 378: проходит `immediate` и `played_in_browser` (окно ролик не взяло, сеанс жив, страница играет его сама); `waited` без предела и `never` проваливают |
//! | `feed-back-recreated-many` | элемент ролика 1 удалён, новый `<video>` с новым MediaSource играет ролик 1 с 0 | то же, но поколение 3 идёт с начала (меньше секунды) |
//! | `feed-back-recreated-pool` | TikTok: заготовленный элемент пула получает новый MediaSource ролика 1, играет с 0 | как `feed-back-recreated-many` |

use super::back_judge::BackStart;
use super::judge::ExpectedWindow;
use super::{BeforeScrolling, FeedRow, FeedScenario};

/// Ширины кадра при возврате: ролик 1, ролик 2 и снова ролик 1 (ассеты `reel-1`, `reel-2`).
pub(super) const BACK_REEL_STREAM_WIDTHS: &[u64] = &[640, 512, 640];

/// Ширины кадра дальнего возврата: ролики 1, 2, 3 вперёд и снова ролик 1 (промежуточный ролик 2 на пути назад
/// мог дать лишнее поколение, поэтому приговор дальнего замера порядок не требует строго).
pub(super) const BACK_FAR_REEL_STREAM_WIDTHS: &[u64] = &[640, 512, 384, 640];

/// Что делает строка с прокруткой назад.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackScenario {
    /// Instagram: возврат к ЖИВОМУ элементу, он продолжает со своей секунды.
    LiveElement,
    /// То же, но перед возвратом страница качает дальние ролики сверх памяти моста (замер решения 369).
    LiveElementUnderMemoryPressure,
    /// Назад на ДВА ролика под давлением памяти (замер решения 378).
    LiveElementFarUnderMemoryPressure,
    /// Страница пересоздаёт ролик 1: новый MediaSource, с нуля.
    RecreatedElement,
}

impl BackScenario {
    /// С какой секунды обязано идти вернувшееся поколение.
    pub(super) fn start(self) -> BackStart {
        match self {
            Self::LiveElement => BackStart::NearPausedSecond,
            Self::LiveElementUnderMemoryPressure => BackStart::Measured,
            Self::LiveElementFarUnderMemoryPressure => BackStart::MeasuredFar,
            Self::RecreatedElement => BackStart::FromTheBeginning,
        }
    }

    /// Качает ли страница память сверх потолка моста между прокрутками.
    pub(super) fn presses_bridge_memory(self) -> bool {
        matches!(
            self,
            Self::LiveElementUnderMemoryPressure | Self::LiveElementFarUnderMemoryPressure
        )
    }

    /// Сколько роликов лента проходит вперёд до возврата (ролик 1 уже на экране): ширины кадра вперёд.
    pub(super) fn forward_reels(self) -> usize {
        match self {
            Self::LiveElementFarUnderMemoryPressure => 3,
            _ => 2,
        }
    }

    /// Метка журнала страницы, которой она доказывает, что ПОЗВАЛА `play()` при возврате и каким видом (тот
    /// же элемент или новый). Это раздражитель строки; «ролик заиграл» (`reel-back-started`) — исход.
    pub(super) fn started_mark(self) -> &'static str {
        match self {
            Self::LiveElement
            | Self::LiveElementUnderMemoryPressure
            | Self::LiveElementFarUnderMemoryPressure => {
                "reel-back-play-called n=1 shape=many element=same"
            }
            Self::RecreatedElement => "reel-back-play-called n=1 shape=",
        }
    }
}

/// Окно, которого ждёт приговор для сценария строки.
pub(super) fn expected_window(scenario: BackScenario) -> ExpectedWindow {
    ExpectedWindow::ReturnsToEarlierReel {
        reel_stream_widths: match scenario {
            BackScenario::LiveElementFarUnderMemoryPressure => BACK_FAR_REEL_STREAM_WIDTHS,
            _ => BACK_REEL_STREAM_WIDTHS,
        },
        start: scenario.start(),
    }
}

const fn back_row(
    step_id: &'static str,
    query_suffix: &'static str,
    scenario: BackScenario,
) -> FeedRow {
    FeedRow {
        step_id,
        page_id: "feed-edges",
        query_suffix,
        scenario: FeedScenario::Back(scenario),
        before_scrolling: BeforeScrolling::Nothing,
    }
}

/// Строки возврата. Каждая — отдельное прогонное окружение (свой плеер и браузер).
pub(super) const BACK_ROWS: &[FeedRow] = &[
    back_row(
        "feed-back-live-many",
        "&mode=handoff&shape=many&reels=3&back=live",
        BackScenario::LiveElement,
    ),
    back_row(
        "feed-back-live-many-pressure",
        "&mode=handoff&shape=many&reels=3&back=live&history=passed-reels-detached",
        BackScenario::LiveElementUnderMemoryPressure,
    ),
    back_row(
        "feed-back-live-many-far",
        "&mode=handoff&shape=many&reels=3&back=live&history=passed-reels-detached",
        BackScenario::LiveElementFarUnderMemoryPressure,
    ),
    back_row(
        "feed-back-recreated-many",
        "&mode=handoff&shape=many&reels=3&back=recreated",
        BackScenario::RecreatedElement,
    ),
    back_row(
        "feed-back-recreated-pool",
        "&mode=handoff&shape=pool&reels=3&back=recreated",
        BackScenario::RecreatedElement,
    ),
];

#[cfg(test)]
mod tests;
