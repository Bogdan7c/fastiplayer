/// Добавляет русские комментарии к полям schema version 7 в default TOML.
pub(super) fn document_current_schema_defaults(toml_text: &mut String) {
    insert_default_config_comment(
        toml_text,
        "[playlist]",
        "# Playlist policy: discovery следующих открытий, traversal defaults и state save timing.",
    );
    insert_default_config_comment(
        toml_text,
        "load_siblings = true",
        "# Автоматически искать соседние media при следующих explicit local opens.",
    );
    insert_default_config_comment(
        toml_text,
        "sibling_media_filter = \"same_as_opened\"",
        "# Topology-фильтр только для будущих sibling discovery jobs.",
    );
    insert_default_config_comment(
        toml_text,
        "playback_behavior = \"stop_after_last\"",
        "# Repeat default только для новой очереди без совместимого persisted state.",
    );
    insert_default_config_comment(
        toml_text,
        "error_behavior = \"stop\"",
        "# Automatic traversal после media-ошибки: остановиться или пропустить.",
    );
    insert_default_config_comment(
        toml_text,
        "state_save_debounce_ms = 2000",
        "# Quiet period перед сохранением newest dirty playlist revision.",
    );
    insert_default_config_comment(
        toml_text,
        "resume_checkpoint_interval_ms = 5000",
        "# Интервал periodic checkpoint-а позиции; lifecycle boundaries сохраняются сразу.",
    );
    insert_default_config_comment(
        toml_text,
        "previous_restart_threshold_ms = 5000",
        "# Порог restart-current для Previous; 0 отключает эту ветку.",
    );
    insert_default_config_comment(
        toml_text,
        "[player.seek]",
        "# Настройки seek commit, resume после seek и hotkey-шагов.",
    );
    insert_default_config_comment(
        toml_text,
        "commit_timeout_ms = 10000",
        "# Timeout финального seek/scrub commit-а.",
    );
    insert_default_config_comment(
        toml_text,
        "resume_audio_min_buffer_ms = 50",
        "# Минимальный audio buffer перед resume после commit-а.",
    );
    insert_default_config_comment(
        toml_text,
        "resume_audio_gate_timeout_ms = 250",
        "# Soft timeout audio gate-а после показанного target video frame.",
    );
    insert_default_config_comment(
        toml_text,
        "resume_video_min_ready_frames = 3",
        "# Минимальный запас готовых video frames перед resume после commit-а.",
    );
    insert_default_config_comment(
        toml_text,
        "fast_preroll_budget_ms = 48",
        "# Bounded окно worker work для accurate seek decode-preroll до target frame.",
    );
    insert_default_config_comment(
        toml_text,
        "fast_preroll_video_packet_burst = 512",
        "# Burst-лимит video packets/frames для accurate seek GOP preroll.",
    );
    insert_default_config_comment(
        toml_text,
        "paused_commit_behavior = \"stay_paused\"",
        "# Поведение seek commit-а, начатого из paused состояния.",
    );
    insert_default_config_comment(
        toml_text,
        "hotkey_small_step_secs = 5",
        "# Малый шаг seek hotkey в секундах.",
    );
    insert_default_config_comment(
        toml_text,
        "hotkey_large_step_secs = 30",
        "# Большой шаг seek hotkey в секундах.",
    );
    insert_default_config_comment(
        toml_text,
        "[player.demux]",
        "# Fail-safe настройки demuxer-а.",
    );
    insert_default_config_comment(
        toml_text,
        "max_consecutive_corrupted_packets = 64",
        "# Сколько corrupted packets подряд можно пропустить до fatal ошибки.",
    );
    insert_default_config_comment(
        toml_text,
        "[network]",
        "# Настройки будущего source/network cache слоя.",
    );
    insert_default_config_comment(
        toml_text,
        "memory_cache_mb = 128",
        "# RAM cache budget; 0 явно отключает RAM cache.",
    );
    insert_default_config_comment(
        toml_text,
        "read_ahead_mb = 256",
        "# RAM window, которое prefetch держит впереди foreground cursor-а.",
    );
    insert_default_config_comment(
        toml_text,
        "prefetch_chunk_mb = 8",
        "# Максимальный размер одного фонового prefetch-чтения.",
    );
    insert_default_config_comment(
        toml_text,
        "prefetch_initial_chunk_kb = 64",
        "# Размер ПЕРВОГО prefetch-чтения (slow-start), КиБ.",
    );
    insert_default_config_comment(
        toml_text,
        "connect_timeout_ms = 15000",
        "# Timeout подключения к сетевому источнику.",
    );
    insert_default_config_comment(
        toml_text,
        "read_timeout_ms = 15000",
        "# Timeout чтения из сетевого источника.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_packet_channel_frames = 32",
        "# Bounded очередь packets между worker и decoder thread.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_frame_channel_frames = 8",
        "# Bounded очередь decoded frames между decoder thread и worker.",
    );
    insert_default_config_comment(
        toml_text,
        "[frame_server]",
        "# Настройки Frame Server, которые остаются нужны для live scrub и будущего playback rate.",
    );
    insert_default_config_comment(
        toml_text,
        "live_scrub_enabled = true",
        "# Включает live drag preview updates; точный seek по click/release остаётся активным.",
    );
    insert_default_config_comment(
        toml_text,
        "live_scrub_decode_mode = \"throttled_latest\"",
        "# Политика live scrub: throttled_latest или every_drag_event, оба latest-only.",
    );
    insert_default_config_comment(
        toml_text,
        "live_scrub_max_hz = 60",
        "# Максимальная частота live scrub decode-work для throttled_latest; допустимо 1..=240.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_ready_queue_frames = 8",
        "# Backend-local ready queue для burst FrameReady events.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_surface_pool_frames = 24",
        "# VA output surface descriptors для hardware decoder-а.",
    );
    insert_default_config_comment(
        toml_text,
        "sw_decoder_surface_pool_frames = 8",
        "# Сколько software (FFmpeg host RAM) кадров держать одновременно; меньше = стабильнее FPS на 4K.",
    );
    insert_default_config_comment(
        toml_text,
        "sw_decode_threads = 0",
        "# Потоки software-декода; 0 = авто (ядра − 2, чтобы render-поток не голодал).",
    );
    insert_default_config_comment(
        toml_text,
        "zero_copy_surface_pool_slots = 24",
        "# Zero-copy external import slots; CPU fallback всё равно запрещён.",
    );
    insert_default_config_comment(
        toml_text,
        "[video.scheduler]",
        "# Настройки worker scheduler-а для bounded catch-up после latency spike.",
    );
    insert_default_config_comment(
        toml_text,
        "demux_packets_per_tick = 12",
        "# Базовый budget чтения container packets за один worker tick.",
    );
    insert_default_config_comment(
        toml_text,
        "video_packets_per_tick = 8",
        "# Базовый budget отправки video packets в decoder thread за tick.",
    );
    insert_default_config_comment(
        toml_text,
        "decoded_frames_per_tick = 8",
        "# Базовый budget приёма decoded frames из decoder thread за tick.",
    );
    insert_default_config_comment(
        toml_text,
        "catch_up_budget_ms = 4",
        "# Дополнительное bounded окно catch-up work после обычного tick.",
    );
    insert_default_config_comment(
        toml_text,
        "present_queue_min_frames = 2",
        "# Минимальный запас ready frames, ниже которого diagnostics считает starvation.",
    );
    insert_default_config_comment(
        toml_text,
        "present_queue_target_frames = 4",
        "# Целевой запас ready frames; максимум задаёт video.present_queue_frames.",
    );
    insert_default_config_comment(
        toml_text,
        "decode_ahead_target_ms = 250",
        "# Целевой video decode-ahead; максимум задаёт video.max_decode_ahead_ms.",
    );
    insert_default_config_comment(
        toml_text,
        "surface_free_slots_min = 2",
        "# Минимальный резерв свободных zero-copy surface/import slots перед decode.",
    );
    insert_default_config_comment(
        toml_text,
        "surface_free_slots_target = 4",
        "# Целевой резерв surface/import slots для adaptive catch-up.",
    );
    insert_default_config_comment(
        toml_text,
        "[yt_dlp]",
        "# Настройки generic yt-dlp service adapter-а.",
    );
    insert_default_config_comment(
        toml_text,
        "hdr_selection = \"sdr_only\"",
        "# Политика yt-dlp dynamic range: только SDR или HDR с автоматическим SDR fallback.",
    );
    insert_default_config_comment(
        toml_text,
        "resolve_timeout_ms = 30000",
        "# Timeout подготовки metadata через системный yt-dlp.",
    );
    insert_default_config_comment(
        toml_text,
        "skin = \"minimal\"",
        "# UI skin id; unknown id является config error.",
    );
    insert_default_config_comment(
        toml_text,
        "[ui.window]",
        "# Настройки кастомного заголовка окна Rustiplayer.",
    );
    insert_default_config_comment(
        toml_text,
        "titlebar_height_px = 40",
        "# Высота кастомного titlebar в логических UI px.",
    );
    insert_default_config_comment(
        toml_text,
        "[ui.sidebar]",
        "# Геометрия общей панели Playlist/Settings/URL/Info.",
    );
    insert_default_config_comment(
        toml_text,
        "width_points = 420",
        "# Запоминаемая ширина полностью открытого sidebar в логических egui points.",
    );
    insert_default_config_comment(
        toml_text,
        "[ui.settings]",
        "# Настройки поведения окна Settings UI.",
    );
    insert_default_config_comment(
        toml_text,
        "live_preview_max_hz = 60",
        "# Максимальная частота live preview updates в Settings UI.",
    );
    insert_default_config_comment(toml_text, "[ui.animations]", "# Настройки UI-анимаций.");
    insert_default_config_comment(
        toml_text,
        "reduced_motion = true",
        "# Убирает пространственное движение и масштабирование; короткие переходы цвета сохраняются.",
    );
    insert_default_config_comment(
        toml_text,
        "sidebar_slide_duration_ms = 500",
        "# Длительность выезда settings sidebar и сжатия видео, мс; 0 отключает анимацию.",
    );
}

fn insert_default_config_comment(toml_text: &mut String, needle: &str, comment: &str) {
    if toml_text.contains(comment) {
        return;
    }

    let documented_line = if needle.starts_with('[') {
        toml_text.lines().find(|line| *line == needle)
    } else {
        let field_name = needle
            .split_once(" = ")
            .map_or(needle, |(field_name, _)| field_name);
        toml_text.lines().find(|line| {
            line.starts_with(field_name) && line[field_name.len()..].starts_with(" = ")
        })
    };

    // Target ищется в TOML, только что сгенерированном из актуального `AppConfig`.
    // Поэтому тесты обнаруживают stale documentation при удалении/переименовании поля,
    // но пользовательское значение не обязано совпадать с default literal.
    let documented_line = documented_line.unwrap_or_else(|| {
        panic!("default config documentation target отсутствует в current schema: {needle}")
    });
    *toml_text = toml_text.replacen(documented_line, &format!("{comment}\n{documented_line}"), 1);
}
