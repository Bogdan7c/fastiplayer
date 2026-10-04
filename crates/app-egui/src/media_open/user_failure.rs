//! Единая пользовательская причина неудачного открытия media.
//!
//! Открытие может сорваться на двух разных стадиях с разными владельцами классификации:
//! подготовка источника (`LocalOpenFailureReason`, сессия 02) и установка в player
//! (`PlayerInstallFailureReason`, сессия 03). Тексты для экрана (`crate::local_open_message`)
//! принимают этот общий тип, поэтому кнопка Open, CLI-старт и строка плейлиста строят
//! сообщение одной функцией независимо от стадии.

use super::{LocalOpenFailureReason, PlayerInstallFailureReason};

/// Причина неудачного открытия в терминах пользователя (`Copy`, без пути и текста ошибки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOpenUserFailureReason {
    /// Источник не удалось подготовить (файл не найден, формат не распознан и т.п.).
    Preparation(LocalOpenFailureReason),
    /// Источник подготовлен, но player отказался его установить.
    PlayerInstall(PlayerInstallFailureReason),
}

impl From<LocalOpenFailureReason> for MediaOpenUserFailureReason {
    fn from(reason: LocalOpenFailureReason) -> Self {
        Self::Preparation(reason)
    }
}

impl From<PlayerInstallFailureReason> for MediaOpenUserFailureReason {
    fn from(reason: PlayerInstallFailureReason) -> Self {
        Self::PlayerInstall(reason)
    }
}
