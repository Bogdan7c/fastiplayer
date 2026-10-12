use super::{
    url_import_failure_message, url_import_internal_failure_message, web_open_failure_message,
    web_open_failure_phrase,
};
use crate::local_open_message::local_open_failure_row_summary;
use crate::media_open::{PlayerInstallFailureReason, WebOpenFailureReason};

/// Все причины — исчерпывающий список проверяется компилятором через `match` в
/// `web_open_failure_phrase`; здесь — тот же список для проверки свойств текста.
const ALL_REASONS: [WebOpenFailureReason; 27] = [
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
    WebOpenFailureReason::CollectionTooLarge,
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

/// Тексты — английские, со строчной буквы, без имён Rust-типов и технического жаргона.
#[test]
fn every_reason_has_readable_english_phrase() {
    let forbidden_fragments = [
        "{",
        "Error",
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
                .any(|character| character.is_ascii_alphabetic()),
            "{debug_name}: message must be readable English: {phrase}"
        );
        assert!(
            phrase.chars().next().is_some_and(char::is_lowercase),
            "{debug_name}: The phrase comes after the colon and begins with the lowercase: {phrase}"
        );
        assert!(
            !phrase.contains(&debug_name),
            "{debug_name}: Type name in the text"
        );
        for fragment in forbidden_fragments {
            assert!(
                !phrase.contains(fragment),
                "{debug_name}: \"{fragment}\" in \"{phrase}\""
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
        "Could not open the link (example.test): page not found (error 404); check the link"
    );
    assert_eq!(
        web_open_failure_message(
            Some("youtube.com"),
            WebOpenFailureReason::ExtractorNotInstalled
        ),
        "Could not open the link (youtube.com): yt-dlp was not found; install the yt-dlp package"
    );
    assert_eq!(
        web_open_failure_message(Some("youtube.com"), WebOpenFailureReason::SitePrivateMedia),
        "Could not open the link (youtube.com): the video is private"
    );
    assert_eq!(
        web_open_failure_message(None, WebOpenFailureReason::NoConnection),
        "Could not open the link: cannot connect to the server; check your internet connection"
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
        "Could not open the link (example.test): unsupported video format"
    );
}

/// Бейдж строки очереди: та же фраза с заглавной буквы, без домена.
#[test]
fn row_summary_is_capitalized_phrase_without_host() {
    assert_eq!(
        local_open_failure_row_summary(WebOpenFailureReason::SiteGeoRestricted),
        "The video is unavailable in your region"
    );
    assert_eq!(
        local_open_failure_row_summary(WebOpenFailureReason::NotFound),
        "Page not found (error 404); check the link"
    );
}

/// Отказ «Добавить URL» показывает домен и ту же фразу причины, что и открытие ссылки;
/// внутренний сбой не выдумывает причину со стороны сайта (UX сессия 09).
#[test]
fn url_import_failure_texts_name_domain_and_reason_without_inventing_site_cause() {
    assert_eq!(
        url_import_failure_message(Some("youtube.com"), WebOpenFailureReason::SitePrivateMedia),
        "Could not add the link (youtube.com): the video is private"
    );
    assert_eq!(
        url_import_failure_message(None, WebOpenFailureReason::ExtractorNotInstalled),
        "Could not add the link: yt-dlp was not found; install the yt-dlp package"
    );
    assert_eq!(
        url_import_internal_failure_message(Some("youtube.com")),
        "Could not add the link (youtube.com); try again"
    );
    assert_eq!(
        url_import_internal_failure_message(None),
        "Could not add the link; try again"
    );
}
