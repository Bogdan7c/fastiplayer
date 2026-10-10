//! Тексты для пользователя о сбое пересборки текущего media при применении настроек (A10).
//!
//! Раньше сюда попадали строки вида `"web media rebuild failed: {error:?}"` — Debug-вывод
//! Rust-типа и английский текст. Теперь технические детали уходят в лог у места сбоя, а
//! пользователь получает русскую фразу; причину для web берём из той же таблицы, что и при
//! обычном открытии ссылки (`web_open_message`).

use crate::media_open::WebOpenFailureReason;

/// Начало фразы для любого сбоя пересборки текущего media при смене настроек.
const REBUILD_FAILED_PREFIX: &str = "Не удалось применить настройки к текущему видео";

/// Локальный файл не удалось подготовить заново (причина — в логе).
pub(crate) fn local_rebuild_failed_message() -> String {
    format!("{REBUILD_FAILED_PREFIX}: не удалось заново открыть файл")
}

/// Web-источник не удалось подготовить заново: причина берётся из таблицы открытия ссылок.
///
/// `None` — сбой без классифицированной причины (отмена, паника подготовки).
pub(crate) fn web_rebuild_failed_message(reason: Option<WebOpenFailureReason>) -> String {
    match reason {
        Some(reason) => format!(
            "{REBUILD_FAILED_PREFIX}: {}",
            crate::web_open_message::web_open_failure_phrase(reason)
        ),
        None => format!("{REBUILD_FAILED_PREFIX}: не удалось заново открыть ссылку"),
    }
}

/// Player не принял пересобранное media (strong install не завершился успешно).
pub(crate) fn install_rebuild_failed_message() -> String {
    format!("{REBUILD_FAILED_PREFIX}: не удалось перезапустить воспроизведение")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_message_uses_open_failure_phrase_without_debug_names() {
        let message = web_rebuild_failed_message(Some(WebOpenFailureReason::SitePrivateMedia));

        assert_eq!(
            message,
            "Не удалось применить настройки к текущему видео: видео приватное"
        );
    }

    #[test]
    fn unclassified_web_failure_is_still_human() {
        let message = web_rebuild_failed_message(None);

        assert!(
            message.contains("не удалось заново открыть ссылку"),
            "{message}"
        );
        assert!(!message.contains("rebuild"), "{message}");
    }

    #[test]
    fn local_and_install_messages_have_no_english_jargon() {
        for message in [
            local_rebuild_failed_message(),
            install_rebuild_failed_message(),
        ] {
            assert!(
                message.starts_with("Не удалось применить настройки"),
                "{message}"
            );
            assert!(!message.contains("rebuild"), "{message}");
            assert!(!message.contains('{'), "{message}");
        }
    }
}
