//! Конфигурация worker-а, не помещающаяся в `worker.rs` (лимит размера модуля).
//!
//! Здесь живут `Debug` для [`PlayerWorkerConfig`] и builder ёмкости трубы субтитров.

use super::*;

impl fmt::Debug for PlayerWorkerConfig {
    /// Не раскрывает trait object, но показывает остальную runtime конфигурацию.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlayerWorkerConfig")
            .field("coarse_wakeup_interval", &self.coarse_wakeup_interval)
            .field(
                "decoder_readiness_poll_interval",
                &self.decoder_readiness_poll_interval,
            )
            .field("tick_config", &self.tick_config)
            .field("decoder_thread_config", &self.decoder_thread_config)
            .field("default_volume", &self.default_volume)
            .field("audio_decoder_factory", &"<dyn AudioDecoderFactory>")
            .field("audio_output_factory", &"<dyn AudioOutputFactory>")
            .field(
                "audio_tempo_processor_factory",
                &"<dyn AudioTempoProcessorFactory>",
            )
            .field("frame_server_config", &self.frame_server_config)
            .field(
                "timeline_activity_wake",
                &self.timeline_activity_wake.as_ref().map(|_| "<configured>"),
            )
            .field(
                "presentation_wake",
                &self.presentation_wake.as_ref().map(|_| "<configured>"),
            )
            .field("subtitle_feed_capacity", &self.subtitle_feed_capacity)
            .finish()
    }
}

/// Ёмкость трубы субтитров из `[subtitles].embedded_feed_capacity` проверенного конфига.
///
/// Диапазон (256..=65536) гарантирует `AppConfig::validate()`, поэтому ноль и переполнение
/// `usize` недостижимы; запасное значение нужно лишь чтобы функция была тотальной и не паниковала.
/// Значение читается при создании worker-а: живой worker трубу не пересоздаёт, поэтому смена
/// настройки действует после перезапуска приложения.
pub(super) fn subtitle_feed_capacity_from_app_config(
    config: &fastiplayer_config::AppConfig,
) -> SubtitleFeedCapacity {
    usize::try_from(config.subtitles.embedded_feed_capacity)
        .ok()
        .and_then(SubtitleFeedCapacity::from_messages)
        .unwrap_or(SubtitleFeedCapacity::FALLBACK)
}

impl PlayerWorkerConfig {
    /// Задаёт ёмкость трубы встроенных субтитров (значение из конфига приложения).
    ///
    /// Без вызова используется [`SubtitleFeedCapacity::FALLBACK`].
    #[must_use]
    pub const fn with_subtitle_feed_capacity(
        mut self,
        subtitle_feed_capacity: SubtitleFeedCapacity,
    ) -> Self {
        self.subtitle_feed_capacity = subtitle_feed_capacity;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Значение из `[subtitles]` доходит до конфига worker-а как есть.
    #[test]
    fn app_config_capacity_reaches_worker_config() {
        let mut app_config = fastiplayer_config::AppConfig::default();
        app_config.subtitles.embedded_feed_capacity = 1_000;

        let worker_config = PlayerWorkerConfig::from_app_config(&app_config);

        assert_eq!(worker_config.subtitle_feed_capacity.messages(), 1_000);
    }

    /// Конфиг по умолчанию даёт прежние 4096 пакетов (равны запасной константе).
    #[test]
    fn default_app_config_gives_default_capacity() {
        let worker_config =
            PlayerWorkerConfig::from_app_config(&fastiplayer_config::AppConfig::default());

        assert_eq!(
            worker_config.subtitle_feed_capacity,
            SubtitleFeedCapacity::FALLBACK
        );
        assert_eq!(worker_config.subtitle_feed_capacity.messages(), 4_096);
    }

    /// Builder перекрывает значение и не трогает остальные поля.
    #[test]
    fn builder_overrides_only_the_capacity() {
        let base = PlayerWorkerConfig::from_app_config(&fastiplayer_config::AppConfig::default());
        let default_volume = base.default_volume;
        let capacity = SubtitleFeedCapacity::from_messages(300).expect("non-zero");

        let tuned = base.with_subtitle_feed_capacity(capacity);

        assert_eq!(tuned.subtitle_feed_capacity.messages(), 300);
        assert!((tuned.default_volume - default_volume).abs() < f32::EPSILON);
    }
}
