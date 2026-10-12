//! Перевод фактов player-а в уведомления.
//!
//! Корень прежней «вечной красной ошибки»: player-core хранит в `last_error` одним полем и
//! фатальную ошибку, и безобидный отказ (seek на не-seekable потоке), а сбрасывает поле только
//! при следующем открытии. Приложение показывало поле как есть. Здесь два источника разведены:
//!
//! - recoverable-отказ приходит событием `PlayerEvent::RecoverableError` → временный toast
//!   (пропажа звукового устройства — информационный toast с человеческим текстом, сессия 10);
//! - фатальность определяется только состоянием `PlaybackState::Failed` из snapshot-а
//!   (как в `playlist_runtime::discovery::navigation::automatic_snapshot_kind`): старый
//!   `last_error` без `Failed` больше ничего не показывает.
//!
//! Модуль только читает snapshot/события; player-core не меняется.

use std::sync::Arc;
use std::time::Instant;

use player_core::{PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent, PlayerSnapshot};

use crate::audio_output_message::{AUDIO_OUTPUT_UNAVAILABLE_MESSAGE, audio_output_switch_message};
use crate::player_error_message::player_error_user_message;
use crate::state::media_retry::{MediaRetryTarget, ReopenPosition, RetryPlan};

use super::{MediaFailureOrigin, NotificationCenter, ObservedPlayerFailure};

impl NotificationCenter {
    /// Реагирует на событие player-а: recoverable-отказ становится временным уведомлением.
    ///
    /// Фатальные события здесь не обрабатываются: состояние `Failed` надёжнее читать из
    /// snapshot-а ([`Self::observe_player_snapshot`]) — player может перейти в `Failed` и без
    /// отдельного `FatalError`-события (сохранение causal-ошибки при сбое prepared seek).
    pub(crate) fn record_player_event(&mut self, event: &PlayerEvent, now: Instant) {
        match event {
            // Пропавшее звуковое устройство (сессия 10): человеческий текст вместо
            // технического `PlayerError`, информационная плашка — читать дольше.
            PlayerEvent::RecoverableError(error)
                if error.kind == PlayerErrorKind::AudioDeviceUnavailable =>
            {
                self.notify_info(AUDIO_OUTPUT_UNAVAILABLE_MESSAGE, now);
            }
            PlayerEvent::RecoverableError(error) => {
                // Технический `Display` ошибки остаётся для логов; пользователь видит английскую
                // фразу по виду ошибки (A10). Toast уходит сам, а не висит до следующего файла.
                self.notify_transient(player_error_user_message(error), now);
            }
            PlayerEvent::AudioOutputSwitchedToSystemDefault(reason) => {
                self.notify_info(audio_output_switch_message(reason), now);
            }
            _ => {}
        }
    }

    /// Синхронизирует фатальную ошибку с состоянием player-а на текущем кадре.
    ///
    /// - переход в `Failed` (или смена текста ошибки внутри `Failed`) показывает ошибку;
    /// - повтор того же `Failed` ничего не делает, поэтому закрытая × ошибка не всплывает;
    /// - выход из `Failed` снимает ошибку, которую показал player, но не трогает ошибку
    ///   открытия: «не удалось открыть новый файл» при играющем старом должна остаться.
    pub(crate) fn observe_player_snapshot(&mut self, snapshot: &PlayerSnapshot) {
        let observed = if snapshot.playback_state == PlaybackState::Failed {
            ObservedPlayerFailure::Failed {
                message: snapshot
                    .last_error
                    .as_ref()
                    .map(|error| Arc::from(player_error_user_message(error))),
            }
        } else {
            ObservedPlayerFailure::Healthy
        };
        if observed == self.observed_player_failure {
            return;
        }
        match &observed {
            ObservedPlayerFailure::Failed {
                message: Some(message),
            } => {
                let retry = snapshot
                    .last_error
                    .as_ref()
                    .and_then(|error| failure_retry_target(error, snapshot.current_position))
                    .map_or(RetryPlan::NotOffered, RetryPlan::Offered);
                self.show_media_failure_planned(
                    Arc::clone(message),
                    MediaFailureOrigin::PlayerFailed,
                    retry,
                );
            }
            // `Failed` без текста: как и раньше, текст не придумываем (новые тексты ошибок —
            // вне объёма сессии), но и устаревшую ошибку player-а не оставляем.
            ObservedPlayerFailure::Failed { message: None } | ObservedPlayerFailure::Healthy => {
                self.resolve_player_failure();
            }
        }
        self.observed_player_failure = observed;
    }

    /// Снимает фатальную ошибку, только если её показал player.
    fn resolve_player_failure(&mut self) {
        if self
            .media_failure
            .as_ref()
            .is_some_and(|failure| failure.origin == MediaFailureOrigin::PlayerFailed)
        {
            self.media_failure = None;
        }
    }
}

/// Что повторять, если воспроизведение упало (`Failed`): есть ли смысл и с какого места.
///
/// Повтор предлагается только для потери сети посреди просмотра: источник мог вернуться,
/// и переоткрытие с места обрыва продолжит просмотр. Остальные виды отказа повтором не
/// лечатся (декодер, формат, устройство рендера). `match` исчерпывающий намеренно:
/// новый `PlayerErrorKind` не скомпилируется, пока для него не решено, нужен ли повтор.
fn failure_retry_target(
    error: &PlayerError,
    position_at_failure: std::time::Duration,
) -> Option<MediaRetryTarget> {
    match error.kind {
        PlayerErrorKind::NetworkError => Some(MediaRetryTarget::ReopenFailedPlayback {
            position: ReopenPosition::from_playback_position(position_at_failure),
        }),
        PlayerErrorKind::UnsupportedVideoCodec
        | PlayerErrorKind::UnsupportedVideoProfile
        | PlayerErrorKind::UnsupportedVideoBitDepth
        | PlayerErrorKind::UnsupportedVideoChroma
        | PlayerErrorKind::UnsupportedHdrMode
        | PlayerErrorKind::UnsupportedAudioCodec
        | PlayerErrorKind::UnsupportedRenderFormat
        | PlayerErrorKind::HardwareDecoderUnavailable
        | PlayerErrorKind::RequiredVideoBackendUnavailable
        | PlayerErrorKind::DemuxError
        | PlayerErrorKind::SeekUnavailable
        | PlayerErrorKind::SeekTimeout
        | PlayerErrorKind::SeekTargetExpired
        | PlayerErrorKind::DecoderFlushFailed
        | PlayerErrorKind::AudioDeviceUnavailable
        | PlayerErrorKind::RenderDeviceLost
        | PlayerErrorKind::ConfigError
        | PlayerErrorKind::RuntimeError
        | PlayerErrorKind::InvalidCommand => None,
    }
}
