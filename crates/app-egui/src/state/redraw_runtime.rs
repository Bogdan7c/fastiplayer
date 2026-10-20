//! Владелец решения «надо ли рисовать» на стороне `AppState` (cpu-parity, сессия 04).
//!
//! Здесь живут:
//! - порт пробуждения показа (`AppWakeOwner::PlayerPresentation`) и его bridge для worker-а;
//! - триггеры, накопленные между пробуждениями (ввод, wake показа, видимые мутации,
//!   страховочный таймер, принудительный проход);
//! - приёмник запросов egui на перерисовку (`Context::set_request_repaint_callback`);
//! - память последнего полного egui-прохода: UI-модель кадра и геометрия для повторного
//!   показа с удержанными примитивами.
//!
//! Сам план кадра считает чистая функция `frame_prepare::frame_work_plan::plan_frame_work`;
//! этот модуль только собирает факты и хранит состояние между кадрами.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use player_core::PlayerSnapshot;
use render_core::RenderViewport;
use render_wgpu_shell::{RenderScreenDescriptor, WindowCornerMask};
use winit::window::Window;

use super::AppState;
use super::present_frame_cache::PresentFrameChange;
use super::startup_context::player_presentation_wake_bridge;
use super::telemetry_panel::TELEMETRY_PANEL_REFRESH_INTERVAL;
use super::ui_frame_model::{
    AppVisualFacts, PlayerVisualFacts, PlaylistVisualKey, QuantizedTimelinePosition,
    SidebarVisualKey, UiFrameModel, WindowFacts, next_visible_position_change,
};
use crate::app_wake::{AppWakeOwner, AppWakePort};
use crate::frame_prepare::PresentReasons;
use crate::ui::notifications::TOAST_FADE_IN;

/// Что просил egui с момента последнего взятия запроса.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct EguiRepaintRequest {
    /// `request_repaint()` без задержки: следующий проход нужен сразу.
    pub(crate) immediate: bool,
    /// Ближайший `request_repaint_after(..)`: проход нужен не позже этого момента.
    pub(crate) deadline: Option<Instant>,
}

impl EguiRepaintRequest {
    /// Проход нужен уже сейчас: немедленный запрос или созревший отложенный.
    #[must_use]
    pub(crate) fn is_due(self, now: Instant) -> bool {
        self.immediate || self.deadline.is_some_and(|deadline| deadline <= now)
    }
}

/// Приёмник callback-а egui: запросы приходят во время прохода и после него, с UI-потока.
struct EguiRepaintRequestSink {
    requests: Mutex<EguiRepaintRequest>,
}

impl EguiRepaintRequestSink {
    /// Ставит callback в контекст; egui вызывает его только когда новый запрос раньше прежнего.
    fn install(egui_ctx: &egui::Context) -> Arc<Self> {
        let sink = Arc::new(Self {
            requests: Mutex::new(EguiRepaintRequest::default()),
        });
        let callback_sink = Arc::clone(&sink);
        egui_ctx.set_request_repaint_callback(move |info| {
            callback_sink.record(info.delay, Instant::now());
        });
        sink
    }

    fn record(&self, delay: Duration, now: Instant) {
        let mut requests = self.lock();
        if delay.is_zero() {
            requests.immediate = true;
            return;
        }
        let deadline = now + delay;
        requests.deadline = Some(match requests.deadline {
            Some(existing) => existing.min(deadline),
            None => deadline,
        });
    }

    fn peek(&self) -> EguiRepaintRequest {
        *self.lock()
    }

    fn take(&self) -> EguiRepaintRequest {
        std::mem::take(&mut *self.lock())
    }

    /// Запросы — два скаляра; после poison данные остаются валидными.
    fn lock(&self) -> std::sync::MutexGuard<'_, EguiRepaintRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Триггеры, накопленные между пробуждениями UI-потока; забираются один раз за кадр.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RedrawTriggers {
    /// Оконные события/ввод с прошлого прохода (egui их ещё не видел).
    pub(crate) window_input: bool,
    /// Worker опубликовал кадр показа или событие плеера.
    pub(crate) presentation_wake: bool,
    /// Другой владелец сообщил о видимой мутации (субтитры, плейлист, startup, настройки).
    pub(crate) visible_mutation: bool,
    /// Это пробуждение вызвал страховочный таймер, а не событие.
    pub(crate) fallback_probe: bool,
    /// Следующий кадр обязан быть полным проходом (после отказа shell-а / пересоздания).
    pub(crate) full_pass_forced: bool,
}

/// Геометрия последнего полного прохода — для показа видео с удержанным интерфейсом.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetainedUiGeometry {
    pub(crate) screen: RenderScreenDescriptor,
    pub(crate) video_viewport: RenderViewport,
    pub(crate) video_exclusion_rects: Vec<RenderViewport>,
    pub(crate) window_corner_mask: WindowCornerMask,
}

/// Память последнего дедлайна позиции: защита от холостого спина, когда snapshot ещё не
/// догнал границу секунды/пикселя (позиция в snapshot-е обновляется только тиком worker-а).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositionWakeMemory {
    /// Квантованная позиция, для которой считался дедлайн.
    position: QuantizedTimelinePosition,
    /// Сам дедлайн.
    deadline: Instant,
}

/// Нижняя граница ожидания, если дедлайн позиции прошёл, а snapshot не изменился.
const POSITION_STALE_BACKOFF: Duration = Duration::from_millis(8);

/// Дедлайн позиции с учётом «snapshot ещё не догнал границу».
///
/// Если предыдущий дедлайн уже прошёл, а квантованная позиция та же, значит worker ещё не
/// опубликовал новую позицию: ждём его тик (длительность кадра, не меньше
/// [`POSITION_STALE_BACKOFF`]), а не считанные миллисекунды до «той же» границы снова.
fn position_wake_with_backoff(
    previous: Option<PositionWakeMemory>,
    current: QuantizedTimelinePosition,
    computed: Instant,
    frame_duration: Duration,
    now: Instant,
) -> Instant {
    match previous {
        Some(previous) if previous.position == current && now >= previous.deadline => {
            now + frame_duration.max(POSITION_STALE_BACKOFF)
        }
        _ => computed,
    }
}

/// Состояние решения о перерисовке; одно поле `AppState`.
pub(crate) struct RedrawRuntime {
    /// Порт владельца `PlayerPresentation`; worker будит его через bridge.
    presentation_wake_port: AppWakePort,
    /// Запросы egui на перерисовку (callback контекста).
    egui_repaint: Arc<EguiRepaintRequestSink>,
    /// Накопленные триггеры до следующего кадра.
    triggers: RedrawTriggers,
    /// UI-модель последнего полного прохода.
    last_ui_frame_model: Option<UiFrameModel>,
    /// Геометрия последнего полного прохода.
    retained_ui_geometry: Option<RetainedUiGeometry>,
    /// Момент последнего полного прохода (ритм обновления панели телеметрии).
    last_full_pass_at: Option<Instant>,
    /// Последний дедлайн позиции и позиция, для которой он считался.
    position_wake: Option<PositionWakeMemory>,
}

impl RedrawRuntime {
    /// Создаёт runtime: порт показа — сосед порта timeline над тем же event loop.
    pub(crate) fn new(egui_ctx: &egui::Context, player_timeline_wake_port: &AppWakePort) -> Self {
        Self {
            presentation_wake_port: player_timeline_wake_port
                .sibling_for_owner(AppWakeOwner::PlayerPresentation),
            egui_repaint: EguiRepaintRequestSink::install(egui_ctx),
            triggers: RedrawTriggers {
                // Первый кадр после создания всегда полный: удерживать ещё нечего.
                full_pass_forced: true,
                ..RedrawTriggers::default()
            },
            last_ui_frame_model: None,
            retained_ui_geometry: None,
            last_full_pass_at: None,
            position_wake: None,
        }
    }

    /// Bridge для `PlayerWorkerConfig::with_presentation_wake`.
    pub(crate) fn presentation_wake_bridge(
        &self,
    ) -> Arc<dyn player_core::PlayerWorkerPresentationWake> {
        player_presentation_wake_bridge(self.presentation_wake_port.clone())
    }
}

/// Факты настроек, которые знает только `SettingsRuntime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SettingsVisualFacts {
    pub(crate) window_open: bool,
    pub(crate) ui_model_revision: u64,
}

impl AppState {
    /// `true`, пока воспроизведение активно: UI держит страховочный таймер (сессия 04),
    /// а не непрерывный redraw, как раньше. Имя сохранено для shell-а.
    #[must_use]
    pub fn wants_continuous_redraw(&self) -> bool {
        let state = self.last_player_snapshot.playback_state;
        state.is_playback_active()
            || matches!(
                state,
                player_core::PlaybackState::Opening | player_core::PlaybackState::Scrubbing
            )
            || self.last_player_snapshot.timeline.scrubbing
            || self.has_pending_vod_endpoint_recovery()
    }

    /// Пришли оконные события или ввод: следующий кадр обязан сделать egui-проход.
    pub(crate) fn note_window_input_for_redraw(&mut self) {
        self.redraw.triggers.window_input = true;
    }

    /// Worker разбудил UI: кадр показа или событие плеера уже опубликованы.
    ///
    /// Edge порта очищается ДО чтения слота/очереди (как у `PlayerTimeline`), иначе публикация
    /// между чтением и очисткой потерялась бы до следующего пробуждения.
    pub(crate) fn note_presentation_wake(&mut self) {
        self.redraw.presentation_wake_port.clear_pending_for_drain();
        self.redraw.triggers.presentation_wake = true;
    }

    /// Другой владелец (субтитры, плейлист, startup, настройки) изменил видимое состояние.
    pub(crate) fn note_visible_mutation_for_redraw(&mut self) {
        self.redraw.triggers.visible_mutation = true;
    }

    /// Это пробуждение инициировал страховочный таймер воспроизведения.
    pub(crate) fn note_redraw_fallback_probe(&mut self) {
        self.redraw.triggers.fallback_probe = true;
    }

    /// Следующий кадр обязан быть полным проходом (shell отказал повторить интерфейс,
    /// renderer пересоздан, размер экрана изменился вне событий).
    pub(crate) fn force_full_ui_pass(&mut self) {
        self.redraw.triggers.full_pass_forced = true;
        self.redraw.retained_ui_geometry = None;
    }

    /// После отказа shell-а кадр видео не показан: просим следующий redraw сразу.
    pub(crate) fn request_follow_up_redraw_after_retained_ui_refusal(&mut self) {
        self.mark_pending_worker_redraw();
    }

    /// Забирает накопленные триггеры; после вызова они сброшены.
    #[must_use]
    pub(crate) fn take_redraw_triggers(&mut self) -> RedrawTriggers {
        std::mem::take(&mut self.redraw.triggers)
    }

    /// Ждёт ли follow-up redraw после асинхронной worker-команды (без сброса флага:
    /// его забирает pacing в конце кадра, как и раньше).
    #[must_use]
    pub(crate) const fn has_pending_worker_redraw(&self) -> bool {
        self.pending_redraw_after_worker_command
    }

    /// Запрос egui без его сброса — для решения о плане кадра.
    #[must_use]
    pub(crate) fn peek_egui_repaint_request(&self) -> EguiRepaintRequest {
        self.redraw.egui_repaint.peek()
    }

    /// Забирает запрос egui (перед проходом и после него: проход сам повторит запрос).
    #[must_use]
    pub(crate) fn take_egui_repaint_request(&mut self) -> EguiRepaintRequest {
        self.redraw.egui_repaint.take()
    }

    /// Идёт ли анимация, которую нужно продвигать проходами без ввода.
    #[must_use]
    pub(crate) fn ui_animation_in_flight(&self) -> bool {
        self.sidebar_controller.is_animating() || self.fullscreen_chrome.is_animating()
    }

    /// Собирает UI-модель кадра из владельцев состояния. Побочные эффекты только те, что
    /// и так делает каждый кадр (истечение тостов, inline-статуса).
    #[must_use]
    pub(crate) fn observe_ui_frame_model(
        &mut self,
        window: &Window,
        player_snapshot: &PlayerSnapshot,
        playlist: PlaylistVisualKey,
        settings: SettingsVisualFacts,
        now: Instant,
    ) -> UiFrameModel {
        let notifications = self.notifications_frame(player_snapshot, now);
        let timeline_inline_status = self.timeline_inline_status_message(now);
        let size = window.inner_size();
        let app = AppVisualFacts {
            window: WindowFacts {
                inner_size: [size.width.max(1), size.height.max(1)],
                pixels_per_point_bits: self.egui_ctx.pixels_per_point().to_bits(),
                is_maximized: window.is_maximized(),
                is_fullscreen: window.fullscreen().is_some(),
            },
            timeline_ui_state: &self.timeline_ui_state,
            timeline_inline_status,
            startup_pending: self.startup_pending.is_some(),
            chrome: self.fullscreen_chrome.presentation(),
            sidebar: SidebarVisualKey {
                displayed: self.sidebar_controller.displayed(),
                target: self.sidebar_controller.target(),
                open_progress: self.sidebar_controller.open_progress(),
                content_transition: self.sidebar_controller.content_transition(),
                open_width_points: self.sidebar_host_state.open_width_points(),
            },
            notifications: &notifications,
            toast_fade_in: TOAST_FADE_IN,
            telemetry_open: self.committed_config_snapshot.show_telemetry(),
            settings_window_open: settings.window_open,
            settings_ui_revision: settings.ui_model_revision,
            reduced_motion: self.committed_config_snapshot.reduced_motion(),
            playlist,
        };
        UiFrameModel::build(
            PlayerVisualFacts {
                snapshot: player_snapshot,
                timeline_track_width_px: self.timeline_ui_state.observed_track_width_px(),
            },
            app,
        )
    }

    /// Отличается ли модель от нарисованной последним полным проходом.
    #[must_use]
    pub(crate) fn ui_frame_model_changed(&self, model: &UiFrameModel) -> bool {
        self.redraw.last_ui_frame_model.as_ref() != Some(model)
    }

    /// Пока хоть один тост проявляется, проходы нужны непрерывно (как у анимации).
    #[must_use]
    pub(crate) fn toast_fade_in_flight(&self, model: &UiFrameModel) -> bool {
        model.notifications.any_toast_fading_in(TOAST_FADE_IN)
    }

    /// Панель телеметрии открыта, и срок обновления её строк (250 мс) подошёл.
    #[must_use]
    pub(crate) fn telemetry_refresh_due(&self, now: Instant) -> bool {
        self.next_telemetry_refresh_deadline()
            .is_some_and(|deadline| deadline <= now)
    }

    /// Когда открытая панель телеметрии снова потребует полный проход.
    #[must_use]
    pub(crate) fn next_telemetry_refresh_deadline(&self) -> Option<Instant> {
        if !self.committed_config_snapshot.show_telemetry() {
            return None;
        }
        self.redraw
            .last_full_pass_at
            .map(|last| last + TELEMETRY_PANEL_REFRESH_INTERVAL)
    }

    /// Ширина дорожки ползунка с последнего прохода — для дедлайна «следующий пиксель».
    #[must_use]
    pub(crate) fn timeline_track_width_px(&self) -> Option<u32> {
        self.timeline_ui_state.observed_track_width_px()
    }

    /// Когда позиция в следующий раз изменит картинку (секунда текста / пиксель ползунка),
    /// с защитой от спина на не догнавшем границу snapshot-е.
    #[must_use]
    pub(crate) fn next_position_wake_deadline(
        &mut self,
        player_snapshot: &PlayerSnapshot,
        quantized_position: QuantizedTimelinePosition,
        now: Instant,
    ) -> Option<Instant> {
        let computed =
            next_visible_position_change(player_snapshot, self.timeline_track_width_px(), now)?;
        let deadline = position_wake_with_backoff(
            self.redraw.position_wake,
            quantized_position,
            computed,
            player_snapshot.video_frame_duration_estimate,
            now,
        );
        self.redraw.position_wake = Some(PositionWakeMemory {
            position: quantized_position,
            deadline,
        });
        Some(deadline)
    }

    /// Причины показать кадр независимо от интерфейса. Read-only: кэш кадра не трогается.
    #[must_use]
    pub(crate) fn present_reasons(&self, player_snapshot: &PlayerSnapshot) -> PresentReasons {
        let video_frame_changed = match self.peek_present_frame_change(player_snapshot) {
            PresentFrameChange::Changed => true,
            PresentFrameChange::Unchanged | PresentFrameChange::NoFrame => false,
        };
        let seek_or_scrub_activity = player_snapshot.timeline.seeking
            || player_snapshot.timeline.scrubbing
            || matches!(
                player_snapshot.playback_state,
                player_core::PlaybackState::Seeking | player_core::PlaybackState::Scrubbing
            )
            || self.has_main_visual_override();
        PresentReasons {
            video_frame_changed,
            seek_or_scrub_activity,
            backend_swap_hold: self.backend_swap_from_generation.is_some(),
        }
    }

    /// Запоминает итог полного прохода: модель и геометрию для повторных показов.
    pub(crate) fn remember_full_ui_pass(
        &mut self,
        model: UiFrameModel,
        geometry: RetainedUiGeometry,
        now: Instant,
    ) {
        self.redraw.last_ui_frame_model = Some(model);
        self.redraw.retained_ui_geometry = Some(geometry);
        self.redraw.last_full_pass_at = Some(now);
    }

    /// Геометрия последнего полного прохода, если он был и ещё действителен.
    #[must_use]
    pub(crate) fn retained_ui_geometry(&self) -> Option<&RetainedUiGeometry> {
        self.redraw.retained_ui_geometry.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Приёмник превращает запросы egui в «сейчас» и «не позже дедлайна», а `take` сбрасывает.
    #[test]
    fn repaint_sink_collects_immediate_and_earliest_deadline() {
        let ctx = egui::Context::default();
        let sink = EguiRepaintRequestSink::install(&ctx);
        let now = Instant::now();

        assert_eq!(sink.peek(), EguiRepaintRequest::default());

        sink.record(Duration::from_millis(500), now);
        sink.record(Duration::from_millis(100), now);
        sink.record(Duration::from_millis(900), now);
        let request = sink.peek();
        assert!(!request.immediate);
        assert_eq!(request.deadline, Some(now + Duration::from_millis(100)));
        assert!(!request.is_due(now));
        assert!(request.is_due(now + Duration::from_millis(100)));

        sink.record(Duration::ZERO, now);
        assert!(sink.peek().immediate);
        assert!(sink.peek().is_due(now));

        let taken = sink.take();
        assert!(taken.immediate);
        assert_eq!(sink.peek(), EguiRepaintRequest::default());
    }

    /// Дедлайн позиции: обычно — вычисленная граница; если прошлый дедлайн прошёл, а snapshot
    /// не изменился, — ожидание тика worker-а, не меньше 8 мс (иначе спин по 1 мс).
    #[test]
    fn position_wake_backs_off_while_snapshot_lags_behind_the_boundary() {
        let now = Instant::now();
        let position = QuantizedTimelinePosition {
            whole_seconds: 12,
            slider_step: Some(120),
        };
        let other_position = QuantizedTimelinePosition {
            whole_seconds: 12,
            slider_step: Some(121),
        };
        let computed = now + Duration::from_millis(1);
        let frame_duration = Duration::from_millis(33);

        assert_eq!(
            position_wake_with_backoff(None, position, computed, frame_duration, now),
            computed,
            "first deadline is the computed boundary"
        );
        let pending = PositionWakeMemory {
            position,
            deadline: now + Duration::from_millis(20),
        };
        assert_eq!(
            position_wake_with_backoff(Some(pending), position, computed, frame_duration, now),
            computed,
            "a deadline still in the future keeps the computed boundary"
        );
        let expired = PositionWakeMemory {
            position,
            deadline: now - Duration::from_millis(1),
        };
        assert_eq!(
            position_wake_with_backoff(Some(expired), position, computed, frame_duration, now),
            now + frame_duration,
            "expired deadline with the same position waits for the worker tick"
        );
        assert_eq!(
            position_wake_with_backoff(
                Some(expired),
                position,
                computed,
                Duration::from_millis(2),
                now
            ),
            now + POSITION_STALE_BACKOFF,
            "backoff never drops below the floor"
        );
        assert_eq!(
            position_wake_with_backoff(
                Some(expired),
                other_position,
                computed,
                frame_duration,
                now
            ),
            computed,
            "a changed position is a fresh boundary, not a stale one"
        );
    }

    /// Настоящий `Context::request_repaint*` доходит до приёмника через callback egui.
    #[test]
    fn egui_context_requests_reach_the_sink_through_the_callback() {
        let ctx = egui::Context::default();
        let sink = EguiRepaintRequestSink::install(&ctx);

        ctx.request_repaint_after(Duration::from_millis(250));
        let request = sink.peek();
        assert!(!request.immediate);
        assert!(
            request.deadline.is_some(),
            "delayed request must become a deadline"
        );

        ctx.request_repaint();
        assert!(
            sink.peek().immediate,
            "immediate request must be visible at once"
        );
    }
}
