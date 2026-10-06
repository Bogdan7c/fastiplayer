use super::{web_open_failure_message, web_open_failure_phrase};
use crate::local_open_message::local_open_failure_row_summary;
use crate::media_open::{PlayerInstallFailureReason, WebOpenFailureReason};

/// Все причины — исчерпывающий список проверяется компилятором через `match` в
/// `web_open_failure_phrase`; здесь — тот же список для проверки свойств текста.
const ALL_REASONS: [WebOpenFailureReason; 26] = [
    WebOpenFailureReason::ExtractorNotInstalled,
    WebOpenFailureReason::ExtractorDisabled,
    WebOpenFailureReason::ExtractorTimedOut,
    WebOpenFailureReason::ExtractorFailed,
    WebOpenFailureReason::SitePrivateMedia,
    WebOpenFailureReason::SiteLoginRequired,
    WebOpenFailureReason::SiteGeoRestricted,
    WebOpenFailureReason::SiteMediaUnavailable,
    WebOpenFailureReason::SiteUnsupported,
    WebOpenFailureReason::SiteRejected,
    WebOpenFailureReason::CollectionLink,
    WebOpenFailureReason::NoPlayableFormat,
    WebOpenFailureReason::NotFound,
    WebOpenFailureReason::Gone,
    WebOpenFailureReason::AuthenticationRequired,
    WebOpenFailureReason::AccessDenied,
    WebOpenFailureReason::RateLimited,
    WebOpenFailureReason::ServerError,
    WebOpenFailureReason::NoConnection,
    WebOpenFailureReason::TimedOut,
    WebOpenFailureReason::ConnectionInterrupted,
    WebOpenFailureReason::InvalidServerResponse,
    WebOpenFailureReason::UnrecognizedFormat,
    WebOpenFailureReason::UnsupportedLink,
    WebOpenFailureReason::UnsupportedLiveProfile,
    WebOpenFailureReason::Unclassified,
];

/// Тексты — русские, со строчной буквы, без имён Rust-типов и технического жаргона.
#[test]
fn every_reason_has_readable_russian_phrase() {
    let forbidden_fragments = [
        "{",
        "Error",
        "error",
        "Network",
        "provider",
        "extractor",
        "transport",
        "Debug",
    ];
    for reason in ALL_REASONS {
        let phrase = web_open_failure_phrase(reason);
        let debug_name = format!("{reason:?}");

        assert!(
            phrase
                .chars()
                .any(|character| ('а'..='я').contains(&character)),
            "{debug_name}: текст должен быть по-русски: {phrase}"
        );
        assert!(
            phrase.chars().next().is_some_and(char::is_lowercase),
            "{debug_name}: фраза идёт после двоеточия и начинается со строчной: {phrase}"
        );
        assert!(
            !phrase.contains(&debug_name),
            "{debug_name}: имя типа в тексте"
        );
        for fragment in forbidden_fragments {
            assert!(
                !phrase.contains(fragment),
                "{debug_name}: «{fragment}» в «{phrase}»"
            );
        }
    }
}

/// Разные причины не сливаются в один текст: пользователь видит разницу.
#[test]
fn every_reason_has_distinct_phrase() {
    let mut phrases: Vec<&str> = ALL_REASONS
        .into_iter()
        .map(web_open_failure_phrase)
        .collect();
    phrases.sort_unstable();
    phrases.dedup();

    assert_eq!(phrases.len(), ALL_REASONS.len());
}

/// Таблица, утверждённая владельцем: ключевые причины с подсказкой действия.
#[test]
fn window_message_names_host_and_actionable_reason() {
    assert_eq!(
        web_open_failure_message(Some("example.test"), WebOpenFailureReason::NotFound),
        "Не удалось открыть ссылку (example.test): страница не найдена (ошибка 404) — проверьте ссылку"
    );
    assert_eq!(
        web_open_failure_message(
            Some("youtube.com"),
            WebOpenFailureReason::ExtractorNotInstalled
        ),
        "Не удалось открыть ссылку (youtube.com): не найдена программа yt-dlp — установите пакет yt-dlp"
    );
    assert_eq!(
        web_open_failure_message(Some("youtube.com"), WebOpenFailureReason::SitePrivateMedia),
        "Не удалось открыть ссылку (youtube.com): видео приватное"
    );
    assert_eq!(
        web_open_failure_message(None, WebOpenFailureReason::NoConnection),
        "Не удалось открыть ссылку: нет соединения с сервером — проверьте интернет"
    );
}

/// Отказ player-а для web-ссылки идёт тем же шаблоном (одна модель «почему не открылось»).
#[test]
fn player_failure_for_link_uses_same_template() {
    assert_eq!(
        web_open_failure_message(
            Some("example.test"),
            PlayerInstallFailureReason::UnsupportedVideoFormat
        ),
        "Не удалось открыть ссылку (example.test): формат видео не поддерживается"
    );
}

/// Бейдж строки очереди: та же фраза с заглавной буквы, без домена.
#[test]
fn row_summary_is_capitalized_phrase_without_host() {
    assert_eq!(
        local_open_failure_row_summary(WebOpenFailureReason::SiteGeoRestricted),
        "Видео недоступно в вашем регионе"
    );
    assert_eq!(
        local_open_failure_row_summary(WebOpenFailureReason::NotFound),
        "Страница не найдена (ошибка 404) — проверьте ссылку"
    );
}
