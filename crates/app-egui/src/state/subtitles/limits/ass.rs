//! Перевод ASS-настроек конфига в лимиты разбора (`subtitle-formats`).
//!
//! Один источник для обоих путей: файл `.ass/.ssa` (`LoadLimits`) и встроенная дорожка
//! (`EmbeddedLimits`). Поля конфига уже проверены `validate()`; перевод не паникует.

use fastiplayer_config::SubtitlesConfig;
use subtitle_formats::AssParseLimits;

/// Байтов в килобайте конфига (КБ = КиБ, как МБ = МиБ в остальных лимитах).
const BYTES_PER_KILOBYTE: usize = 1024;

/// Сколько разных имён неизвестных тегов запоминается в статистике дорожки. Это размер
/// диагностического отчёта («что не поддержано» для матрицы), а не ресурсный лимит
/// пользователя, поэтому в конфиг не выносится.
const ASS_TRACKED_UNKNOWN_TAG_NAMES: usize = 64;

/// Лимиты разбора ASS из секции `[subtitles]`.
pub(in crate::state::subtitles) fn ass_parse_limits_from_config(
    config: &SubtitlesConfig,
) -> AssParseLimits {
    AssParseLimits {
        max_header_bytes: widen(config.max_ass_header_kb).saturating_mul(BYTES_PER_KILOBYTE),
        max_styles: widen(config.max_ass_styles),
        max_override_tags_per_event: widen(config.max_ass_override_tags_per_event),
        max_tracked_unknown_tag_names: ASS_TRACKED_UNKNOWN_TAG_NAMES,
    }
}

/// `u32` -> `usize` без паники (на 16-битных платформах насыщается).
fn widen(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Значения конфига доезжают до лимитов разбора с пересчётом КБ в байты.
    #[test]
    fn config_values_become_parse_limits() {
        let defaults = ass_parse_limits_from_config(&SubtitlesConfig::default());
        assert_eq!(defaults.max_header_bytes, 1024 * 1024);
        assert_eq!(defaults.max_styles, 1_000);
        assert_eq!(defaults.max_override_tags_per_event, 512);
        assert_eq!(defaults.max_tracked_unknown_tag_names, 64);

        let custom = ass_parse_limits_from_config(&SubtitlesConfig {
            max_ass_header_kb: 64,
            max_ass_styles: 10,
            max_ass_override_tags_per_event: 32,
            ..SubtitlesConfig::default()
        });
        assert_eq!(custom.max_header_bytes, 64 * 1024);
        assert_eq!(custom.max_styles, 10);
        assert_eq!(custom.max_override_tags_per_event, 32);
    }
}
