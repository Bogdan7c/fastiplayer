//! Отказ стартовой подготовки web-ссылки (CLI и восстановленная очередь, UX сессия 08).
//!
//! Раньше фоновые startup job-ы превращали ошибку в строку `format!("{error:#}")`, и эта
//! строка — технический дамп цепочки на смеси языков — попадала прямо в окно и в бейдж
//! строки очереди. Теперь job возвращает `StartupWebPreparationFailure`: причину для
//! пользователя (классифицирована по типам цепочки ещё в фоновом потоке) отдельно от
//! диагностики для лога. Форматирование — в `crate::web_open_message`.

use std::sync::Arc;

use media_source_open::web_open_failure::{WebOpenFailureReason, classify_web_open_failure};

use super::StartupMediaController;

/// Почему startup-подготовка web-ссылки не удалась.
#[derive(Debug)]
pub(crate) struct StartupWebPreparationFailure {
    /// Причина для пользователя (без URL и текста ошибки).
    reason: WebOpenFailureReason,
    /// Полная цепочка ошибки для лога. Locator-ы в ней уже редактированы владельцами
    /// (`safe_label`, `SecretHttpUrl`); в UI этот текст больше не показывается.
    diagnostic: String,
}

impl StartupWebPreparationFailure {
    /// Классифицирует ошибку подготовки и сохраняет её цепочку для лога.
    pub(crate) fn from_preparation_error(error: &anyhow::Error) -> Self {
        Self {
            reason: classify_web_open_failure(error),
            diagnostic: format!("{error:#}"),
        }
    }

    /// Отказ без типизированной причины (job завершился без результата, panic и т.п.).
    pub(crate) fn unclassified(diagnostic: impl Into<String>) -> Self {
        Self {
            reason: WebOpenFailureReason::Unclassified,
            diagnostic: diagnostic.into(),
        }
    }

    /// Причина для текста в окне и бейджа строки.
    pub(crate) const fn reason(&self) -> WebOpenFailureReason {
        self.reason
    }

    /// Диагностика для лога.
    pub(crate) fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

impl StartupMediaController {
    /// Запоминает безопасный домен ссылки, которую startup сейчас начнёт открывать.
    pub(crate) fn remember_web_display_host(&mut self, display_host: Option<String>) {
        self.web_display_host = display_host;
    }

    /// Домен текущей startup-ссылки для подписи текста ошибки.
    pub(super) fn web_display_host(&self) -> Option<&str> {
        self.web_display_host.as_deref()
    }

    /// Стартовая ошибка web-ссылки: понятный текст в окне и короткая причина на строке.
    ///
    /// В лог — типизированная причина и диагностическая цепочка (как и раньше, без
    /// raw URL). Окно: «Не удалось открыть ссылку (youtube.com): видео приватное»;
    /// строка восстановленного элемента: «Видео приватное».
    pub(super) fn handle_web_preparation_failure(
        &mut self,
        failure: StartupWebPreparationFailure,
        app_state: &mut crate::state::AppState,
        playlist_runtime: &mut crate::playlist_runtime::PlaylistRuntime,
    ) {
        tracing::warn!(
            reason = ?failure.reason(),
            error = %failure.diagnostic(),
            "Startup web media preparation failed"
        );
        let user_message = crate::web_open_message::web_open_failure_message(
            self.web_display_host(),
            failure.reason(),
        );
        let row_summary = Arc::<str>::from(
            crate::local_open_message::local_open_failure_row_summary(failure.reason()),
        );
        self.publish_preparation_failure(user_message, row_summary, app_state, playlist_runtime);
    }
}

#[cfg(test)]
#[path = "web_failure/tests.rs"]
mod tests;
