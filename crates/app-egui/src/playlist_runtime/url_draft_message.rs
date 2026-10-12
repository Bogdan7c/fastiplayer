//! Тексты ошибок формы «Добавить URL» плейлиста.
//!
//! Отдельно от `ui_interaction` (владелец состояния формы): здесь только перевод
//! типизированного отказа `append_playlist_url` в безопасный английский текст. Введённая
//! строка сюда не попадает, поэтому секреты из ссылки в тексте появиться не могут.

use std::sync::Arc;

use super::actions::{NotUrlInputHint, UrlAppendValidationError};

/// Подсказка для ссылки без схемы (UX сессия 09): схему не дописываем сами, а говорим,
/// чего не хватает.
const MISSING_SCHEME_MESSAGE: &str =
    "The link may be missing https:// at the beginning; for example, https://youtube.com/…";

/// Текст не похож на ссылку вовсе.
const NOT_URL_MESSAGE: &str = "Enter correct http(s) URL";

/// Безопасный текст ошибки под полем ввода URL.
pub(super) fn url_append_error_message(error: UrlAppendValidationError) -> Arc<str> {
    match error {
        UrlAppendValidationError::NotUrl(NotUrlInputHint::MissingScheme) => {
            MISSING_SCHEME_MESSAGE.into()
        }
        UrlAppendValidationError::NotUrl(NotUrlInputHint::Unrecognized) => NOT_URL_MESSAGE.into(),
        // Текст уже безопасен: его формирует классификатор сервиса без исходной строки.
        UrlAppendValidationError::Unsupported { safe_error } => Arc::from(safe_error),
        UrlAppendValidationError::RuntimeShuttingDown => "The app is shutting down".into(),
        UrlAppendValidationError::LoadDecisionPending => "Wait for Playlist to load".into(),
        UrlAppendValidationError::LocatorMapping
        | UrlAppendValidationError::ConfirmationIdentityExhausted
        | UrlAppendValidationError::TopologyGenerationExhausted
        | UrlAppendValidationError::TopologyWorkerUnavailable
        | UrlAppendValidationError::CommitRejected => "Could not add URL".into(),
    }
}
