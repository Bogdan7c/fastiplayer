//! Тексты для пользователя об ошибках player-а (UX edge cases 2, задача A10).
//!
//! Корень проблемы: `Display` у `PlayerError` печатает `"{Kind:?}: {message}"` — это формат
//! для логов и диагностики, но его брали прямо в toast, центр уведомлений и бейдж строки
//! плейлиста, поэтому пользователь видел «SeekUnavailable: …» с английским текстом нижнего
//! слоя. `Display` и `Debug` остаются как есть (на них опираются логи и тесты player-core),
//! а в интерфейс текст идёт только через [`player_error_user_message`]: это единственная
//! таблица «вид ошибки -> английская фраза». Сырой `message` нижнего слоя в текст не попадает —
//! он может содержать пути, адреса и английский жаргон; он пишется в лог через `tracing`.

use player_core::{PlayerError, PlayerErrorKind};

use crate::audio_output_message::AUDIO_OUTPUT_UNAVAILABLE_MESSAGE;

/// Что сказать человеку при пропаже сети посреди просмотра (сессия 16).
///
/// К этому моменту плеер уже ждал возвращения связи весь бюджет
/// `network.reconnect_wait_ms` с повторами и показывал буферизацию.
pub(crate) const NETWORK_LOST_DURING_PLAYBACK_MESSAGE: &str = "Cannot connect to the server: playback stopped. Check your network connection and open the video again.";

/// Человеческая формулировка вида ошибки player-а.
///
/// Без `_`: новый `PlayerErrorKind` в player-core должен ломать компиляцию здесь, чтобы
/// его текст выбрал человек, а не утёк в интерфейс через общий запасной вариант.
pub(crate) const fn player_error_kind_phrase(kind: &PlayerErrorKind) -> &'static str {
    match kind {
        PlayerErrorKind::UnsupportedVideoCodec => "Unsupported video format",
        PlayerErrorKind::UnsupportedVideoProfile => "Video profile is unsupported on this computer",
        PlayerErrorKind::UnsupportedVideoBitDepth => "Video color depth is unsupported",
        PlayerErrorKind::UnsupportedVideoChroma => "Video color format is unsupported",
        PlayerErrorKind::UnsupportedHdrMode => "HDR video cannot be displayed correctly yet",
        PlayerErrorKind::UnsupportedAudioCodec => "Unsupported audio format",
        PlayerErrorKind::UnsupportedRenderFormat => {
            "The graphics card cannot display frames from this video"
        }
        PlayerErrorKind::HardwareDecoderUnavailable => "Hardware video decoding is unavailable",
        PlayerErrorKind::RequiredVideoBackendUnavailable => {
            "The decoder mode selected in Settings is unsuitable for this video or unavailable on this computer. Select Automatic in Settings"
        }
        PlayerErrorKind::DemuxError => {
            "Could not read media: the file is corrupted or the transfer was interrupted"
        }
        PlayerErrorKind::SeekUnavailable => "Seeking is unavailable for this video",
        PlayerErrorKind::SeekTimeout => "Seeking timed out; try again",
        PlayerErrorKind::SeekTargetExpired => {
            "This part of the live stream is no longer available; seek closer to the live edge"
        }
        PlayerErrorKind::DecoderFlushFailed => "Could not seek the video; try again",
        PlayerErrorKind::NetworkError => NETWORK_LOST_DURING_PLAYBACK_MESSAGE,
        PlayerErrorKind::AudioDeviceUnavailable => AUDIO_OUTPUT_UNAVAILABLE_MESSAGE,
        PlayerErrorKind::RenderDeviceLost => {
            "The graphics card stopped responding; open the video again"
        }
        PlayerErrorKind::ConfigError => "Player settings are invalid",
        PlayerErrorKind::RuntimeError => "Internal player error; open the video again",
        PlayerErrorKind::InvalidCommand => "This action is currently unavailable",
    }
}

/// Текст ошибки player-а для toast и центра уведомлений.
pub(crate) fn player_error_user_message(error: &PlayerError) -> String {
    player_error_kind_phrase(&error.kind).to_owned()
}

/// Короткий текст для бейджа строки плейлиста: «Ошибка воспроизведения: перемотка …».
pub(crate) fn player_error_row_summary(error: &PlayerError) -> String {
    format!("Playback error: {}", player_error_kind_phrase(&error.kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [PlayerErrorKind; 20] = [
        PlayerErrorKind::UnsupportedVideoCodec,
        PlayerErrorKind::UnsupportedVideoProfile,
        PlayerErrorKind::UnsupportedVideoBitDepth,
        PlayerErrorKind::UnsupportedVideoChroma,
        PlayerErrorKind::UnsupportedHdrMode,
        PlayerErrorKind::UnsupportedAudioCodec,
        PlayerErrorKind::UnsupportedRenderFormat,
        PlayerErrorKind::HardwareDecoderUnavailable,
        PlayerErrorKind::RequiredVideoBackendUnavailable,
        PlayerErrorKind::DemuxError,
        PlayerErrorKind::SeekUnavailable,
        PlayerErrorKind::SeekTimeout,
        PlayerErrorKind::SeekTargetExpired,
        PlayerErrorKind::DecoderFlushFailed,
        PlayerErrorKind::NetworkError,
        PlayerErrorKind::AudioDeviceUnavailable,
        PlayerErrorKind::RenderDeviceLost,
        PlayerErrorKind::ConfigError,
        PlayerErrorKind::RuntimeError,
        PlayerErrorKind::InvalidCommand,
    ];

    #[test]
    fn user_message_never_leaks_kind_name_or_raw_english_message() {
        for kind in ALL_KINDS {
            let error = PlayerError::new(
                kind.clone(),
                "Live stream does not support seek at /tmp/x.mkv",
            );
            let message = player_error_user_message(&error);

            assert!(
                !message.contains(&format!("{kind:?}")),
                "{kind:?}: {message}"
            );
            assert!(
                !message.contains("Live stream does not support"),
                "{kind:?}: {message}"
            );
            assert!(!message.contains("/tmp"), "{kind:?}: {message}");
            assert!(
                message.chars().any(|ch| ch.is_ascii_alphabetic()),
                "{kind:?} has no readable English message: {message}"
            );
        }
    }

    #[test]
    fn required_backend_unavailable_tells_to_pick_auto_mode() {
        // Режим «Hardware» без VA-API (или «Software» без FFmpeg-пути) приходит именно так.
        let error = PlayerError::new(
            PlayerErrorKind::RequiredVideoBackendUnavailable,
            "Backend 'vaapi' doesn't have playable output for candidate video stream",
        );
        let message = player_error_user_message(&error);

        assert!(message.contains("decoder mode"), "{message}");
        assert!(message.contains("Automatic"), "{message}");
        assert!(!message.contains("vaapi"), "{message}");
    }

    #[test]
    fn display_of_player_error_stays_technical_for_logs() {
        let error = PlayerError::new(PlayerErrorKind::SeekUnavailable, "live seek");

        assert_eq!(error.to_string(), "SeekUnavailable: live seek");
    }

    #[test]
    fn seek_unavailable_says_seek_is_unavailable() {
        let error = PlayerError::new(PlayerErrorKind::SeekUnavailable, "english detail");

        assert_eq!(
            player_error_user_message(&error),
            "Seeking is unavailable for this video"
        );
    }

    #[test]
    fn row_summary_has_human_prefix_and_no_kind_name() {
        let error = PlayerError::new(PlayerErrorKind::DemuxError, "bad atom");
        let summary = player_error_row_summary(&error);

        assert!(summary.starts_with("Playback error: "), "{summary}");
        assert!(!summary.contains("DemuxError"), "{summary}");
        assert!(!summary.contains("bad atom"), "{summary}");
    }
}
