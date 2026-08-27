//! Audio, network и yt-dlp validation с неизменными setting/error paths.

use crate::{AppConfig, ConfigResult, YtDlpConfig};

use super::{
    MAX_AUDIO_BUFFER_TARGET_MS, MAX_AUDIO_VOLUME, MIN_AUDIO_BUFFER_TARGET_MS, MIN_AUDIO_VOLUME,
    invalid_value, validate_positive_u64, validate_u64_range,
};

/// Верхний предел network read-ahead на раннем этапе без полноценного cache manager.
pub(crate) const MAX_NETWORK_READ_AHEAD_MB: u64 = 4096;

/// Верхний предел начального prefetch chunk-а в КиБ, привязанный к общему read-ahead budget.
pub(crate) const MAX_NETWORK_PREFETCH_INITIAL_CHUNK_KB: u64 = MAX_NETWORK_READ_AHEAD_MB * 1024;

/// Верхний предел RAM cache, чтобы ошибочный config не занял всю память.
pub(crate) const MAX_NETWORK_MEMORY_CACHE_MB: u64 = 4096;

/// Верхний предел ожидания `yt-dlp`, чтобы зависший resolver не жил бесконечно.
pub(crate) const MAX_YT_DLP_RESOLVE_TIMEOUT_MS: u64 = 300_000;

/// Максимально настраиваемый stdout single-item extraction.
pub(crate) const MAX_YT_DLP_SINGLE_ITEM_STDOUT_BYTES: u64 = 1024 * 1024 * 1024;

/// Максимально настраиваемый stderr single-item extraction.
pub(crate) const MAX_YT_DLP_SINGLE_ITEM_STDERR_BYTES: u64 = 64 * 1024 * 1024;

/// Максимально настраиваемое число JSON values одного single-item extraction.
pub(crate) const MAX_YT_DLP_SINGLE_ITEM_JSON_NODES: u64 = 10_000_000;

/// Recovery budget остаётся малым и не допускает бесконечный extraction loop.
pub(crate) const MAX_YT_DLP_VOD_RECOVERY_ATTEMPTS: u64 = 10;

/// Backoff не должен замораживать UI/runtime дольше одной минуты за attempt.
pub(crate) const MAX_YT_DLP_VOD_RECOVERY_BACKOFF_MS: u64 = 60_000;

/// Stable reset interval ограничен одним часом.
pub(crate) const MAX_YT_DLP_VOD_RECOVERY_STABLE_RESET_MS: u64 = 3_600_000;

/// Проверяет audio section.
pub(super) fn validate_audio_section(config: &AppConfig) -> ConfigResult<()> {
    if !config.audio.volume.is_finite()
        || !(MIN_AUDIO_VOLUME..=MAX_AUDIO_VOLUME).contains(&config.audio.volume)
    {
        return Err(invalid_value(
            "audio.volume",
            format!(
                "громкость должна быть конечным числом в диапазоне {MIN_AUDIO_VOLUME}..={MAX_AUDIO_VOLUME}, получено {}",
                config.audio.volume,
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
pub(super) fn validate_network_section(config: &AppConfig) -> ConfigResult<()> {
    validate_u64_range(
        "network.memory_cache_mb",
        config.network.memory_cache_mb,
        0,
        MAX_NETWORK_MEMORY_CACHE_MB,
    )?;
    validate_u64_range(
        "network.read_ahead_mb",
        config.network.read_ahead_mb,
        1,
        MAX_NETWORK_READ_AHEAD_MB,
    )?;
    validate_u64_range(
        "network.prefetch_chunk_mb",
        config.network.prefetch_chunk_mb,
        1,
        MAX_NETWORK_READ_AHEAD_MB,
    )?;
    validate_u64_range(
        "network.prefetch_initial_chunk_kb",
        config.network.prefetch_initial_chunk_kb,
        1,
        MAX_NETWORK_PREFETCH_INITIAL_CHUNK_KB,
    )?;
    let chunk_size_kb = config
        .network
        .prefetch_chunk_mb
        .checked_mul(1024)
        .ok_or_else(|| {
            invalid_value(
                "network.prefetch_chunk_mb",
                format!(
                    "prefetch chunk не помещается в КиБ: prefetch_chunk_mb={}",
                    config.network.prefetch_chunk_mb
                ),
            )
        })?;
    if config.network.prefetch_initial_chunk_kb > chunk_size_kb {
        return Err(invalid_value(
            "network.prefetch_initial_chunk_kb",
            format!(
                "initial prefetch chunk должен быть не больше chunk: prefetch_initial_chunk_kb={}, prefetch_chunk_mb={}",
                config.network.prefetch_initial_chunk_kb, config.network.prefetch_chunk_mb
            ),
        ));
    }
    if config.network.read_ahead_mb < config.network.prefetch_chunk_mb {
        return Err(invalid_value(
            "network.read_ahead_mb",
            format!(
                "prefetch window должен быть не меньше chunk: read_ahead_mb={}, prefetch_chunk_mb={}",
                config.network.read_ahead_mb, config.network.prefetch_chunk_mb
            ),
        ));
    }
    validate_positive_u64(
        "network.connect_timeout_ms",
        config.network.connect_timeout_ms,
    )?;
    validate_positive_u64("network.read_timeout_ms", config.network.read_timeout_ms)?;
    Ok(())
}

/// Проверяет YtDlp/service section.
pub(crate) fn validate_yt_dlp_config(config: &YtDlpConfig) -> ConfigResult<()> {
    validate_u64_range(
        "yt_dlp.resolve_timeout_ms",
        config.resolve_timeout_ms,
        1,
        MAX_YT_DLP_RESOLVE_TIMEOUT_MS,
    )?;
    validate_u64_range(
        "yt_dlp.single_item_stdout_limit_bytes",
        config.single_item_stdout_limit_bytes,
        1,
        MAX_YT_DLP_SINGLE_ITEM_STDOUT_BYTES,
    )?;
    validate_u64_range(
        "yt_dlp.single_item_stderr_limit_bytes",
        config.single_item_stderr_limit_bytes,
        1,
        MAX_YT_DLP_SINGLE_ITEM_STDERR_BYTES,
    )?;
    validate_u64_range(
        "yt_dlp.single_item_json_node_limit",
        config.single_item_json_node_limit,
        1,
        MAX_YT_DLP_SINGLE_ITEM_JSON_NODES,
    )?;
    validate_u64_range(
        "yt_dlp.vod_endpoint_recovery_max_consecutive_attempts",
        config.vod_endpoint_recovery_max_consecutive_attempts,
        1,
        MAX_YT_DLP_VOD_RECOVERY_ATTEMPTS,
    )?;
    validate_u64_range(
        "yt_dlp.vod_endpoint_recovery_initial_backoff_ms",
        config.vod_endpoint_recovery_initial_backoff_ms,
        1,
        MAX_YT_DLP_VOD_RECOVERY_BACKOFF_MS,
    )?;
    validate_u64_range(
        "yt_dlp.vod_endpoint_recovery_max_backoff_ms",
        config.vod_endpoint_recovery_max_backoff_ms,
        1,
        MAX_YT_DLP_VOD_RECOVERY_BACKOFF_MS,
    )?;
    validate_u64_range(
        "yt_dlp.vod_endpoint_recovery_stable_reset_ms",
        config.vod_endpoint_recovery_stable_reset_ms,
        1,
        MAX_YT_DLP_VOD_RECOVERY_STABLE_RESET_MS,
    )?;
    if config.vod_endpoint_recovery_initial_backoff_ms > config.vod_endpoint_recovery_max_backoff_ms
    {
        return Err(invalid_value(
            "yt_dlp.vod_endpoint_recovery_initial_backoff_ms",
            "initial recovery backoff не может превышать maximum backoff".to_owned(),
        ));
    }
    Ok(())
}
