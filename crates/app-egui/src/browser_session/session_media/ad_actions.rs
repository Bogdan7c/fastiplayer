//! Реклама в медиа-части сеанса: слово страницы, команды зрителя и запрет перемотки
//! (сессия 09d, решения 184, 206, 207).
//!
//! Состоянием владеет [`AdPresence`](super::ad_presence::AdPresence); здесь — то, что
//! требует остального `SessionMedia`: страница слушает или ушла (доигрывание), куда
//! уходит команда (`PageOutbound`), и что запрещено зрителю, пока плашка на экране.
//!
//! **Решение 206.** Пока реклама на экране, перемотка и скорость зрителя не исполняются:
//! плеер ничего не получает, странице ничего не уходит. Пауза, «играть» и громкость
//! работают. Запрет касается только зрителя: страница сама ведёт своё время и скорость
//! (`PageIntent`), и плеер идёт за ней как раньше. Окно закрывает те же входы раньше
//! (`state::ad_transport_gate`), чтобы бегунок и значение скорости не прыгали; здесь —
//! последний рубеж на случай, когда плашка появилась между кликом и этим проходом.

use std::time::Instant;

use browser_bridge_protocol::session::PageAdState;
use tracing::{debug, info};

use super::SessionMedia;
use crate::ad_plate::{
    AD_RATE_REFUSED_LINE, AD_SEEK_REFUSED_LINE, AD_SKIP_SENT_LINE, AdActionDecision, AdPlate,
    AdPlateAction,
};

impl SessionMedia {
    /// Страница сказала, что с рекламой. Если страницы уже нет (доигрываем), слово
    /// опоздало: плашка снята вместе с вкладкой и не возвращается.
    pub(super) fn apply_ad_state(&mut self, state: PageAdState) {
        if self.is_draining() {
            debug!("Ad state of a page that has already left dropped");
            return;
        }
        self.ad.on_page_state(state, Instant::now());
    }

    /// Плашка для окна на этот момент (`None` — рекламы нет).
    pub(crate) fn ad_plate(&self, now: Instant) -> Option<AdPlate> {
        self.ad.plate(now)
    }

    /// Зритель просит действие плашки (кнопка, «N», MPRIS Next).
    ///
    /// Команда уходит странице, только если плашка это действие предлагает, прошлая
    /// команда не в пути и страница слушает (после её ухода плашки уже нет).
    pub(super) fn viewer_ad_action(
        &mut self,
        wanted: AdPlateAction,
        now: Instant,
        page_listens: bool,
    ) {
        if !page_listens {
            debug!("Ad action ignored: the page is gone");
            return;
        }
        match self.ad.begin_action(wanted, now) {
            AdActionDecision::Send => match wanted {
                AdPlateAction::Skip => {
                    info!("{AD_SKIP_SENT_LINE}");
                    self.outbound.request_skip_ad();
                }
            },
            decision @ (AdActionDecision::AlreadyInFlight | AdActionDecision::NotOffered) => {
                debug!(?wanted, ?decision, "Ad action not sent to the page");
            }
        }
    }

    /// Перемотку зрителя надо отклонить: на экране реклама (журнал + `true`).
    pub(super) fn refuse_viewer_seek_during_ad(&self) -> bool {
        let refuse = self.ad.locks_transport(Instant::now());
        if refuse {
            info!("{AD_SEEK_REFUSED_LINE}");
        }
        refuse
    }

    /// Смену скорости зрителем надо отклонить: на экране реклама (журнал + `true`).
    pub(super) fn refuse_viewer_rate_during_ad(&self) -> bool {
        let refuse = self.ad.locks_transport(Instant::now());
        if refuse {
            info!("{AD_RATE_REFUSED_LINE}");
        }
        refuse
    }
}
