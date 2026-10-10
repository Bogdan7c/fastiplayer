//! Имеет ли смысл кнопка «Повторить» после отказа открытия (B9).
//!
//! Решение принимает владелец причины, а не тот, кто рисует кнопку: «файл не найден»
//! можно исправить снаружи (вернули флешку, подключили сетевой диск), а «формат не
//! поддерживается» от повтора не изменится. `match` исчерпывающий намеренно, без `_`: новая
//! причина в любом из трёх перечислений не скомпилируется, пока для неё явно не решено,
//! предлагать ли повтор.
//!
//! Критерий один: **может ли тот же запрос дать другой результат без действий в самом
//! плеере** (сеть вернулась, сервер оправился, диск подключили, права поправили).
//! Если для исправления нужно менять настройки, ссылку или файл, повтор не предлагается.

use super::{
    LocalOpenFailureReason, MediaOpenUserFailureReason, PlayerInstallFailureReason,
    WebOpenFailureReason,
};

/// Совет интерфейсу: показывать ли рядом с ошибкой кнопку «Повторить».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RetryAdvice {
    /// Повтор осмыслен: обстоятельство внешнее и могло измениться.
    Offer,
    /// Повтор бессмыслен: результат будет тем же, пока пользователь не изменит файл,
    /// ссылку или настройки.
    Withhold,
}

impl MediaOpenUserFailureReason {
    /// Предлагать ли повтор для этой причины отказа открытия.
    pub(crate) const fn retry_advice(self) -> RetryAdvice {
        match self {
            Self::Preparation(reason) => local_retry_advice(reason),
            Self::WebOpen(reason) => web_retry_advice(reason),
            Self::PlayerInstall(reason) => player_install_retry_advice(reason),
        }
    }
}

/// Локальный файл: повтор нужен там, где виновато состояние диска или прав.
const fn local_retry_advice(reason: LocalOpenFailureReason) -> RetryAdvice {
    match reason {
        // Флешку вернули, сетевой диск подключили, файл дописали, права поправили.
        LocalOpenFailureReason::FileNotFound
        | LocalOpenFailureReason::AccessDenied
        | LocalOpenFailureReason::ReadFailed
        | LocalOpenFailureReason::ReadTimedOut
        | LocalOpenFailureReason::ChangedDuringOpen => RetryAdvice::Offer,
        // Содержимое файла не изменится от повторной попытки.
        LocalOpenFailureReason::IsDirectory
        | LocalOpenFailureReason::EmptyFile
        | LocalOpenFailureReason::UnrecognizedFormat
        | LocalOpenFailureReason::DamagedOrTruncated
        | LocalOpenFailureReason::NoAudioOrVideo
        // Внутренний сбой: подсказать нечего, повтор пользователя только запутает.
        | LocalOpenFailureReason::InternalError => RetryAdvice::Withhold,
    }
}

/// Web-ссылка: повтор нужен для сети и временных отказов сервера.
const fn web_retry_advice(reason: WebOpenFailureReason) -> RetryAdvice {
    match reason {
        // Сеть вернулась, сервер оправился, лимит запросов сбросился.
        WebOpenFailureReason::NoConnection
        | WebOpenFailureReason::TimedOut
        | WebOpenFailureReason::ConnectionInterrupted
        | WebOpenFailureReason::ServerError
        | WebOpenFailureReason::RateLimited
        | WebOpenFailureReason::ExtractorTimedOut => RetryAdvice::Offer,
        // Нужно менять настройки (yt-dlp), ссылку, вход в аккаунт или это свойство
        // самого видео: повтор вернёт тот же отказ. `InvalidServerResponse` и
        // `Unclassified` тоже не предлагаем: мы не знаем, что именно сломалось, и не
        // обещаем пользователю исправления, которого может не быть.
        WebOpenFailureReason::ExtractorNotInstalled
        | WebOpenFailureReason::ExtractorDisabled
        | WebOpenFailureReason::ExtractorFailed
        | WebOpenFailureReason::SitePrivateMedia
        | WebOpenFailureReason::SiteLoginRequired
        | WebOpenFailureReason::SiteGeoRestricted
        | WebOpenFailureReason::SiteMediaUnavailable
        | WebOpenFailureReason::SiteUnsupported
        | WebOpenFailureReason::SiteRejected
        | WebOpenFailureReason::CollectionLink
        | WebOpenFailureReason::CollectionTooLarge
        | WebOpenFailureReason::NoPlayableFormat
        | WebOpenFailureReason::NotFound
        | WebOpenFailureReason::Gone
        | WebOpenFailureReason::AuthenticationRequired
        | WebOpenFailureReason::AccessDenied
        | WebOpenFailureReason::InvalidServerResponse
        | WebOpenFailureReason::UnrecognizedFormat
        | WebOpenFailureReason::UnsupportedLink
        | WebOpenFailureReason::UnsupportedLiveProfile
        | WebOpenFailureReason::Unclassified => RetryAdvice::Withhold,
    }
}

/// Отказ player-а при установке: повтор нужен только для «долго читается» и «не отвечает».
const fn player_install_retry_advice(reason: PlayerInstallFailureReason) -> RetryAdvice {
    match reason {
        PlayerInstallFailureReason::PreparationTimedOut
        | PlayerInstallFailureReason::PlayerNotResponding => RetryAdvice::Offer,
        // Формат или декодер от повтора не появятся.
        PlayerInstallFailureReason::UnsupportedVideoFormat
        | PlayerInstallFailureReason::UnsupportedAudioFormat
        | PlayerInstallFailureReason::NoSuitableVideoDecoder
        | PlayerInstallFailureReason::InternalError => RetryAdvice::Withhold,
    }
}

#[cfg(test)]
mod tests;
