//! Учёт загрузок плеера браузерного режима: чья загрузка показана и чей отказ.
//!
//! Плеер один, медиа сменяют друг друга, а снимок и события плеера ещё какое-то время
//! описывают ПРЕЖНЕЕ медиа. Поэтому (ревью 03b, пакет I):
//! - отказ всегда приписан загрузке ([`LoadFault`]) и только той, чьё медиа его дало:
//!   отказ установки — по квитанции установки; `Failed` в снимке и ошибки в событиях —
//!   только если их экземпляр медиа (`MediaInstanceId`) — установленный экземпляр
//!   ТЕКУЩЕЙ загрузки; отказ прежнего медиа новой загрузке не достаётся никогда;
//! - наблюдение относится к загрузке, только если снимок показывает её экземпляр
//!   ([`ShownMediaTracker`]);
//! - ошибка «видео нам не по силам» от экземпляра, о котором итог установки ещё не
//!   сообщил, не теряется: плеер сообщает итог установки в момент коммита, а события
//!   нового медиа идут сразу следом — читатель (окно продвигает установку по этапу за
//!   проход, безоконный плеер забирает итог до событий) может увидеть событие раньше
//!   итога. Такие экземпляры учёт помнит ([`UnclaimedVideoErrors`]) и приписывает
//!   ошибку загрузке, когда узнаёт, что экземпляр — её (04a, ревью пакета P3);
//! - отмена установки различается явно ([`CancelledInstall`]): отмена заменённой
//!   загрузки — норма, отмена ТЕКУЩЕЙ (остановка, структурная отмена до барьера,
//!   установка пропала без итога) — её отказ, иначе сеанс ждал бы её вечно.
//!
//! Учёт не знает, КАК медиа попадает в плеер: безоконный [`super::BrowserPlayback`]
//! грузит через свой `PlayerWorker`, окно браузерного режима (04a) — через обычный путь
//! открытия `AppState`. Оба сообщают сюда одно и то же: «загрузка начата», «установлена
//! как экземпляр N», «не установилась», события плеера и снимок.

use std::collections::VecDeque;

use player_core::{MediaInstanceId, PlaybackState, PlayerErrorKind, PlayerSnapshot};

use super::media_identity::ShownMediaTracker;
use super::{BrowserPlaybackFault, LoadFault, MediaLoadTicket, PlaybackObservation};

/// Учёт загрузок одного плеера.
#[derive(Debug, Default)]
pub(crate) struct LoadLedger {
    /// Отказ текущей загрузки (отказ прежних забывается при новой загрузке).
    fault: Option<LoadFault>,
    /// Последняя начатая загрузка: только ей приписываются отказы.
    current_load: Option<MediaLoadTicket>,
    /// Какая установленная загрузка сейчас в снимке плеера.
    shown_media: ShownMediaTracker,
    /// Экземпляры, сообщившие «видео не по силам» раньше итога своей установки.
    unclaimed_video_errors: UnclaimedVideoErrors,
}

/// Чем для учёта оказалась отменённая установка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelledInstall {
    /// Загрузку уже заменила более новая: отмена — норма «новее заменяет», не отказ.
    ReplacedByNewerLoad,
    /// Отменена текущая загрузка, которую никто не заменял: это её отказ
    /// ([`BrowserPlaybackFault::InstallAborted`]).
    CurrentLoadAborted,
}

impl LoadLedger {
    /// Начата новая загрузка: отказ прежней забыт, отказы дальше — только её.
    pub(crate) fn begin_load(&mut self, ticket: MediaLoadTicket) {
        self.fault = None;
        self.current_load = Some(ticket);
        // Экземпляр новой загрузки появится только после её начала: всё запомненное
        // раньше — ошибки прежних медиа, новой они не принадлежат.
        self.unclaimed_video_errors.forget_all();
    }

    /// Плеер установил медиа загрузки `ticket` как экземпляр `instance`.
    ///
    /// Если этот экземпляр уже успел сообщить «видео не по силам» (событие обогнало
    /// итог установки), отказ приписывается загрузке сейчас.
    pub(crate) fn note_installed(&mut self, ticket: MediaLoadTicket, instance: MediaInstanceId) {
        self.shown_media.note_installed(ticket, instance);
        if Some(ticket) != self.current_load {
            // Поздняя установка заменённой загрузки: экземпляр текущей ещё впереди,
            // запомненные ошибки ждут его.
            return;
        }
        if self.unclaimed_video_errors.claim(instance) {
            self.note_fault(ticket, BrowserPlaybackFault::UnsupportedVideo);
        }
        // Экземпляр текущей загрузки известен: остальным запомненным ждать некого.
        self.unclaimed_video_errors.forget_all();
    }

    /// Установка загрузки `ticket` отменена. Ответ говорит, отказ ли это (см.
    /// [`CancelledInstall`]); отказ текущей загрузки учёт записывает сам.
    pub(crate) fn note_install_cancelled(&mut self, ticket: MediaLoadTicket) -> CancelledInstall {
        if Some(ticket) == self.current_load {
            self.note_fault(ticket, BrowserPlaybackFault::InstallAborted);
            CancelledInstall::CurrentLoadAborted
        } else {
            CancelledInstall::ReplacedByNewerLoad
        }
    }

    /// Медиа загрузки `ticket` не установилось: плеер остался без него.
    pub(crate) fn note_install_failed(&mut self, ticket: MediaLoadTicket, kind: PlayerErrorKind) {
        self.note_fault(ticket, fault_for_error_kind(kind));
    }

    /// Приписывает отказ загрузке (только текущей; прежние уже забыты).
    pub(crate) fn note_fault(&mut self, ticket: MediaLoadTicket, fault: BrowserPlaybackFault) {
        if Some(ticket) == self.current_load {
            self.fault = Some(LoadFault {
                load: ticket,
                fault,
            });
        }
    }

    /// Отказ текущей загрузке без сверки экземпляра: плеер просил декодер в момент
    /// установки, когда событие ещё несёт экземпляр прежнего медиа (см.
    /// `BrowserPlayback::answer_video_backend_request`).
    pub(crate) fn note_current_load_fault(&mut self, fault: BrowserPlaybackFault) {
        if let Some(ticket) = self.current_load {
            self.note_fault(ticket, fault);
        }
    }

    /// Плеер сообщил об ошибке экземпляра `instance`. Отказом текущей загрузки она
    /// становится, только если это «видео нам не по силам» и экземпляр — её.
    ///
    /// Экземпляр, о котором итог установки ещё не сообщил, может оказаться экземпляром
    /// текущей загрузки: ошибка запоминается до [`Self::note_installed`].
    pub(crate) fn note_player_error(
        &mut self,
        instance: Option<MediaInstanceId>,
        kind: &PlayerErrorKind,
    ) {
        if !is_unsupported_video(kind) {
            return;
        }
        let Some(instance) = instance else {
            // Без экземпляра ошибку не связать ни с одной загрузкой.
            return;
        };
        match self.shown_media.shown_load(Some(instance)) {
            Some(load) if Some(load) == self.current_load => {
                self.note_fault(load, BrowserPlaybackFault::UnsupportedVideo);
            }
            // Установленный экземпляр заменённой загрузки: прежнее медиа, не наше.
            Some(_) => {}
            // Неизвестный экземпляр: ждём итога установки текущей загрузки.
            None if self.current_load.is_some() => {
                self.unclaimed_video_errors.remember(instance);
            }
            None => {}
        }
    }

    /// Свежий снимок плеера: `Failed` экземпляра текущей загрузки — её отказ (если
    /// другого ещё нет).
    pub(crate) fn note_snapshot(&mut self, snapshot: &PlayerSnapshot) {
        if snapshot.playback_state != PlaybackState::Failed || self.fault.is_some() {
            return;
        }
        if let Some(load) = self.current_load_of_instance(snapshot.media_instance_id) {
            let kind = snapshot
                .last_error
                .as_ref()
                .map_or(PlayerErrorKind::RuntimeError, |error| error.kind.clone());
            self.fault = Some(LoadFault {
                load,
                fault: fault_for_error_kind(kind),
            });
        }
    }

    /// Наблюдение для медиа-части по снимку плеера.
    pub(crate) fn observation(&self, snapshot: &PlayerSnapshot) -> PlaybackObservation {
        PlaybackObservation {
            position: snapshot.current_position,
            duration: snapshot.duration,
            state: snapshot.playback_state,
            rate: snapshot.playback_rate,
            buffering: snapshot.playback_state == PlaybackState::Buffering,
            ended: snapshot.playback_state == PlaybackState::Ended,
            fault: self.fault.clone(),
            shown_load: self.shown_media.shown_load(snapshot.media_instance_id),
        }
    }

    /// Загрузка, которой принадлежит экземпляр медиа плеера, — если это ТЕКУЩАЯ
    /// загрузка и плеер уже сообщил, что поставил её медиа. Иначе `None`: экземпляр
    /// прежнего медиа (или неизвестный) текущей загрузке не принадлежит.
    fn current_load_of_instance(
        &self,
        instance: Option<MediaInstanceId>,
    ) -> Option<MediaLoadTicket> {
        self.shown_media
            .shown_load(instance)
            .filter(|load| Some(*load) == self.current_load)
    }
}

/// Сколько неопознанных экземпляров с ошибкой видео помнить.
///
/// Между началом загрузки и итогом её установки новые экземпляры дают только поздняя
/// установка заменённой загрузки и сама текущая — единицы. Потолок — защита памяти
/// от сбойного плеера, а не рабочий предел; вытесняется самый старый.
const UNCLAIMED_VIDEO_ERRORS_CAPACITY: usize = 4;

/// Экземпляры медиа, сообщившие «видео не по силам» раньше, чем учёт узнал, чьи они.
#[derive(Debug, Default)]
struct UnclaimedVideoErrors {
    instances: VecDeque<MediaInstanceId>,
}

impl UnclaimedVideoErrors {
    /// Запомнить экземпляр (повтор не дублируется; при переполнении старейший забыт).
    fn remember(&mut self, instance: MediaInstanceId) {
        if self.instances.contains(&instance) {
            return;
        }
        if self.instances.len() >= UNCLAIMED_VIDEO_ERRORS_CAPACITY
            && let Some(evicted) = self.instances.pop_front()
        {
            tracing::debug!(
                ?evicted,
                "Too many unclaimed video errors; forgetting the oldest media instance"
            );
        }
        self.instances.push_back(instance);
    }

    /// Сообщал ли `instance` об ошибке видео (запись при этом забывается).
    fn claim(&mut self, instance: MediaInstanceId) -> bool {
        let position = self.instances.iter().position(|known| *known == instance);
        position
            .and_then(|index| self.instances.remove(index))
            .is_some()
    }

    /// Забыть всё.
    fn forget_all(&mut self) {
        self.instances.clear();
    }
}

/// Ошибки «видео этой дорожки нам не по силам».
pub(crate) const fn is_unsupported_video(kind: &PlayerErrorKind) -> bool {
    matches!(
        kind,
        PlayerErrorKind::UnsupportedVideoCodec
            | PlayerErrorKind::UnsupportedVideoProfile
            | PlayerErrorKind::UnsupportedVideoBitDepth
            | PlayerErrorKind::UnsupportedVideoChroma
            | PlayerErrorKind::UnsupportedHdrMode
            | PlayerErrorKind::UnsupportedRenderFormat
    )
}

/// Отказ по виду ошибки, с которой плеер остановился.
pub(crate) fn fault_for_error_kind(kind: PlayerErrorKind) -> BrowserPlaybackFault {
    if is_unsupported_video(&kind) {
        BrowserPlaybackFault::UnsupportedVideo
    } else {
        BrowserPlaybackFault::PlaybackFailed(kind)
    }
}

#[cfg(test)]
#[path = "load_ledger_tests.rs"]
mod tests;
