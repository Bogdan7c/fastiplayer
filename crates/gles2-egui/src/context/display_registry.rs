//! Один `EGLDisplay` на нативный display процесса и подсчёт его пользователей.
//!
//! Зачем: `eglGetPlatformDisplay` на тот же `wl_display`/Xlib `Display` возвращает ТОТ ЖЕ
//! `EGLDisplay` (EGL 1.5, раздел 3.2), а `eglInitialize` не считает вызовы. Поэтому
//! `eglTerminate` одного пользователя разрушил бы контексты всех остальных. Ровно это случилось
//! бы при пересоздании рендера (сессия 02a): старый рендер отпускает окно, новый уже создан на
//! том же display, и старый при разрушении вызывает `eglTerminate`.
//!
//! Правило: каждый контекст держит одну аренду ([`DisplayLease`]). `eglTerminate` вызывается,
//! только когда отпущена последняя аренда. Fence-ы держат `Arc` аренды своего контекста —
//! display не завершится, пока жив хоть один `EGLSync` на нём.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::driver::{DisplayId, DisplayString, EglCallError, EglDriver, EglVersion, NativeDisplay};
use crate::capabilities::ExtensionSet;

/// Что известно о display после `eglInitialize`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisplayInfo {
    /// Версия EGL этого display.
    pub(crate) version: EglVersion,
    /// `EGL_VENDOR`.
    pub(crate) vendor: String,
    /// `EGL_EXTENSIONS` display.
    pub(crate) extensions: ExtensionSet,
}

/// Почему display не открылся.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DisplayOpenError {
    /// `eglGetPlatformDisplay` или `eglInitialize` отказали.
    #[error("EGL display could not be opened: {0}")]
    Driver(#[from] EglCallError),
    /// Display работает на интерфейсе старше EGL 1.5 (наш минимум).
    #[error("EGL display version {found} is older than 1.5")]
    VersionTooOld {
        /// Что сообщил `eglInitialize`.
        found: EglVersion,
    },
}

/// Запись реестра: открытый display и число его аренд.
struct DisplayEntry {
    native: NativeDisplay,
    display: DisplayId,
    info: Arc<DisplayInfo>,
    leases: usize,
}

/// Реестр открытых display одного драйвера.
pub(crate) struct DisplayRegistry {
    driver: Arc<dyn EglDriver>,
    entries: Mutex<Vec<DisplayEntry>>,
}

impl DisplayRegistry {
    /// Пустой реестр поверх драйвера.
    pub(crate) fn new(driver: Arc<dyn EglDriver>) -> Arc<Self> {
        Arc::new(Self {
            driver,
            entries: Mutex::new(Vec::new()),
        })
    }

    /// Драйвер, на котором открыты display этого реестра.
    pub(crate) fn driver(&self) -> &Arc<dyn EglDriver> {
        &self.driver
    }

    /// Аренда display: существующего (счётчик +1) или нового (`eglGetPlatformDisplay` +
    /// `eglInitialize`). Весь шаг идёт под блокировкой реестра, поэтому параллельные открытие
    /// и завершение одного display не пересекаются.
    pub(crate) fn acquire(
        self: &Arc<Self>,
        native: NativeDisplay,
    ) -> Result<DisplayLease, DisplayOpenError> {
        let mut entries = self.lock_entries();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.native == native) {
            entry.leases += 1;
            return Ok(DisplayLease {
                registry: Arc::clone(self),
                native,
                display: entry.display,
                info: Arc::clone(&entry.info),
            });
        }

        let display = self.driver.get_platform_display(native)?;
        let version = self.driver.initialize(display)?;
        let info = match self.read_display_info(display, version) {
            Ok(info) => info,
            Err(error) => {
                // Display инициализировали мы и других пользователей у него нет (записи не было
                // под блокировкой) — завершаем, чтобы не оставить висящий display.
                self.terminate_logged(display);
                return Err(error);
            }
        };
        let info = Arc::new(info);
        entries.push(DisplayEntry {
            native,
            display,
            info: Arc::clone(&info),
            leases: 1,
        });
        Ok(DisplayLease {
            registry: Arc::clone(self),
            native,
            display,
            info,
        })
    }

    fn read_display_info(
        &self,
        display: DisplayId,
        version: EglVersion,
    ) -> Result<DisplayInfo, DisplayOpenError> {
        if version < (EglVersion { major: 1, minor: 5 }) {
            return Err(DisplayOpenError::VersionTooOld { found: version });
        }
        let vendor = self
            .driver
            .query_display_string(display, DisplayString::Vendor)?;
        let extensions = self
            .driver
            .query_display_string(display, DisplayString::Extensions)?;
        Ok(DisplayInfo {
            version,
            vendor,
            extensions: ExtensionSet::from_space_separated(&extensions),
        })
    }

    fn release(&self, native: NativeDisplay) {
        let mut entries = self.lock_entries();
        let Some(index) = entries.iter().position(|entry| entry.native == native) else {
            log::error!("EGL display lease released twice or for an unknown display");
            return;
        };
        let Some(entry) = entries.get_mut(index) else {
            return;
        };
        entry.leases = entry.leases.saturating_sub(1);
        if entry.leases == 0 {
            let removed = entries.swap_remove(index);
            // Завершение под блокировкой: новое открытие того же display дождётся конца
            // `eglTerminate` и инициализирует display заново, а не получит умирающий.
            self.terminate_logged(removed.display);
        }
    }

    fn terminate_logged(&self, display: DisplayId) {
        if let Err(error) = self.driver.terminate(display) {
            // Разрушение не может вернуть ошибку наверх (вызывается из Drop); фиксируем в логе.
            log::warn!("eglTerminate failed: {error}");
        }
    }

    /// Блокировка реестра.
    ///
    /// Отравление снимается: каждая критическая секция меняет список только ПОСЛЕ успешного
    /// вызова драйвера одним шагом (`push`, `+= 1`, `swap_remove`). Паника внутри вызова
    /// драйвера оставляет список прежним и согласованным.
    fn lock_entries(&self) -> MutexGuard<'_, Vec<DisplayEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Сколько аренд у display сейчас (для тестов).
    #[cfg(test)]
    pub(crate) fn lease_count(&self, native: NativeDisplay) -> usize {
        self.lock_entries()
            .iter()
            .find(|entry| entry.native == native)
            .map_or(0, |entry| entry.leases)
    }
}

/// Аренда открытого display. Drop последней аренды вызывает `eglTerminate`.
pub(crate) struct DisplayLease {
    registry: Arc<DisplayRegistry>,
    native: NativeDisplay,
    display: DisplayId,
    info: Arc<DisplayInfo>,
}

impl DisplayLease {
    /// Дескриптор display.
    pub(crate) fn display(&self) -> DisplayId {
        self.display
    }

    /// Драйвер display.
    pub(crate) fn driver(&self) -> &dyn EglDriver {
        self.registry.driver().as_ref()
    }

    /// Версия, вендор и расширения display.
    pub(crate) fn info(&self) -> &DisplayInfo {
        &self.info
    }
}

impl Drop for DisplayLease {
    fn drop(&mut self) {
        self.registry.release(self.native);
    }
}

impl std::fmt::Debug for DisplayLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DisplayLease")
            .field("native", &self.native)
            .field("display", &self.display)
            .finish_non_exhaustive()
    }
}
