//! Единая пользовательская причина неудачного открытия media.
//!
//! Открытие может сорваться на разных стадиях с разными владельцами классификации:
//! подготовка локального файла (`LocalOpenFailureReason`, сессия 02), подготовка
//! web-ссылки (`WebOpenFailureReason` из `media-source-open`, сессия 08) и установка в
//! player (`PlayerInstallFailureReason`, сессия 03). Тексты для экрана (`crate::local_open_message`)
//! принимают этот общий тип, поэтому кнопка Open, CLI-старт и строка плейлиста строят
//! сообщение одной функцией независимо от стадии.

use super::{LocalOpenFailureReason, PlayerInstallFailureReason, WebOpenFailureReason};

/// Причина неудачного открытия в терминах пользователя (`Copy`, без пути и текста ошибки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenUserFailureReason {
    /// Источник не удалось подготовить (файл не найден, формат не распознан и т.п.).
    Preparation(LocalOpenFailureReason),
    /// Web-ссылку не удалось подготовить (нет yt-dlp, 404, нет сети и т.п.).
    WebOpen(WebOpenFailureReason),
    /// Источник подготовлен, но player отказался его установить.
    PlayerInstall(PlayerInstallFailureReason),
}

impl From<LocalOpenFailureReason> for MediaOpenUserFailureReason {
    fn from(reason: LocalOpenFailureReason) -> Self {
        Self::Preparation(reason)
    }
}

impl From<WebOpenFailureReason> for MediaOpenUserFailureReason {
    fn from(reason: WebOpenFailureReason) -> Self {
        Self::WebOpen(reason)
    }
}

impl From<PlayerInstallFailureReason> for MediaOpenUserFailureReason {
    fn from(reason: PlayerInstallFailureReason) -> Self {
        Self::PlayerInstall(reason)
    }
}
