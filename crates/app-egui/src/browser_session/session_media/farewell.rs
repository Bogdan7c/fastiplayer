//! «Прощание» со страницей (мост, сессия 04b, решения 12, 40, 51, 127).
//!
//! Когда видео уходит из нашего плеера к странице — вкладку заменила другая или
//! зритель закрыл окно, — странице уходит последнее сообщение
//! `return_to_page{T, paused}`: «продолжай с секунды T, на паузе» (решение 51: при
//! закрытии и замене — пауза; кнопка «Return» с игрой — сессия 11+). Сервер
//! гарантирует порядок «очередь исходящих → прощание → кадр закрытия» (решение 127).
//!
//! **Позиция T** — одно правило, то же, что у часов странице ([`PlaybackObservation::shows`]):
//! это позиция плеера, только если наблюдение показывает медиа загруженного поколения.
//! Иначе позиции нет ([`FarewellPosition::Unknown`]) и прощание НЕ отправляется:
//! - ничего не загружено (страница ещё не прислала видео) — у нас нет её позиции,
//!   элемент страницы и так стоит, где стоял;
//! - плеер ещё ставит новое поколение — показанная позиция принадлежит прежнему
//!   `MediaSource`, а часы странице в это время честно говорят «жду» в 0
//!   ([`PlaybackObservation::while_media_installs`]). Послать 0 значило бы отмотать
//!   страницу в начало.
//!
//! Без прощания поведение — как до 04b: замененная вкладка закрывается сервером 4001
//! по сроку коридора, окно — кодом 4000. Странице это не хуже: позиция у неё своя.
//!
//! [`PlaybackObservation::shows`]: crate::browser_session::playback::PlaybackObservation::shows
//! [`PlaybackObservation::while_media_installs`]: crate::browser_session::playback::PlaybackObservation::while_media_installs

use browser_bridge_protocol::session::player::ResumeState;
use browser_bridge_protocol::{MediaTime, PlayerSessionMessage, SessionCloseReason};
use browser_bridge_session::SessionRef;
use tracing::{debug, error, info};

use super::SessionMedia;
use crate::browser_session::outbound::clock_position;

/// Где странице продолжить, когда видео возвращается к ней.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FarewellPosition {
    /// Плеер показывает загруженное поколение на этой позиции (как сказали бы часы).
    Shown(MediaTime),
    /// Честной позиции нет (ничего не загружено или поколение ещё ставится).
    Unknown,
}

/// Что сделало закрытие окна зрителем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewerCloseStep {
    /// Сеанс шёл: странице ушло прощание (или закрытие без него), сеанс закрыт кодом
    /// «плеер закрыт» (4000).
    SessionClosed,
    /// Страницы уже нет (доигрывание после ухода вкладки): прощаться не с кем, закрытие
    /// окна означает «доиграно» — решает машина выхода.
    DrainFinishedByViewer,
    /// Сеанса нет (окно без сеанса не открывается; ветка — страховка).
    NoSession,
}

/// Чем вызвано закрытие с прощанием (только метка журнала, поведение одно).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FarewellOccasion {
    /// Зритель закрыл окно.
    ViewerClosedWindow,
    /// Плеер сам оборвал поток ошибкой (4003).
    StreamError,
}

impl FarewellOccasion {
    const fn log_label(self) -> &'static str {
        match self {
            Self::ViewerClosedWindow => "viewer_closed_window",
            Self::StreamError => "stream_error",
        }
    }
}

/// Прощание: «продолжай с T, на паузе» (решение 51).
fn return_paused_at(position_us: MediaTime) -> PlayerSessionMessage {
    PlayerSessionMessage::ReturnToPage {
        position_us,
        resume: ResumeState::Paused,
    }
}

impl SessionMedia {
    /// Позиция для прощания (правило — в описании модуля). Читает последнее наблюдение
    /// плеера: оно не старше одного прохода цикла, а часы странице уходили не чаще
    /// тика — поэтому T отличается от последних часов не больше чем на тик.
    pub(super) fn farewell_position(&self) -> FarewellPosition {
        let (Some(loaded), Some(observation)) =
            (self.loaded.as_ref(), self.last_observation.as_ref())
        else {
            return FarewellPosition::Unknown;
        };
        if loaded.is_shown_by(observation) {
            FarewellPosition::Shown(clock_position(observation.position))
        } else {
            FarewellPosition::Unknown
        }
    }

    /// Новая вкладка заменила `replaced` (решения 40, 127): ей — прощание «на T, пауза».
    ///
    /// Зовётся главным циклом В ТОМ ЖЕ ШАГЕ, что открытие нового сеанса, и ДО
    /// [`SessionMedia::on_session_opened`]: тот ставит видео на паузу и забывает
    /// загруженное поколение, после чего позиции старой вкладки уже нет.
    ///
    /// Ничего не трогает, если `replaced` — не текущий сеанс медиа-части или его
    /// страница уже ушла (доигрывание): прощаться не с кем.
    pub(crate) fn bid_farewell_to_replaced(&mut self, replaced: &SessionRef) {
        if !self.is_current(replaced) || self.is_draining() {
            debug!("The replaced session is not the one being played; no farewell");
            return;
        }
        let FarewellPosition::Shown(position_us) = self.farewell_position() else {
            info!(
                "Replaced tab gets no farewell: the player shows no position of its video; the server closes it by the corridor deadline"
            );
            return;
        };
        let farewell = return_paused_at(position_us);
        match self.closer.bid_farewell_to_replaced(replaced, &farewell) {
            // Только класс итога: позиция — не приватные данные, но и нужды в ней нет.
            Ok(outcome) => info!(?outcome, "Farewell to the replaced tab"),
            Err(farewell_error) => error!(
                %farewell_error,
                "Farewell to the replaced tab was refused; the server closes it by the corridor deadline"
            ),
        }
    }

    /// Зритель закрыл окно (решения 12, 48, 51).
    ///
    /// - сеанс идёт → `return_to_page{T, paused}` и закрытие кодом 4000 (без позиции —
    ///   просто закрытие); плеер и страница больше ничего не получают;
    /// - страница ушла (доигрываем) → ничего не шлётся, [`ViewerCloseStep::DrainFinishedByViewer`].
    ///
    /// Обычный плеер не трогается: он остаётся на паузе, как его поставил старт сеанса
    /// (решение 48; «играть» обычному плееру процесс не шлёт нигде).
    pub(crate) fn close_for_viewer(&mut self) -> ViewerCloseStep {
        if self.is_draining() {
            return ViewerCloseStep::DrainFinishedByViewer;
        }
        let Some(session) = self.current_session.clone() else {
            return ViewerCloseStep::NoSession;
        };
        self.close_bidding_farewell(
            &session,
            SessionCloseReason::PlayerClosed,
            FarewellOccasion::ViewerClosedWindow,
        );
        ViewerCloseStep::SessionClosed
    }

    /// Закрытие ошибкой потока, которое решил сам плеер (переполнение очереди или
    /// полки, запросов диапазонов, неверное обращение с вводом — код 4003, решение 132).
    ///
    /// Видео возвращается странице: если сеанс текущий и страница ещё с нами —
    /// прощание `return_to_page{T, paused}` и только потом кадр закрытия. Для чужого
    /// (уже заменённого) экземпляра и для доигрывания страница не получает ничего
    /// лишнего: чужому — обычное закрытие по адресу (оно безвредно при `NotActive`),
    /// ушедшей странице — тоже обычное закрытие, прощаться не с кем.
    pub(super) fn close_with_stream_error(&self, session: &SessionRef) {
        let reason = SessionCloseReason::ProtocolViolation;
        if self.is_current(session) && !self.is_draining() {
            self.close_bidding_farewell(session, reason, FarewellOccasion::StreamError);
        } else {
            let outcome = self.closer.close_session(session, reason);
            info!(
                ?reason,
                ?outcome,
                "Session not being played closed by the player without a farewell"
            );
        }
    }

    /// Общее тело «попрощаться и закрыть» (зритель закрыл окно, плеер оборвал поток).
    ///
    /// - позиция известна → одно действие сервера «прощание + кадр закрытия»;
    /// - позиции нет → обычное закрытие, прощания нет (правило — в описании модуля);
    /// - прощание отклонено как «не по протоколу» (дефект) → откат на обычное закрытие,
    ///   сеанс закрывается всё равно, причина — в журнале.
    ///
    /// Порядок на проводе гарантирует сервер: очередь исходящих → прощание → кадр закрытия.
    fn close_bidding_farewell(
        &self,
        session: &SessionRef,
        reason: SessionCloseReason,
        occasion: FarewellOccasion,
    ) {
        let occasion = occasion.log_label();
        match self.farewell_position() {
            FarewellPosition::Shown(position_us) => {
                let farewell = return_paused_at(position_us);
                match self
                    .closer
                    .close_session_with_farewell(session, &farewell, reason)
                {
                    Ok(outcome) => {
                        info!(
                            ?outcome,
                            occasion, "Session closed with a farewell to the page"
                        )
                    }
                    Err(farewell_error) => {
                        let outcome = self.closer.close_session(session, reason);
                        error!(
                            %farewell_error,
                            ?outcome,
                            occasion,
                            "Farewell to the page was refused; the session is closed without it"
                        );
                    }
                }
            }
            FarewellPosition::Unknown => {
                let outcome = self.closer.close_session(session, reason);
                info!(
                    ?outcome,
                    occasion,
                    "Player showed no video position; the session is closed without a farewell"
                );
            }
        }
    }
}
