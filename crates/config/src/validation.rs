use std::collections::HashSet;

use crate::{
    AppConfig, CURRENT_SCHEMA_VERSION, ConfigError, ConfigResult, HdrToSdrConfig,
    HdrToSdrOperatorConfig, PlayerDemuxConfig, PlayerSeekConfig, RenderColorAdjustmentConfig,
    VideoCodec, VideoSchedulerConfig,
};

/// Минимальный decode-ahead: ноль ломает смысл backpressure окна.
const MIN_DECODE_AHEAD_MS: u64 = 1;

/// Максимальный decode-ahead, который ещё не превращает video queue в cache.
const MAX_DECODE_AHEAD_MS: u64 = 10_000;

/// Минимальный размер presentation queue.
const MIN_PRESENT_QUEUE_FRAMES: usize = 1;

/// Верхний предел, чтобы ошибочный config не удерживал слишком много GPU textures.
const MAX_PRESENT_QUEUE_FRAMES: usize = 64;

/// Минимум bounded decoder queue/pool capacity.
const MIN_DECODER_QUEUE_FRAMES: usize = 1;

/// Верхний предел decoder packet/frame queues, чтобы config не стал memory cache.
const MAX_DECODER_QUEUE_FRAMES: usize = 128;

/// Верхний предел VA output surface descriptors.
const MAX_DECODER_SURFACE_POOL_FRAMES: usize = 64;

/// Верхний предел zero-copy import slots.
const MAX_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 64;

/// Верхний предел demux work за tick, чтобы ошибочный config не блокировал worker.
const MAX_SCHEDULER_DEMUX_PACKETS_PER_TICK: usize = 512;

/// Верхний предел packet submit work за tick.
const MAX_SCHEDULER_VIDEO_PACKETS_PER_TICK: usize = MAX_DECODER_QUEUE_FRAMES;

/// Верхний предел drain work за tick.
const MAX_SCHEDULER_DECODED_FRAMES_PER_TICK: usize = MAX_DECODER_QUEUE_FRAMES;

/// Верхний предел catch-up окна; worker не должен занимать весь frame interval.
const MAX_SCHEDULER_CATCH_UP_BUDGET_MS: u64 = 16;

/// Минимальный audio high-water mark.
const MIN_AUDIO_BUFFER_TARGET_MS: u64 = 1;

/// Верхний предел audio buffer target для интерактивного player-а.
const MAX_AUDIO_BUFFER_TARGET_MS: u64 = 10_000;

/// Верхний предел video preroll перед seek resume, чтобы config не удерживал лишние GPU frames.
const MAX_SEEK_RESUME_VIDEO_READY_FRAMES: usize = MAX_PRESENT_QUEUE_FRAMES + 1;

/// Верхний предел demux skip-window, чтобы повреждённый stream не держал worker слишком долго.
const MAX_CONSECUTIVE_CORRUPTED_PACKETS: usize = 4096;

/// Верхний предел network read-ahead на раннем этапе без полноценного cache manager.
const MAX_NETWORK_READ_AHEAD_MB: u64 = 4096;

/// Верхний предел RAM cache, чтобы ошибочный config не занял всю память.
const MAX_NETWORK_MEMORY_CACHE_MB: u64 = 4096;

/// Верхний предел ожидания `yt-dlp`, чтобы зависший resolver не жил бесконечно.
const MAX_YOUTUBE_RESOLVE_TIMEOUT_MS: u64 = 300_000;

/// Верхний предел render latency, выше которого config почти наверняка ошибочен.
const MAX_VULKAN_FRAME_LATENCY: u32 = 8;

/// Единственный skin, для которого текущий UI гарантирует layout contract.
const DEFAULT_UI_SKIN: &str = "minimal";

/// Верхний предел reference luminance для Phase 10 SDR/HDR nits fields.
const MAX_HDR_TO_SDR_REFERENCE_NITS: f32 = 10_000.0;

/// Количество каналов в пользовательских RGB triplet-полях.
const RGB_CHANNEL_COUNT: usize = 3;

/// Проверяет весь config после TOML/Serde deserialization.
pub(crate) fn validate_app_config(config: &AppConfig) -> ConfigResult<()> {
    validate_schema_version(config.schema_version)?;
    validate_player_section(config)?;
    validate_video_section(config)?;
    validate_audio_section(config)?;
    validate_network_section(config)?;
    validate_youtube_section(config)?;
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

    validate_player_seek_config(&config.player.seek)?;
    validate_player_demux_config(&config.player.demux)?;

    Ok(())
}

/// Проверяет seek/scrub параметры до использования scheduler-ом.
fn validate_player_seek_config(seek: &PlayerSeekConfig) -> ConfigResult<()> {
    validate_positive_u64("player.seek.commit_timeout_ms", seek.commit_timeout_ms)?;
    validate_positive_u64(
        "player.seek.resume_audio_min_buffer_ms",
        seek.resume_audio_min_buffer_ms,
    )?;
    validate_positive_u64(
        "player.seek.resume_audio_gate_timeout_ms",
        seek.resume_audio_gate_timeout_ms,
    )?;
    validate_usize_range(
        "player.seek.resume_video_min_ready_frames",
        seek.resume_video_min_ready_frames,
        1,
        MAX_SEEK_RESUME_VIDEO_READY_FRAMES,
    )?;
    validate_positive_u64(
        "player.seek.hotkey_small_step_secs",
        seek.hotkey_small_step_secs,
    )?;
    validate_positive_u64(
        "player.seek.hotkey_large_step_secs",
        seek.hotkey_large_step_secs,
    )?;

    Ok(())
}

/// Проверяет demux fail-safe параметры до открытия media.
fn validate_player_demux_config(demux: &PlayerDemuxConfig) -> ConfigResult<()> {
    validate_usize_range(
        "player.demux.max_consecutive_corrupted_packets",
        demux.max_consecutive_corrupted_packets,
        1,
        MAX_CONSECUTIVE_CORRUPTED_PACKETS,
    )
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
    validate_usize_range(
        "video.decoder_packet_channel_frames",
        config.video.decoder_packet_channel_frames,
        MIN_DECODER_QUEUE_FRAMES,
        MAX_DECODER_QUEUE_FRAMES,
    )?;
    validate_usize_range(
        "video.decoder_frame_channel_frames",
        config.video.decoder_frame_channel_frames,
        MIN_DECODER_QUEUE_FRAMES,
        MAX_DECODER_QUEUE_FRAMES,
    )?;
    validate_usize_range(
        "video.decoder_ready_queue_frames",
        config.video.decoder_ready_queue_frames,
        MIN_DECODER_QUEUE_FRAMES,
        MAX_DECODER_QUEUE_FRAMES,
    )?;
    validate_usize_range(
        "video.decoder_surface_pool_frames",
        config.video.decoder_surface_pool_frames,
        MIN_DECODER_QUEUE_FRAMES,
        MAX_DECODER_SURFACE_POOL_FRAMES,
    )?;
    validate_usize_range(
        "video.zero_copy_surface_pool_slots",
        config.video.zero_copy_surface_pool_slots,
        MIN_DECODER_QUEUE_FRAMES,
        MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
    )?;
    validate_video_scheduler_config(config)?;

    Ok(())
}

/// Проверяет scheduler budgets и cross-field watermarks video pipeline-а.
fn validate_video_scheduler_config(config: &AppConfig) -> ConfigResult<()> {
    let scheduler = &config.video.scheduler;

    validate_usize_range(
        "video.scheduler.demux_packets_per_tick",
        scheduler.demux_packets_per_tick,
        1,
        MAX_SCHEDULER_DEMUX_PACKETS_PER_TICK,
    )?;
    validate_usize_range(
        "video.scheduler.video_packets_per_tick",
        scheduler.video_packets_per_tick,
        1,
        MAX_SCHEDULER_VIDEO_PACKETS_PER_TICK,
    )?;
    validate_usize_range(
        "video.scheduler.decoded_frames_per_tick",
        scheduler.decoded_frames_per_tick,
        1,
        MAX_SCHEDULER_DECODED_FRAMES_PER_TICK,
    )?;
    validate_u64_range(
        "video.scheduler.catch_up_budget_ms",
        scheduler.catch_up_budget_ms,
        1,
        MAX_SCHEDULER_CATCH_UP_BUDGET_MS,
    )?;
    validate_scheduler_present_queue_watermarks(scheduler, config.video.present_queue_frames)?;
    validate_scheduler_decode_ahead_watermarks(scheduler, config.video.max_decode_ahead_ms)?;
    validate_scheduler_surface_watermarks(scheduler, config.video.zero_copy_surface_pool_slots)?;

    Ok(())
}

/// Проверяет min/target/max для presentation queue.
fn validate_scheduler_present_queue_watermarks(
    scheduler: &VideoSchedulerConfig,
    present_queue_max_frames: usize,
) -> ConfigResult<()> {
    validate_usize_range(
        "video.scheduler.present_queue_min_frames",
        scheduler.present_queue_min_frames,
        1,
        present_queue_max_frames,
    )?;
    validate_usize_range(
        "video.scheduler.present_queue_target_frames",
        scheduler.present_queue_target_frames,
        scheduler.present_queue_min_frames,
        present_queue_max_frames,
    )
}

/// Проверяет target/max для decode-ahead относительно audio clock.
fn validate_scheduler_decode_ahead_watermarks(
    scheduler: &VideoSchedulerConfig,
    decode_ahead_max_ms: u64,
) -> ConfigResult<()> {
    validate_u64_range(
        "video.scheduler.decode_ahead_target_ms",
        scheduler.decode_ahead_target_ms,
        MIN_DECODE_AHEAD_MS,
        decode_ahead_max_ms,
    )
}

/// Проверяет watermarks свободных zero-copy surface/import slots.
fn validate_scheduler_surface_watermarks(
    scheduler: &VideoSchedulerConfig,
    zero_copy_surface_pool_slots: usize,
) -> ConfigResult<()> {
    validate_usize_range(
        "video.scheduler.surface_free_slots_min",
        scheduler.surface_free_slots_min,
        0,
        zero_copy_surface_pool_slots,
    )?;
    validate_usize_range(
        "video.scheduler.surface_free_slots_target",
        scheduler.surface_free_slots_target,
        scheduler.surface_free_slots_min,
        zero_copy_surface_pool_slots,
    )
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
    validate_positive_u64(
        "network.connect_timeout_ms",
        config.network.connect_timeout_ms,
    )?;
    validate_positive_u64("network.read_timeout_ms", config.network.read_timeout_ms)?;
    Ok(())
}

/// Проверяет YouTube/service section.
fn validate_youtube_section(config: &AppConfig) -> ConfigResult<()> {
    validate_u64_range(
        "youtube.resolve_timeout_ms",
        config.youtube.resolve_timeout_ms,
        1,
        MAX_YOUTUBE_RESOLVE_TIMEOUT_MS,
    )?;
    Ok(())
}

/// Проверяет render section.
fn validate_render_section(config: &AppConfig) -> ConfigResult<()> {
    validate_hdr_to_sdr_config(&config.render.hdr_to_sdr)?;
    validate_render_color_adjustment(&config.render.color_adjustment)?;
    validate_u32_range(
        "render.vulkan.max_frame_latency",
        config.render.vulkan.max_frame_latency,
        1,
        MAX_VULKAN_FRAME_LATENCY,
    )
}

/// Проверяет HDR-to-SDR config до передачи settings renderer-у.
fn validate_hdr_to_sdr_config(hdr_to_sdr: &HdrToSdrConfig) -> ConfigResult<()> {
    match hdr_to_sdr.operator {
        HdrToSdrOperatorConfig::Bt2446C => {}
    }

    validate_positive_reference_nits(
        "render.hdr_to_sdr.sdr_reference_white_nits",
        hdr_to_sdr.sdr_reference_white_nits,
    )?;
    validate_positive_reference_nits(
        "render.hdr_to_sdr.hdr_reference_peak_nits",
        hdr_to_sdr.hdr_reference_peak_nits,
    )?;

    if hdr_to_sdr.hdr_reference_peak_nits <= hdr_to_sdr.sdr_reference_white_nits {
        return Err(invalid_value(
            "render.hdr_to_sdr.hdr_reference_peak_nits",
            format!(
                "HDR peak должен быть выше SDR reference white: peak={}, white={}",
                hdr_to_sdr.hdr_reference_peak_nits, hdr_to_sdr.sdr_reference_white_nits
            ),
        ));
    }

    Ok(())
}

/// Проверяет положительное конечное значение luminance в nits.
fn validate_positive_reference_nits(field: &'static str, value: f32) -> ConfigResult<()> {
    if value.is_finite() && value > 0.0 && value <= MAX_HDR_TO_SDR_REFERENCE_NITS {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!(
            "значение должно быть конечным числом в диапазоне (0, {MAX_HDR_TO_SDR_REFERENCE_NITS}], получено {value}"
        ),
    ))
}

/// Проверяет SDR/RGB корректировки до передачи значений renderer-у.
fn validate_render_color_adjustment(
    color_adjustment: &RenderColorAdjustmentConfig,
) -> ConfigResult<()> {
    validate_f32_finite(
        "render.color_adjustment.brightness",
        color_adjustment.brightness,
    )?;
    validate_f32_finite(
        "render.color_adjustment.contrast",
        color_adjustment.contrast,
    )?;
    validate_f32_finite(
        "render.color_adjustment.saturation",
        color_adjustment.saturation,
    )?;
    validate_f32_finite(
        "render.color_adjustment.exposure",
        color_adjustment.exposure,
    )?;
    validate_rgb_triplet(
        "render.color_adjustment.rgb_gain",
        &color_adjustment.rgb_gain,
    )?;
    validate_rgb_triplet(
        "render.color_adjustment.rgb_offset",
        &color_adjustment.rgb_offset,
    )?;

    Ok(())
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

    if config.ui.skin.trim() != DEFAULT_UI_SKIN {
        return Err(invalid_value(
            "ui.skin",
            format!(
                "неизвестный skin `{}`; поддерживается только `{DEFAULT_UI_SKIN}`",
                config.ui.skin
            ),
        ));
    }

    Ok(())
}

/// Проверяет, что `u64` значение положительное.
fn validate_positive_u64(field: &'static str, value: u64) -> ConfigResult<()> {
    if value > 0 {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!("значение должно быть положительным, получено {value}"),
    ))
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

/// Проверяет, что `f32` поле можно безопасно отправить в shader uniforms.
fn validate_f32_finite(field: &'static str, value: f32) -> ConfigResult<()> {
    if value.is_finite() {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!("значение должно быть конечным числом, получено {value}"),
    ))
}

/// Проверяет RGB-массив: ровно три конечных значения в порядке R, G, B.
fn validate_rgb_triplet(field: &'static str, values: &[f32]) -> ConfigResult<()> {
    if values.len() != RGB_CHANNEL_COUNT {
        return Err(invalid_value(
            field,
            format!(
                "RGB-массив должен содержать ровно {RGB_CHANNEL_COUNT} значения, получено {}",
                values.len()
            ),
        ));
    }

    for (channel_index, channel_value) in values.iter().copied().enumerate() {
        if !channel_value.is_finite() {
            return Err(invalid_value(
                field,
                format!(
                    "RGB-канал #{channel_index} должен быть конечным числом, получено {channel_value}"
                ),
            ));
        }
    }

    Ok(())
}

/// Создаёт validation error без потери имени TOML-поля.
fn invalid_value(field: &'static str, message: String) -> ConfigError {
    ConfigError::InvalidValue { field, message }
}
