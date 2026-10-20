//! Play/Pause во время одноразового seek-а: смена resume intent вместо отмены.
//!
//! Решение владельца (2026-10-20, вариант A): если пользователь жмёт Play/Pause
//! (Space, кнопка, MPRIS), пока обычная перемотка ещё «садится», перемотка не
//! отменяется. Команда меняет только то, в каком состоянии playback окажется *после*
//! commit-а: Playing или Paused, но уже на новой позиции.
//!
//! Почему не отмена: к моменту команды demuxer и decoder уже переставлены к новой
//! цели, а часы и позиция — ещё нет. Отмена landing-а бросала playback в `Playing`
//! на старой позиции при декодере у новой цели: кадры выбрасывались как опоздавшие,
//! картинка замерзала (тот же класс бага, что restart в коммите 20eed77f).
//!
//! Граница: живой scrub-жест (drag по timeline до EndScrub) сохраняет прежнюю
//! cancel-first семантику — это решает `PlaybackCommandSeekRoute::ScrubGestureActive`.

use tracing::debug;

use crate::seek_state::{InFlightSeekResumeIntentRetarget, PlaybackResumeIntent};

use super::PlayerSession;

/// Как playback-команда (Play/Pause/Toggle) должна обойтись с seek-ом в полёте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackCommandSeekRoute {
    /// Seek-а в полёте нет: обычный Play/Pause.
    NoSeekInFlight,

    /// Seek принадлежит ещё не отпущенному scrub-жесту: прежняя cancel-first семантика.
    ScrubGestureActive,

    /// Одноразовый seek в полёте: команда меняет только его resume intent.
    OneShotSeekInFlight {
        /// Намерение, которое seek применит при commit-е, если команд больше не будет.
        resume_intent: PlaybackResumeIntent,
    },
}

/// Чем закончилась попытка отдать Play/Pause одноразовому seek-у в полёте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OneShotSeekPlaybackCommand {
    /// Seek принял намерение как resume intent; отменять и запускать ничего не нужно.
    ResumeIntentUpdated,

    /// Одноразового seek-а нет (или его держит scrub-жест): нужен обычный путь команды.
    NotApplicable,
}

impl PlayerSession {
    /// Классифицирует seek в полёте для Play/Pause/Toggle, ничего не меняя.
    #[must_use]
    pub(super) fn playback_command_seek_route(&self) -> PlaybackCommandSeekRoute {
        if self.seek_runtime.in_flight_seek_belongs_to_scrub_gesture() {
            return PlaybackCommandSeekRoute::ScrubGestureActive;
        }
        match self.in_flight_seek_resume_intent() {
            Some(resume_intent) => PlaybackCommandSeekRoute::OneShotSeekInFlight { resume_intent },
            None => PlaybackCommandSeekRoute::NoSeekInFlight,
        }
    }

    /// Resume intent seek-а в полёте с учётом всех носителей.
    ///
    /// Seek-runtime держит intent SeekLanding-а/commit-а, а worker-receipted seek до
    /// receipt-а — только `prepared_demux_seek`. Без второго источника Play/Pause и
    /// новый seek, пришедшие до receipt-а, не видели бы seek в полёте.
    #[must_use]
    pub(super) fn in_flight_seek_resume_intent(&self) -> Option<PlaybackResumeIntent> {
        self.seek_runtime
            .in_flight_seek_resume_intent()
            .or_else(|| self.prepared_demux_seek.pending_resume_intent())
    }

    /// Отдаёт Play/Pause одноразовому seek-у в полёте, если такой есть.
    ///
    /// Public state не трогается: во время reused-decoder SeekLanding он `Scrubbing`
    /// (от него зависит decode-предикат `seek_landing_decode_active`), во время commit-а
    /// без landing-а — `Seeking`. Итоговое Playing/Paused публикует штатный commit,
    /// audio возобновляется только через его preroll gate.
    pub(super) fn apply_playback_command_to_one_shot_seek(
        &mut self,
        resume_intent: PlaybackResumeIntent,
    ) -> OneShotSeekPlaybackCommand {
        let PlaybackCommandSeekRoute::OneShotSeekInFlight { .. } =
            self.playback_command_seek_route()
        else {
            return OneShotSeekPlaybackCommand::NotApplicable;
        };
        let InFlightSeekResumeIntentRetarget::Retargeted { previous } =
            self.retarget_in_flight_seek_resume_intent(resume_intent)
        else {
            // Классификация выше уже доказала seek в полёте; сюда попасть нельзя, но
            // обычный путь команды безопаснее молчаливого no-op.
            return OneShotSeekPlaybackCommand::NotApplicable;
        };
        if resume_intent == PlaybackResumeIntent::Pause {
            // Audio и так остановлен стартом seek-а; повтор идемпотентен и сохраняет
            // инвариант «Pause никогда не оставляет звук играть».
            self.pause_audio_output_for_seek();
        }
        debug!(
            previous_resume_intent = ?previous,
            resume_intent = ?resume_intent,
            target_position = ?self.snapshot.timeline.target_position,
            playback_state = ?self.snapshot.playback_state,
            "Playback command retargeted in-flight seek resume intent"
        );
        OneShotSeekPlaybackCommand::ResumeIntentUpdated
    }

    /// Меняет resume intent seek-а в полёте у всех его владельцев сразу.
    ///
    /// Не проверяет scrub-жест: вызывающий уже решил, что intent менять можно.
    pub(super) fn retarget_in_flight_seek_resume_intent(
        &mut self,
        resume_intent: PlaybackResumeIntent,
    ) -> InFlightSeekResumeIntentRetarget {
        let seek_runtime_outcome = self
            .seek_runtime
            .retarget_in_flight_seek_resume_intent(resume_intent);
        let worker_receipt_outcome = self
            .prepared_demux_seek
            .retarget_pending_resume_intent(resume_intent);
        match (seek_runtime_outcome, worker_receipt_outcome) {
            (retargeted @ InFlightSeekResumeIntentRetarget::Retargeted { .. }, _)
            | (_, retargeted @ InFlightSeekResumeIntentRetarget::Retargeted { .. }) => retargeted,
            _ => InFlightSeekResumeIntentRetarget::NoSeekInFlight,
        }
    }
}
