//! Связь презентатора с потоком-рисовальщиком: жизненный цикл потока, отправка, приём.
//!
//! Здесь собрано всё, что связано с «жив ли поток»: создание, отказ (запуск не удался или поток
//! пропал), отложенная остановка старых потоков (UI не ждёт долгой растеризации при смене
//! media). Презентатор остаётся про «что показать», а не про каналы.

use super::super::render_worker::RenderWorkerFailure;
use super::super::render_worker::commands::{WorkerCommand, WorkerEvent};
use super::super::render_worker::handle::{EventChannel, RenderWorkerHandle, WorkerLost};
use super::super::render_worker::worker::WorkerSettings;
use crate::app_wake::AppWakePort;
use subtitle_raster::SubtitleFontLibrary;

/// Поток-рисовальщик презентатора.
pub(super) struct WorkerLink {
    wake_port: AppWakePort,
    running: Option<RenderWorkerHandle>,
    /// Остановленные, но ещё завершающиеся потоки: хэндл join-ит их в `Drop`.
    retiring: Vec<RenderWorkerHandle>,
    /// Отказ, о котором владелец ещё не узнал.
    pending_failure: Option<RenderWorkerFailure>,
    /// Переиспользуемый буфер событий.
    events: Vec<WorkerEvent>,
}

impl WorkerLink {
    pub(super) fn new(wake_port: AppWakePort) -> Self {
        Self {
            wake_port,
            running: None,
            retiring: Vec::new(),
            pending_failure: None,
            events: Vec::new(),
        }
    }

    /// Есть ли живой поток (то есть растеризатор).
    pub(super) fn is_running(&self) -> bool {
        self.running.is_some()
    }

    /// Запускает новый поток из библиотеки шрифтов; прежний уходит в отложенную остановку.
    /// Ошибка запуска превращается в отказ (поток остаётся отсутствующим).
    pub(super) fn start(&mut self, fonts: SubtitleFontLibrary, settings: WorkerSettings) {
        self.stop();
        match RenderWorkerHandle::spawn(fonts, settings, self.wake_port.clone()) {
            Ok(handle) => self.running = Some(handle),
            Err(error) => {
                tracing::warn!(kind = ?error.kind(), "Subtitle render thread could not be started");
                self.pending_failure =
                    Some(RenderWorkerFailure::StartFailed { kind: error.kind() });
            }
        }
    }

    /// Останавливает поток без ожидания: хэндл дожидается его позже (`prune_retired`/`Drop`).
    pub(super) fn stop(&mut self) {
        if let Some(handle) = self.running.take() {
            handle.request_shutdown();
            self.retiring.push(handle);
        }
    }

    /// Освобождает потоки, уже завершившиеся после `stop` (join мгновенный).
    pub(super) fn prune_retired(&mut self) {
        self.retiring.retain(|handle| !handle.is_finished());
    }

    /// Отправляет команду. Потерянный поток превращается в отказ.
    pub(super) fn send(&mut self, command: WorkerCommand) -> Result<(), WorkerLost> {
        let Some(handle) = self.running.as_ref() else {
            return Err(WorkerLost);
        };
        let result = handle.send(command);
        if result.is_err() {
            self.fail(RenderWorkerFailure::Lost);
        }
        result
    }

    /// Помечает отказ: поток считается отсутствующим, о причине узнает владелец.
    pub(super) fn fail(&mut self, failure: RenderWorkerFailure) {
        tracing::warn!(
            ?failure,
            "Subtitle render thread is unavailable; playback continues"
        );
        self.stop();
        self.pending_failure.get_or_insert(failure);
    }

    /// Забирает отказ, о котором владелец ещё не знал (отдаётся один раз).
    pub(super) fn take_failure(&mut self) -> Option<RenderWorkerFailure> {
        self.pending_failure.take()
    }

    /// Забирает накопившиеся события. Пропажа потока становится отказом.
    pub(super) fn take_events(&mut self) -> Vec<WorkerEvent> {
        let mut events = std::mem::take(&mut self.events);
        if let Some(handle) = self.running.as_ref()
            && handle.drain_events(&mut events) == EventChannel::Disconnected
        {
            self.fail(RenderWorkerFailure::Lost);
        }
        events
    }

    /// Возвращает буфер событий для переиспользования.
    pub(super) fn recycle_events(&mut self, mut events: Vec<WorkerEvent>) {
        events.clear();
        self.events = events;
    }

    /// Только тесты: сколько остановленных потоков ещё не освобождено.
    #[cfg(test)]
    pub(super) fn retiring_count(&self) -> usize {
        self.retiring.len()
    }

    /// Только тесты: возвращает событие в буфер, чтобы обычный приём его обработал.
    #[cfg(test)]
    pub(super) fn push_back_event(&mut self, event: WorkerEvent) {
        self.events.push(event);
    }

    /// Только тесты: ждёт одно событие не дольше `timeout`.
    #[cfg(test)]
    pub(super) fn wait_event(&mut self, timeout: std::time::Duration) -> Option<WorkerEvent> {
        self.running.as_ref()?.wait_event(timeout)
    }
}
