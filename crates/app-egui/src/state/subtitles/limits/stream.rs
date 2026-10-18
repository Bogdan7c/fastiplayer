//! Перевод лимитов потоковых субтитров из конфига (`[subtitles.stream]`) в настройки ленты
//! `media-source-open` и политику истории live.
//!
//! В конфиге лежит всё, что пользователь может осмысленно менять (окна, размер сегмента,
//! порог сбоев, история live). Ёмкость очереди сообщений от ленты к приложению - внутренний
//! инвариант реализации (лента при переполнении просто ждёт), поэтому она именованная
//! константа, а не настройка.

use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use fastiplayer_config::SubtitlesConfig;
use media_source_open::stream_subtitles::StreamSubtitleFeedConfig;
use subtitle_core::SubtitleTime;

/// Байтов в килобайте конфига (КБ = КиБ).
const BYTES_PER_KILOBYTE: usize = 1024;

/// Микросекунд в секунде (история live в конфиге задана секундами).
const MICROS_PER_SECOND: i64 = 1_000_000;

/// Ёмкость очереди «лента -> приложение». Приложение вычитывает её каждый кадр, а лента при
/// переполнении ждёт, не теряя сообщений, поэтому небольшого запаса достаточно.
const FEED_MESSAGE_CAPACITY: NonZeroUsize = NonZeroUsize::MIN.saturating_add(63);

/// Лимиты потоковых субтитров, разобранные из конфига.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) struct StreamLimits {
    /// Настройки ленты: окна, размер сегмента, порог сбоев (захватываются при её запуске).
    pub(in crate::state::subtitles) feed: StreamSubtitleFeedConfig,
    /// Сколько реплик позади позиции хранит живая трансляция.
    pub(in crate::state::subtitles) live_retention: SubtitleTime,
}

impl StreamLimits {
    /// Строит лимиты из конфига; поля уже проверены `validate()`.
    ///
    /// `network_reconnect_wait` - бюджет переподключения основного источника
    /// (`network.reconnect_wait_ms`): столько лента ждёт возвращения сети, прежде чем
    /// признать дорожку недоступной.
    pub(in crate::state::subtitles) fn from_config(
        config: &SubtitlesConfig,
        network_reconnect_wait: Duration,
    ) -> Self {
        let stream = &config.stream;
        Self {
            feed: StreamSubtitleFeedConfig {
                ahead: Duration::from_secs(u64::from(stream.ahead_s)),
                behind: Duration::from_secs(u64::from(stream.behind_s)),
                max_segment_bytes: non_zero_usize(
                    usize::from(stream.max_segment_kb).saturating_mul(BYTES_PER_KILOBYTE),
                ),
                max_pending_segments: non_zero_usize(usize::from(stream.max_pending_segments)),
                failure_threshold: NonZeroU32::new(u32::from(stream.failure_threshold))
                    .unwrap_or(NonZeroU32::MIN),
                outage_budget: network_reconnect_wait,
                message_capacity: FEED_MESSAGE_CAPACITY,
            },
            live_retention: SubtitleTime::from_micros(
                i64::from(stream.live_retention_s).saturating_mul(MICROS_PER_SECOND),
            ),
        }
    }
}

/// Ненулевое значение; ноль (недостижим после валидации) превращается в минимальное.
fn non_zero_usize(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_values_are_converted_with_their_units() {
        let mut config = SubtitlesConfig::default();
        config.stream.ahead_s = 90;
        config.stream.behind_s = 0;
        config.stream.max_segment_kb = 512;
        config.stream.max_pending_segments = 4;
        config.stream.failure_threshold = 5;
        config.stream.live_retention_s = 300;

        let limits = StreamLimits::from_config(&config, Duration::from_millis(45_000));

        assert_eq!(limits.feed.ahead, Duration::from_secs(90));
        assert_eq!(limits.feed.behind, Duration::ZERO);
        assert_eq!(limits.feed.max_segment_bytes.get(), 512 * 1024);
        assert_eq!(limits.feed.max_pending_segments.get(), 4);
        assert_eq!(limits.feed.failure_threshold.get(), 5);
        assert_eq!(limits.live_retention, SubtitleTime::from_millis(300_000));
        assert_eq!(limits.feed.message_capacity.get(), 64);
        assert_eq!(limits.feed.outage_budget, Duration::from_secs(45));
    }

    #[test]
    fn defaults_follow_the_config_defaults() {
        let limits = StreamLimits::from_config(&SubtitlesConfig::default(), Duration::ZERO);
        assert_eq!(limits.feed.ahead, Duration::from_secs(60));
        assert_eq!(limits.live_retention, SubtitleTime::from_millis(600_000));
    }
}
