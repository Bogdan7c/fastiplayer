//! Клик зрителя главнее самовольного возврата страницы (решение владельца 149, 04d).
//!
//! **Находка стенда.** После смены качества YouTube сам возвращает себя на позицию до
//! смены: зритель кликнул в X, пока открывался новый `MediaSource`, а страница потом
//! прислала СВОЮ перемотку (`seek_started{page_seek}`) на старое место Y. По правилу
//! «последняя побеждает» (решение 146) клик терялся — в 5 раундах из 6.
//!
//! **Правило.** Если зритель кликнул во время смены поколения, а страница потом сама
//! перемоталась в ДРУГОЕ место (не эхо клика в допуске совпадения источника), плеер
//! один раз повторяет клик странице — когда новое поколение уже загружено и показано
//! (у страницы новый `MediaSource` готов, наша перемотка больше не теряется в его
//! загрузке). Не подчинилась и со второго раза — остаёмся там, где страница, без петель.
//! На один такой клик странице уходит не больше двух `seek`.
//!
//! ```text
//!  клик X во время смены ─► Armed{X}
//!     page_seek ≈ X (эхо) ──────────────► Armed{X}          (страница подчинилась)
//!     page_seek Y ≠ X ──────────────────► RepeatOwed{X}     (страницу пока слушаем)
//!  поколение показано:
//!     Armed{X} ─► окно подтверждения [`VIEWER_WILL_CONFIRMATION_WINDOW`] от показа;
//!                 page_seek Y в окне ─► повтор X сразу; окно прошло ─► претензии нет
//!     RepeatOwed{X} ─► повтор X (странице seek X + плеер к X), претензия израсходована
//!  page_seek после повтора ─► следуем за страницей (третьего seek нет)
//! ```
//!
//! **Кто чем владеет.** Состояние претензии — [`ViewerClickClaim`] внутри воли зрителя
//! (`ViewerWill`, правило эха 04a): это та же «неподтверждённая воля зрителя», только у
//! клика окно подтверждения открывается с показа нового поколения, а не с клика (до
//! показа страница его подтвердить не может — её `MediaSource` ещё грузится). Сообщения
//! странице и команды плееру — у `SessionMedia` (методы внизу); модуль решает, а не шлёт.
//!
//! **Инварианты.**
//! 1. Претензию даёт только клик во время смены поколения (ждущее поколение есть).
//!    Новое действие зрителя заменяет претензию; клик без смены поколения её снимает.
//! 2. Повтор — не больше одного на клик; после повтора своя перемотка страницы
//!    исполняется как обычно.
//! 3. Эхо клика (перемотка страницы рядом с X) — не возврат: претензия остаётся до
//!    конца окна, но странице ничего не шлётся.
//! 4. Повтор шлётся странице всегда (даже если X уже на полке): страница сама ушла на
//!    Y, и без команды её элемент так и остался бы там. Источник при этом помечает X,
//!    поэтому перемотка плеера к X второй просьбы не ставит.

use std::time::{Duration, Instant};

use pushed_media_source::PageSeekRequest;
use tracing::{debug, info};

use super::SessionMedia;
use super::generation_feed::LoadedFeed;
use super::generation_placement::PendingAim;
use super::viewer_transport::{LoadedSeek, VIEWER_WILL_CONFIRMATION_WINDOW};

/// Состояние претензии клика зрителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ClaimState {
    /// Претензии нет.
    #[default]
    None,
    /// Зритель кликнул во время смены поколения; страница клику пока не перечила.
    Armed {
        /// Цель клика.
        target: Duration,
        /// Окно подтверждения кончается здесь (`None` — поколение ещё не показано).
        confirm_until: Option<Instant>,
    },
    /// Страница сама ушла в другое место: клик повторить, когда поколение покажется.
    RepeatOwed {
        /// Цель клика.
        target: Duration,
    },
}

/// Что своя перемотка страницы значит для претензии клика.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageSeekAgainstClick {
    /// Претензии нет: обычная перемотка страницы.
    Unclaimed,
    /// Эхо клика (в допуске): страница подчинилась.
    EchoOfClick,
    /// Страница ушла в другое место: клик повторить, когда можно (`target` — цель клика).
    Contradicts {
        /// Цель клика.
        target: Duration,
    },
}

/// Претензия клика зрителя (правило — в описании модуля).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct ViewerClickClaim {
    state: ClaimState,
}

impl ViewerClickClaim {
    /// Зритель кликнул в `target` во время смены поколения: новая претензия (прежняя
    /// заменяется — каждое действие зрителя новое).
    pub(super) fn arm(&mut self, target: Duration) {
        self.state = ClaimState::Armed {
            target,
            confirm_until: None,
        };
    }

    /// Претензии больше нет (клик без смены поколения, новая страница).
    pub(super) fn forget(&mut self) {
        self.state = ClaimState::None;
    }

    /// Своя перемотка страницы к `page_target`; `is_same_target` — допуск совпадения
    /// источника.
    pub(super) fn judge_page_seek(
        &mut self,
        page_target: Duration,
        is_same_target: impl FnOnce(Duration, Duration) -> bool,
    ) -> PageSeekAgainstClick {
        match self.state {
            ClaimState::None => PageSeekAgainstClick::Unclaimed,
            ClaimState::Armed { target, .. } | ClaimState::RepeatOwed { target } => {
                if is_same_target(target, page_target) {
                    // Страница подчинилась (пусть и не сразу): долга повтора нет. Уже
                    // идущее окно подтверждения (поколение показано) эхо НЕ сбрасывает:
                    // иначе его никто не открыл бы снова (показ бывает раз на поколение),
                    // претензия жила бы вечно, и минуты спустя любая своя перемотка
                    // страницы (глава, реклама) тащила бы её назад к старому клику.
                    // Долг повтора бывает только до показа — тогда окно откроет показ.
                    let confirm_until = match self.state {
                        ClaimState::Armed { confirm_until, .. } => confirm_until,
                        ClaimState::RepeatOwed { .. } | ClaimState::None => None,
                    };
                    self.state = ClaimState::Armed {
                        target,
                        confirm_until,
                    };
                    return PageSeekAgainstClick::EchoOfClick;
                }
                self.state = ClaimState::RepeatOwed { target };
                PageSeekAgainstClick::Contradicts { target }
            }
        }
    }

    /// Новое поколение показано (страница готова): долг повтора возвращается и
    /// гасится; у взведённой претензии с этого момента идёт окно подтверждения.
    pub(super) fn take_repeat_when_page_ready(&mut self, now: Instant) -> Option<Duration> {
        match self.state {
            ClaimState::RepeatOwed { target } => {
                self.state = ClaimState::None;
                Some(target)
            }
            ClaimState::Armed {
                target,
                confirm_until: None,
            } => {
                self.state = ClaimState::Armed {
                    target,
                    confirm_until: Some(now + VIEWER_WILL_CONFIRMATION_WINDOW),
                };
                None
            }
            ClaimState::Armed { .. } | ClaimState::None => None,
        }
    }

    /// Долг повтора, который можно исполнить сразу (поколение уже показано): гасится.
    pub(super) fn take_owed_repeat(&mut self) -> Option<Duration> {
        match self.state {
            ClaimState::RepeatOwed { target } => {
                self.state = ClaimState::None;
                Some(target)
            }
            ClaimState::Armed { .. } | ClaimState::None => None,
        }
    }

    /// Окно подтверждения после показа прошло — страница клик не оспорила, претензии нет.
    pub(super) fn expire(&mut self, now: Instant) {
        if let ClaimState::Armed {
            confirm_until: Some(until),
            ..
        } = self.state
            && until <= now
        {
            self.state = ClaimState::None;
        }
    }
}

impl SessionMedia {
    /// Своя перемотка страницы (`seek_started{page_seek}`): ждущему поколению — его
    /// цель (ворота 04d), загруженному — перемотка плеера; если она перечит клику
    /// зрителя во время смены поколения — клик повторяется один раз (решение 149).
    pub(super) fn follow_page_seek(&mut self, target: Duration) {
        let page_seek = self.judge_page_seek_against_click(target);
        // Переходник сообщил перемотку источнику САМОГО НОВОГО поколения: пока оно не
        // отдано плееру, это его цель — данные страницы будут там. Повтор клика — когда
        // поколение покажется (`seek_after_show_with_viewer_click`).
        if self.aim_pending_at_page_seek(target) == PendingAim::AimedPending {
            return;
        }
        if let PageSeekAgainstClick::Contradicts { .. } = page_seek
            && self.loaded_generation_is_shown()
            && let Some(click) = self.viewer_will.click_claim().take_owed_repeat()
        {
            // Поколение уже показано: повтор сразу, за страницей на её место не идём.
            self.repeat_viewer_click(click);
            return;
        }
        // Плеер ещё ставит поколение — перемотка ждёт показа (последняя побеждает).
        if self.seek_loaded_generation(target) == LoadedSeek::NothingLoaded {
            debug!("Page seek before the stream opened; the player starts where the bytes start");
        }
    }

    /// Клик зрителя во время смены поколения: претензия (повтор, если страница
    /// вернётся сама). Клик без смены поколения — новое действие без претензии.
    pub(super) fn note_viewer_click(&mut self, target: Duration, aim: PendingAim) {
        let claim = self.viewer_will.click_claim();
        match aim {
            PendingAim::AimedPending => claim.arm(target),
            PendingAim::NoPendingGeneration => claim.forget(),
        }
    }

    /// Перемотка после показа нового поколения: долг повтора клика зрителя побеждает
    /// перемотку, отложенную до показа (это была перемотка страницы, которую клик
    /// оспаривает). Без долга — как было.
    pub(super) fn seek_after_show_with_viewer_click(
        &mut self,
        seek_when_shown: Option<Duration>,
        now: Instant,
    ) -> Option<Duration> {
        let Some(click) = self
            .viewer_will
            .click_claim()
            .take_repeat_when_page_ready(now)
        else {
            return seek_when_shown;
        };
        self.ask_page_to_repeat_click(click);
        Some(click)
    }

    /// Повтор клика сейчас: странице `seek` X, плеер — к X.
    fn repeat_viewer_click(&mut self, click: Duration) {
        self.ask_page_to_repeat_click(click);
        self.seek_loaded_generation(click);
    }

    /// Странице — повтор клика (одна просьба); источник помечает цель, чтобы перемотка
    /// плеера к ней вторую просьбу не поставила.
    fn ask_page_to_repeat_click(&mut self, click: Duration) {
        if self.is_draining() || self.current_session.is_none() {
            return;
        }
        if let Some(LoadedFeed::Segments { monitor, .. }) =
            self.loaded.as_ref().map(|loaded| &loaded.feed)
        {
            monitor.note_page_seek_requested(click);
        }
        let seek_id = self
            .outbound
            .request_page_seek(PageSeekRequest { target: click });
        info!(
            seek_id = seek_id.0,
            "The page returned by itself after a viewer click during a stream switch; repeating the click once"
        );
    }

    /// Своя перемотка страницы против претензии клика (допуск — источника поколения,
    /// которому перемотка достаётся).
    fn judge_page_seek_against_click(&mut self, target: Duration) -> PageSeekAgainstClick {
        let monitor = self
            .pending
            .as_ref()
            .and_then(|pending| pending.feed.segments_monitor().cloned())
            .or_else(|| match self.loaded.as_ref().map(|loaded| &loaded.feed) {
                Some(LoadedFeed::Segments { monitor, .. }) => Some(monitor.clone()),
                _ => None,
            });
        let verdict = self
            .viewer_will
            .click_claim()
            .judge_page_seek(target, |click, page| {
                monitor
                    .as_ref()
                    .is_some_and(|monitor| monitor.is_same_seek_target(click, page))
            });
        if verdict != PageSeekAgainstClick::Unclaimed {
            debug!(?verdict, "Page seek judged against the viewer click");
        }
        verdict
    }

    /// Показано ли загруженное поколение (плеер его поставил и привёл к странице).
    fn loaded_generation_is_shown(&self) -> bool {
        self.loaded
            .as_ref()
            .is_some_and(|loaded| loaded.page_state_reconciled)
    }
}

#[cfg(test)]
#[path = "viewer_click_claim_tests.rs"]
mod tests;
