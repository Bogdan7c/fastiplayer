//! Отказ от поколения: странице `stream_rejected`, зрителю окна — короткое сообщение
//! (мост, 03b; отказ плеера и сообщение зрителю — сессия 04b, решения 128–129).
//!
//! Вынесено из `session_media.rs` (предел размера модуля). Владелец — по-прежнему
//! [`SessionMedia`]: здесь его единая точка отказа [`SessionMedia::reject`] и перевод
//! отказа плеера в причину протокола.
//!
//! ```text
//!  поток приёма (Reject), открытие (Failed/Unplayable), плеер (load_fault_of) ─► reject
//!     ├─ ждущее открытие этого поколения снято
//!     ├─ доигрываем → странице некому сказать: только журнал
//!     └─ PageOutbound::reject_generation (раз на поколение) ─► stream_rejected
//!            └─ зритель ещё не знает → сообщение окну (take_generation_rejection_notice)
//! ```
//!
//! Инварианты:
//! - отказ плеера принадлежит своей загрузке (`LoadLedger`): отмена установки, которую
//!   заменило более новое поколение, — не отказ, поэтому намеренная замена поколения
//!   странице ложного `stream_rejected` не шлёт;
//! - сообщение зрителю — ровно тогда, когда странице действительно ушёл отказ (не при
//!   повторе того же поколения и не во время доигрывания);
//! - отказ, о котором плеер сообщил сам (событие ошибки или `Failed`), окно уже показало
//!   обычным путём `AppState` — второго сообщения на то же событие нет.

use browser_bridge_protocol::StreamGeneration;
use browser_bridge_protocol::session::player::StreamRejection;
use tracing::{info, warn};

use super::SessionMedia;
use crate::browser_session::playback::{BrowserPlaybackFault, FaultAnnouncement, LoadFault};
use crate::browser_session::rejection_notice::GenerationRejectionNotice;

/// Знает ли зритель окна об этом отказе без нашего сообщения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewerAwareness {
    /// Не знает: отказ решили мы (поток приёма, открытие) или плеер не публиковал его
    /// ошибкой (установка). Окну нужно сообщение.
    Unaware,
    /// Плеер сам сообщил об ошибке, окно её уже показало.
    ToldByPlayer,
}

impl ViewerAwareness {
    /// По тому, сообщил ли плеер об отказе сам.
    const fn of(announcement: FaultAnnouncement) -> Self {
        match announcement {
            FaultAnnouncement::AnnouncedByPlayer => Self::ToldByPlayer,
            FaultAnnouncement::NotAnnounced => Self::Unaware,
        }
    }
}

/// Причина протокола для отказа плеера.
///
/// «Видео нам не по силам» — свойство байтов (`unsupported_codec`); всё остальное —
/// плеер не смог поставить или играть приемлемые байты (`playback_failed`, решение 128).
pub(super) const fn rejection_for_fault(fault: &BrowserPlaybackFault) -> StreamRejection {
    match fault {
        BrowserPlaybackFault::UnsupportedVideo => StreamRejection::UnsupportedCodec,
        BrowserPlaybackFault::VideoDecoderStartFailed
        | BrowserPlaybackFault::PlaybackFailed(_)
        | BrowserPlaybackFault::InstallAborted => StreamRejection::PlaybackFailed,
    }
}

impl SessionMedia {
    /// Отказ от поколения: странице `stream_rejected`; ждущее открытие этого
    /// поколения больше не нужно; зрителю окна — сообщение, если он об отказе не знает.
    pub(super) fn reject(
        &mut self,
        generation: StreamGeneration,
        reason: StreamRejection,
        awareness: ViewerAwareness,
    ) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation == generation)
        {
            self.pending = None;
            // Клик зрителя относился к этому поколению: повторять его после показа
            // какого-то следующего поколения было бы чужой волей (решение 149).
            self.viewer_will.click_claim().forget();
        }
        if self.drain.is_some() {
            // Сеанс кончился: сказать отказ некому (отложенное повторялось бы вечно).
            info!(
                generation = generation.get(),
                ?reason,
                "Stream generation rejected after the session ended"
            );
            return;
        }
        if self.outbound.reject_generation(generation, reason) {
            info!(
                generation = generation.get(),
                ?reason,
                "Stream generation rejected"
            );
            if awareness == ViewerAwareness::Unaware {
                // Несколько отказов за шаг — окно скажет о последнем (самом новом).
                self.rejection_notice = GenerationRejectionNotice::Rejected(reason);
            }
        }
    }

    /// Отказ плеера на загруженном поколении → отказ поколения странице (решение 128).
    pub(super) fn report_fault(&mut self, generation: StreamGeneration, load_fault: &LoadFault) {
        let reason = rejection_for_fault(&load_fault.fault);
        if reason == StreamRejection::PlaybackFailed {
            // Только вид отказа: текст ошибки может нести подробности байтов страницы.
            warn!(
                fault = ?load_fault.fault,
                generation = generation.get(),
                "Browser stream playback failed"
            );
        }
        self.reject(
            generation,
            reason,
            ViewerAwareness::of(load_fault.announcement),
        );
    }

    /// Что сказать зрителю окна об отказах с прошлого вызова; забирает сказанное
    /// (каждый отказ окно показывает один раз). Безоконный процесс это не читает.
    pub(crate) fn take_generation_rejection_notice(&mut self) -> GenerationRejectionNotice {
        std::mem::replace(
            &mut self.rejection_notice,
            GenerationRejectionNotice::Nothing,
        )
    }
}
