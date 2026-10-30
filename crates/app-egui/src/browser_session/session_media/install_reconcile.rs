//! Правило «согласовать после установки» (03b, пакет I; перемотка — 03e).
//!
//! Плеер ставит новое медиа асинхронно: команды, отданные, пока оно ставилось,
//! достаются прежнему медиа, а установка сбрасывает скорость на 1× и берёт
//! «играть/пауза» из загрузки. Поэтому, как только наблюдение впервые показывает
//! медиа поколения, плеер один раз приводится к намерению страницы: скорость,
//! «играть/пауза» и перемотка, отложенная до показа (перемотка страницы после
//! загрузки, но до показа; старт поколения-файла с позиции страницы).

use std::time::Duration;

use player_core::PlaybackState;

use super::SessionMedia;
use crate::browser_session::playback::PlaybackObservation;

impl SessionMedia {
    /// Плеер только что поставил медиа поколения: привести его к странице.
    ///
    /// Установка сбрасывает скорость на 1× и берёт «играть/пауза» из загрузки, а
    /// команды страницы, пришедшие во время установки, достались прежнему медиа.
    /// Команда уходит, только если наблюдение расходится с намерением страницы.
    pub(super) fn reconcile_with_page(
        &mut self,
        observation: &PlaybackObservation,
        seek_when_shown: Option<Duration>,
    ) {
        if observation.rate != self.page_rate {
            let rate = self.page_rate;
            self.command("restore the page rate", |playback| playback.set_rate(rate));
        }
        if let Some(target) = seek_when_shown {
            // Как перемотка страницы после показа: «играть/пауза» — следом (перемотка
            // после `Ended` кончается паузой).
            self.command("seek after install", |playback| playback.seek(target));
            self.apply_page_play_state();
            return;
        }
        let player_plays = matches!(
            observation.state,
            PlaybackState::Playing
                | PlaybackState::Draining
                | PlaybackState::Opening
                | PlaybackState::Buffering
        );
        if self.page_wants_play && observation.state == PlaybackState::Paused {
            self.command("play after install", |playback| playback.play());
        } else if !self.page_wants_play && player_plays {
            self.command("pause after install", |playback| playback.pause());
        }
    }
}
