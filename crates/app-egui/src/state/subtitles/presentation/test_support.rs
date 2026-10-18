//! Помощники тестов презентатора: ожидание готовности картинки у настоящего потока.
//!
//! По умолчанию тесты работают в режиме [`PumpMode::SettleBeforeSelect`]: перед выбором картинки
//! презентатор дожидается, пока поток закончит всю накопленную работу (с таймаутом). Поток
//! настоящий, растеризация настоящая — тест лишь не гоняется со временем. Тесты асинхронного
//! поведения включают [`PumpMode::FreeRunning`], тогда выбор идёт сразу, как в продукте.

use std::time::{Duration, Instant};

use subtitle_raster::{AttachedFontsClearOutcome, AttachedFontsInstallReport, RasterStatistics};

use super::presenter::SubtitlePresenter;
use crate::state::subtitles::render_worker::commands::{
    WorkerCommand, WorkerDiagnostics, WorkerEvent,
};

/// Сколько тест ждёт поток.
const WORKER_TIMEOUT: Duration = Duration::from_secs(20);

/// Как презентатор ведёт себя перед выбором картинки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::state::subtitles) enum PumpMode {
    /// Дождаться, пока поток закончит работу (детерминированные тесты результата).
    #[default]
    SettleBeforeSelect,
    /// Не ждать (тесты поведения при занятом потоке).
    FreeRunning,
}

/// Состояние помощника.
#[derive(Default)]
pub(in crate::state::subtitles) struct TestPump {
    pub(in crate::state::subtitles) mode: PumpMode,
    next_token: u64,
    pub(super) idle_token: Option<u64>,
    pub(super) last_installed: Option<AttachedFontsInstallReport>,
    pub(super) last_cleared: Option<AttachedFontsClearOutcome>,
    pub(super) last_diagnostics: Option<WorkerDiagnostics>,
}

impl SubtitlePresenter {
    /// Помнит ли презентатор что-то о прежних кадрах (холст, настройки ASS, шаг кадров).
    pub(in crate::state::subtitles) fn remembers_playback_context(&self) -> bool {
        self.remembers_playback_context_impl()
    }

    /// Счётчики показа.
    pub(in crate::state::subtitles) fn stats(&self) -> super::PresentationStats {
        self.stats
    }

    /// Режим ожидания потока в тестах.
    pub(in crate::state::subtitles) fn set_pump_mode(&mut self, mode: PumpMode) {
        self.test_pump.mode = mode;
    }

    /// Ждёт, пока поток отработает все команды и запланированное рисование.
    pub(in crate::state::subtitles) fn settle_worker_for_test(&mut self) {
        if self.test_pump.mode == PumpMode::FreeRunning || !self.link.is_running() {
            return;
        }
        self.wait_for_idle();
    }

    /// Безусловное ожидание простоя потока (для тестов, ждущих результат явно).
    pub(in crate::state::subtitles) fn wait_for_idle(&mut self) -> bool {
        self.test_pump.next_token += 1;
        let token = self.test_pump.next_token;
        self.test_pump.idle_token = None;
        if self.link.send(WorkerCommand::Flush { token }).is_err() {
            return false;
        }
        let deadline = Instant::now() + WORKER_TIMEOUT;
        while self.test_pump.idle_token != Some(token) {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                tracing::error!("The subtitle render thread did not become idle in time");
                return false;
            };
            let Some(event) = self
                .link
                .wait_event(remaining.min(Duration::from_millis(50)))
            else {
                self.ingest_events();
                continue;
            };
            self.handle_waited_event(event);
        }
        true
    }

    /// Обрабатывает событие, полученное блокирующим ожиданием, как обычный приём.
    fn handle_waited_event(&mut self, event: WorkerEvent) {
        self.link.push_back_event(event);
        self.ingest_events();
    }

    /// Диагностика потока (если он есть): запрос идёт после завершения всей работы.
    pub(in crate::state::subtitles) fn worker_diagnostics(&mut self) -> Option<WorkerDiagnostics> {
        if !self.link.is_running() {
            return None;
        }
        self.wait_for_idle();
        self.test_pump.last_diagnostics = None;
        self.link.send(WorkerCommand::QueryDiagnostics).ok()?;
        let deadline = Instant::now() + WORKER_TIMEOUT;
        while self.test_pump.last_diagnostics.is_none() {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let event = self
                .link
                .wait_event(remaining.min(Duration::from_millis(50)));
            if let Some(event) = event {
                self.handle_waited_event(event);
            }
        }
        self.test_pump.last_diagnostics
    }

    /// Счётчики растеризатора (если поток есть).
    pub(in crate::state::subtitles) fn rasterizer_statistics(
        &mut self,
    ) -> Option<RasterStatistics> {
        self.worker_diagnostics()
            .map(|diagnostics| diagnostics.raster)
    }

    /// Снимает вложенные шрифты и ждёт итог от потока.
    pub(in crate::state::subtitles) fn clear_attached_fonts_and_wait(
        &mut self,
    ) -> AttachedFontsClearOutcome {
        if !self.link.is_running() {
            self.clear_attached_fonts();
            return AttachedFontsClearOutcome::NothingAttached;
        }
        // События прежних команд (например, снятия при смене media) не должны сойти за ответ.
        self.wait_for_idle();
        self.test_pump.last_cleared = None;
        self.clear_attached_fonts();
        let deadline = Instant::now() + WORKER_TIMEOUT;
        while self.test_pump.last_cleared.is_none() {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return AttachedFontsClearOutcome::NothingAttached;
            };
            if let Some(event) = self
                .link
                .wait_event(remaining.min(Duration::from_millis(50)))
            {
                self.handle_waited_event(event);
            }
        }
        self.test_pump
            .last_cleared
            .unwrap_or(AttachedFontsClearOutcome::NothingAttached)
    }

    /// Искусственная задержка каждой растеризации (модель занятого потока).
    pub(in crate::state::subtitles) fn set_render_delay(&mut self, delay: Duration) {
        let _sent = self.link.send(WorkerCommand::SetRenderDelay(delay));
    }

    /// Выбор текущей очереди в тестах (счётчики).
    pub(in crate::state::subtitles) fn queue_counters(
        &self,
    ) -> crate::state::subtitles::render_worker::prepared_queue::SelectionCounters {
        self.queue.counters()
    }

    /// Забирает журнал отправленных потоку команд (порядок отправки).
    pub(in crate::state::subtitles) fn take_sent_command_log(
        &mut self,
    ) -> Vec<super::worker_link::SentCommandKind> {
        self.link.take_sent_log()
    }

    /// Сколько остановленных потоков ещё не освобождено.
    pub(in crate::state::subtitles) fn retiring_workers(&self) -> usize {
        self.link.retiring_count()
    }

    /// Поток гибнет без прощания (модель паники).
    pub(in crate::state::subtitles) fn crash_worker_for_test(&mut self) {
        let _sent = self.link.send(WorkerCommand::SimulateCrash);
    }

    /// Поток запущен (для тестов Drop и отказа).
    pub(in crate::state::subtitles) fn worker_is_running(&self) -> bool {
        self.link.is_running()
    }
}
