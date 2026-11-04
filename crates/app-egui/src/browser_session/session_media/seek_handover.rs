//! Перемотка, отданная плееру, пока он на неё не ответил (живая проверка 09b2, тик «0»).
//!
//! Перемотка уходит плееру командой в очередь его потока. Страница при этом уже на цели, а
//! наблюдение описывает мир ДО команды: старое место, иногда начало только что показанного
//! медиа. Часы протокола без поколения и номера перемотки, а тень без своего удержания
//! (T0 передачи) верит любому тику: тик старого места отмотал бы её, а прощание вернуло бы
//! зрителя туда же.
//!
//! **Правило.** Перемотка, отданная плееру, — долг медиа-части перед страницей: от команды до
//! ответа плеера страница слышит цель. Ответ — не позиция: перемотка законно может сесть
//! далеко от цели (эфир на новой шкале, обрезка окна), и сравнение чисел либо спрятало бы
//! настоящий прыжок, либо зависло бы на нём. Ответ — счётчик [`PlayerSeekAnswers`]:
//! принята или отвергнута. После ответа наблюдение уже знает о перемотке: идёт посадка
//! ([`PlaybackObservation::landing_target`]) или она кончилась, и правда снова у плеера.
//!
//! **Владение.** Долг лежит на загруженном поколении: новое поколение заменяет его вместе с
//! долгом, чужая перемотка новому поколению не достаётся.
//!
//! **Потолок ожидания.** Ответ приходит событием, а очередь событий плеера конечна. Потерянный
//! ответ (`SEEK_ANSWER_PATIENCE`) не оставит часы на цели навсегда: долг снимается с записью в
//! журнал, страница снова слышит плеер, как и до появления этого правила.
//!
//! [`PlaybackObservation::landing_target`]: crate::browser_session::playback::PlaybackObservation::landing_target

use std::time::{Duration, Instant};

use tracing::warn;

use super::{LoadedGeneration, SessionMedia};
use crate::browser_session::playback::{PlaybackObservation, PlayerSeekAnswers};

/// Сколько ждать ответа плеера на перемотку. Обычно это миллисекунды очереди его потока;
/// потолок рассчитан не на нагрузку, а на потерянное событие.
pub(super) const SEEK_ANSWER_PATIENCE: Duration = Duration::from_secs(5);

/// Перемотка, отданная плееру: ответа на неё ещё не видно.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UnansweredSeek {
    /// Куда страница уже пришла.
    target: Duration,
    /// Сколько ответов плеера было видно при отправке: ответ на эту команду изменит значение.
    answers_when_sent: PlayerSeekAnswers,
    /// Когда перестать ждать (ставит первый проход после отправки: команды приходят и между
    /// проходами, у них своего времени нет).
    give_up_at: Option<Instant>,
}

impl LoadedGeneration {
    /// Сверяет отданную перемотку с наблюдением: ответ плеера (или истёкший потолок) снимает
    /// долг. Новая отправка заменяет старый долг сама, поэтому здесь снимается только последний.
    pub(super) fn settle_unanswered_seek(&mut self, answers: PlayerSeekAnswers, now: Instant) {
        let Some(seek) = self.unanswered_seek.as_mut() else {
            return;
        };
        if answers != seek.answers_when_sent {
            self.unanswered_seek = None;
            return;
        }
        if now >= *seek.give_up_at.get_or_insert(now + SEEK_ANSWER_PATIENCE) {
            warn!(
                target_us = seek.target.as_micros(),
                "The player never answered a seek; the page clock follows the player again"
            );
            self.unanswered_seek = None;
        }
    }

    /// Цель перемотки, на которую плеер ещё не ответил.
    pub(super) fn unanswered_seek_target(&self) -> Option<Duration> {
        self.unanswered_seek.map(|seek| seek.target)
    }

    /// Где поколение стоит для страницы (часы и прощание говорят одно): отданная перемотка
    /// или посадка — их цель, иначе позиция плеера.
    pub(super) fn page_position(&self, observation: &PlaybackObservation) -> Duration {
        self.unanswered_seek_target()
            .unwrap_or_else(|| observation.page_clock_position())
    }
}

impl SessionMedia {
    /// Отдаёт перемотку плееру и записывает долг перед страницей. Единственная дорога
    /// перемоток загруженного поколения: страница слышит цель с этого момента.
    ///
    /// Команду, которую плеер не принял, ждать некому: пишем в журнал, долга нет.
    pub(super) fn hand_seek_to_player(&mut self, what: &'static str, target: Duration) {
        if let Err(command_error) = self.playback.seek(target) {
            warn!(%command_error, command = what, "Browser session player command failed");
            return;
        }
        let answers_when_sent = self.seek_answers_seen;
        if let Some(loaded) = self.loaded.as_mut() {
            loaded.unanswered_seek = Some(UnansweredSeek {
                target,
                answers_when_sent,
                give_up_at: None,
            });
        }
    }
}
