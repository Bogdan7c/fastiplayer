//! Выбор поддерживаемой CPAL output-конфигурации.
//!
//! Здесь изолирована device capability policy: порядок sample formats,
//! preferred sample rate и неизменный fallback на лучший доступный range.

use std::cmp::Ordering;

use anyhow::{Context, Result};
use cpal::SampleFormat;
use cpal::traits::DeviceTrait;
use tracing::{info, warn};

/// Проверяет, что sample format можно отдать в typed CPAL output callback.
///
/// CPAL 0.15.3 объявляет `SampleFormat` как `non_exhaustive`, поэтому wildcard
/// остаётся явной защитой от будущих форматов, которые нельзя silently принять.
pub(super) fn output_sample_format_is_supported(sample_format: SampleFormat) -> bool {
    matches!(
        sample_format,
        SampleFormat::I8
            | SampleFormat::I16
            | SampleFormat::I32
            | SampleFormat::I64
            | SampleFormat::U8
            | SampleFormat::U16
            | SampleFormat::U32
            | SampleFormat::U64
            | SampleFormat::F32
            | SampleFormat::F64
    )
}

/// Даёт стабильный приоритет fallback-формату, когда default config unusable.
///
/// Обычно используется `default_output_config`; этот порядок нужен только для
/// редкого fallback path-а через `supported_output_configs`.
pub(super) fn sample_format_priority(sample_format: SampleFormat) -> u8 {
    match sample_format {
        SampleFormat::F64 => 100,
        SampleFormat::F32 => 95,
        SampleFormat::I64 => 90,
        SampleFormat::I32 => 80,
        SampleFormat::I16 => 70,
        SampleFormat::I8 => 60,
        SampleFormat::U64 => 50,
        SampleFormat::U32 => 40,
        SampleFormat::U16 => 30,
        SampleFormat::U8 => 20,
        _ => 0,
    }
}

/// Проверяет, попадает ли желаемая частота в range CPAL config-а.
pub(super) fn sample_rate_is_supported(
    config_range: &cpal::SupportedStreamConfigRange,
    sample_rate: cpal::SampleRate,
) -> bool {
    config_range.min_sample_rate() <= sample_rate && sample_rate <= config_range.max_sample_rate()
}

/// Сравнивает два supported ranges для fallback config selection.
pub(super) fn compare_output_config_ranges(
    left: &cpal::SupportedStreamConfigRange,
    right: &cpal::SupportedStreamConfigRange,
    preferred_sample_rate: Option<cpal::SampleRate>,
) -> Ordering {
    let stereo_order = (left.channels() == 2).cmp(&(right.channels() == 2));
    if stereo_order != Ordering::Equal {
        return stereo_order;
    }

    let mono_order = (left.channels() == 1).cmp(&(right.channels() == 1));
    if mono_order != Ordering::Equal {
        return mono_order;
    }

    let channel_order = left.channels().cmp(&right.channels());
    if channel_order != Ordering::Equal {
        return channel_order;
    }

    let format_order = sample_format_priority(left.sample_format())
        .cmp(&sample_format_priority(right.sample_format()));
    if format_order != Ordering::Equal {
        return format_order;
    }

    let preferred_rate_order = preferred_sample_rate
        .map(|sample_rate| {
            sample_rate_is_supported(left, sample_rate)
                .cmp(&sample_rate_is_supported(right, sample_rate))
        })
        .unwrap_or(Ordering::Equal);
    if preferred_rate_order != Ordering::Equal {
        return preferred_rate_order;
    }

    left.max_sample_rate().cmp(&right.max_sample_rate())
}

/// Превращает supported range в concrete config без panic на sample rate.
pub(super) fn config_from_supported_range(
    config_range: cpal::SupportedStreamConfigRange,
    preferred_sample_rate: Option<cpal::SampleRate>,
) -> cpal::SupportedStreamConfig {
    if let Some(sample_rate) = preferred_sample_rate
        && let Some(config) = config_range.try_with_sample_rate(sample_rate)
    {
        return config;
    }

    config_range.with_max_sample_rate()
}

/// Выбирает fallback output config только из форматов, которые умеет `AudioOutput`.
pub(super) fn select_supported_output_config<I>(
    supported_ranges: I,
    preferred_sample_rate: Option<cpal::SampleRate>,
) -> Option<cpal::SupportedStreamConfig>
where
    I: IntoIterator<Item = cpal::SupportedStreamConfigRange>,
{
    supported_ranges
        .into_iter()
        .filter(|config_range| output_sample_format_is_supported(config_range.sample_format()))
        .max_by(|left, right| compare_output_config_ranges(left, right, preferred_sample_rate))
        .map(|config_range| config_from_supported_range(config_range, preferred_sample_rate))
}

/// Возвращает output config, предпочитая частоту декодера, затем default/fallback.
///
/// Прямое совпадение stream rate с decoder rate убирает linear resampler из
/// hot path целиком: без него нет aliasing-потрескивания (у ресемплера нет
/// анти-алиас фильтра), особенно заметного на tempo-output.
pub(super) fn choose_supported_output_config(
    device: &cpal::Device,
    decoder_rate: u32,
) -> Result<cpal::SupportedStreamConfig> {
    let default_config = match device.default_output_config() {
        Ok(config) => Some(config),
        Err(error) => {
            warn!(error = %error, "CPAL default output config недоступен, пробуем supported list");
            None
        }
    };

    // Default уже на частоте декодера — берём его без сканирования ranges.
    if let Some(config) = default_config.as_ref().filter(|config| {
        output_sample_format_is_supported(config.sample_format())
            && config.sample_rate().0 == decoder_rate
    }) {
        return Ok(config.clone());
    }

    // Ищем supported config ровно на частоте декодера.
    if decoder_rate > 0
        && let Ok(supported_ranges) = device.supported_output_configs()
    {
        let decoder_rate_config =
            select_supported_output_config(supported_ranges, Some(cpal::SampleRate(decoder_rate)))
                .filter(|config| config.sample_rate().0 == decoder_rate);
        if let Some(config) = decoder_rate_config {
            info!(
                decoder_rate,
                channels = config.channels(),
                format = ?config.sample_format(),
                "Output stream открыт на частоте декодера без ресемплера"
            );
            return Ok(config);
        }
    }

    if let Some(config) = default_config
        .as_ref()
        .filter(|config| output_sample_format_is_supported(config.sample_format()))
    {
        return Ok(config.clone());
    }

    let preferred_sample_rate = default_config.as_ref().map(|config| config.sample_rate());
    let supported_ranges = device
        .supported_output_configs()
        .context("Не удалось получить supported output configs")?;
    let fallback_error_context = match default_config.as_ref() {
        Some(config) => format!(
            "Default CPAL output format {:?} не поддерживается AudioOutput, fallback не найден",
            config.sample_format()
        ),
        None => "CPAL не вернул ни одного поддерживаемого output config".to_string(),
    };
    let fallback_config = select_supported_output_config(supported_ranges, preferred_sample_rate)
        .context(fallback_error_context)?;

    if let Some(config) = default_config {
        warn!(
            default_format = ?config.sample_format(),
            fallback_format = ?fallback_config.sample_format(),
            fallback_rate = fallback_config.sample_rate().0,
            fallback_channels = fallback_config.channels(),
            "Default CPAL output config заменён supported fallback"
        );
    } else {
        info!(
            format = ?fallback_config.sample_format(),
            rate = fallback_config.sample_rate().0,
            channels = fallback_config.channels(),
            "Выбран CPAL output config из supported list"
        );
    }

    Ok(fallback_config)
}
