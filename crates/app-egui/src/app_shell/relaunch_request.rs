//! Граница между shell-ом и `main` для перезапуска приложения («Restart now»).
//!
//! Shell не запускает новый процесс сам: пока он жив, он держит замок single-instance, и
//! новый процесс проиграл бы гонку. Он лишь сообщает `main`, просили ли перезапуск и как
//! прошло закрытие, и отдаёт замок для освобождения (`into_instance_lease`).

use super::AppShell;
use super::shutdown::shutdown_quality_of;
use crate::app_instance::{AppInstanceLease, RelaunchRequest, ShutdownQuality};

impl AppShell {
    /// Просил ли пользователь перезапуск (кнопка «Restart now» в настройках).
    #[must_use]
    pub(crate) fn relaunch_request(&self) -> RelaunchRequest {
        self.settings_runtime.relaunch_request()
    }

    /// Как закончилось закрытие владельцев: перезапуск разрешён только после чистого.
    ///
    #[must_use]
    pub(crate) fn shutdown_quality(&self) -> ShutdownQuality {
        shutdown_quality_of(self.process_lifecycle)
    }

    /// Разбирает завершённый shell: все остальные владельцы освобождаются здесь, а замок
    /// single-instance возвращается вызывающему последним (он отпускает его сам).
    #[must_use]
    pub(crate) fn into_instance_lease(self) -> AppInstanceLease {
        self._instance_lease
    }
}
