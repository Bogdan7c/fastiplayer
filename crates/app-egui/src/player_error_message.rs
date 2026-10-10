//! Тексты для пользователя об ошибках player-а (UX edge cases 2, задача A10).
//!
//! Корень проблемы: `Display` у `PlayerError` печатает `"{Kind:?}: {message}"` — это формат
//! для логов и диагностики, но его брали прямо в toast, центр уведомлений и бейдж строки
//! плейлиста, поэтому пользователь видел «SeekUnavailable: …» с английским текстом нижнего
//! слоя. `Display` и `Debug` остаются как есть (на них опираются логи и тесты player-core),
//! а в интерфейс текст идёт только через [`player_error_user_message`]: это единственная
//! таблица «вид ошибки -> русская фраза». Сырой `message` нижнего слоя в текст не попадает —
//! он может содержать пути, адреса и английский жаргон; он пишется в лог через `tracing`.

use player_core::{PlayerError, PlayerErrorKind};

use crate::audio_output_message::AUDIO_OUTPUT_UNAVAILABLE_MESSAGE;

/// Что сказать человеку при пропаже сети посреди просмотра (сессия 16).
///
/// К этому моменту плеер уже ждал возвращения связи весь бюджет
/// `network.reconnect_wait_ms` с повторами и показывал буферизацию.
pub(crate) const NETWORK_LOST_DURING_PLAYBACK_MESSAGE: &str = "Нет связи с сервером: \
    воспроизведение остановлено. Проверьте подключение к сети и откройте видео снова.";

/// Человеческая формулировка вида ошибки player-а.
///
/// Без `_`: новый `PlayerErrorKind` в player-core должен ломать компиляцию здесь, чтобы
/// его текст выбрал человек, а не утёк в интерфейс через общий запасной вариант.
pub(crate) const fn player_error_kind_phrase(kind: &PlayerErrorKind) -> &'static str {
    match kind {
        PlayerErrorKind::UnsupportedVideoCodec => "Формат видео не поддерживается",
        PlayerErrorKind::UnsupportedVideoProfile => {
            "Профиль видео не поддерживается на этом компьютере"
        }
        PlayerErrorKind::UnsupportedVideoBitDepth => "Глубина цвета видео не поддерживается",
        PlayerErrorKind::UnsupportedVideoChroma => "Цветовой формат видео не поддерживается",
        PlayerErrorKind::UnsupportedHdrMode => "HDR-видео пока нельзя показать правильно",
        PlayerErrorKind::UnsupportedAudioCodec => "Формат звука не поддерживается",
        PlayerErrorKind::UnsupportedRenderFormat => {
            "Видеокарта не может показать кадры этого видео"
        }
        PlayerErrorKind::HardwareDecoderUnavailable => "Аппаратное декодирование видео недоступно",
        PlayerErrorKind::RequiredVideoBackendUnavailable => {
            "Выбранный способ показа видео недоступен на этом компьютере"
        }
        PlayerErrorKind::DemuxError => {
            "Не удалось прочитать данные: файл повреждён или передача прервалась"
        }
        PlayerErrorKind::SeekUnavailable => "Перемотка для этого видео недоступна",
        PlayerErrorKind::SeekTimeout => "Перемотка не успела выполниться — попробуйте ещё раз",
        PlayerErrorKind::SeekTargetExpired => {
            "Эта часть прямого эфира уже недоступна — перемотайте ближе к текущему моменту"
        }
        PlayerErrorKind::DecoderFlushFailed => "Не удалось перемотать видео — попробуйте ещё раз",
        PlayerErrorKind::NetworkError => NETWORK_LOST_DURING_PLAYBACK_MESSAGE,
        PlayerErrorKind::AudioDeviceUnavailable => AUDIO_OUTPUT_UNAVAILABLE_MESSAGE,
        PlayerErrorKind::RenderDeviceLost => "Видеокарта перестала отвечать — откройте видео снова",
        PlayerErrorKind::ConfigError => "Настройки плеера некорректны",
        PlayerErrorKind::RuntimeError => "Внутренняя ошибка плеера — откройте видео снова",
        PlayerErrorKind::InvalidCommand => "Это действие сейчас недоступно",
    }
}

/// Текст ошибки player-а для toast и центра уведомлений.
pub(crate) fn player_error_user_message(error: &PlayerError) -> String {
    player_error_kind_phrase(&error.kind).to_owned()
}

/// Короткий текст для бейджа строки плейлиста: «Ошибка воспроизведения: перемотка …».
pub(crate) fn player_error_row_summary(error: &PlayerError) -> String {
    format!(
        "Ошибка воспроизведения: {}",
        player_error_kind_phrase(&error.kind)
    )
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
            assert!(!message.contains("seek"), "{kind:?}: {message}");
            assert!(!message.contains("/tmp"), "{kind:?}: {message}");
            assert!(
                message
                    .chars()
                    .any(|ch| ('а'..='я').contains(&ch.to_lowercase().next().unwrap_or(' '))),
                "{kind:?} без русского текста: {message}"
            );
        }
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
            "Перемотка для этого видео недоступна"
        );
    }

    #[test]
    fn row_summary_has_human_prefix_and_no_kind_name() {
        let error = PlayerError::new(PlayerErrorKind::DemuxError, "bad atom");
        let summary = player_error_row_summary(&error);

        assert!(summary.starts_with("Ошибка воспроизведения: "), "{summary}");
        assert!(!summary.contains("DemuxError"), "{summary}");
        assert!(!summary.contains("bad atom"), "{summary}");
    }
}
