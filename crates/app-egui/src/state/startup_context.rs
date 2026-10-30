//! Узкая startup-композиция `AppState` и payload-free wake boundary player timeline.

use std::sync::Arc;
use std::time::Instant;

use crate::app_wake::AppWakePort;

/// Process-origin и безопасная startup-ошибка передаются в `AppState` одним typed контекстом.
pub(crate) struct AppStateStartupContext {
    /// Момент process bootstrap нужен только startup-readiness owner-у в родительском модуле.
    pub(super) process_started_at: Instant,
    /// Уже санитизированная bootstrap-ошибка переносится без изменения текста и lifecycle.
    pub(super) startup_error: Option<String>,
    /// Предупреждение об итоге загрузки config-а (сессия 05): показывается до ×.
    ///
    /// `AppShell` передаёт его ровно один раз — в первый созданный `AppState`, поэтому
    /// пересоздание `AppState` после suspend не показывает предупреждение повторно.
    pub(super) config_warning: Option<String>,
    /// Wake-порт фоновой загрузки субтитров: process-lifetime, копируется в каждый `AppState`.
    pub(super) subtitle_wake_port: AppWakePort,
    /// Память выбора дорожек: общая для всех `AppState` процесса, чтобы пересоздание после
    /// suspend не теряло выбор и не перечитывало файл.
    pub(super) track_memory: super::track_memory::TrackMemoryHandle,
    /// Отчёт выбора рендера: строка телеметрии и уведомление о сбое выбора (сессия 09).
    pub(super) renderer_report: Option<crate::render_backend::RendererStartupReport>,
    /// Куда идёт звук плеера, если не на устройство из настроек (окно браузерного режима:
    /// выход со счётчиком журнала, в тестах — тихая карта). Обычное окно его не задаёт.
    audio_output_override: Option<Arc<dyn player_core::AudioOutputFactory>>,
    /// Нужен ли отвод копий событий плеера (окно браузерного режима).
    player_event_tap: PlayerEventTapRequest,
    /// Вид окна и маршрут транспорта (`state/window_mode.rs`).
    window_kind: WindowKind,
}

/// Какое окно рисует `AppState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    /// Обычный плеер.
    Standard,
    /// Окно браузерного режима: без очереди, транспорт ведёт страница.
    BrowserSession,
}

/// Нужен ли `AppState` отвод копий событий плеера для второго наблюдателя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlayerEventTapRequest {
    /// Обычное окно: события читает только `AppState`.
    NotNeeded,
    /// Окно браузерного режима: копии событий нужны его журналу и учёту загрузок.
    Copies,
}

impl AppStateStartupContext {
    /// Создаёт точный startup-контекст до запуска player worker-а.
    pub(crate) fn new(
        process_started_at: Instant,
        startup_error: Option<String>,
        config_warning: Option<String>,
        subtitle_wake_port: AppWakePort,
        track_memory: super::track_memory::TrackMemoryHandle,
    ) -> Self {
        Self {
            process_started_at,
            startup_error,
            config_warning,
            subtitle_wake_port,
            track_memory,
            renderer_report: None,
            audio_output_override: None,
            player_event_tap: PlayerEventTapRequest::NotNeeded,
            window_kind: WindowKind::Standard,
        }
    }

    /// Окно браузерного режима: вид без очереди и открытия, транспорт ведёт страница.
    #[must_use]
    pub(crate) fn for_browser_session_window(mut self) -> Self {
        self.window_kind = WindowKind::BrowserSession;
        self
    }

    /// Режим окна для нового `AppState`.
    pub(super) fn window_mode(&self) -> super::window_mode::WindowMode {
        match self.window_kind {
            WindowKind::Standard => super::window_mode::WindowMode::standard(),
            WindowKind::BrowserSession => super::window_mode::WindowMode::browser_session(),
        }
    }

    /// Включает отвод копий событий плеера (`AppState::take_tapped_player_events`).
    #[must_use]
    pub(crate) fn with_player_event_tap(mut self) -> Self {
        self.player_event_tap = PlayerEventTapRequest::Copies;
        self
    }

    /// Отвод для нового `AppState`, если его просили.
    pub(super) fn player_event_tap(&self) -> Option<super::player_event_tap::PlayerEventTap> {
        match self.player_event_tap {
            PlayerEventTapRequest::NotNeeded => None,
            PlayerEventTapRequest::Copies => {
                Some(super::player_event_tap::PlayerEventTap::with_capacity(
                    super::player_event_tap::PLAYER_EVENT_TAP_CAPACITY,
                ))
            }
        }
    }

    /// Звук плеера идёт в эту фабрику выхода, а не на устройство из настроек.
    ///
    /// Выбор устройства пользователем всё равно доходит до фабрики: окно браузерного
    /// режима оборачивает той же `CpalAudioOutputFactory` с тем же контроллером
    /// устройства, добавляя только счётчик.
    #[must_use]
    pub(crate) fn with_audio_output_factory(
        mut self,
        factory: Arc<dyn player_core::AudioOutputFactory>,
    ) -> Self {
        self.audio_output_override = Some(factory);
        self
    }

    /// Фабрика выхода звука для плеера: заданная явно или CPAL на устройство из настроек.
    pub(super) fn audio_output_factory(
        &self,
        device_controller: audio::AudioOutputDeviceController,
    ) -> Arc<dyn player_core::AudioOutputFactory> {
        match &self.audio_output_override {
            Some(factory) => Arc::clone(factory),
            None => Arc::new(audio::CpalAudioOutputFactory::new(device_controller)),
        }
    }

    /// Добавляет отчёт выбора рендера, с которым стартует приложение.
    #[must_use]
    pub(crate) fn with_renderer_report(
        mut self,
        report: crate::render_backend::RendererStartupReport,
    ) -> Self {
        self.renderer_report = Some(report);
        self
    }
}

/// Показывает уведомление «до ×» о сбое выбора рендера; обычный выбор молчит (решение 40).
///
/// Идёт тем же путём, что предупреждение о битом config-е: `AppShell` передаёт отчёт в
/// startup-контекст, `AppState::new` показывает уведомление первым же `NotificationCenter`.
pub(super) fn announce_renderer_selection(
    renderer_report: Option<&crate::render_backend::RendererStartupReport>,
    notifications: &mut super::notifications::NotificationCenter,
    now: Instant,
) {
    if let Some(notice) = renderer_report.and_then(|report| report.user_notice()) {
        notifications.notify_until_dismissed(notice, now);
    }
}

/// Создаёт единственный intent-level wake port для player timeline activity.
pub(super) fn player_timeline_wake_bridge(
    wake_port: AppWakePort,
) -> Arc<dyn player_core::PlayerWorkerTimelineWake> {
    Arc::new(PlayerTimelineWakeBridge { wake_port })
}

/// Payload-free adapter между player worker и process-owned winit wake port.
struct PlayerTimelineWakeBridge {
    /// Точный application owner-порт; player payload остаётся в snapshot owner-е.
    wake_port: AppWakePort,
}

impl player_core::PlayerWorkerTimelineWake for PlayerTimelineWakeBridge {
    /// Преобразует player timeline activity ровно в один coalescing application wake edge.
    fn wake_player_timeline(&self) {
        // Delivery не меняет player semantics: закрытый event loop остаётся terminal для порта.
        let _delivery = self.wake_port.request_wake();
    }
}

/// Создаёт wake bridge «новый кадр показа / событие плеера» (cpu-parity, сессия 04).
pub(super) fn player_presentation_wake_bridge(
    wake_port: AppWakePort,
) -> Arc<dyn player_core::PlayerWorkerPresentationWake> {
    Arc::new(PlayerPresentationWakeBridge { wake_port })
}

/// Payload-free adapter: кадр и события уже лежат в слотах worker-а, порт только будит.
struct PlayerPresentationWakeBridge {
    /// Порт владельца `AppWakeOwner::PlayerPresentation`; наводнение сливается в один edge.
    wake_port: AppWakePort,
}

impl player_core::PlayerWorkerPresentationWake for PlayerPresentationWakeBridge {
    /// Один coalescing wake edge на любое число публикаций между проходами UI.
    fn wake_presentation(&self) {
        let _delivery = self.wake_port.request_wake();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{player_presentation_wake_bridge, player_timeline_wake_bridge};
    use crate::app_wake::{AppWakeEvent, AppWakeOwner, AppWakePort, WakeEmitter};

    /// Запоминает application owner настоящего `AppWakePort`, не подменяя сам порт.
    struct RecordingWakeEmitter {
        /// Shared observation позволяет проверить delivery после вызова player trait object-а.
        delivered_owners: Arc<Mutex<Vec<AppWakeOwner>>>,
    }

    impl WakeEmitter for RecordingWakeEmitter {
        /// Принимает тот же `AppWakeEvent`, который production emitter передаёт winit loop-у.
        fn emit(&self, event: AppWakeEvent) -> Result<(), ()> {
            self.delivered_owners
                .lock()
                .expect("recording wake mutex must not be poisoned")
                .push(event.owner());
            Ok(())
        }
    }

    /// Startup bridge проходит реальную application/player boundary и не теряет owner identity.
    #[test]
    fn startup_timeline_wake_bridge_reaches_application_player_boundary() {
        // Observation принадлежит тесту, а emitter получает только clone shared owner-а.
        let delivered_owners = Arc::new(Mutex::new(Vec::new()));
        // Real application port сохраняет production coalescing/epoch semantics.
        let wake_port = AppWakePort::new(
            AppWakeOwner::PlayerTimeline,
            Arc::new(RecordingWakeEmitter {
                delivered_owners: Arc::clone(&delivered_owners),
            }),
        );
        // Та же фабрика вызывается `AppState::new` во время настоящего startup.
        let player_timeline_wake = player_timeline_wake_bridge(wake_port.clone());

        // Вызов идёт через player-core trait object, а не напрямую в application helper.
        player_timeline_wake.wake_player_timeline();

        // Application port увидел ровно одну публикацию player timeline activity.
        assert_eq!(wake_port.publish_epoch_for_test(), 1);
        // Winit-side boundary получил точного owner-а без payload или подмены события.
        assert_eq!(
            delivered_owners
                .lock()
                .expect("recording wake mutex must not be poisoned")
                .as_slice(),
            &[AppWakeOwner::PlayerTimeline]
        );
    }

    /// Bridge показа доходит до winit как owner `PlayerPresentation`, и наводнение публикаций
    /// между проходами UI сливается в одно пробуждение (edge не очищен — второй wake коalesced).
    #[test]
    fn presentation_wake_bridge_reaches_event_loop_once_until_drained() {
        let delivered_owners = Arc::new(Mutex::new(Vec::new()));
        let timeline_port = AppWakePort::new(
            AppWakeOwner::PlayerTimeline,
            Arc::new(RecordingWakeEmitter {
                delivered_owners: Arc::clone(&delivered_owners),
            }),
        );
        // Как в `AppState::new`: порт показа — сосед порта timeline над тем же event loop.
        let presentation_port = timeline_port.sibling_for_owner(AppWakeOwner::PlayerPresentation);
        let presentation_wake = player_presentation_wake_bridge(presentation_port.clone());

        presentation_wake.wake_presentation();
        presentation_wake.wake_presentation();
        presentation_wake.wake_presentation();

        assert_eq!(presentation_port.publish_epoch_for_test(), 3);
        assert_eq!(
            delivered_owners
                .lock()
                .expect("recording wake mutex must not be poisoned")
                .as_slice(),
            &[AppWakeOwner::PlayerPresentation],
            "three publications before a UI drain must become one event-loop wake"
        );

        presentation_port.clear_pending_for_drain();
        presentation_wake.wake_presentation();
        assert_eq!(
            delivered_owners
                .lock()
                .expect("recording wake mutex must not be poisoned")
                .len(),
            2,
            "after the UI drained the edge, the next publication wakes the loop again"
        );
    }

    /// Фабрика-заглушка: в тесте важна только её идентичность, выход она не создаёт.
    struct RefusingAudioFactory;

    impl player_core::AudioOutputFactory for RefusingAudioFactory {
        fn create_output(
            &self,
            _spec: player_core::AudioOutputSpec,
            _device_request: player_core::AudioOutputDeviceRequest,
        ) -> anyhow::Result<player_core::CreatedAudioOutput> {
            Err(anyhow::anyhow!("test factory never opens an output"))
        }
    }

    /// Явная фабрика звука доходит до плеера как есть; без неё — устройство из настроек.
    #[test]
    fn audio_output_override_replaces_the_configured_device_factory() {
        let context = || {
            super::AppStateStartupContext::new(
                std::time::Instant::now(),
                None,
                None,
                AppWakePort::disconnected(AppWakeOwner::Subtitles),
                crate::state::start_track_memory_in_memory_only(
                    playlist_state::TrackMemoryCapacity::new(1).expect("capacity"),
                ),
            )
        };
        let custom: Arc<dyn player_core::AudioOutputFactory> = Arc::new(RefusingAudioFactory);
        let overridden = context().with_audio_output_factory(Arc::clone(&custom));
        let chosen =
            overridden.audio_output_factory(audio::AudioOutputDeviceController::new("default"));
        assert!(
            Arc::ptr_eq(&chosen, &custom),
            "the override reaches the player"
        );

        let default_choice =
            context().audio_output_factory(audio::AudioOutputDeviceController::new("default"));
        assert!(
            !Arc::ptr_eq(&default_choice, &custom),
            "without an override the configured device factory is built"
        );
    }
}
