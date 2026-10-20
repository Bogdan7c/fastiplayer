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
        }
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
}
