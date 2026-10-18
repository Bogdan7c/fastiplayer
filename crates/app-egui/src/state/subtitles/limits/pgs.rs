//! Перевод настроек памяти PGS из конфига в лимиты декодера и политику удержания.
//!
//! # Откуда берутся числа
//! В конфиге ровно два значения (решение владельца, сессия 11): бюджет показываемой дорожки и
//! окно фоновых. Остальные пределы выводятся из них и из уже существующих пределов
//! растеризатора, а не плодят новые настройки:
//! - `max_epoch_rle_bytes` = бюджет показываемой дорожки: эпоха, не помещающаяся в бюджет,
//!   всё равно была бы вытеснена целиком;
//! - `max_object_rle_bytes` = худший случай RLE объекта на холсте максимального размера
//!   (`max_canvas_side`², байт на пиксель при полном отсутствии повторов), но не больше
//!   бюджета. Реальные объекты на порядки меньше; предел защищает от враждебного потока.

use fastiplayer_config::SubtitlesConfig;
use subtitle_core::SubtitleTime;
use subtitle_pgs::{PgsDecoderLimits, PgsRetention};

use super::{BYTES_PER_MEGABYTE, RASTER_MAX_CANVAS_SIDE};

/// Микросекунд в секунде (окно в конфиге задано секундами).
const MICROS_PER_SECOND: i64 = 1_000_000;

/// Доля бюджета, до которой вытеснение опускает кэш показываемой дорожки, когда бюджет
/// превышен: 1/8 запаса. Без запаса кэш на пределе вытеснял бы и пересобирал шкалу на КАЖДОЙ
/// новой композиции; с запасом пересборка случается раз в несколько десятков наборов.
const SHOWN_BUDGET_HEADROOM_DIVISOR: usize = 8;

/// Лимиты PGS встроенных дорожек.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct PgsLimits {
    /// Пределы декодера одной дорожки (захватываются при создании состояния дорожки).
    pub(in crate::state::subtitles) decoder: PgsDecoderLimits,
    /// Бюджет памяти показываемой дорожки, байт.
    shown_budget_bytes: usize,
    /// Хвост фоновых дорожек перед позицией воспроизведения.
    background_trailing_window: SubtitleTime,
}

impl PgsLimits {
    /// Строит лимиты из конфига; поля уже проверены `validate()`.
    pub(in crate::state::subtitles) fn from_config(config: &SubtitlesConfig) -> Self {
        let shown_budget_bytes =
            usize::from(config.pgs_displayed_track_cache_mb).saturating_mul(BYTES_PER_MEGABYTE);
        let canvas_side = usize::try_from(RASTER_MAX_CANVAS_SIDE).unwrap_or(usize::MAX);
        let worst_case_object_bytes = canvas_side.saturating_mul(canvas_side);
        Self {
            decoder: PgsDecoderLimits {
                max_object_rle_bytes: worst_case_object_bytes.min(shown_budget_bytes),
                max_epoch_rle_bytes: shown_budget_bytes,
            },
            shown_budget_bytes,
            background_trailing_window: SubtitleTime::from_micros(
                i64::from(config.pgs_background_track_window_s).saturating_mul(MICROS_PER_SECOND),
            ),
        }
    }

    /// Политика удержания для показываемой дорожки. `current_bytes` — сколько занято сейчас:
    /// пока бюджет не превышен, вытеснять нечего (политика с тем же бюджетом ничего не
    /// тронула бы), а при превышении целимся ниже бюджета на [`SHOWN_BUDGET_HEADROOM_DIVISOR`].
    pub(in crate::state::subtitles) fn shown_retention(
        &self,
        current_bytes: usize,
    ) -> PgsRetention {
        let budget_bytes = if current_bytes > self.shown_budget_bytes {
            self.shown_budget_bytes - self.shown_budget_bytes / SHOWN_BUDGET_HEADROOM_DIVISOR
        } else {
            self.shown_budget_bytes
        };
        PgsRetention::Shown { budget_bytes }
    }

    /// Только тесты: тот же набор лимитов с другим бюджетом показываемой дорожки (минимум в
    /// конфиге — 1 МБ, а картинки фикстур — килобайты).
    #[cfg(test)]
    pub(in crate::state::subtitles) fn with_shown_budget_bytes(self, budget_bytes: usize) -> Self {
        Self {
            shown_budget_bytes: budget_bytes,
            ..self
        }
    }

    /// Политика удержания для фоновой дорожки.
    pub(in crate::state::subtitles) fn background_retention(&self) -> PgsRetention {
        PgsRetention::Background {
            trailing_window: self.background_trailing_window,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_gives_the_owner_decided_budget_and_window() {
        let limits = PgsLimits::from_config(&SubtitlesConfig::default());

        assert_eq!(limits.decoder.max_epoch_rle_bytes, 32 * 1024 * 1024);
        // 8192² = 64 МиБ больше бюджета: объект зажат бюджетом.
        assert_eq!(limits.decoder.max_object_rle_bytes, 32 * 1024 * 1024);
        assert_eq!(
            limits.background_retention(),
            PgsRetention::Background {
                trailing_window: SubtitleTime::from_micros(60_000_000)
            }
        );
        assert_eq!(
            limits.shown_retention(0),
            PgsRetention::Shown {
                budget_bytes: 32 * 1024 * 1024
            }
        );
    }

    #[test]
    fn config_values_reach_the_limits_and_small_budgets_clamp_the_object_limit() {
        let limits = PgsLimits::from_config(&SubtitlesConfig {
            pgs_displayed_track_cache_mb: 1,
            pgs_background_track_window_s: 0,
            ..SubtitlesConfig::default()
        });

        assert_eq!(limits.decoder.max_epoch_rle_bytes, 1024 * 1024);
        assert_eq!(limits.decoder.max_object_rle_bytes, 1024 * 1024);
        assert_eq!(
            limits.background_retention(),
            PgsRetention::Background {
                trailing_window: SubtitleTime::ZERO
            }
        );
    }

    #[test]
    fn over_budget_retention_aims_below_the_budget_to_avoid_rebuilding_on_every_insert() {
        let limits = PgsLimits::from_config(&SubtitlesConfig {
            pgs_displayed_track_cache_mb: 8,
            ..SubtitlesConfig::default()
        });
        let budget = 8 * 1024 * 1024;

        assert_eq!(
            limits.shown_retention(budget),
            PgsRetention::Shown {
                budget_bytes: budget
            },
            "exactly at the budget nothing is over yet"
        );
        assert_eq!(
            limits.shown_retention(budget + 1),
            PgsRetention::Shown {
                budget_bytes: budget - budget / 8
            }
        );
    }
}
