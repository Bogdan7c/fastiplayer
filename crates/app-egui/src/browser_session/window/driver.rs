//! Водитель окна: исполняет команды пульта на плеере окна и публикует наблюдение.
//!
//! Плеер окна — `AppState` (решение 120). Водитель видит его только через шов
//! [`WindowPlayer`] (настоящий — `player_port::AppStateWindowPlayer`), поэтому вся
//! логика «чья загрузка», «новее заменяет», журнал и отказы проверяется тестами без
//! окна и без GPU.
//!
//! Чем владеет водитель (одним на процесс, а не на окно: журнал и учёт загрузок
//! переживают окно и пишут итог даже без него):
//! - [`LoadLedger`] — чья загрузка показана и чей отказ (общий с безоконным плеером);
//! - [`PlaybackJournal`] — строки `Browser playback summary` / `Browser playback ended`;
//! - счётчик звука [`AudioMeter`] — выход звука окна обёрнут им при сборке окна;
//! - слотом открытия: одна загрузка ставится, следующая ждёт. Новая загрузка во
//!   время установки прежней отменяет прежнюю (если та ещё до барьера установки) и
//!   ставится сразу после её итога — «новее заменяет» (в 03e поколения меняются
//!   быстро).

use std::time::Instant;

use player_core::{
    PlaybackIntent, PlayerCommand, PlayerEvent, PlayerSnapshot, PlayerWorkerSendError,
    PreparedMedia, SeekRequest,
};
use tracing::{debug, warn};

use super::super::journal::{
    BROWSER_PLAYBACK_SUMMARY_INTERVAL, JournalNotice, PlaybackJournal, PlaybackSummary,
    SummaryKind, log_playback_ended, log_playback_summary,
};
use super::super::playback::load_ledger::{CancelledInstall, LoadLedger};
use super::super::playback::metered_audio::AudioMeter;
use super::super::playback::{BrowserPlaybackFault, LoadStart, MediaLoadTicket};
use super::remote::{LoadOrder, PlaybackDesk, RemoteCommand};
use crate::state::TappedPlayerEvents;

/// Чем кончилась попытка начать загрузку.
pub(super) enum BeginLoad {
    /// Плеер окна начал ставить медиа.
    Started,
    /// Плеер окна отказал сразу; медиа не ставится.
    Refused(BrowserPlaybackFault),
}

/// Итог опроса начатой загрузки.
pub(super) enum LoadPoll {
    /// Ещё ставится.
    Pending,
    /// Поставлено: плеер показывает экземпляр медиа `instance`.
    Installed(player_core::MediaInstanceId),
    /// Отменено: заменено более новой загрузкой ИЛИ прервано по другой причине
    /// (остановка, структурная отмена до барьера, установка пропала без итога).
    /// Что именно — решает учёт загрузок по тому, текущая ли это загрузка
    /// (`LoadLedger::note_install_cancelled`): плеер окна этого не знает.
    Cancelled,
    /// Не поставилось.
    Failed(BrowserPlaybackFault),
}

/// Плеер окна глазами водителя. Настоящий — `AppState` окна (`player_port`).
pub(super) trait WindowPlayer {
    /// Есть ли незавершённая установка медиа (наша или чужая).
    fn has_pending_load(&self) -> bool;
    /// Начать ставить медиа. Зовётся только без незавершённой установки.
    fn begin_load(&mut self, prepared: PreparedMedia, intent: PlaybackIntent) -> BeginLoad;
    /// Попросить отменить незавершённую установку (нет установки или поздно — no-op).
    fn supersede_pending_load(&mut self);
    /// Продвинуть незавершённую установку.
    fn poll_load(&mut self) -> LoadPoll;
    /// Команда транспорта плееру.
    ///
    /// # Errors
    /// Очередь команд плеера отказала.
    fn send(&mut self, command: PlayerCommand) -> Result<(), PlayerWorkerSendError>;
    /// Свежий снимок плеера.
    fn snapshot(&mut self) -> std::sync::Arc<PlayerSnapshot>;
    /// Копии событий плеера с прошлого раза (`AppState` — единственный их читатель).
    fn take_player_events(&mut self) -> TappedPlayerEvents;
    /// Сколько кадров видео плеер отдал на показ за всё время (накопительно).
    fn video_frames_presented(&self) -> u64;
}

/// Загрузка, которую плеер окна сейчас ставит.
#[derive(Debug, Clone, Copy)]
struct InFlightLoad {
    ticket: MediaLoadTicket,
}

/// Водитель окна (см. описание модуля).
pub(super) struct WindowPlaybackDriver {
    desk: PlaybackDesk,
    ledger: LoadLedger,
    journal: PlaybackJournal,
    audio_meter: AudioMeter,
    in_flight: Option<InFlightLoad>,
    /// Загрузка, ждущая конца прежней установки (самая новая; старшие выброшены).
    waiting: Option<LoadOrder>,
    /// Сколько кадров показа уже учтено в журнале.
    presented_frames_seen: u64,
}

impl WindowPlaybackDriver {
    /// Водитель над столом пульта; `audio_meter` — счётчик, которым обёрнут звук окна.
    pub(super) fn new(desk: PlaybackDesk, audio_meter: AudioMeter, now: Instant) -> Self {
        Self {
            desk,
            ledger: LoadLedger::default(),
            journal: PlaybackJournal::new(now, BROWSER_PLAYBACK_SUMMARY_INTERVAL),
            audio_meter,
            in_flight: None,
            waiting: None,
            presented_frames_seen: 0,
        }
    }

    /// Счётчик звука (окно оборачивает им фабрику выхода звука при сборке).
    pub(super) fn audio_meter(&self) -> AudioMeter {
        self.audio_meter.clone()
    }

    /// Появилось новое окно: его счётчик кадров показа начинается с нуля (телеметрия
    /// принадлежит экземпляру окна, а журнал — процессу).
    pub(super) fn attach_window(&mut self) {
        self.presented_frames_seen = 0;
    }

    /// Ждёт ли что-то установки (окну нужно будить цикл, чтобы опрашивать её).
    pub(super) fn has_load_in_progress(&self) -> bool {
        self.in_flight.is_some() || self.waiting.is_some()
    }

    /// Исполняет команды пульта в порядке подачи.
    pub(super) fn execute_commands(&mut self, player: &mut dyn WindowPlayer) {
        for command in self.desk.take_commands() {
            let label = command.log_label();
            let sent = match command {
                RemoteCommand::Load(order) => {
                    self.accept_load(*order, player);
                    Ok(())
                }
                RemoteCommand::Play => player.send(PlayerCommand::Play),
                RemoteCommand::Pause => player.send(PlayerCommand::Pause),
                RemoteCommand::SetRate(rate) => player.send(PlayerCommand::SetPlaybackRate(rate)),
                RemoteCommand::Seek(target) => {
                    let sent = player.send(PlayerCommand::Seek(SeekRequest::accurate(target)));
                    if sent.is_ok() {
                        self.journal.note_seek_requested();
                    }
                    sent
                }
            };
            if let Err(error) = sent {
                warn!(command = label, %error, "The browser window player did not accept a command");
            }
        }
    }

    /// Продвигает установку и, когда прежняя кончилась, начинает ждущую.
    pub(super) fn drive_loads(&mut self, player: &mut dyn WindowPlayer) {
        if let Some(in_flight) = self.in_flight {
            match player.poll_load() {
                LoadPoll::Pending => return,
                LoadPoll::Installed(instance) => {
                    debug!("Browser window media installed");
                    self.ledger.note_installed(in_flight.ticket, instance);
                }
                LoadPoll::Cancelled => match self.ledger.note_install_cancelled(in_flight.ticket) {
                    CancelledInstall::ReplacedByNewerLoad => {
                        debug!("Browser window media install cancelled for a newer load");
                    }
                    // Текущую загрузку никто не заменял: учёт записал её отказ, иначе
                    // сеанс ждал бы показа, который не наступит.
                    CancelledInstall::CurrentLoadAborted => {
                        warn!("Browser window media install was aborted");
                    }
                },
                LoadPoll::Failed(fault) => {
                    warn!(?fault, "Browser window media did not install");
                    self.ledger.note_fault(in_flight.ticket, fault);
                }
            }
            self.in_flight = None;
        }
        if let Some(order) = self.waiting.take() {
            self.begin_or_wait(order, player);
        }
    }

    /// Наблюдение за плеером: события, кадры, звук, снимок → журнал и стол пульта.
    pub(super) fn observe(&mut self, player: &mut dyn WindowPlayer, now: Instant) {
        let tapped = player.take_player_events();
        if tapped.evicted > 0 {
            warn!(
                evicted = tapped.evicted,
                "Browser window missed player events; the tap overflowed"
            );
        }
        for correlated in &tapped.events {
            self.journal.note_player_event(&correlated.event);
            if let PlayerEvent::RecoverableError(error) | PlayerEvent::FatalError(error) =
                &correlated.event
            {
                // В журнал — только вид ошибки: текст может нести метку источника.
                warn!(kind = ?error.kind, "Browser window player reported an error");
                self.ledger
                    .note_player_error(correlated.media_instance_id, &error.kind);
            }
        }
        // Окно рисует кадры, которые плеер отдал на показ: тот же счётчик, что у
        // безоконного вывода (worker `video_frames_presented`), — и забранные, и
        // показанные считаются по нему.
        let presented_total = player.video_frames_presented();
        let presented_new = presented_total.saturating_sub(self.presented_frames_seen);
        self.presented_frames_seen = presented_total;
        self.journal.note_presented_frames(presented_new);
        self.journal.note_taken_frames(presented_new);
        self.journal.note_audio(self.audio_meter.reading());

        let snapshot = player.snapshot();
        self.ledger.note_snapshot(&snapshot);
        let notice = self.journal.note_snapshot(
            snapshot.media_instance_id,
            snapshot.current_position,
            snapshot.playback_state,
        );
        if notice == Some(JournalNotice::PlaybackEnded) {
            log_playback_ended(&self.journal.summary());
        }
        if let Some(summary) = self.journal.take_due_summary(now) {
            log_playback_summary(SummaryKind::Periodic, &summary);
        }
        self.desk.publish(self.ledger.observation(&snapshot));
    }

    /// Итоговая строка журнала (процесс выходит). Плеер останавливает окно после.
    pub(super) fn finish(&self) -> PlaybackSummary {
        let summary = self.journal.summary();
        log_playback_summary(SummaryKind::Final, &summary);
        summary
    }

    /// Сводка на текущий момент (тесты).
    #[cfg(test)]
    pub(super) fn summary(&self) -> PlaybackSummary {
        self.journal.summary()
    }

    /// Новая загрузка от пульта: с этого момента отказы приписываются только ей.
    fn accept_load(&mut self, order: LoadOrder, player: &mut dyn WindowPlayer) {
        self.journal.note_media_load();
        self.ledger.begin_load(order.ticket);
        if self.in_flight.is_some() {
            // Прежняя ещё ставится: её поколение уже заменено. Отменяем (если плеер ещё
            // может) и ждём её итога — слот открытия у плеера один.
            player.supersede_pending_load();
        }
        self.begin_or_wait(order, player);
    }

    /// Начинает загрузку, если слот свободен; иначе она ждёт (вытесняя старшую ждущую).
    fn begin_or_wait(&mut self, order: LoadOrder, player: &mut dyn WindowPlayer) {
        if self.in_flight.is_some() || player.has_pending_load() {
            self.waiting = Some(order);
            return;
        }
        let intent = match order.start {
            LoadStart::Playing => PlaybackIntent::StartPlaying,
            LoadStart::Paused => PlaybackIntent::StartPaused,
        };
        match player.begin_load(order.prepared, intent) {
            BeginLoad::Started => {
                self.in_flight = Some(InFlightLoad {
                    ticket: order.ticket,
                });
            }
            BeginLoad::Refused(fault) => {
                warn!(?fault, "Browser window refused to install the media");
                self.ledger.note_fault(order.ticket, fault);
            }
        }
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
