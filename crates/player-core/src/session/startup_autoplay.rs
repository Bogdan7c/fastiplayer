//! Стартовый autoplay preroll как явное состояние session.
//!
//! Зачем отдельное состояние, а не вывод из `playback_state == Buffering`: короткий файл
//! (весь помещается в decoder/pending очереди) демуксится до EOF раньше, чем decoder
//! отдаёт первый кадр. Session уходит в EOF drain (`Draining`) ещё во время стартового
//! preroll-а — и по одному `playback_state` уже не видно, что первый старт (якорь часов,
//! запуск audio stream-а) так и не случился. Раньше из-за этого часы не запускались, а
//! запоздавший `Play` трактовался как «повтор после конца» и делал seek в 0.
//!
//! Выбранная семантика (наблюдаемые состояния): `Buffering → Draining → Ended`.
//! Session остаётся в `Draining` (EOF drain продолжает владеть хвостом и концом файла),
//! а когда первый кадр (и звук, если есть) готов, выполняется тот же старт, что и при
//! `Buffering → Playing`: запуск audio output с `AudioPlaybackResumed`, якорь no-audio
//! часов. Отдельного `Playing` в середине нет: переход `Draining → Playing` выключил бы
//! EOF drain и снова включил бы demux уже дочитанного источника. Для всех потребителей
//! `Draining` и так означает «идёт воспроизведение» (кнопка паузы, MPRIS, очередь).
//!
//! Пока длится такой drain первого просмотра (старт ещё ждёт или уже выполнен внутри
//! него), `Play`/D52 `StartPlaying` не считается повтором: replay-seek в 0 остаётся только
//! для drain-а после обычного старта и для настоящего `Ended`.

use std::time::Instant;

use tracing::{debug, info};

use crate::PlaybackState;
use crate::pipeline::AudioEofDrainState;

use super::PlayerSession;

/// Стадия стартового autoplay preroll-а текущего media.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum StartupAutoplayPreroll {
    /// Старта нет: media на паузе, уже запущен, или стартом владеет кто-то другой (seek).
    #[default]
    NotPending,

    /// Media установлен с намерением играть, но первый старт ещё не выполнен.
    Pending,

    /// Первый старт выполнен уже внутри EOF drain (короткий файл): текущий drain — это
    /// всё ещё первый просмотр, а не «конец просмотренного файла». `Play` здесь не повтор.
    StartedDuringEofDrain,
}

impl StartupAutoplayPreroll {
    /// Синхронизирует стадию с high-level playback state.
    ///
    /// Старт может ждать только в `Buffering` (обычный preroll) и в `Draining` (EOF пришёл
    /// раньше первого кадра); `StartedDuringEofDrain` живёт только в `Draining`. Любой
    /// другой переход — `Playing`, `Paused`, `Seeking`, `Scrubbing`, `Stopped`, `Ended`,
    /// `Opening`, `Error` — значит, что старт выполнен либо им теперь владеет другой
    /// lifecycle (seek с resume intent, пауза, сброс media). Поэтому стадия не переживает
    /// seek, паузу, stop, reset, `Ended` и установку нового media.
    fn sync_from_playback_state(&mut self, playback_state: PlaybackState) {
        let stage_survives = match (*self, playback_state) {
            (Self::Pending, PlaybackState::Buffering | PlaybackState::Draining)
            | (Self::StartedDuringEofDrain, PlaybackState::Draining) => true,
            (Self::NotPending | Self::Pending | Self::StartedDuringEofDrain, _) => false,
        };
        if !stage_survives {
            *self = Self::NotPending;
        }
    }
}

impl PlayerSession {
    /// Отмечает, что media установлен с намерением играть и ждёт первого старта.
    pub(super) fn mark_startup_autoplay_preroll_pending(&mut self) {
        self.startup_autoplay = StartupAutoplayPreroll::Pending;
    }

    /// Явно сбрасывает стадию старта (смена media).
    pub(super) fn clear_startup_autoplay_preroll(&mut self) {
        self.startup_autoplay = StartupAutoplayPreroll::NotPending;
    }

    /// Вызывается из единственной точки смены playback state (`set_playback_state`).
    pub(super) fn sync_startup_autoplay_from_playback_state(
        &mut self,
        playback_state: PlaybackState,
    ) {
        self.startup_autoplay
            .sync_from_playback_state(playback_state);
    }

    /// `true`, пока первый старт текущего media ещё не выполнен.
    #[must_use]
    pub(crate) fn startup_autoplay_preroll_pending(&self) -> bool {
        self.startup_autoplay == StartupAutoplayPreroll::Pending
    }

    /// `true`, если EOF drain начался раньше, чем стартовый preroll запустил playback.
    ///
    /// Активный seek commit владеет lifecycle сам (и обычно уже сбросил стадию через
    /// `Seeking`), поэтому здесь он явно исключён.
    pub(super) fn startup_autoplay_pending_during_eof_drain(&self) -> bool {
        self.is_eof_draining()
            && self.startup_autoplay_preroll_pending()
            && !self.has_active_seek_commit()
    }

    /// `true`, пока старт, отложенный в EOF drain, ждёт первый кадр (или отказ от него).
    ///
    /// Без выбранного видео ждать нечего: audio-only старт решает только audio gate.
    pub(super) fn startup_autoplay_waits_for_first_video_frame_in_eof_drain(&self) -> bool {
        self.startup_autoplay_pending_during_eof_drain() && !self.autoplay_video_preroll_ready()
    }

    /// `true`, пока decoder ещё не выдал первый кадр текущего startup/buffering preroll-а.
    ///
    /// Объединяет обычный голодный preroll (`Buffering`, см. `video_preroll_is_starved`) и
    /// старт, который из-за раннего EOF ждёт первый кадр внутри `Draining`. Intent-метод для
    /// video decode-ahead policy: пока кадра нет, audio clock стоит, и временное окно
    /// относительно него не должно морить frame-threaded decoder. Seek сюда не попадает.
    pub(crate) fn video_first_frame_is_awaited(&self) -> bool {
        self.video_preroll_is_starved()
            || self.startup_autoplay_waits_for_first_video_frame_in_eof_drain()
    }

    /// `true`, пока идёт EOF drain первого просмотра: старт ещё ждёт либо уже выполнен
    /// внутри этого drain-а. Такой drain — не «конец просмотренного файла».
    pub(super) fn eof_drain_belongs_to_startup_playthrough(&self) -> bool {
        self.is_eof_draining()
            && matches!(
                self.startup_autoplay,
                StartupAutoplayPreroll::Pending | StartupAutoplayPreroll::StartedDuringEofDrain
            )
            && !self.has_active_seek_commit()
    }

    /// Уже выполняется ли намерение «играть» без нового `Play`.
    ///
    /// Для D52 intent `StartPlaying` сразу после install: и обычный preroll, и старт в
    /// EOF drain первого просмотра уже ведут к воспроизведению; повторный `Play` здесь
    /// лишний, а в drain он раньше превращался в replay-seek.
    ///
    /// То же для drain-а, который доигрывает уже начатое воспроизведение: хвост ролика
    /// играет. D52 intent — это состояние «играть», а не нажатие Play; replay-seek здесь
    /// (например, intent старому instance-у во время смены web-версии звука у самого конца)
    /// перематывал старый playback в начало и ломал staged reopen («moved backward»).
    pub(super) fn start_playing_intent_already_in_progress(&self) -> bool {
        self.snapshot.playback_state == PlaybackState::Buffering
            || self.eof_drain_belongs_to_startup_playthrough()
            || self.eof_drain.continues_started_playback()
    }

    /// Play во время EOF drain первого просмотра присоединяется к старту.
    ///
    /// Возвращает `true`, если команда поглощена: replay-seek здесь запрещён, иначе
    /// стартовая готовность была бы подменена повтором файла. Play после настоящего
    /// `Ended` (стадия уже сброшена) по-прежнему повторяет файл с начала.
    pub(super) fn play_joins_startup_playthrough_in_eof_drain(&self) -> bool {
        if !self.eof_drain_belongs_to_startup_playthrough() {
            return false;
        }
        debug!(
            startup_still_pending = self.startup_autoplay_preroll_pending(),
            "Play during first-playthrough EOF drain joins startup; no replay"
        );
        true
    }

    /// Выполняет отложенный старт внутри EOF drain, когда первый кадр и звук готовы.
    ///
    /// Возвращает `true`, если старт выполнен в этом вызове. Звук запускается тем же
    /// EOF-tail путём (`start_eof_audio_tail_if_needed`), что и раньше: он публикует
    /// `AudioPlaybackResumed` один раз, а при ошибке play сбрасывает output, чтобы drain
    /// не завис. Затем якорится no-audio часы. State остаётся `Draining`.
    pub(super) fn complete_startup_autoplay_during_eof_drain_if_ready(
        &mut self,
        audio_preroll_target_ms: f64,
    ) -> bool {
        if !self.autoplay_video_preroll_ready()
            || !self.startup_audio_ready_during_eof_drain(audio_preroll_target_ms)
        {
            return false;
        }

        // Стадию меняем до запуска звука: tail-путь больше не должен ждать первый кадр.
        self.startup_autoplay = StartupAutoplayPreroll::StartedDuringEofDrain;
        self.start_eof_audio_tail_if_needed();
        self.anchor_monotonic_media_clock_if_needed(Instant::now());
        info!(
            current_position_ms = self.snapshot.current_position.as_millis(),
            has_audio_clock = self.pipeline.has_audio_clock(),
            "Startup autoplay completed during EOF drain (short media)"
        );
        true
    }

    /// Audio gate отложенного старта в EOF drain.
    ///
    /// Обычный gate (`autoplay_audio_readiness`) требует preroll target в output buffer.
    /// После EOF нового звука уже не будет: если весь оставшийся звук короче target-а,
    /// ждать нечего. Поэтому gate готов, когда обычный audio gate готов, ЛИБО все уже
    /// считанные audio packets обработаны (нет pending packets): что есть — то и играем.
    fn startup_audio_ready_during_eof_drain(&self, audio_preroll_target_ms: f64) -> bool {
        if self.preroll_audio_gate(audio_preroll_target_ms).is_ready() {
            return true;
        }
        !matches!(
            self.pipeline.audio_eof_drain_state(),
            AudioEofDrainState::PendingPackets { .. }
        )
    }
}
