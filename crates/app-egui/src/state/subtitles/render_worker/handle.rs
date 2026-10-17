//! Хэндл потока-рисовальщика на стороне UI: создание, отправка команд, приём событий, остановка.
//!
//! # Владение и остановка
//! Хэндл владеет потоком. `Drop` отправляет `Shutdown` и дожидается потока. Чтобы UI не ждал
//! долгой растеризации при смене media, владелец сначала зовёт [`RenderWorkerHandle::request_shutdown`]
//! и откладывает хэндл до завершения потока.
//!
//! # Отказ
//! Паника или гибель потока видны как закрытый канал: [`RenderWorkerHandle::send`] возвращает
//! [`WorkerLost`], [`RenderWorkerHandle::drain_events`] сообщает об отключении после того, как
//! выданы все события, успевшие прийти.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};

use subtitle_raster::SubtitleFontLibrary;

use super::commands::{WorkerCommand, WorkerEvent};
use super::worker::{WorkerSettings, run_worker};
use crate::app_wake::AppWakePort;

/// Имя потока для диагностики.
const RENDER_WORKER_THREAD_NAME: &str = "subtitle-render";

/// Поток-рисовальщик недоступен: он завершился или упал.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct WorkerLost;

/// Состояние канала событий после выдачи накопленного.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum EventChannel {
    /// Поток жив (новых событий пока нет).
    Open,
    /// Поток исчез: больше событий не будет.
    Disconnected,
}

/// UI-сторона потока-рисовальщика.
pub(in crate::state::subtitles) struct RenderWorkerHandle {
    commands: Sender<WorkerCommand>,
    events: Receiver<WorkerEvent>,
    join_handle: Option<JoinHandle<()>>,
}

impl RenderWorkerHandle {
    /// Запускает поток; растеризатор создаётся внутри него из `fonts`.
    pub(in crate::state::subtitles) fn spawn(
        fonts: SubtitleFontLibrary,
        settings: WorkerSettings,
        wake_port: AppWakePort,
    ) -> Result<Self, io::Error> {
        let (commands, command_receiver) = mpsc::channel();
        let (event_sender, events) = mpsc::channel();
        let join_handle = thread::Builder::new()
            .name(RENDER_WORKER_THREAD_NAME.to_owned())
            .spawn(move || {
                run_worker(fonts, settings, command_receiver, event_sender, wake_port)
            })?;
        Ok(Self {
            commands,
            events,
            join_handle: Some(join_handle),
        })
    }

    /// Отправляет команду, не ожидая потока.
    pub(in crate::state::subtitles) fn send(
        &self,
        command: WorkerCommand,
    ) -> Result<(), WorkerLost> {
        self.commands.send(command).map_err(|_| WorkerLost)
    }

    /// Забирает все накопившиеся события в `output` без ожидания.
    pub(in crate::state::subtitles) fn drain_events(
        &self,
        output: &mut Vec<WorkerEvent>,
    ) -> EventChannel {
        loop {
            match self.events.try_recv() {
                Ok(event) => output.push(event),
                Err(TryRecvError::Empty) => return EventChannel::Open,
                Err(TryRecvError::Disconnected) => return EventChannel::Disconnected,
            }
        }
    }

    /// Только тесты: ждёт одно событие не дольше `timeout`.
    #[cfg(test)]
    pub(in crate::state::subtitles) fn wait_event(
        &self,
        timeout: std::time::Duration,
    ) -> Option<WorkerEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Просит поток остановиться, не дожидаясь его.
    pub(in crate::state::subtitles) fn request_shutdown(&self) {
        // Ошибка отправки значит, что поток уже завершился: просить некого.
        let _already_stopped = self.commands.send(WorkerCommand::Shutdown);
    }

    /// Поток уже завершился (хэндл можно освободить без ожидания).
    pub(in crate::state::subtitles) fn is_finished(&self) -> bool {
        self.join_handle
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for RenderWorkerHandle {
    fn drop(&mut self) {
        self.request_shutdown();
        let Some(join_handle) = self.join_handle.take() else {
            return;
        };
        if join_handle.join().is_err() {
            tracing::warn!("Subtitle render thread panicked");
        }
    }
}
