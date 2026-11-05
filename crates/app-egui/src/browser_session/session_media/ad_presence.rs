//! Реклама на странице глазами медиа-части: владелец состояния плашки (сессия 09d).
//!
//! Страница шлёт `ad_state` (только числа и метки, инвариант 1); здесь они превращаются
//! в [`AdPlate`] для окна и в решение «уходит ли команда странице»:
//!
//! ```text
//!  ad_state ──► on_page_state ──► plate(now) ──► окно рисует «Ad 1 of 2 · Skip ›»
//!  клик / N / MPRIS Next ──► begin_action(действие, now) ──► Send ─► skip_ad
//!                                                         └► AlreadyInFlight / NotOffered
//! ```
//!
//! Инварианты:
//! - состояние живёт только в памяти и только пока жив сеанс: конец сеанса, уход вкладки
//!   (доигрывание) и новый сеанс снимают плашку ([`AdPresence::clear`]); на диск оно не
//!   пишется (инвариант 7);
//! - после команды кнопка помечена «в пути»: вторая команда не уходит, пока состояние
//!   рекламы не изменилось (страница ответила) или не прошло [`ACTION_IN_FLIGHT_TIMEOUT`];
//! - предлагается только то, что страница сама показала (`available` / `on_page`): мы не
//!   обходим рекламу, а даём нажать кнопку, которую сайт уже вывел (решение 184).

use std::time::{Duration, Instant};

use browser_bridge_protocol::session::{AdPod, AdSkip, PageAdState};
use tracing::{debug, info};

use crate::ad_plate::{
    AD_PLATE_CLEARED_LINE, AD_PLATE_SHOWN_LINE, AdActionDecision, AdActionProgress, AdPlate,
    AdPlateAction, AdPodLabel, AdSkipOffer,
};

/// Сколько кнопка плашки остаётся «в пути» без ответа страницы.
///
/// Нормально страница отвечает за десятки миллисекунд: реклама сменилась или исчезла
/// (`ad_state` изменился), и пометка снимается сама. Но страница могла не отреагировать
/// на команду (клик не принят, вкладка зависла) — тогда зритель не должен застрять с
/// мёртвой кнопкой навсегда. Три секунды: заметно дольше любого честного ответа и
/// достаточно коротко, чтобы повторить нажатие, если оно ушло впустую.
pub(crate) const ACTION_IN_FLIGHT_TIMEOUT: Duration = Duration::from_secs(3);

/// Сколько одна непрерывная реклама держит запрет перемотки и скорости (решение 206).
///
/// Предохранитель от ложной тревоги: забытый на странице счётчик или ошибочно принятый за
/// рекламу фильм не должны запирать зрителю таймлайн навсегда. Живые ролики идут секунды
/// и минуты, 10 минут заметно дольше обычной рекламы; более длинный ролик после этого срока
/// перематывается, плашка при этом остаётся. Срок считается с первого слова `showing`, смена
/// номера рекламы его не сбрасывает, сбрасывает только `none`.
pub(crate) const AD_TRANSPORT_LOCK_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Команда за кнопкой плашки, ушедшая странице без ответа.
#[derive(Debug, Clone, Copy)]
struct ActionInFlight {
    /// До какого момента кнопка считается занятой.
    until: Instant,
}

/// Идущая реклама: слово страницы и пометка «команда в пути».
#[derive(Debug, Clone, Copy)]
struct Showing {
    pod: AdPod,
    skip: AdSkip,
    in_flight: Option<ActionInFlight>,
    /// С какого момента реклама идёт непрерывно.
    since: Instant,
}

impl Showing {
    /// Пометка «в пути» действует и ещё не истекла к `now`.
    fn live_in_flight(&self, now: Instant) -> Option<ActionInFlight> {
        self.in_flight.filter(|in_flight| now < in_flight.until)
    }

    /// Когда запрет перемотки и скорости снимается сам.
    fn transport_lock_ends_at(&self) -> Instant {
        self.since + AD_TRANSPORT_LOCK_LIMIT
    }
}

/// Состояние рекламы текущего сеанса.
#[derive(Debug, Default)]
pub(in crate::browser_session) struct AdPresence {
    showing: Option<Showing>,
}

impl AdPresence {
    /// Страница сообщила состояние рекламы (последнее слово побеждает).
    ///
    /// Любое изменение снимает пометку «в пути»: страница ответила. Повтор того же слова
    /// ничего не меняет и в журнал не пишется.
    pub(in crate::browser_session) fn on_page_state(&mut self, state: PageAdState, now: Instant) {
        match state {
            PageAdState::None {} => self.clear(),
            PageAdState::Showing { pod, skip } => {
                let unchanged = self
                    .showing
                    .is_some_and(|showing| showing.pod == pod && showing.skip == skip);
                if unchanged {
                    return;
                }
                let since = self.showing.map_or(now, |showing| showing.since);
                self.showing = Some(Showing {
                    pod,
                    skip,
                    in_flight: None,
                    since,
                });
                log_shown(pod, offer_of(skip));
            }
        }
    }

    /// Плашку снимает не страница, а жизнь сеанса: конец, уход вкладки, новый сеанс.
    pub(in crate::browser_session) fn clear(&mut self) {
        if self.showing.take().is_some() {
            info!("{AD_PLATE_CLEARED_LINE}");
        }
    }

    /// Плашка для окна (`None` — рекламы нет).
    pub(in crate::browser_session) fn plate(&self, now: Instant) -> Option<AdPlate> {
        let showing = self.showing.as_ref()?;
        Some(AdPlate {
            pod: pod_label(showing.pod),
            offer: offer_of(showing.skip),
            progress: if showing.live_in_flight(now).is_some() {
                AdActionProgress::InFlight
            } else {
                AdActionProgress::Idle
            },
            locks_transport: now < showing.transport_lock_ends_at(),
        })
    }

    /// Заперты ли сейчас перемотка и скорость зрителя (решение 206).
    pub(in crate::browser_session) fn locks_transport(&self, now: Instant) -> bool {
        self.showing
            .is_some_and(|showing| now < showing.transport_lock_ends_at())
    }

    /// Зритель просит действие плашки: уходит ли команда странице.
    ///
    /// `Send` помечает кнопку «в пути» до `now + ACTION_IN_FLIGHT_TIMEOUT`.
    pub(in crate::browser_session) fn begin_action(
        &mut self,
        wanted: AdPlateAction,
        now: Instant,
    ) -> AdActionDecision {
        let Some(showing) = self.showing.as_mut() else {
            return AdActionDecision::NotOffered;
        };
        if action_of(showing.skip) != Some(wanted) {
            return AdActionDecision::NotOffered;
        }
        if showing.live_in_flight(now).is_some() {
            debug!(
                ?wanted,
                "Ad action ignored: the previous one is still in flight"
            );
            return AdActionDecision::AlreadyInFlight;
        }
        showing.in_flight = Some(ActionInFlight {
            until: now + ACTION_IN_FLIGHT_TIMEOUT,
        });
        AdActionDecision::Send
    }

    /// Когда истечёт живая пометка «в пути» (цикл просыпается, чтобы кнопка ожила и без
    /// событий страницы). Истёкшая пометка срока не даёт: прошедший срок заставил бы цикл
    /// крутиться без сна, пока страница молчит.
    pub(in crate::browser_session) fn in_flight_expires_at(&self, now: Instant) -> Option<Instant> {
        self.showing
            .as_ref()?
            .live_in_flight(now)
            .map(|in_flight| in_flight.until)
    }

    /// Когда снимется запрет перемотки и скорости, если он ещё действует.
    pub(in crate::browser_session) fn transport_lock_expires_at(
        &self,
        now: Instant,
    ) -> Option<Instant> {
        self.showing
            .as_ref()
            .map(Showing::transport_lock_ends_at)
            .filter(|ends_at| now < *ends_at)
    }
}

/// Слово страницы про пропуск → предложение плашки.
const fn offer_of(skip: AdSkip) -> AdSkipOffer {
    match skip {
        AdSkip::Unavailable {} => AdSkipOffer::Nothing,
        AdSkip::Countdown { seconds } => AdSkipOffer::Countdown {
            seconds: seconds.get(),
        },
        AdSkip::Available {} => AdSkipOffer::Skip,
        AdSkip::OnPage {} => AdSkipOffer::OnPage,
    }
}

/// Действие, которое стоит за кнопкой при этом слове страницы.
const fn action_of(skip: AdSkip) -> Option<AdPlateAction> {
    match skip {
        AdSkip::Available {} => Some(AdPlateAction::Skip),
        AdSkip::Unavailable {} | AdSkip::Countdown { .. } | AdSkip::OnPage {} => None,
    }
}

/// Слово страницы про номер рекламы → подпись плашки.
const fn pod_label(pod: AdPod) -> AdPodLabel {
    match pod {
        AdPod::Unknown {} => AdPodLabel::Unknown,
        AdPod::Known { index, count } => AdPodLabel::Known {
            index: index.get(),
            count: count.get(),
        },
    }
}

/// Строка журнала «плашка показана/изменилась»: только числа и метки.
fn log_shown(pod: AdPod, offer: AdSkipOffer) {
    // 0 в журнале — «сайт счётчик не показывает» (настоящие номера начинаются с 1).
    let (ad_index, ad_count) = match pod_label(pod) {
        AdPodLabel::Unknown => (0, 0),
        AdPodLabel::Known { index, count } => (index, count),
    };
    let countdown_seconds = match offer {
        AdSkipOffer::Countdown { seconds } => seconds,
        _ => 0,
    };
    info!(
        ad_index,
        ad_count,
        skip = %offer.log_label(),
        countdown_seconds,
        "{AD_PLATE_SHOWN_LINE}"
    );
}

#[cfg(test)]
mod tests {
    use browser_bridge_protocol::session::{AdCountdownSeconds, AdPodNumber};

    use super::*;

    fn number(value: u16) -> AdPodNumber {
        AdPodNumber::new(value).expect("pod number")
    }

    fn showing(index: u16, count: u16, skip: AdSkip) -> PageAdState {
        PageAdState::Showing {
            pod: AdPod::Known {
                index: number(index),
                count: number(count),
            },
            skip,
        }
    }

    fn countdown(seconds: u16) -> AdSkip {
        AdSkip::Countdown {
            seconds: AdCountdownSeconds::new(seconds).expect("seconds"),
        }
    }

    #[test]
    fn no_ad_means_no_plate() {
        let mut presence = AdPresence::default();
        assert_eq!(presence.plate(Instant::now()), None);
        presence.on_page_state(PageAdState::None {}, Instant::now());
        assert_eq!(presence.plate(Instant::now()), None);
        assert!(!presence.locks_transport(Instant::now()));
    }

    #[test]
    fn the_plate_counts_the_ad_and_follows_the_skip_word_and_then_disappears() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 2, countdown(5)), Instant::now());
        assert_eq!(
            presence.plate(now),
            Some(AdPlate {
                locks_transport: true,
                pod: AdPodLabel::Known { index: 1, count: 2 },
                offer: AdSkipOffer::Countdown { seconds: 5 },
                progress: AdActionProgress::Idle,
            })
        );
        presence.on_page_state(showing(1, 2, AdSkip::Available {}), Instant::now());
        assert_eq!(
            presence.plate(now).map(|plate| plate.offer),
            Some(AdSkipOffer::Skip)
        );
        // Смена рекламы: второй ролик, пропуск «только на странице», счётчика нет.
        presence.on_page_state(
            PageAdState::Showing {
                pod: AdPod::Unknown {},
                skip: AdSkip::OnPage {},
            },
            Instant::now(),
        );
        assert_eq!(
            presence.plate(now),
            Some(AdPlate {
                locks_transport: true,
                pod: AdPodLabel::Unknown,
                offer: AdSkipOffer::OnPage,
                progress: AdActionProgress::Idle,
            })
        );
        presence.on_page_state(PageAdState::None {}, Instant::now());
        assert_eq!(presence.plate(now), None);
    }

    #[test]
    fn a_second_click_sends_nothing_until_the_page_state_changes() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 2, AdSkip::Available {}), Instant::now());
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::Send
        );
        assert_eq!(
            presence.plate(now).map(|plate| plate.progress),
            Some(AdActionProgress::InFlight)
        );
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::AlreadyInFlight
        );
        // Страница ответила: реклама сменилась — кнопка свободна, команда уходит снова.
        presence.on_page_state(showing(2, 2, AdSkip::Available {}), Instant::now());
        assert_eq!(
            presence.plate(now).map(|plate| plate.progress),
            Some(AdActionProgress::Idle)
        );
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::Send
        );
    }

    #[test]
    fn the_same_page_word_again_does_not_free_the_button() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 2, AdSkip::Available {}), Instant::now());
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::Send
        );
        presence.on_page_state(showing(1, 2, AdSkip::Available {}), Instant::now());
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::AlreadyInFlight
        );
    }

    #[test]
    fn a_page_that_never_reacts_frees_the_button_after_the_timeout() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 1, AdSkip::Available {}), Instant::now());
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::Send
        );
        assert_eq!(
            presence.in_flight_expires_at(now),
            Some(now + ACTION_IN_FLIGHT_TIMEOUT)
        );
        let almost = now + ACTION_IN_FLIGHT_TIMEOUT - Duration::from_millis(1);
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, almost),
            AdActionDecision::AlreadyInFlight
        );
        let expired = now + ACTION_IN_FLIGHT_TIMEOUT;
        assert_eq!(
            presence.plate(expired).map(|plate| plate.progress),
            Some(AdActionProgress::Idle)
        );
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, expired),
            AdActionDecision::Send
        );
    }

    #[test]
    fn only_the_action_the_plate_offers_is_sent() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::NotOffered,
            "no ad"
        );
        presence.on_page_state(showing(1, 2, countdown(3)), Instant::now());
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::NotOffered,
            "countdown offers nothing yet"
        );
        presence.on_page_state(showing(1, 2, AdSkip::OnPage {}), Instant::now());
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::NotOffered,
            "the page only trusts a human here: skip is not ours to send"
        );
        assert_eq!(
            presence.in_flight_expires_at(now),
            None,
            "a refused action marks nothing"
        );
    }

    #[test]
    fn an_expired_in_flight_mark_gives_no_wake_deadline_so_the_loop_can_sleep() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 1, AdSkip::Available {}), now);
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::Send
        );
        assert_eq!(
            presence.in_flight_expires_at(now),
            Some(now + ACTION_IN_FLIGHT_TIMEOUT)
        );
        // Страница молчит дальше: прошедший срок заставил бы цикл крутиться без сна.
        assert_eq!(
            presence.in_flight_expires_at(now + ACTION_IN_FLIGHT_TIMEOUT),
            None
        );
    }

    #[test]
    fn the_transport_lock_lifts_by_itself_but_the_plate_stays() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 2, countdown(5)), now);
        let almost = now + AD_TRANSPORT_LOCK_LIMIT - Duration::from_millis(1);
        assert!(presence.locks_transport(almost));
        assert_eq!(
            presence.transport_lock_expires_at(now),
            Some(now + AD_TRANSPORT_LOCK_LIMIT)
        );
        // Следующая реклама блока срок не продлевает: забытый счётчик не запрёт фильм навсегда.
        presence.on_page_state(showing(2, 2, countdown(4)), almost);
        let expired = now + AD_TRANSPORT_LOCK_LIMIT;
        assert!(!presence.locks_transport(expired));
        assert_eq!(presence.transport_lock_expires_at(expired), None);
        let plate = presence.plate(expired).expect("the plate stays");
        assert!(!plate.locks_transport);
    }

    #[test]
    fn a_new_ad_after_none_gets_a_fresh_transport_lock() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 1, countdown(5)), now);
        let later = now + AD_TRANSPORT_LOCK_LIMIT;
        presence.on_page_state(PageAdState::None {}, later);
        presence.on_page_state(showing(1, 1, countdown(5)), later);
        assert!(presence.locks_transport(later));
    }

    #[test]
    fn clearing_the_session_removes_the_plate_and_the_in_flight_mark() {
        let now = Instant::now();
        let mut presence = AdPresence::default();
        presence.on_page_state(showing(1, 2, AdSkip::Available {}), Instant::now());
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::Send
        );
        presence.clear();
        assert_eq!(presence.plate(now), None);
        assert_eq!(presence.in_flight_expires_at(now), None);
        assert_eq!(
            presence.begin_action(AdPlateAction::Skip, now),
            AdActionDecision::NotOffered
        );
    }
}
