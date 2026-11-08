//! Когда процессу браузерного режима выходить (решение 56).
//!
//! Процесс поднимается D-Bus-активацией ради одного сеанса и уходит сразу после
//! его конца: ничего не висит в памяти и не оставляет следов. Чистая машина
//! состояний без часов и потоков — главный цикл приложения кормит её событиями и
//! временем и выходит, когда она скажет.
//!
//! Правила:
//! - сеанс не открыли за `idle_exit_after_start` после старта — выход
//!   ([`ExitCause::IdleWithoutSession`]);
//! - активный сеанс закончился — выход ([`ExitCause::SessionEnded`]), КРОМЕ концов,
//!   после которых зритель ещё смотрит скачанное: уход вкладки (`tab_gone`) и
//!   пропажа экстеншена без возврата ([`ends_with_playback_drain`], решение 42).
//!   Тогда машина переходит в «доигрываем» ([`LifecycleVerdict::AwaitPlaybackDrain`]) и
//!   выходит только по [`ProcessLifecycle::on_playback_drained`]; сроков доигрывания
//!   машина не знает и по времени не будит — их знает медиа-часть приложения;
//! - новый сеанс во время доигрывания (решение 40) заменяет доигрываемый: доигрывание
//!   старого забывается, дальше всё как для обычного активного сеанса;
//! - события считаются по ЭКЗЕМПЛЯРУ сеанса, а не по ключу: ключ может повториться
//!   (сеанс K закончился по сроку, экстеншен открыл K заново), и запоздавший
//!   `Ended(K, старый экземпляр)` не должен убить новый сеанс;
//! - конец ВЫТЕСНЕННОГО сеанса (новое видео из другой вкладки) не повод для
//!   выхода: событие `Ended` старого ключа может прийти раньше, чем приложение
//!   узнает об открытии нового, поэтому оно игнорируется и по ключу, и по причине;
//! - решение об выходе необратимо: после него новый сеанс означает новый запуск.
//!
//! «Сирота» (протокол 1.4, сессия 11b1, решения 240–241): страница ушла, но связь с
//! экстеншеном держится, и страницу можно вернуть в тот же сеанс. Сервер сеанс НЕ
//! заканчивает, поэтому о такой смене машине говорит приложение (уведомления потока
//! приёма идут в порядке кадров на проводе):
//! - [`ProcessLifecycle::on_page_left`] — страница активного сеанса ушла: «доигрываем»,
//!   как после `tab_gone` с той же причиной (выход — по «доиграно»);
//! - [`ProcessLifecycle::on_page_returned`] — страница вернулась в этот же экземпляр:
//!   снова «сеанс идёт». Это единственный путь назад из «доигрываем»; решение «выход»
//!   по-прежнему необратимо.
//!
//! Позднее `Ended` того же экземпляра (сирота закрылась: отказ, браузер закрыт) в
//! «доигрываем» игнорируется, как любой повторный конец, — доигрывание идёт дальше.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use browser_bridge_protocol::SessionCloseReason;
use browser_bridge_protocol::session::TabGoneReason;

use crate::registry::{SessionInstance, SessionRef};

use crate::sink::{SessionEndReason, SessionLifecycleEvent};

/// Почему процессу пора выходить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCause {
    /// За отведённое время никто не открыл сеанс.
    IdleWithoutSession,
    /// Активный сеанс закончился по этой причине.
    SessionEnded(SessionEndReason),
}

/// Заканчивается ли сеанс по этой причине доигрыванием, а не немедленным выходом.
///
/// Единственное место правила (решение 42): доигрываем, когда у зрителя на экране
/// уже есть полученное, а сеанс оборвался не по его воле:
/// - `tab_gone` с любой причиной ([`SessionEndReason::TabGone`]) — вкладку закрыли или
///   ушли, браузер закрывается, плеер сайта сломался (1.3, решение 237);
/// - [`SessionEndReason::ReconnectGraceExpired`] — экстеншен пропал и не вернулся
///   (браузер упал).
///
/// Остальные концы (окно плеера закрыто, нарушение протокола, закрытие страницей,
/// завершение плеера, не пришедший `attach`) — выход сразу. В том числе честные концы
/// страницы (сессия 09a2): «страница забрала видео» (4009, решение 179) — страница уже
/// играет сама, доигрывание дало бы два звука; сбой на стороне страницы (её 4003) — видео
/// возвращено странице; зритель нажал «Return» на заглушке (4010, решение 236) — видео
/// играет на странице, окно закрывается молча.
#[must_use]
pub const fn ends_with_playback_drain(reason: SessionEndReason) -> bool {
    matches!(
        reason,
        SessionEndReason::TabGone { .. } | SessionEndReason::ReconnectGraceExpired
    )
}

/// Решение машины.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleVerdict {
    /// Работаем. Если задан `wake_at`, к этому моменту надо спросить снова
    /// (иначе будить нас может только событие).
    KeepRunning {
        /// Когда истекает ближайший срок машины.
        wake_at: Option<Instant>,
    },
    /// Сеанс закончился, но зритель досматривает полученное: процесс работает, пока
    /// приложение не сообщит, что плеер доиграл ([`ProcessLifecycle::on_playback_drained`]).
    /// По времени машина в этом состоянии не будит.
    AwaitPlaybackDrain,
    /// Выходим по этой причине.
    Exit(ExitCause),
}

/// Итог [`ProcessLifecycle::on_playback_drained`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackDrainOutcome {
    /// Доигрывание шло — теперь решение «выход» принято (см. `verdict`).
    ExitDecided,
    /// Машина не доигрывала (сеанс активен, не открыт или выход уже решён): ничего не изменилось.
    NotDraining,
}

/// Итог намерения о странице сеанса ([`ProcessLifecycle::on_page_left`],
/// [`ProcessLifecycle::on_page_returned`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageLinkOutcome {
    /// Машина сменила состояние: «доигрываем» после ухода или «сеанс идёт» после возврата.
    Followed,
    /// Ничего не изменилось: это не тот экземпляр, который машина ведёт в нужном
    /// состоянии (чужой или старый сеанс, уход не активного, возврат без ухода, выход уже
    /// решён).
    NotFollowed,
}

/// Что известно машине о сеансах.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tracking {
    /// Ни один сеанс ещё не открывали.
    NothingOpenedYet,
    /// Идёт сеанс с этим ключом.
    Active(SessionRef),
    /// Сеанс закончился концом, который доигрывается; ждём «доиграно».
    Draining {
        /// Закончившийся экземпляр: новый сеанс должен быть новее него.
        session: SessionRef,
        /// Причина конца — станет причиной выхода.
        reason: SessionEndReason,
    },
    /// Решение принято, оно окончательное.
    Finished(ExitCause),
}

/// Машина «когда выходить».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLifecycle {
    idle_deadline: Instant,
    tracking: Tracking,
    /// Концы экземпляров, о которых приложение ещё не сообщило `on_session_opened`
    /// (событие сервера обогнало вызов входа). Ограничено [`MAX_EARLY_ENDS`].
    early_ends: BTreeMap<SessionInstance, SessionEndReason>,
}

/// Сколько «ранних» концов помним: сеанс один за раз, гонка бывает на единицы.
const MAX_EARLY_ENDS: usize = 8;

impl ProcessLifecycle {
    /// Машина процесса, стартовавшего в `started_at`.
    #[must_use]
    pub fn new(started_at: Instant, idle_exit_after_start: Duration) -> Self {
        Self {
            idle_deadline: started_at + idle_exit_after_start,
            tracking: Tracking::NothingOpenedYet,
            early_ends: BTreeMap::new(),
        }
    }

    /// Приложение открыло (или повторно выдало, или заменило) этот экземпляр сеанса.
    pub fn on_session_opened(&mut self, session: &SessionRef) {
        match &self.tracking {
            // Выход решён — новый сеанс уже не спасёт этот процесс.
            Tracking::Finished(_) => {}
            Tracking::Active(active) if active.instance > session.instance => {
                // Запоздавшее открытие старого экземпляра: новый уже известен.
            }
            // Доигрываемый экземпляр (или старее) заново не открывается: игнор.
            Tracking::Draining { session: ended, .. } if ended.instance >= session.instance => {}
            Tracking::NothingOpenedYet | Tracking::Active(_) | Tracking::Draining { .. } => {
                // Конец этого экземпляра мог прийти раньше, чем мы узнали об открытии.
                self.tracking = match self.early_ends.remove(&session.instance) {
                    Some(reason) => Self::tracking_after_end(session, reason),
                    None => Tracking::Active(session.clone()),
                };
            }
        }
    }

    /// Событие жизни сеанса от сервера.
    pub fn on_lifecycle_event(&mut self, event: &SessionLifecycleEvent) {
        let SessionLifecycleEvent::Ended { session, reason } = event else {
            return;
        };
        if matches!(
            reason,
            SessionEndReason::ClosedByPlayer(SessionCloseReason::ReplacedByAnotherTab)
        ) {
            return;
        }
        match &self.tracking {
            Tracking::Active(active) if active == session => {
                self.tracking = Self::tracking_after_end(session, *reason);
            }
            // Конец экземпляра НОВЕЕ известного (или до первого открытия): запомнить.
            Tracking::NothingOpenedYet => self.remember_early_end(session.instance, *reason),
            Tracking::Active(SessionRef { instance, .. })
            | Tracking::Draining {
                session: SessionRef { instance, .. },
                ..
            } if session.instance > *instance => {
                self.remember_early_end(session.instance, *reason);
            }
            // Чужой/старый/повторный экземпляр или решение уже принято: игнорируем.
            Tracking::Active(_) | Tracking::Draining { .. } | Tracking::Finished(_) => {}
        }
    }

    /// Страница активного сеанса ушла, но связь держится («сирота», протокол 1.4):
    /// машина доигрывает так же, как после `tab_gone` с этой причиной — та же причина
    /// станет причиной выхода, если страница не вернётся.
    pub fn on_page_left(&mut self, session: &SessionRef, reason: TabGoneReason) -> PageLinkOutcome {
        match &self.tracking {
            Tracking::Active(active) if active == session => {
                self.tracking = Tracking::Draining {
                    session: session.clone(),
                    reason: SessionEndReason::TabGone { reason },
                };
                PageLinkOutcome::Followed
            }
            _ => PageLinkOutcome::NotFollowed,
        }
    }

    /// Страница вернулась в доигрываемый экземпляр (тот же сеанс, решение 241): доигрывание
    /// отменяется, дальше всё как для активного сеанса — в том числе новый конец.
    pub fn on_page_returned(&mut self, session: &SessionRef) -> PageLinkOutcome {
        match &self.tracking {
            Tracking::Draining {
                session: draining, ..
            } if draining == session => {
                self.tracking = Tracking::Active(session.clone());
                PageLinkOutcome::Followed
            }
            _ => PageLinkOutcome::NotFollowed,
        }
    }

    /// Состояние после конца сеанса: доигрываем или сразу выходим (см. [`ends_with_playback_drain`]).
    fn tracking_after_end(session: &SessionRef, reason: SessionEndReason) -> Tracking {
        if ends_with_playback_drain(reason) {
            Tracking::Draining {
                session: session.clone(),
                reason,
            }
        } else {
            Tracking::Finished(ExitCause::SessionEnded(reason))
        }
    }

    /// Плеер доиграл полученное (или зритель закрыл окно): в состоянии «доигрываем»
    /// принимает решение «выход» с запомненной причиной конца. В любом другом
    /// состоянии ничего не меняет — результат различает эти случаи.
    pub fn on_playback_drained(&mut self) -> PlaybackDrainOutcome {
        match self.tracking {
            Tracking::Draining { reason, .. } => {
                self.tracking = Tracking::Finished(ExitCause::SessionEnded(reason));
                PlaybackDrainOutcome::ExitDecided
            }
            Tracking::NothingOpenedYet | Tracking::Active(_) | Tracking::Finished(_) => {
                PlaybackDrainOutcome::NotDraining
            }
        }
    }

    fn remember_early_end(&mut self, instance: SessionInstance, reason: SessionEndReason) {
        if self.early_ends.len() >= MAX_EARLY_ENDS {
            self.early_ends.pop_first();
        }
        self.early_ends.insert(instance, reason);
    }

    /// Решение на момент `now`.
    #[must_use]
    pub fn verdict(&mut self, now: Instant) -> LifecycleVerdict {
        match &self.tracking {
            Tracking::Finished(cause) => LifecycleVerdict::Exit(*cause),
            Tracking::Active(_) => LifecycleVerdict::KeepRunning { wake_at: None },
            Tracking::Draining { .. } => LifecycleVerdict::AwaitPlaybackDrain,
            Tracking::NothingOpenedYet if now >= self.idle_deadline => {
                self.tracking = Tracking::Finished(ExitCause::IdleWithoutSession);
                LifecycleVerdict::Exit(ExitCause::IdleWithoutSession)
            }
            Tracking::NothingOpenedYet => LifecycleVerdict::KeepRunning {
                wake_at: Some(self.idle_deadline),
            },
        }
    }
}

#[cfg(test)]
mod tests;
