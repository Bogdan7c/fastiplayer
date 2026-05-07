use std::collections::HashSet;

use crate::{AppConfig, CURRENT_SCHEMA_VERSION, ConfigError, ConfigResult, VideoCodec};

/// Минимальный decode-ahead: ноль ломает смысл backpressure окна.
const MIN_DECODE_AHEAD_MS: u64 = 1;

/// Максимальный decode-ahead, который ещё не превращает video queue в cache.
const MAX_DECODE_AHEAD_MS: u64 = 10_000;

/// Минимальный размер presentation queue.
const MIN_PRESENT_QUEUE_FRAMES: usize = 1;

/// Верхний предел, чтобы ошибочный config не удерживал слишком много GPU textures.
const MAX_PRESENT_QUEUE_FRAMES: usize = 64;

/// Минимальный audio high-water mark.
const MIN_AUDIO_BUFFER_TARGET_MS: u64 = 1;

/// Верхний предел audio buffer target для интерактивного player-а.
const MAX_AUDIO_BUFFER_TARGET_MS: u64 = 10_000;

/// Верхний предел network read-ahead на раннем этапе без полноценного cache manager.
const MAX_NETWORK_READ_AHEAD_MB: u64 = 4096;

/// Верхний предел render latency, выше которого config почти наверняка ошибочен.
const MAX_VULKAN_FRAME_LATENCY: u32 = 8;

/// Проверяет весь config после TOML/Serde deserialization.
pub(crate) fn validate_app_config(config: &AppConfig) -> ConfigResult<()> {
    validate_schema_version(config.schema_version)?;
    validate_player_section(config)?;
    validate_video_section(config)?;
    validate_audio_section(config)?;
    validate_network_section(config)?;
    validate_render_section(config)?;
    validate_ui_section(config)?;

    Ok(())
}

/// Проверяет schema version как явную точку будущих миграций.
fn validate_schema_version(schema_version: u32) -> ConfigResult<()> {
    if schema_version == CURRENT_SCHEMA_VERSION {
        return Ok(());
    }

    Err(invalid_value(
        "schema_version",
        format!("поддерживается только версия {CURRENT_SCHEMA_VERSION}, получена {schema_version}"),
    ))
}

/// Проверяет player section.
fn validate_player_section(config: &AppConfig) -> ConfigResult<()> {
    let codec_order = &config.player.preferred_video_codec_order;
    if codec_order.is_empty() {
        return Err(invalid_value(
            "player.preferred_video_codec_order",
            "список codec priority не должен быть пустым".to_string(),
        ));
    }

    let mut seen_codecs = HashSet::<VideoCodec>::new();
    for codec in codec_order {
        if !seen_codecs.insert(*codec) {
            return Err(invalid_value(
                "player.preferred_video_codec_order",
                format!("codec {codec:?} указан больше одного раза"),
            ));
        }
    }

    Ok(())
}

/// Проверяет video section.
fn validate_video_section(config: &AppConfig) -> ConfigResult<()> {
    validate_u64_range(
        "video.max_decode_ahead_ms",
        config.video.max_decode_ahead_ms,
        MIN_DECODE_AHEAD_MS,
        MAX_DECODE_AHEAD_MS,
    )?;
    validate_usize_range(
        "video.present_queue_frames",
        config.video.present_queue_frames,
        MIN_PRESENT_QUEUE_FRAMES,
        MAX_PRESENT_QUEUE_FRAMES,
    )?;

    Ok(())
}

/// Проверяет audio section.
fn validate_audio_section(config: &AppConfig) -> ConfigResult<()> {
    if !config.audio.volume.is_finite() || !(0.0..=1.0).contains(&config.audio.volume) {
        return Err(invalid_value(
            "audio.volume",
            format!(
                "громкость должна быть конечным числом в диапазоне 0.0..=1.0, получено {}",
                config.audio.volume
            ),
        ));
    }

    if config.audio.output_device.trim().is_empty() {
        return Err(invalid_value(
            "audio.output_device",
            "имя audio device не должно быть пустым".to_string(),
        ));
    }

    validate_u64_range(
        "audio.buffer_target_ms",
        config.audio.buffer_target_ms,
        MIN_AUDIO_BUFFER_TARGET_MS,
        MAX_AUDIO_BUFFER_TARGET_MS,
    )?;

    Ok(())
}

/// Проверяет network section.
fn validate_network_section(config: &AppConfig) -> ConfigResult<()> {
    validate_u64_range(
        "network.max_read_ahead_mb",
        config.network.max_read_ahead_mb,
        1,
        MAX_NETWORK_READ_AHEAD_MB,
    )
}

/// Проверяет render section.
fn validate_render_section(config: &AppConfig) -> ConfigResult<()> {
    validate_u32_range(
        "render.vulkan.max_frame_latency",
        config.render.vulkan.max_frame_latency,
        1,
        MAX_VULKAN_FRAME_LATENCY,
    )
}

/// Проверяет UI section.
fn validate_ui_section(config: &AppConfig) -> ConfigResult<()> {
    let language = config.ui.language.trim();
    if language.is_empty() {
        return Err(invalid_value(
            "ui.language",
            "язык UI не должен быть пустым".to_string(),
        ));
    }

    if language.len() > 16 {
        return Err(invalid_value(
            "ui.language",
            "язык UI должен быть коротким кодом, например `ru` или `en`".to_string(),
        ));
    }

    Ok(())
}

/// Проверяет `u64` диапазон с единым сообщением.
fn validate_u64_range(field: &'static str, value: u64, min: u64, max: u64) -> ConfigResult<()> {
    if (min..=max).contains(&value) {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!("значение должно быть в диапазоне {min}..={max}, получено {value}"),
    ))
}

/// Проверяет `u32` диапазон с единым сообщением.
fn validate_u32_range(field: &'static str, value: u32, min: u32, max: u32) -> ConfigResult<()> {
    if (min..=max).contains(&value) {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!("значение должно быть в диапазоне {min}..={max}, получено {value}"),
    ))
}

/// Проверяет `usize` диапазон с единым сообщением.
fn validate_usize_range(
    field: &'static str,
    value: usize,
    min: usize,
    max: usize,
) -> ConfigResult<()> {
    if (min..=max).contains(&value) {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!("значение должно быть в диапазоне {min}..={max}, получено {value}"),
    ))
}

/// Создаёт validation error без потери имени TOML-поля.
fn invalid_value(field: &'static str, message: String) -> ConfigError {
    ConfigError::InvalidValue { field, message }
}
