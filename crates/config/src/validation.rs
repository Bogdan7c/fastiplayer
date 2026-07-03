use std::collections::HashSet;

use crate::{
    AppConfig, CURRENT_SCHEMA_VERSION, ConfigError, ConfigResult, HdrToSdrConfig,
    HdrToSdrOperatorConfig, PlayerDemuxConfig, PlayerSeekConfig, RenderColorAdjustmentConfig,
    VideoCodec, VideoSchedulerConfig,
};

/// Минимальный decode-ahead: ноль ломает смысл backpressure окна.
pub(crate) const MIN_DECODE_AHEAD_MS: u64 = 1;

/// Максимальный decode-ahead, который ещё не превращает video queue в cache.
pub(crate) const MAX_DECODE_AHEAD_MS: u64 = 10_000;

/// Минимальный размер presentation queue.
pub(crate) const MIN_PRESENT_QUEUE_FRAMES: usize = 1;

/// Верхний предел, чтобы ошибочный config не удерживал слишком много GPU textures.
pub(crate) const MAX_PRESENT_QUEUE_FRAMES: usize = 64;

/// Минимум bounded decoder queue/pool capacity.
pub(crate) const MIN_DECODER_QUEUE_FRAMES: usize = 1;

/// Верхний предел decoder packet/frame queues, чтобы config не стал memory cache.
pub(crate) const MAX_DECODER_QUEUE_FRAMES: usize = 128;

/// Верхний предел VA output surface descriptors.
pub(crate) const MAX_DECODER_SURFACE_POOL_FRAMES: usize = 64;

/// Верхний предел zero-copy import slots.
pub(crate) const MAX_ZERO_COPY_SURFACE_POOL_SLOTS: usize = 64;

/// Минимум потоков software-декода; 0 = auto (ядра − 2).
pub(crate) const MIN_SW_DECODE_THREADS: usize = 0;

/// Верхний предел потоков software-декода.
pub(crate) const MAX_SW_DECODE_THREADS: usize = 64;

/// Верхний предел demux work за tick, чтобы ошибочный config не блокировал worker.
pub(crate) const MAX_SCHEDULER_DEMUX_PACKETS_PER_TICK: usize = 512;

/// Верхний предел packet submit work за tick.
pub(crate) const MAX_SCHEDULER_VIDEO_PACKETS_PER_TICK: usize = MAX_DECODER_QUEUE_FRAMES;

/// Верхний предел drain work за tick.
pub(crate) const MAX_SCHEDULER_DECODED_FRAMES_PER_TICK: usize = MAX_DECODER_QUEUE_FRAMES;

/// Верхний предел catch-up окна; worker не должен занимать весь frame interval.
pub(crate) const MAX_SCHEDULER_CATCH_UP_BUDGET_MS: u64 = 16;

/// Минимальный audio high-water mark.
pub(crate) const MIN_AUDIO_BUFFER_TARGET_MS: u64 = 1;

/// Верхний предел audio buffer target для интерактивного player-а.
pub(crate) const MAX_AUDIO_BUFFER_TARGET_MS: u64 = 10_000;

/// Верхний предел video preroll перед seek resume, чтобы config не удерживал лишние GPU frames.
pub(crate) const MAX_SEEK_RESUME_VIDEO_READY_FRAMES: usize = MAX_PRESENT_QUEUE_FRAMES + 1;

/// Верхний предел seek-only preroll work window; это интерактивный bounded burst, не idle loop.
pub(crate) const MAX_SEEK_FAST_PREROLL_BUDGET_MS: u64 = 250;

/// Верхний предел GOP preroll burst-а; реальные decoder/resource лимиты остаются ниже.
pub(crate) const MAX_SEEK_FAST_PREROLL_VIDEO_PACKET_BURST: usize = 4096;

/// Верхний предел demux skip-window, чтобы повреждённый stream не держал worker слишком долго.
pub(crate) const MAX_CONSECUTIVE_CORRUPTED_PACKETS: usize = 4096;

/// Верхний предел network read-ahead на раннем этапе без полноценного cache manager.
pub(crate) const MAX_NETWORK_READ_AHEAD_MB: u64 = 4096;

/// Верхний предел начального prefetch chunk-а в КиБ, привязанный к общему read-ahead budget.
pub(crate) const MAX_NETWORK_PREFETCH_INITIAL_CHUNK_KB: u64 = MAX_NETWORK_READ_AHEAD_MB * 1024;

/// Верхний предел RAM cache, чтобы ошибочный config не занял всю память.
pub(crate) const MAX_NETWORK_MEMORY_CACHE_MB: u64 = 4096;

/// Верхний предел ожидания `yt-dlp`, чтобы зависший resolver не жил бесконечно.
pub(crate) const MAX_YOUTUBE_RESOLVE_TIMEOUT_MS: u64 = 300_000;

/// Верхний предел render latency, выше которого config почти наверняка ошибочен.
pub(crate) const MAX_VULKAN_FRAME_LATENCY: u32 = 8;

/// Единственный skin, для которого текущий UI гарантирует layout contract.
pub(crate) const DEFAULT_UI_SKIN: &str = "minimal";

/// Минимальная частота live preview: ноль означал бы выключенный pacing, а не валидную частоту.
pub(crate) const MIN_LIVE_PREVIEW_MAX_HZ: u16 = 1;

/// Верхняя граница live preview защищает runtime от слишком частых preview updates.
pub(crate) const MAX_LIVE_PREVIEW_MAX_HZ: u16 = 240;

/// Минимальная live-scrub частота: ноль означал бы выключенный throttle, а не частоту.
pub(crate) const MIN_FRAME_SERVER_LIVE_SCRUB_MAX_HZ: u16 = 1;

/// Верхняя live-scrub частота защищает будущий runtime от слишком частых decode starts.
pub(crate) const MAX_FRAME_SERVER_LIVE_SCRUB_MAX_HZ: u16 = 240;

/// Нижняя граница времени анимации sidebar: ноль валиден и означает «без анимации».
pub(crate) const MIN_SIDEBAR_SLIDE_DURATION_MS: u16 = 0;

/// Верхняя граница времени анимации sidebar: дольше 5 секунд UI ощущается сломанным.
pub(crate) const MAX_SIDEBAR_SLIDE_DURATION_MS: u16 = 5000;

/// Минимальная высота кастомного titlebar: ниже кнопки окна становятся слишком мелкими.
pub(crate) const MIN_TITLEBAR_HEIGHT_PX: u16 = 32;

/// Максимальная высота кастомного titlebar: выше этого overlay начинает занимать слишком много видео.
pub(crate) const MAX_TITLEBAR_HEIGHT_PX: u16 = 96;

/// Нижний предел reference luminance: значения ниже 1 nit не имеют полезного UI-смысла.
pub(crate) const MIN_HDR_TO_SDR_REFERENCE_NITS: f32 = 1.0;

/// Верхний предел reference luminance для Phase 10 SDR/HDR nits fields.
pub(crate) const MAX_HDR_TO_SDR_REFERENCE_NITS: f32 = 10_000.0;

/// Нижний предел default startup volume.
pub(crate) const MIN_AUDIO_VOLUME: f64 = 0.0;

/// Верхний предел default startup volume.
pub(crate) const MAX_AUDIO_VOLUME: f64 = 1.0;

/// Минимальная длина кода языка UI.
pub(crate) const MIN_UI_LANGUAGE_LEN: usize = 1;

/// Максимальная длина кода языка UI.
pub(crate) const MAX_UI_LANGUAGE_LEN: usize = 16;

/// Минимум для положительных `u64` полей без более узкой доменной границы.
pub(crate) const MIN_POSITIVE_U64_SETTING_VALUE: u64 = 1;

/// Максимум, который settings registry может представить как signed integer.
pub(crate) const MAX_POSITIVE_U64_SETTING_VALUE: u64 = i64::MAX as u64;

/// Минимальное additive brightness смещение SDR shader-а.
pub(crate) const MIN_RENDER_COLOR_BRIGHTNESS: f32 = -1.0;

/// Максимальное additive brightness смещение SDR shader-а.
pub(crate) const MAX_RENDER_COLOR_BRIGHTNESS: f32 = 1.0;

/// Минимальный contrast multiplier.
pub(crate) const MIN_RENDER_COLOR_CONTRAST: f32 = 0.0;

/// Максимальный contrast multiplier.
pub(crate) const MAX_RENDER_COLOR_CONTRAST: f32 = 4.0;

/// Минимальный saturation multiplier.
pub(crate) const MIN_RENDER_COLOR_SATURATION: f32 = 0.0;

/// Максимальный saturation multiplier.
pub(crate) const MAX_RENDER_COLOR_SATURATION: f32 = 4.0;

/// Минимальное exposure offset значение.
pub(crate) const MIN_RENDER_COLOR_EXPOSURE: f32 = -4.0;

/// Максимальное exposure offset значение.
pub(crate) const MAX_RENDER_COLOR_EXPOSURE: f32 = 4.0;

/// Минимальный поканальный RGB gain.
pub(crate) const MIN_RENDER_RGB_GAIN: f32 = 0.0;

/// Максимальный поканальный RGB gain.
pub(crate) const MAX_RENDER_RGB_GAIN: f32 = 4.0;

/// Минимальный поканальный RGB offset.
pub(crate) const MIN_RENDER_RGB_OFFSET: f32 = -1.0;

/// Максимальный поканальный RGB offset.
pub(crate) const MAX_RENDER_RGB_OFFSET: f32 = 1.0;

/// Количество каналов в пользовательских RGB triplet-полях.
pub(crate) const RGB_CHANNEL_COUNT: usize = 3;

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
    validate_frame_server_section(config)?;

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
    validate_u64_range(
        "player.seek.fast_preroll_budget_ms",
        seek.fast_preroll_budget_ms,
        1,
        MAX_SEEK_FAST_PREROLL_BUDGET_MS,
    )?;
    validate_usize_range(
        "player.seek.fast_preroll_video_packet_burst",
        seek.fast_preroll_video_packet_burst,
        1,
        MAX_SEEK_FAST_PREROLL_VIDEO_PACKET_BURST,
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
        "video.sw_decoder_surface_pool_frames",
        config.video.sw_decoder_surface_pool_frames,
        MIN_DECODER_QUEUE_FRAMES,
        MAX_DECODER_SURFACE_POOL_FRAMES,
    )?;
    validate_usize_range(
        "video.sw_decode_threads",
        config.video.sw_decode_threads,
        MIN_SW_DECODE_THREADS,
        MAX_SW_DECODE_THREADS,
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
    if value.is_finite()
        && (MIN_HDR_TO_SDR_REFERENCE_NITS..=MAX_HDR_TO_SDR_REFERENCE_NITS).contains(&value)
    {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!(
            "значение должно быть конечным числом в диапазоне {MIN_HDR_TO_SDR_REFERENCE_NITS}..={MAX_HDR_TO_SDR_REFERENCE_NITS}, получено {value}"
        ),
    ))
}

/// Проверяет SDR/RGB корректировки до передачи значений renderer-у.
fn validate_render_color_adjustment(
    color_adjustment: &RenderColorAdjustmentConfig,
) -> ConfigResult<()> {
    validate_f32_range(
        "render.color_adjustment.brightness",
        color_adjustment.brightness,
        MIN_RENDER_COLOR_BRIGHTNESS,
        MAX_RENDER_COLOR_BRIGHTNESS,
    )?;
    validate_f32_range(
        "render.color_adjustment.contrast",
        color_adjustment.contrast,
        MIN_RENDER_COLOR_CONTRAST,
        MAX_RENDER_COLOR_CONTRAST,
    )?;
    validate_f32_range(
        "render.color_adjustment.saturation",
        color_adjustment.saturation,
        MIN_RENDER_COLOR_SATURATION,
        MAX_RENDER_COLOR_SATURATION,
    )?;
    validate_f32_range(
        "render.color_adjustment.exposure",
        color_adjustment.exposure,
        MIN_RENDER_COLOR_EXPOSURE,
        MAX_RENDER_COLOR_EXPOSURE,
    )?;
    validate_rgb_triplet(
        "render.color_adjustment.rgb_gain",
        &color_adjustment.rgb_gain,
        MIN_RENDER_RGB_GAIN,
        MAX_RENDER_RGB_GAIN,
    )?;
    validate_rgb_triplet(
        "render.color_adjustment.rgb_offset",
        &color_adjustment.rgb_offset,
        MIN_RENDER_RGB_OFFSET,
        MAX_RENDER_RGB_OFFSET,
    )?;

    Ok(())
}

/// Проверяет `[frame_server]` как persisted schema, не как runtime resolver.
fn validate_frame_server_section(config: &AppConfig) -> ConfigResult<()> {
    validate_u16_range(
        "frame_server.live_scrub_max_hz",
        config.frame_server.live_scrub_max_hz,
        MIN_FRAME_SERVER_LIVE_SCRUB_MAX_HZ,
        MAX_FRAME_SERVER_LIVE_SCRUB_MAX_HZ,
    )?;

    Ok(())
}

/// Проверяет UI section.
fn validate_ui_section(config: &AppConfig) -> ConfigResult<()> {
    let language = config.ui.language.trim();
    if language.chars().count() < MIN_UI_LANGUAGE_LEN {
        return Err(invalid_value(
            "ui.language",
            "язык UI не должен быть пустым".to_string(),
        ));
    }

    if language.chars().count() > MAX_UI_LANGUAGE_LEN {
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

    validate_u16_range(
        "ui.settings.live_preview_max_hz",
        config.ui.settings.live_preview_max_hz,
        MIN_LIVE_PREVIEW_MAX_HZ,
        MAX_LIVE_PREVIEW_MAX_HZ,
    )?;

    validate_u16_range(
        "ui.animations.sidebar_slide_duration_ms",
        config.ui.animations.sidebar_slide_duration_ms,
        MIN_SIDEBAR_SLIDE_DURATION_MS,
        MAX_SIDEBAR_SLIDE_DURATION_MS,
    )?;

    validate_u16_range(
        "ui.window.titlebar_height_px",
        config.ui.window.titlebar_height_px,
        MIN_TITLEBAR_HEIGHT_PX,
        MAX_TITLEBAR_HEIGHT_PX,
    )?;

    Ok(())
}

/// Проверяет, что `u64` значение положительное.
fn validate_positive_u64(field: &'static str, value: u64) -> ConfigResult<()> {
    validate_u64_range(
        field,
        value,
        MIN_POSITIVE_U64_SETTING_VALUE,
        MAX_POSITIVE_U64_SETTING_VALUE,
    )
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

/// Проверяет `u16` диапазон с единым сообщением.
fn validate_u16_range(field: &'static str, value: u16, min: u16, max: u16) -> ConfigResult<()> {
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

/// Проверяет `f32` диапазон с защитой от NaN/Inf перед отправкой в shader uniforms.
fn validate_f32_range(field: &'static str, value: f32, min: f32, max: f32) -> ConfigResult<()> {
    if value.is_finite() && (min..=max).contains(&value) {
        return Ok(());
    }

    Err(invalid_value(
        field,
        format!("значение должно быть конечным числом в диапазоне {min}..={max}, получено {value}"),
    ))
}

/// Проверяет RGB-массив: ровно три конечных значения в порядке R, G, B.
fn validate_rgb_triplet(
    field: &'static str,
    values: &[f32],
    min: f32,
    max: f32,
) -> ConfigResult<()> {
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
        if !channel_value.is_finite() || !(min..=max).contains(&channel_value) {
            return Err(invalid_value(
                field,
                format!(
                    "RGB-канал #{channel_index} должен быть конечным числом в диапазоне {min}..={max}, получено {channel_value}"
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
