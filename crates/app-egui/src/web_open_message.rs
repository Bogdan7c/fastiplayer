//! Тексты для пользователя об открытии web-ссылки (UX сессия 08).
//!
//! Единственный владелец формулировок web-причин. Причину определяет
//! `media_source_open::web_open_failure` (классификация живёт рядом с ошибками
//! владельцев), здесь — только превращение причины в английский текст с подсказкой.
//!
//! Бейдж строки плейлиста строится общей функцией
//! `local_open_message::local_open_failure_row_summary`: она диспетчеризует сюда через
//! `MediaOpenUserFailureReason::WebOpen`, поэтому строка очереди для local и web
//! выглядит одинаково («Файл не найден» / «Страница не найдена (ошибка 404) — …»).
//!
//! Приватность (решение владельца, сессия 08): в тексте только домен сайта — без пути,
//! query, логина и пароля. Домен вычисляет владелец locator-а
//! (`StartupUrlLocator::display_host`), сюда он приходит уже безопасным.

use crate::media_open::{MediaOpenUserFailureReason, WebOpenFailureReason};

/// Сообщение об ошибке: «Не удалось открыть ссылку (youtube.com): видео приватное».
///
/// `display_host` — безопасный домен сайта; `None`, если домен неизвестен
/// (тогда «Не удалось открыть ссылку: …»). Принимает любую причину через `Into`:
/// для web-ссылки может отказать и подготовка, и установка в player.
pub(crate) fn web_open_failure_message(
    display_host: Option<&str>,
    reason: impl Into<MediaOpenUserFailureReason>,
) -> String {
    let phrase = crate::local_open_message::failure_reason_phrase(reason.into());
    match display_host {
        Some(host) => format!("Could not open the link ({host}): {phrase}"),
        None => format!("Could not open the link: {phrase}"),
    }
}

/// Отказ кнопки «Добавить URL» плейлиста (UX сессия 09): «Не удалось добавить ссылку
/// (youtube.com): видео приватное».
///
/// Фраза причины — та же таблица, что и для открытия ссылки; отличается только начало,
/// потому что пользователь не открывал видео, а добавлял ссылку в очередь.
pub(crate) fn url_import_failure_message(
    display_host: Option<&str>,
    reason: WebOpenFailureReason,
) -> String {
    let phrase = web_open_failure_phrase(reason);
    match display_host {
        Some(host) => format!("Could not add the link ({host}): {phrase}"),
        None => format!("Could not add the link: {phrase}"),
    }
}

/// Внутренний сбой приложения при добавлении ссылки (не причина со стороны сайта).
pub(crate) fn url_import_internal_failure_message(display_host: Option<&str>) -> String {
    match display_host {
        Some(host) => format!("Could not add the link ({host}); try again"),
        None => "Could not add the link; try again".to_owned(),
    }
}

/// Человеческая формулировка web-причины, со строчной буквы (идёт после двоеточия).
///
/// Таблица утверждена владельцем в сессии 08. Где пользователь может что-то сделать,
/// после тире идёт подсказка действия.
pub(crate) const fn web_open_failure_phrase(reason: WebOpenFailureReason) -> &'static str {
    match reason {
        WebOpenFailureReason::ExtractorNotInstalled => {
            "yt-dlp was not found; install the yt-dlp package"
        }
        WebOpenFailureReason::ExtractorDisabled => {
            "website downloads through yt-dlp are disabled in Settings"
        }
        WebOpenFailureReason::ExtractorTimedOut => {
            "yt-dlp is taking too long to respond; try again"
        }
        WebOpenFailureReason::ExtractorFailed => "yt-dlp failed; try updating yt-dlp",
        WebOpenFailureReason::SitePrivateMedia => "the video is private",
        WebOpenFailureReason::SiteLoginRequired => {
            "the website requires signing in (age restriction or verification)"
        }
        WebOpenFailureReason::SiteGeoRestricted => "the video is unavailable in your region",
        WebOpenFailureReason::SiteMediaUnavailable => "the video was deleted or does not exist",
        WebOpenFailureReason::SiteUnsupported => "yt-dlp does not support this website",
        WebOpenFailureReason::SiteRejected => {
            "the website did not provide the video: it may be private, deleted, require signing in, or be unavailable in your region"
        }
        WebOpenFailureReason::CollectionLink => {
            "the link points to a collection, not a single video"
        }
        WebOpenFailureReason::CollectionTooLarge => {
            "the collection has too many videos to add in full"
        }
        WebOpenFailureReason::NoPlayableFormat => "the video has no format the player can play",
        WebOpenFailureReason::NotFound => "page not found (error 404); check the link",
        WebOpenFailureReason::Gone => "the link has expired; get a new one",
        WebOpenFailureReason::AuthenticationRequired => {
            "the server requires signing in (username and password)"
        }
        WebOpenFailureReason::AccessDenied => "the server denied access (error 403)",
        WebOpenFailureReason::RateLimited => {
            "the server asks you to wait: too many requests; try again later"
        }
        WebOpenFailureReason::ServerError => "server error; try again later",
        WebOpenFailureReason::NoConnection => {
            "cannot connect to the server; check your internet connection"
        }
        WebOpenFailureReason::TimedOut => "the server is taking too long to respond",
        WebOpenFailureReason::ConnectionInterrupted => "the connection was interrupted; try again",
        WebOpenFailureReason::InvalidServerResponse => "the server returned an invalid response",
        WebOpenFailureReason::UnrecognizedFormat => {
            "the link contains no video or audio the player recognizes"
        }
        WebOpenFailureReason::UnsupportedLink => "this link type is unsupported",
        WebOpenFailureReason::UnsupportedLiveProfile => "this live stream format is unsupported",
        WebOpenFailureReason::Unclassified => "could not retrieve video from the link",
    }
}

#[cfg(test)]
#[path = "web_open_message/tests.rs"]
mod tests;
