//! Конец сеанса в медиа-части: доигрывание (03c) и то, что о нём видит окно (04b).
//!
//! Вынесено из `session_media.rs` (предел размера модуля). Владелец — по-прежнему
//! [`SessionMedia`]: здесь его методы о конце сеанса, а чистые правила «доиграно ли» —
//! в [`super::drain`].
//!
//! ```text
//!  Ended{reason} текущего экземпляра ─► ActiveDrain { правила, причина, последний ответ }
//!  главный цикл: «доиграно?» ─► DrainWatch::evaluate ─► Draining / AwaitingViewer / Drained
//!  окно: session_end_view() ─► Silent / WaitingForViewer / Finished (по последнему ответу)
//!                             или StreamError (обрыв 4003: не доигрывается, но не молчит)
//! ```

use std::time::Instant;

use browser_bridge_session::{SessionEndReason, SessionRef};

use super::SessionMedia;
use super::drain::{
    DrainContent, DrainEnd, DrainPolicy, DrainProgress, DrainSnapshot, DrainWatch, IntakeStance,
    PageStance,
};
use crate::browser_session::session_end::{DrainCause, DrainFinish, SessionEndView};

/// Доигрывание закончившегося сеанса.
pub(super) struct ActiveDrain {
    /// Правила «доиграно ли».
    watch: DrainWatch,
    /// Почему кончился сеанс (для сообщения окна).
    ended_because: SessionEndReason,
    /// Последний ответ на «доиграно?» (`None` — цикл ещё не спрашивал).
    last_progress: Option<DrainProgress>,
}

impl SessionMedia {
    /// Та же медиа-часть с другой политикой доигрывания (окно ждёт зрителя на паузе,
    /// безоконный процесс выходит сразу — [`super::PausedDrain`]).
    pub(crate) fn with_drain_policy(mut self, policy: DrainPolicy) -> Self {
        self.drain_policy = policy;
        self
    }

    /// Сеанс закончился: отложенные сообщения его странице больше некому слать, а
    /// всё полученное доигрывается (см. [`Self::playback_drain`]).
    ///
    /// Конец чужого (уже заменённого) экземпляра ничего не трогает. Причина конца
    /// запоминается для сообщения окна; ждёт ли процесс доигрывания, решает машина
    /// выхода (`ProcessLifecycle`), а не медиа-часть.
    pub(crate) fn on_session_ended(&mut self, session: &SessionRef, reason: SessionEndReason) {
        if self.is_current(session) {
            self.outbound.forget_pending_of_ended_session();
            self.drain = Some(ActiveDrain {
                watch: DrainWatch::begin(self.drain_policy),
                ended_because: reason,
                last_progress: None,
            });
        }
    }

    /// Доиграно ли полученное после конца сеанса (зовёт главный цикл, пока машина
    /// выхода в состоянии «доигрываем»).
    ///
    /// Без `on_session_ended` у текущего экземпляра доигрывать нечего — процесс
    /// выходит сразу.
    pub(crate) fn playback_drain(&mut self, now: Instant) -> DrainProgress {
        let intake =
            if self.current_session.is_some() && self.sealed_instance == self.current_session {
                IntakeStance::Sealed
            } else {
                IntakeStance::Open
            };
        let snapshot = DrainSnapshot {
            intake,
            content: self.drain_content(),
            page: if self.page_wants_play {
                PageStance::WantsPlay
            } else {
                PageStance::WantsPause
            },
        };
        match self.drain.as_mut() {
            Some(active) => {
                let progress = active.watch.evaluate(snapshot, now);
                active.last_progress = Some(progress);
                progress
            }
            None => DrainProgress::Drained(DrainEnd::NothingToPlay),
        }
    }

    /// Что окно должно сказать о конце сеанса сейчас (по последнему ответу «доиграно?»).
    ///
    /// Сообщение есть, только пока доигрывание этого сеанса реально идёт (цикл его
    /// спрашивал) и конец — из тех, что доигрываются: так окно не скажет «вкладка
    /// закрыта» про сеанс, закрытый, например, нарушением протокола.
    pub(crate) fn session_end_view(&self) -> SessionEndView {
        let Some(active) = self.drain.as_ref() else {
            return SessionEndView::Silent;
        };
        let Some(cause) = DrainCause::of(active.ended_because) else {
            // Обрыв по ошибке потока не доигрывается, но окно о нём говорит (решение 132).
            return SessionEndView::of_undrained_end(active.ended_because)
                .unwrap_or(SessionEndView::Silent);
        };
        match active.last_progress {
            Some(DrainProgress::AwaitingViewer) => SessionEndView::WaitingForViewer(cause),
            Some(DrainProgress::Drained(end)) => {
                SessionEndView::Finished(DrainFinish { cause, end })
            }
            Some(DrainProgress::Draining { .. }) | None => SessionEndView::Silent,
        }
    }

    /// Идёт ли доигрывание (страница ушла): ей больше ничего не шлём.
    pub(super) const fn is_draining(&self) -> bool {
        self.drain.is_some()
    }

    /// Что держит медиа-часть для вопроса «доиграно?».
    fn drain_content(&self) -> DrainContent {
        // Ждущее поколение новее загруженного и заменит его: пока оно не открылось,
        // конец, отказ или позиция прежнего видео ничего не решают.
        if self.pending.is_some() {
            return DrainContent::Opening;
        }
        let Some(loaded) = self.loaded.as_ref() else {
            return DrainContent::Nothing;
        };
        let shown = self
            .last_observation
            .as_ref()
            .filter(|observation| loaded.is_shown_by(observation));
        DrainContent::Loaded {
            position: shown.map(|observation| observation.position),
            ended: shown.is_some_and(|observation| observation.ended),
            faulted: self
                .last_observation
                .as_ref()
                .and_then(|observation| observation.fault_of(loaded.load_ticket))
                .is_some(),
        }
    }
}
