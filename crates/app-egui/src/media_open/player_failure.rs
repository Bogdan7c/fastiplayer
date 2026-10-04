//! Классификация отказа player-а установить media в причину, понятную пользователю.
//!
//! player-core сам по себе ничего не знает про UI: он отдаёт типизированный
//! `MediaInstallFailure` (стадия + `PlayerError` с `PlayerErrorKind`). Здесь живёт только
//! «бизнес-решение» приложения: какой технической ошибке какая человеческая причина
//! соответствует. Тексты строит `crate::local_open_message`, технические детали пишет в
//! лог вызывающий код (coordinator), поэтому классификация — чистая функция без I/O.

use player_core::{MediaInstallFailure, MediaInstallFailureStage, PlayerErrorKind};

use super::PlayerDispatchRejection;

/// Почему player не установил подготовленное media — в терминах пользователя.
///
/// Тип `Copy` и не содержит ни текста ошибки, ни имени файла: его безопасно хранить в
/// terminal-исходе coordinator-а и передавать в UI. Пригоден и для web-источников
/// (сессия 08): ни один вариант не предполагает, что источник — локальный файл.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlayerInstallFailureReason {
    /// Видеодорожка в формате, который плеер не умеет декодировать
    /// (кодек, профиль, битность, цветовая субдискретизация или HDR-режим).
    UnsupportedVideoFormat,
    /// Звуковая дорожка в неподдерживаемом формате.
    UnsupportedAudioFormat,
    /// Формат видео известен, но нет декодера, который его потянет: нет аппаратного
    /// декодера или выбранный в настройках backend не поддерживает этот поток.
    NoSuitableVideoDecoder,
    /// Player не успел подготовить видео за отведённое время (медленное чтение).
    PreparationTimedOut,
    /// Команду не удалось доставить player-у: очередь переполнена или поток player-а
    /// завершился. Это единственный случай, когда player действительно «недоступен».
    PlayerNotResponding,
    /// Всё остальное: нарушение протокола, ошибка, которую пользователь не может
    /// исправить сам. Подробности — только в логе.
    InternalError,
}

impl PlayerInstallFailureReason {
    /// Причина отказа, который player опубликовал в terminal-слоте установки.
    ///
    /// Стадия проверяется первой: таймаут подготовки видео player оформляет как
    /// `DemuxError`, но для пользователя это «слишком долго», а не «повреждён».
    pub(crate) const fn from_install_failure(failure: &MediaInstallFailure) -> Self {
        if matches!(
            failure.stage,
            MediaInstallFailureStage::VideoPreflightTimeout
        ) {
            return Self::PreparationTimedOut;
        }
        Self::from_player_error_kind(&failure.error.kind)
    }

    /// Причина, если команда установки даже не дошла до player-а.
    pub(crate) const fn from_dispatch_rejection(rejection: PlayerDispatchRejection) -> Self {
        // Оба варианта означают одно и то же для пользователя: player сейчас не принимает
        // команды. Различие (переполнение / поток умер) сохраняется в логе вызывающего кода.
        match rejection {
            PlayerDispatchRejection::Backpressure | PlayerDispatchRejection::Disconnected => {
                Self::PlayerNotResponding
            }
        }
    }

    /// Исчерпывающее сопоставление вида ошибки player-а причине.
    ///
    /// Wildcard `_` намеренно не используется: новый `PlayerErrorKind` в player-core
    /// не скомпилируется, пока здесь явно не решат, что видит пользователь.
    const fn from_player_error_kind(kind: &PlayerErrorKind) -> Self {
        match kind {
            PlayerErrorKind::UnsupportedVideoCodec
            | PlayerErrorKind::UnsupportedVideoProfile
            | PlayerErrorKind::UnsupportedVideoBitDepth
            | PlayerErrorKind::UnsupportedVideoChroma
            | PlayerErrorKind::UnsupportedHdrMode => Self::UnsupportedVideoFormat,
            PlayerErrorKind::UnsupportedAudioCodec => Self::UnsupportedAudioFormat,
            PlayerErrorKind::HardwareDecoderUnavailable
            | PlayerErrorKind::RequiredVideoBackendUnavailable => Self::NoSuitableVideoDecoder,
            // Утверждённая владельцем таблица (сессия 03): остальные виды — внутренняя
            // ошибка. `UnsupportedRenderFormat` на стадии установки означает рассинхрон
            // выбранного backend-а, а не свойство файла; сетевые/demux причины для web
            // уточнит сессия 08.
            PlayerErrorKind::UnsupportedRenderFormat
            | PlayerErrorKind::DemuxError
            | PlayerErrorKind::SeekUnavailable
            | PlayerErrorKind::SeekTimeout
            | PlayerErrorKind::SeekTargetExpired
            | PlayerErrorKind::DecoderFlushFailed
            | PlayerErrorKind::NetworkError
            | PlayerErrorKind::AudioDeviceUnavailable
            | PlayerErrorKind::RenderDeviceLost
            | PlayerErrorKind::ConfigError
            | PlayerErrorKind::RuntimeError
            | PlayerErrorKind::InvalidCommand => Self::InternalError,
        }
    }
}

#[cfg(test)]
mod tests;
