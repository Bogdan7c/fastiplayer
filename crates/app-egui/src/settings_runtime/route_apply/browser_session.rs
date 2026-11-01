//! Применение маршрута `browser_session` (`[browser_session]` и запоминаемое окно).
//!
//! Две группы маршрута (контракт — `fastiplayer_settings::routing::browser_session`):
//! - `BrowserSessionBuffer` (`buffer_ahead_s`, потолок памяти `max_held_mb`) — окно буфера источника «байты от
//!   страницы». Действует на СЛЕДУЮЩЕЕ видео со страницы (решения 96/124): здесь новое
//!   окно отдаётся владельцу [`BrowserBufferWindowOwner`], идущий поток не перестраивается.
//!   Владелец есть только в окне браузерного режима. В обычном плеере владельца нет, и
//!   маршрут честно ничего не меняет: у обычного плеера нет источника «байты от страницы»,
//!   значения просто записаны в config и подхватятся окном браузерного режима при его
//!   следующем запуске (результат — `Applied` без эффекта, не ошибка и не отказ);
//! - `BrowserSessionWindow` (размер и фуллскрин окна) — «запоминаемое состояние»
//!   (решение 137): ключи только записываются, живое окно по ним не меняется.

use std::sync::Arc;

use fastiplayer_config::BrowserSessionBufferWindow;
use fastiplayer_settings::BrowserSessionRuntimeSettingsUpdate;

use super::*;

/// Кто принимает окно буфера для следующего видео со страницы.
///
/// Интерфейс по смыслу, а не по устройству хранения: реализация (процесс браузерного
/// режима) сама решает, где лежит значение и кто его прочитает (поток приёма).
pub(crate) trait BrowserBufferWindowOwner: Send + Sync {
    /// Следующий поток со страницы начнёт с этого окна; идущий не меняется.
    fn adopt_buffer_window_for_next_stream(&self, window: BrowserSessionBufferWindow);
}

/// Исполнитель маршрута `browser_session` внутри `SettingsRuntimeRouteAppliers`.
#[derive(Default)]
pub(in crate::settings_runtime) struct BrowserSessionRouteApplier {
    /// Владелец окна буфера: есть только в окне браузерного режима.
    buffer_window_owner: Option<Arc<dyn BrowserBufferWindowOwner>>,
}

impl BrowserSessionRouteApplier {
    /// Подключает владельца окна буфера (окно браузерного режима, один раз при сборке).
    pub(in crate::settings_runtime) fn attach_buffer_window_owner(
        &mut self,
        owner: Arc<dyn BrowserBufferWindowOwner>,
    ) {
        self.buffer_window_owner = Some(owner);
    }

    /// Применяет committed-снимок секции. Всегда `Applied`: живьём ничего не может
    /// отказать (значение уже проверено схемой, владелец только запоминает копию).
    pub(in crate::settings_runtime) fn apply(
        &self,
        update: &BrowserSessionRuntimeSettingsUpdate,
        groups: &[AppRuntimeRouteGroupUpdate],
    ) -> AppRouteApplyResult {
        let buffer_changed = groups
            .iter()
            .any(|group| group.group == AppRuntimeRouteGroup::BrowserSessionBuffer);
        if buffer_changed && let Some(owner) = &self.buffer_window_owner {
            owner.adopt_buffer_window_for_next_stream(update.buffer_window());
        }
        // `BrowserSessionWindow`: запоминаемое состояние — живьём ничего не делаем.
        AppRouteApplyResult::Applied
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::Duration;

    use fastiplayer_config::BrowserSessionConfig;

    use super::*;

    /// Запись усыновлённых окон.
    #[derive(Default)]
    struct RecordingOwner {
        adopted: Mutex<Vec<(Duration, usize)>>,
    }

    impl BrowserBufferWindowOwner for RecordingOwner {
        fn adopt_buffer_window_for_next_stream(&self, window: BrowserSessionBufferWindow) {
            self.adopted
                .lock()
                .expect("adopted windows")
                .push((window.ahead(), window.max_held_bytes()));
        }
    }

    fn update_with_buffer(ahead_s: u16, max_held_mb: u16) -> BrowserSessionRuntimeSettingsUpdate {
        BrowserSessionRuntimeSettingsUpdate {
            browser_session: BrowserSessionConfig {
                buffer_ahead_s: ahead_s,
                max_held_mb,
                ..BrowserSessionConfig::default()
            },
        }
    }

    fn group(group: AppRuntimeRouteGroup) -> AppRuntimeRouteGroupUpdate {
        AppRuntimeRouteGroupUpdate {
            group,
            affected_settings: Vec::new(),
        }
    }

    /// Группа буфера доносит новое окно владельцу; результат — Applied.
    #[test]
    fn buffer_group_hands_the_new_window_to_the_owner() {
        let owner = Arc::new(RecordingOwner::default());
        let mut applier = BrowserSessionRouteApplier::default();
        applier.attach_buffer_window_owner(owner.clone());

        let result = applier.apply(
            &update_with_buffer(45, 512),
            &[group(AppRuntimeRouteGroup::BrowserSessionBuffer)],
        );

        assert_eq!(result, AppRouteApplyResult::Applied);
        assert_eq!(
            *owner.adopted.lock().expect("adopted windows"),
            vec![(Duration::from_secs(45), 512 * 1024 * 1024)]
        );
    }

    /// Запоминаемое окно живьём не применяется: владелец буфера не трогается.
    #[test]
    fn window_group_does_not_touch_the_buffer_owner() {
        let owner = Arc::new(RecordingOwner::default());
        let mut applier = BrowserSessionRouteApplier::default();
        applier.attach_buffer_window_owner(owner.clone());

        let result = applier.apply(
            &update_with_buffer(45, 512),
            &[group(AppRuntimeRouteGroup::BrowserSessionWindow)],
        );

        assert_eq!(result, AppRouteApplyResult::Applied);
        assert!(owner.adopted.lock().expect("adopted windows").is_empty());
    }

    /// Обычный плеер (владельца нет): Applied без эффекта и без паники.
    #[test]
    fn without_an_owner_the_route_is_applied_with_no_effect() {
        let applier = BrowserSessionRouteApplier::default();

        let result = applier.apply(
            &update_with_buffer(45, 512),
            &[group(AppRuntimeRouteGroup::BrowserSessionBuffer)],
        );

        assert_eq!(result, AppRouteApplyResult::Applied);
    }
}
