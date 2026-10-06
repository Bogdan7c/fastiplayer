//! Тексты для пользователя об открытии web-ссылки (UX сессия 08).
//!
//! Единственный владелец формулировок web-причин. Причину определяет
//! `media_source_open::web_open_failure` (классификация живёт рядом с ошибками
//! владельцев), здесь — только превращение причины в русский текст с подсказкой.
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
        Some(host) => format!("Не удалось открыть ссылку ({host}): {phrase}"),
        None => format!("Не удалось открыть ссылку: {phrase}"),
    }
}

/// Человеческая формулировка web-причины, со строчной буквы (идёт после двоеточия).
///
/// Таблица утверждена владельцем в сессии 08. Где пользователь может что-то сделать,
/// после тире идёт подсказка действия.
pub(crate) const fn web_open_failure_phrase(reason: WebOpenFailureReason) -> &'static str {
    match reason {
        WebOpenFailureReason::ExtractorNotInstalled => {
            "не найдена программа yt-dlp — установите пакет yt-dlp"
        }
        WebOpenFailureReason::ExtractorDisabled => {
            "загрузка с сайтов через yt-dlp отключена в настройках"
        }
        WebOpenFailureReason::ExtractorTimedOut => {
            "yt-dlp слишком долго не отвечает — попробуйте ещё раз"
        }
        WebOpenFailureReason::ExtractorFailed => {
            "yt-dlp завершился с ошибкой — попробуйте обновить yt-dlp"
        }
        WebOpenFailureReason::SitePrivateMedia => "видео приватное",
        WebOpenFailureReason::SiteLoginRequired => {
            "сайт требует войти в аккаунт (возрастное ограничение или проверка)"
        }
        WebOpenFailureReason::SiteGeoRestricted => "видео недоступно в вашем регионе",
        WebOpenFailureReason::SiteMediaUnavailable => "видео удалено или не существует",
        WebOpenFailureReason::SiteUnsupported => "этот сайт не поддерживается yt-dlp",
        WebOpenFailureReason::SiteRejected => {
            "сайт не отдал видео: возможно, оно приватное, удалено, требует входа или недоступно в вашем регионе"
        }
        WebOpenFailureReason::CollectionLink => "ссылка ведёт на подборку, а не на одно видео",
        WebOpenFailureReason::NoPlayableFormat => {
            "у видео нет формата, который плеер умеет воспроизводить"
        }
        WebOpenFailureReason::NotFound => "страница не найдена (ошибка 404) — проверьте ссылку",
        WebOpenFailureReason::Gone => "ссылка устарела — получите новую",
        WebOpenFailureReason::AuthenticationRequired => "сервер требует вход (логин и пароль)",
        WebOpenFailureReason::AccessDenied => "сервер запретил доступ (ошибка 403)",
        WebOpenFailureReason::RateLimited => {
            "сервер просит подождать: слишком много запросов — попробуйте позже"
        }
        WebOpenFailureReason::ServerError => "ошибка на стороне сервера — попробуйте позже",
        WebOpenFailureReason::NoConnection => "нет соединения с сервером — проверьте интернет",
        WebOpenFailureReason::TimedOut => "сервер слишком долго не отвечает",
        WebOpenFailureReason::ConnectionInterrupted => "соединение оборвалось — попробуйте ещё раз",
        WebOpenFailureReason::InvalidServerResponse => "сервер ответил некорректно",
        WebOpenFailureReason::UnrecognizedFormat => {
            "по ссылке нет видео или звука, которые плеер узнаёт"
        }
        WebOpenFailureReason::UnsupportedLink => "такой тип ссылки не поддерживается",
        WebOpenFailureReason::UnsupportedLiveProfile => {
            "формат прямой трансляции не поддерживается"
        }
        WebOpenFailureReason::Unclassified => "не удалось получить видео по ссылке",
    }
}

#[cfg(test)]
#[path = "web_open_message/tests.rs"]
mod tests;
