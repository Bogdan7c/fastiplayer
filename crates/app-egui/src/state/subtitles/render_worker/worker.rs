//! Тело потока-рисовальщика: цикл «забрать команды -> спланировать -> нарисовать одно время».
//!
//! # Цикл
//! 1. Забираются ВСЕ ожидающие команды (последние «часы показа» побеждают: устаревшие цели
//!    scrub не рисуются).
//! 2. [`plan_next_render`] по часам и интервалам уже выданных картинок отвечает, какое время
//!    рисовать, либо «нечего делать».
//! 3. Рисуется ровно ОДНО время, результат уходит UI, запись о выданном пополняется, и цикл
//!    начинается заново (новая команда может прийти между рисованиями).
//! 4. Когда делать нечего, поток спит на `recv` (никакого опроса по таймеру).
//!
//! Растеризатор принадлежит только этому потоку и создаётся в нём: UI его никогда не видит.

use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use subtitle_core::{SubtitleEventId, SubtitleLanguage};
use subtitle_raster::{RasterLimits, SubtitleFontLibrary, SubtitleRasterizer};

#[cfg(test)]
use super::commands::TrackCommand;
#[cfg(test)]
use super::commands::WorkerDiagnostics;
use super::commands::{LookInputs, RenderTarget, WorkerCommand, WorkerEvent};
use super::prepared_queue::OverlayIdentity;
use super::published_record::PublishedRecord;
use super::render_step::{StepInput, StepOutcome, render_at};
use super::schedule::{
    FrameCadence, FrameStep, PrerenderFrames, PresentationClock, PresentationMode,
    ScheduleDecision, plan_next_render,
};
use super::track_mirror::TrackMirrors;
use crate::app_wake::AppWakePort;

/// Параметры потока, известные при создании.
#[derive(Debug, Clone)]
pub(in crate::state::subtitles) struct WorkerSettings {
    /// Лимиты растеризатора (действуют с его создания).
    pub(in crate::state::subtitles) raster_limits: RasterLimits,
    /// Язык системы: запасной язык текста.
    pub(in crate::state::subtitles) system_language: Option<SubtitleLanguage>,
    /// Сколько кадров вперёд готовить.
    pub(in crate::state::subtitles) prerender_frames: PrerenderFrames,
    /// Размер очереди готовых картинок (от него зависит память о выданном).
    pub(in crate::state::subtitles) prepared_overlays: usize,
}

/// Отправитель событий UI: каждое событие будит окно.
struct EventSink {
    events: Sender<WorkerEvent>,
    wake_port: AppWakePort,
}

impl EventSink {
    /// Отправляет событие и будит UI. `false` — UI ушёл (приёмник закрыт): поток завершается.
    fn send(&self, event: WorkerEvent) -> bool {
        if self.events.send(event).is_err() {
            return false;
        }
        // Результат пробуждения не важен: закрытый event loop означает конец приложения.
        let _wake_delivery = self.wake_port.request_wake();
        true
    }
}

/// Что рисовать следующим.
struct RenderJob {
    identity: OverlayIdentity,
    target: RenderTarget,
    lookup_time: subtitle_core::SubtitleTime,
    /// Шаг кадров из часов показа на момент планирования.
    frame_step: Option<FrameStep>,
}

/// Состояние потока.
struct WorkerState {
    rasterizer: SubtitleRasterizer,
    mirrors: TrackMirrors,
    look: Option<LookInputs>,
    presentation: Option<(RenderTarget, PresentationClock)>,
    /// Цель прыжка (seek/scrub), перед которой история раскладки растеризатора уже забыта.
    /// UI шлёт часы на каждом кадре: без этой пометки память сбрасывалась бы на каждой
    /// повторной команде с той же целью и стирала бы то, что поток нарисовал после цели.
    forgotten_for_jump_target: Option<subtitle_core::SubtitleTime>,
    /// Что уже выдано UI для текущей идентичности (по ней планируется работа).
    record: PublishedRecord,
    system_language: Option<SubtitleLanguage>,
    /// Горизонт, зажатый размером записи (иначе поток вытеснял бы свои же результаты).
    horizon: PrerenderFrames,
    scratch_active: Vec<SubtitleEventId>,
    /// Последнее нарисованное время с момента последней команды. Структурная защита от
    /// зацикливания: то же самое второй раз подряд без новой команды не рисуется.
    last_job: Option<(OverlayIdentity, subtitle_core::SubtitleTime)>,
    /// Сколько раз защита сработала (должно быть 0; ненулевое значение - ошибка планирования).
    repeated_jobs_suppressed: u64,
    sink: EventSink,
    #[cfg(test)]
    render_delay: std::time::Duration,
    #[cfg(test)]
    pending_flush_tokens: Vec<u64>,
    /// Только тесты: сколько полных копий дорожки принято.
    #[cfg(test)]
    track_replaces: u64,
    /// Только тесты: сколько узких дельт принято.
    #[cfg(test)]
    track_updates: u64,
}

impl WorkerState {
    fn new(fonts: SubtitleFontLibrary, settings: WorkerSettings, sink: EventSink) -> Self {
        let record = PublishedRecord::new(settings.prepared_overlays);
        // Нужно `горизонт + 1` записей: текущий кадр и все кадры горизонта одновременно.
        // Презентатор уже выбрал размер записи не меньше `горизонт + 1`; зажим здесь только
        // страхует от неверного использования и пишет об этом в лог.
        let max_horizon = u32::try_from(record.capacity().saturating_sub(1)).unwrap_or(u32::MAX);
        let horizon = PrerenderFrames::new(settings.prerender_frames.get().min(max_horizon));
        if horizon != settings.prerender_frames {
            tracing::debug!(
                requested = settings.prerender_frames.get(),
                used = horizon.get(),
                "Subtitle prerender horizon was clamped to the prepared picture capacity"
            );
        }
        Self {
            rasterizer: SubtitleRasterizer::new(fonts, settings.raster_limits),
            mirrors: TrackMirrors::default(),
            look: None,
            presentation: None,
            forgotten_for_jump_target: None,
            record,
            system_language: settings.system_language,
            horizon,
            scratch_active: Vec::new(),
            last_job: None,
            repeated_jobs_suppressed: 0,
            sink,
            #[cfg(test)]
            render_delay: std::time::Duration::ZERO,
            #[cfg(test)]
            pending_flush_tokens: Vec::new(),
            #[cfg(test)]
            track_replaces: 0,
            #[cfg(test)]
            track_updates: 0,
        }
    }

    /// Применяет команду. `false` — поток должен завершиться.
    fn apply(&mut self, command: WorkerCommand) -> bool {
        // Любая команда меняет состояние: прежнее «уже рисовали» больше не доказательство.
        self.last_job = None;
        match command {
            WorkerCommand::Track(track_command) => {
                // Запись о выданном для задетых интервалов больше не верна: после выброса
                // планировщик нарисует их заново. UI выбрасывает своё сам (по номеру
                // изменения на картинках), поэтому событий об этом нет.
                #[cfg(test)]
                match &track_command {
                    TrackCommand::Replace { .. } => self.track_replaces += 1,
                    TrackCommand::Update { .. } => self.track_updates += 1,
                    TrackCommand::Forget { .. } => {}
                }
                let effect = self.mirrors.apply(track_command);
                if let Some(span) = effect.invalidated {
                    self.record.invalidate_overlapping(span);
                }
                // Укороченное событие: выданное до нового конца остаётся (срок обрезан), выданное
                // за ним нарисуется заново. UI делает то же со своей очередью.
                if let Some(lost) = effect.shortened {
                    self.record.apply_shortening(lost);
                }
                true
            }
            WorkerCommand::Present {
                target,
                clock,
                look,
            } => {
                if look.is_some() {
                    self.look = look;
                }
                self.forget_layout_history_on_new_jump(clock.mode);
                self.presentation = Some((target, clock));
                true
            }
            WorkerCommand::InstallAttachedFonts {
                prepared,
                limits,
                look,
            } => {
                if look.is_some() {
                    self.look = look;
                }
                let report = self
                    .rasterizer
                    .install_prepared_attached_fonts(prepared, &limits);
                // Шрифт мог изменить вид уже нарисованных реплик: выданное устарело. UI в это
                // же время сменит ревизию входов, но запись обнулим и здесь для надёжности.
                self.record.clear();
                self.sink.send(WorkerEvent::AttachedFontsInstalled(report))
            }
            WorkerCommand::ClearAttachedFonts { look } => {
                if look.is_some() {
                    self.look = look;
                }
                let outcome = self.rasterizer.clear_attached_fonts();
                self.record.clear();
                self.sink.send(WorkerEvent::AttachedFontsCleared(outcome))
            }
            WorkerCommand::Shutdown => false,
            #[cfg(test)]
            WorkerCommand::Flush { token } => {
                self.pending_flush_tokens.push(token);
                true
            }
            #[cfg(test)]
            WorkerCommand::SimulateCrash => false,
            #[cfg(test)]
            WorkerCommand::SetRenderDelay(delay) => {
                self.render_delay = delay;
                true
            }
            #[cfg(test)]
            WorkerCommand::QueryDiagnostics => {
                self.sink
                    .send(WorkerEvent::Diagnostics(Box::new(WorkerDiagnostics {
                        raster: self.rasterizer.statistics(),
                        repeated_jobs_suppressed: self.repeated_jobs_suppressed,
                        track_replaces: self.track_replaces,
                        track_updates: self.track_updates,
                    })))
            }
        }
    }

    /// Новая цель прыжка (seek/scrub): растеризатор забывает историю раскладки, чтобы положение
    /// реплик в цели не зависело от того, откуда прыгнули. Выход из режима прыжка снимает
    /// пометку: следующий прыжок, даже в то же время, снова начнёт с чистой раскладки.
    /// Отличие от libass (там память живёт между любыми кадрами) задокументировано в
    /// `mem:subtitles/ass-layer2-s09`.
    fn forget_layout_history_on_new_jump(&mut self, mode: PresentationMode) {
        let jump_target = mode.jump_target();
        if jump_target.is_some() && jump_target != self.forgotten_for_jump_target {
            self.rasterizer.forget_layout_history();
        }
        self.forgotten_for_jump_target = jump_target;
    }

    /// Следующее время для рисования или `None`, если делать нечего.
    fn next_job(&mut self) -> Option<RenderJob> {
        let look = self.look.as_ref()?;
        let (target, clock) = self.presentation?;
        // Дорожка ещё не доехала до зеркала: рисовать не из чего (ждём команды).
        self.mirrors.get(target.source)?;
        let identity = OverlayIdentity {
            source: target.source,
            canvas: target.canvas,
            inputs: look.revision,
        };
        self.record.align_to(identity);
        let (window_start, window_end) = clock.planning_window(self.horizon);
        self.record.retain_window(window_start, window_end);
        match plan_next_render(&clock, self.horizon, self.record.intervals()) {
            ScheduleDecision::Render { lookup_time } => {
                if self.last_job == Some((identity, lookup_time)) {
                    // Только что нарисовали ровно это, а планировщик снова просит то же:
                    // запись о выданном не удержала результат. Не крутимся вхолостую.
                    self.repeated_jobs_suppressed += 1;
                    tracing::debug!(
                        suppressed = self.repeated_jobs_suppressed,
                        "Subtitle render thread refused to draw the same time twice in a row"
                    );
                    return None;
                }
                let frame_step = match clock.cadence {
                    FrameCadence::Known(step) => Some(step),
                    FrameCadence::Unknown => None,
                };
                Some(RenderJob {
                    identity,
                    target,
                    lookup_time,
                    frame_step,
                })
            }
            ScheduleDecision::Idle => None,
        }
    }

    /// Рисует одно время. `false` — поток должен завершиться.
    fn run_job(&mut self, job: RenderJob) -> bool {
        #[cfg(test)]
        if !self.render_delay.is_zero() {
            std::thread::sleep(self.render_delay);
        }
        let (Some(look), Some(track)) = (self.look.as_ref(), self.mirrors.get(job.target.source))
        else {
            return true;
        };
        self.last_job = Some((job.identity, job.lookup_time));
        let mirror_serial = track.serial;
        let input = StepInput {
            identity: job.identity,
            canvas: job.target.canvas,
            track,
            look,
            system_language: self.system_language.as_ref(),
            lookup_time: job.lookup_time,
            frame_step: job.frame_step,
        };
        match render_at(&mut self.rasterizer, &input, &mut self.scratch_active) {
            StepOutcome::NothingActive { interval } => {
                self.record.push(interval);
                true
            }
            StepOutcome::Rendered { picture, report } => {
                self.record.push(picture.interval());
                self.sink.send(WorkerEvent::Prepared {
                    picture,
                    report,
                    mirror_serial,
                })
            }
        }
    }

    /// Работы нет: отвечаем на ожидающие `Flush` (только тесты). `false` — завершиться.
    fn acknowledge_idle(&mut self) -> bool {
        #[cfg(test)]
        for token in std::mem::take(&mut self.pending_flush_tokens) {
            if !self.sink.send(WorkerEvent::Idle { token }) {
                return false;
            }
        }
        true
    }
}

/// Точка входа потока. Возврат = завершение потока.
pub(super) fn run_worker(
    fonts: SubtitleFontLibrary,
    settings: WorkerSettings,
    commands: Receiver<WorkerCommand>,
    events: Sender<WorkerEvent>,
    wake_port: AppWakePort,
) {
    let sink = EventSink { events, wake_port };
    let mut state = WorkerState::new(fonts, settings, sink);
    loop {
        // 1. Забираем все накопившиеся команды.
        loop {
            match commands.try_recv() {
                Ok(command) => {
                    if !state.apply(command) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        // 2-3. Рисуем одно время, если есть что.
        if let Some(job) = state.next_job() {
            if !state.run_job(job) {
                return;
            }
            continue;
        }
        // 4. Делать нечего: отвечаем на сверку и спим до следующей команды.
        if !state.acknowledge_idle() {
            return;
        }
        match commands.recv() {
            Ok(command) => {
                if !state.apply(command) {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}
