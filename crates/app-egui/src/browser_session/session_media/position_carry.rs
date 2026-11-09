//! Возврат на ту же секунду после смены озвучки или качества (решение 280, сессия 14a).
//!
//! **Зачем.** Зритель в нашем меню выбирает другую озвучку или качество, странице уходит
//! `select_variant`, и сайт сам качает новый поток. Почти всегда это новый `MediaSource`
//! (новое поколение), а новый `MediaSource` по правилам HTML начинает с нуля. Многие плееры
//! сайтов сами возвращаются на прежнее место, но не все — адаптер, который этого не умеет,
//! оставил бы зрителя на нуле. Поэтому возвращает плеер, на любом сайте одинаково.
//!
//! **Правило.**
//! ```text
//!  выбор зрителя (озвучка/качество) ─► Armed{T = позиция зрителя}
//!     новое поколение, на экране реклама ──► пропущено, Armed остаётся (реклама Ashdi ~35 с)
//!     новое поколение без рекламы ─────────► Placed: aim_pending_for_viewer(T) — ворота 04d
//!                                            и не больше одной просьбы `seek` странице
//!         своя перемотка страницы ≈ T ─────► эхо: страница подчинилась
//!         своя перемотка в другое место:
//!             до ответа на нашу просьбу ──► сброс нового MediaSource на ноль: не перебивает T
//!             после ответа ──────────────► один повтор T странице; ещё раз — страница главнее
//!         поколение показано ─► окно подтверждения [`VIEWER_WILL_CONFIRMATION_WINDOW`] ─► снято
//!         на экране появилась реклама ─► это было рекламное поколение: снова Armed
//! ```
//! Снимается: когда применено; когда зритель сам перемотал, выбрал другой вариант (новое T)
//! или нажал Next (следующая серия позицию НЕ переносит); когда смена прошла в том же
//! поколении (новое описание дорожек у того же показанного медиа — эстафета 03d, YouTube
//! меняет звук внутри одного `SourceBuffer`); по сроку из настроек
//! (`[browser_session] variant_return_window_s`, по умолчанию 120 с — с запасом на рекламу).
//!
//! **Кто чем владеет.** Состояние — [`PositionCarry`] в медиа-части (`SessionMedia`): только
//! решения, без отправки. Просьбы странице и ворота поколения — у `SessionMedia` (методы
//! внизу) через уже существующие `aim_pending_for_viewer` и `request_page_seek`. Так же
//! устроена претензия клика зрителя (`viewer_click_claim`, решение 149), с которой модуль
//! делит идею «воля зрителя главнее самовольства страницы».
//!
//! **Инварианты.**
//! 1. Рекламное поколение позицию не получает, а возврат его переживает.
//! 2. Срок истёк — возврата нет, даже если поколение придёт потом.
//! 3. Не больше двух `seek` странице на один выбор (просьба и один повтор).
//! 4. Самовольная перемотка страницы до ответа на нашу просьбу не перебивает T.

use std::time::{Duration, Instant};

use browser_bridge_protocol::StreamGeneration;
use fastiplayer_config::BrowserSessionConfig;
use pushed_media_source::{PageSeekRequest, PushedStreamMonitor};
use tracing::{debug, info};

use super::SessionMedia;
use super::generation_feed::LoadedFeed;
use super::viewer_transport::VIEWER_WILL_CONFIRMATION_WINDOW;
use crate::browser_session::playback::track_layout::ShownTrackLayout;

/// Строка журнала: новое поколение после смены варианта поставлено на секунду зрителя.
pub(crate) const POSITION_CARRY_PLACED_LINE: &str =
    "Position carried to the new generation after a variant switch";

/// Сколько возврат ждёт новое поколение фильма после выбора зрителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PositionCarryPolicy {
    lifetime: Duration,
}

impl PositionCarryPolicy {
    /// Срок ожидания явно.
    pub(crate) const fn new(lifetime: Duration) -> Self {
        Self { lifetime }
    }

    /// Срок из настроек (`[browser_session] variant_return_window_s`).
    pub(crate) fn from_config(config: &BrowserSessionConfig) -> Self {
        Self::new(config.variant_return_window())
    }

    /// Умолчание — из умолчаний config-а, а не своим числом.
    pub(crate) fn recommended() -> Self {
        Self::from_config(&BrowserSessionConfig::default())
    }
}

/// Почему возврат снят (для журнала и тестов).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CarryEnd {
    /// Новое поколение показано на T, страница не перечила.
    Applied,
    /// Зритель сам перемотал.
    ViewerSeeked,
    /// Зритель нажал Next: следующая серия начинается сначала.
    NextEpisode,
    /// Новый сеанс (другая страница).
    NewSession,
    /// Срок вышел.
    Expired,
    /// Смена прошла в том же поколении: позиция и так та же.
    SwitchedInPlace,
    /// Страница ушла с T и после повтора: остаёмся там, где страница.
    PageInsisted,
}

/// Попросили ли страницу перемотаться к T.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PageAsk {
    /// Не просили: T уже лежала на полке нового поколения.
    NotAsked,
    /// Просили, страница ещё не ответила `seek_started{player_seek}`.
    AwaitingAnswer,
    /// Страница начала нашу перемотку.
    Answered,
}

/// Есть ли ещё повтор просьбы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepeatBudget {
    /// Повтор не использован.
    Available,
    /// Повтор использован (инвариант 3).
    Spent,
}

/// Возврат ждёт новое поколение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArmedCarry {
    /// Секунда зрителя в миг выбора.
    target: Duration,
    /// Срок (инвариант 2).
    until: Instant,
    /// Описание дорожек показанного медиа в миг выбора (признак смены внутри поколения).
    origin_tracks: ShownTrackLayout,
}

/// Возврат отдан новому поколению.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlacedCarry {
    armed: ArmedCarry,
    generation: StreamGeneration,
    ask: PageAsk,
    repeat: RepeatBudget,
    /// Окно подтверждения после показа (`None` — поколение ещё не показано).
    confirm_until: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CarryState {
    #[default]
    Idle,
    Armed(ArmedCarry),
    Placed(PlacedCarry),
}

/// Что делать с новым поколением.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NewGenerationCarry {
    /// Возврата нет.
    NotCarried,
    /// На экране реклама: поколение позицию не получает, возврат ждёт дальше.
    SkippedAd,
    /// Поставить поколение на T (ворота и одна просьба странице — у вызывающего).
    PlaceAt(Duration),
    /// Поколение заменило уже поставленное до передачи плееру: цель зрителя переходит к нему
    /// сама (`carried_to_replacing_generation`), делать ничего не нужно.
    AlreadyPlaced,
}

/// Что своя перемотка страницы значит для возврата.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CarryPageSeek {
    /// Возврата нет (или он ещё ждёт поколение): обычная перемотка страницы.
    NotCarried,
    /// Страница пришла на T: подчинилась.
    EchoOfCarry,
    /// Сброс нового `MediaSource` до ответа на нашу просьбу: не исполняется (инвариант 4).
    AbsorbedBeforeOurSeek,
    /// Страница ушла с T после ответа: повторить T странице один раз (перемотка не исполняется).
    RepeatToPage(Duration),
    /// Страница ушла с T и после повтора: возврат снят, перемотка исполняется как обычно.
    PageWins,
}

/// Что видит медиа-часть в этот проход (для [`PositionCarry::settle`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CarrySettleView {
    /// Поколение, получившее T, загружено, показано и приведено к странице.
    pub(crate) placed_generation_shown: PlacedGenerationShown,
    /// Плашка рекламы на экране.
    pub(crate) ad_on_screen: AdOnScreen,
    /// Описание дорожек показанного медиа.
    pub(crate) shown_tracks: ShownTrackLayout,
}

/// Показано ли поколение, получившее T.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlacedGenerationShown {
    /// Да.
    Shown,
    /// Нет (ещё ждёт ворот, ставится, или его вытеснило другое).
    NotShown,
}

/// Есть ли на экране реклама.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdOnScreen {
    /// Плашка рекламы есть.
    Yes,
    /// Рекламы нет.
    No,
}

/// Итог прохода.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CarrySettle {
    /// Ничего не сменилось.
    Nothing,
    /// Поколение с T оказалось рекламой: возврат снова ждёт, цель у поколения снять.
    RolledBackForAd(StreamGeneration),
    /// Возврат снят.
    Ended(CarryEnd),
}

/// Возврат на секунду (правило — в описании модуля).
#[derive(Debug, Clone, Copy)]
pub(crate) struct PositionCarry {
    policy: PositionCarryPolicy,
    state: CarryState,
}

impl PositionCarry {
    /// Возврата нет.
    pub(crate) const fn new(policy: PositionCarryPolicy) -> Self {
        Self {
            policy,
            state: CarryState::Idle,
        }
    }

    /// Та же логика с другим сроком (настройки процесса).
    pub(crate) const fn set_policy(&mut self, policy: PositionCarryPolicy) {
        self.policy = policy;
    }

    /// Зритель выбрал вариант на секунде `target`: новый возврат (прежний заменяется —
    /// каждый выбор зрителя новый).
    pub(crate) fn arm(&mut self, target: Duration, now: Instant, origin_tracks: ShownTrackLayout) {
        self.state = CarryState::Armed(ArmedCarry {
            target,
            until: now + self.policy.lifetime,
            origin_tracks,
        });
    }

    /// Секунда, которую возврат держит (`None` — возврата нет).
    pub(crate) const fn target(&self) -> Option<Duration> {
        match self.state {
            CarryState::Idle => None,
            CarryState::Armed(armed) | CarryState::Placed(PlacedCarry { armed, .. }) => {
                Some(armed.target)
            }
        }
    }

    /// Снять возврат (`end` — в журнал).
    pub(crate) fn forget(&mut self, end: CarryEnd) {
        if self.state != CarryState::Idle {
            info!(?end, "Position carry after a variant switch ended");
        }
        self.state = CarryState::Idle;
    }

    /// Пришло новое поколение `generation`.
    pub(crate) fn on_new_generation(
        &mut self,
        generation: StreamGeneration,
        now: Instant,
        ad: AdOnScreen,
    ) -> NewGenerationCarry {
        match self.state {
            CarryState::Idle => NewGenerationCarry::NotCarried,
            CarryState::Armed(armed) if armed.until <= now => {
                self.forget(CarryEnd::Expired);
                NewGenerationCarry::NotCarried
            }
            CarryState::Armed(_) if ad == AdOnScreen::Yes => {
                debug!(
                    generation = generation.get(),
                    "Position carry skips a generation shown while an ad is on screen"
                );
                NewGenerationCarry::SkippedAd
            }
            CarryState::Armed(armed) => {
                self.state = CarryState::Placed(PlacedCarry {
                    armed,
                    generation,
                    ask: PageAsk::NotAsked,
                    repeat: RepeatBudget::Available,
                    confirm_until: None,
                });
                NewGenerationCarry::PlaceAt(armed.target)
            }
            CarryState::Placed(placed) if ad == AdOnScreen::Yes => {
                self.state = CarryState::Armed(placed.armed);
                NewGenerationCarry::SkippedAd
            }
            CarryState::Placed(mut placed) => {
                placed.generation = generation;
                placed.confirm_until = None;
                self.state = CarryState::Placed(placed);
                NewGenerationCarry::AlreadyPlaced
            }
        }
    }

    /// Странице ушла просьба перемотаться к T.
    pub(crate) fn note_page_asked(&mut self) {
        if let CarryState::Placed(placed) = &mut self.state {
            placed.ask = PageAsk::AwaitingAnswer;
        }
    }

    /// Страница начала перемотку плеера к `page_target` (`seek_started{player_seek}`).
    pub(crate) fn note_player_seek_answered(
        &mut self,
        page_target: Duration,
        is_same_target: impl FnOnce(Duration, Duration) -> bool,
    ) {
        if let CarryState::Placed(placed) = &mut self.state
            && placed.ask == PageAsk::AwaitingAnswer
            && is_same_target(placed.armed.target, page_target)
        {
            placed.ask = PageAsk::Answered;
        }
    }

    /// Своя перемотка страницы к `page_target` (правило — в описании модуля).
    pub(crate) fn judge_page_seek(
        &mut self,
        page_target: Duration,
        is_same_target: impl FnOnce(Duration, Duration) -> bool,
    ) -> CarryPageSeek {
        let CarryState::Placed(mut placed) = self.state else {
            return CarryPageSeek::NotCarried;
        };
        let target = placed.armed.target;
        if is_same_target(target, page_target) {
            placed.ask = PageAsk::Answered;
            self.state = CarryState::Placed(placed);
            return CarryPageSeek::EchoOfCarry;
        }
        if placed.ask == PageAsk::AwaitingAnswer {
            return CarryPageSeek::AbsorbedBeforeOurSeek;
        }
        match placed.repeat {
            RepeatBudget::Available => {
                placed.repeat = RepeatBudget::Spent;
                placed.ask = PageAsk::AwaitingAnswer;
                self.state = CarryState::Placed(placed);
                CarryPageSeek::RepeatToPage(target)
            }
            RepeatBudget::Spent => {
                self.forget(CarryEnd::PageInsisted);
                CarryPageSeek::PageWins
            }
        }
    }

    /// Проход медиа-части: срок, показ, реклама, смена в том же поколении.
    pub(crate) fn settle(&mut self, now: Instant, view: CarrySettleView) -> CarrySettle {
        match self.state {
            CarryState::Idle => CarrySettle::Nothing,
            CarryState::Armed(armed) if armed.until <= now => self.end(CarryEnd::Expired),
            CarryState::Armed(armed)
                if switched_in_place(armed.origin_tracks, view.shown_tracks) =>
            {
                self.end(CarryEnd::SwitchedInPlace)
            }
            CarryState::Armed(_) => CarrySettle::Nothing,
            CarryState::Placed(placed) if view.ad_on_screen == AdOnScreen::Yes => {
                debug!(
                    generation = placed.generation.get(),
                    "The generation given the carried position shows an ad; the carry waits again"
                );
                self.state = CarryState::Armed(placed.armed);
                CarrySettle::RolledBackForAd(placed.generation)
            }
            CarryState::Placed(placed) if placed.armed.until <= now => self.end(CarryEnd::Expired),
            CarryState::Placed(mut placed) => {
                if view.placed_generation_shown == PlacedGenerationShown::NotShown {
                    return CarrySettle::Nothing;
                }
                match placed.confirm_until {
                    None => {
                        placed.confirm_until = Some(now + VIEWER_WILL_CONFIRMATION_WINDOW);
                        self.state = CarryState::Placed(placed);
                        CarrySettle::Nothing
                    }
                    Some(until) if until <= now => self.end(CarryEnd::Applied),
                    Some(_) => CarrySettle::Nothing,
                }
            }
        }
    }

    /// Поколение, получившее T (`None` — возврат его ещё не отдал).
    pub(crate) const fn placed_generation(&self) -> Option<StreamGeneration> {
        match self.state {
            CarryState::Placed(placed) => Some(placed.generation),
            CarryState::Idle | CarryState::Armed(_) => None,
        }
    }

    fn end(&mut self, end: CarryEnd) -> CarrySettle {
        self.forget(end);
        CarrySettle::Ended(end)
    }
}

/// То же показанное медиа получило другое описание дорожек: смена прошла внутри поколения.
fn switched_in_place(origin: ShownTrackLayout, now_shown: ShownTrackLayout) -> bool {
    match (origin, now_shown) {
        (
            ShownTrackLayout::Shown {
                load: origin_load,
                stamp: origin_stamp,
            },
            ShownTrackLayout::Shown { load, stamp },
        ) => origin_load == load && origin_stamp != stamp,
        _ => false,
    }
}

impl SessionMedia {
    /// Та же медиа-часть с другим сроком возврата (процесс берёт его из config-а).
    pub(crate) fn with_position_carry_policy(mut self, policy: PositionCarryPolicy) -> Self {
        self.variants.carry.set_policy(policy);
        self
    }

    /// Зритель выбрал вариант: запомнить его секунду. Секунда — то, что видит зритель:
    /// цель ждущего поколения (спиннер у цели), иначе часы показанного медиа.
    pub(super) fn arm_position_carry(&mut self, now: Instant) {
        let shown_position = self.last_observation.as_ref().and_then(|observation| {
            self.loaded
                .as_ref()
                .filter(|loaded| loaded.is_shown_by(observation))
                .map(|_| observation.page_clock_position())
        });
        let Some(target) = self.awaited_generation_target().or(shown_position) else {
            debug!("Variant selected before anything was shown; no position to return to");
            return;
        };
        let origin_tracks = self.playback.shown_track_layout();
        self.variants.carry.arm(target, now, origin_tracks);
    }

    /// Новое ждущее поколение: если возврат ждёт и рекламы нет — поставить его на T.
    pub(super) fn carry_position_to_new_generation(&mut self, now: Instant) {
        let Some(generation) = self.pending.as_ref().map(|pending| pending.generation) else {
            return;
        };
        let ad = self.ad_on_screen(now);
        match self.variants.carry.on_new_generation(generation, now, ad) {
            NewGenerationCarry::NotCarried => {}
            NewGenerationCarry::SkippedAd => self.release_carried_target(generation),
            NewGenerationCarry::PlaceAt(target) => self.place_pending_at_carried(target),
            NewGenerationCarry::AlreadyPlaced => {
                // Цель переходит к заменившему поколению сама, только если прежнее ещё ждало
                // (`carried_to_replacing_generation`); иначе — поставить заново.
                let carried = self
                    .pending
                    .as_ref()
                    .and_then(|pending| pending.placement.awaited_viewer_target());
                if let Some(target) = self.variants.carry.target()
                    && carried != Some(target)
                {
                    self.place_pending_at_carried(target);
                }
            }
        }
    }

    /// Ждущее поколение — на T через ворота 04d (не больше одной просьбы странице).
    fn place_pending_at_carried(&mut self, target: Duration) {
        let page_listens = self.drain.is_none() && self.current_session.is_some();
        self.aim_pending_for_viewer(target, page_listens);
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let generation = pending.generation.get();
        let asked = pending.placement.asked_viewer_target().is_some();
        if asked {
            self.variants.carry.note_page_asked();
        }
        info!(generation, asked, "{POSITION_CARRY_PLACED_LINE}");
    }

    /// Своя перемотка страницы против возврата; `true` — перемотка поглощена (не исполнять).
    pub(super) fn position_carry_absorbs_page_seek(&mut self, target: Duration) -> bool {
        let monitor = self.newest_segments_monitor();
        let verdict = self
            .variants
            .carry
            .judge_page_seek(target, |carried, page| {
                same_target(monitor.as_ref(), carried, page)
            });
        match verdict {
            CarryPageSeek::NotCarried | CarryPageSeek::EchoOfCarry | CarryPageSeek::PageWins => {
                false
            }
            CarryPageSeek::AbsorbedBeforeOurSeek => {
                debug!("Page seek before it answered the carried position; the carry stands");
                true
            }
            CarryPageSeek::RepeatToPage(carried) => {
                self.ask_page_to_return_to(carried, monitor.as_ref());
                true
            }
        }
    }

    /// Страница начала перемотку плеера (`seek_started{player_seek}`).
    pub(super) fn note_position_carry_answer(&mut self, target: Duration) {
        let monitor = self.newest_segments_monitor();
        self.variants
            .carry
            .note_player_seek_answered(target, |carried, page| {
                same_target(monitor.as_ref(), carried, page)
            });
    }

    /// Проход: срок, показ, реклама, смена в том же поколении.
    pub(super) fn settle_position_carry(&mut self, now: Instant) {
        let placed = self.variants.carry.placed_generation();
        let shown = self.loaded.as_ref().is_some_and(|loaded| {
            Some(loaded.generation) == placed && loaded.page_state_reconciled
        });
        let view = CarrySettleView {
            placed_generation_shown: if shown {
                PlacedGenerationShown::Shown
            } else {
                PlacedGenerationShown::NotShown
            },
            ad_on_screen: self.ad_on_screen(now),
            shown_tracks: self.playback.shown_track_layout(),
        };
        if let CarrySettle::RolledBackForAd(generation) = self.variants.carry.settle(now, view) {
            self.release_carried_target(generation);
        }
    }

    /// Зритель сам перемотал или нажал Next: возврат снимается.
    pub(super) fn forget_position_carry(&mut self, end: CarryEnd) {
        self.variants.carry.forget(end);
    }

    /// Рекламному поколению цель зрителя не нужна: ждущее поколение встаёт как без неё.
    ///
    /// Снимается только СВОЯ цель (секунда возврата): цель клика зрителя, перешедшая к этому
    /// поколению от заменённого (`carried_to_replacing_generation`), остаётся — это его воля.
    fn release_carried_target(&mut self, generation: StreamGeneration) {
        let carried = self.variants.carry.target();
        if let Some(pending) = self.pending.as_mut()
            && pending.generation == generation
            && carried.is_some()
            && pending.placement.awaited_viewer_target() == carried
        {
            pending.placement.release_viewer_target();
            pending.retry_at = None;
        }
    }

    /// Повтор T странице (инвариант 3): источник помечает цель, чтобы установка поколения
    /// вторую просьбу не поставила.
    fn ask_page_to_return_to(&mut self, target: Duration, monitor: Option<&PushedStreamMonitor>) {
        if self.is_draining() || self.current_session.is_none() {
            return;
        }
        if let Some(monitor) = monitor {
            monitor.note_page_seek_requested(target);
        }
        let seek_id = self.outbound.request_page_seek(PageSeekRequest { target });
        info!(
            seek_id = seek_id.0,
            "The page left the carried position after a variant switch; asking it once more"
        );
    }

    /// Плашка рекламы на экране.
    fn ad_on_screen(&self, now: Instant) -> AdOnScreen {
        if self.ad.plate(now).is_some() {
            AdOnScreen::Yes
        } else {
            AdOnScreen::No
        }
    }

    /// Монитор самого нового поколения-сегментов (ждущего, иначе загруженного).
    fn newest_segments_monitor(&self) -> Option<PushedStreamMonitor> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.feed.segments_monitor().cloned())
            .or_else(|| match self.loaded.as_ref().map(|loaded| &loaded.feed) {
                Some(LoadedFeed::Segments { monitor, .. }) => Some(monitor.clone()),
                _ => None,
            })
    }
}

/// Та же ли это цель: допуск источника, без источника — точное совпадение.
fn same_target(monitor: Option<&PushedStreamMonitor>, first: Duration, second: Duration) -> bool {
    monitor.map_or(first == second, |monitor| {
        monitor.is_same_seek_target(first, second)
    })
}

#[cfg(test)]
#[path = "position_carry_tests.rs"]
mod tests;
