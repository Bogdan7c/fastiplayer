use serde::{Deserialize, Serialize};

/// Фильтр соседних файлов, который фиксируется новым discovery job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistSiblingMediaFilter {
    /// Принимать только media с video track.
    VideoOnly,
    /// Принимать video и audio-only media.
    AllMedia,
    /// Принимать только audio-only media.
    AudioOnly,
    /// Принимать media того же topology-типа, что и явно открытый файл.
    SameAsOpened,
}

/// Default traversal для новой очереди без совместимого persisted state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistPlaybackBehavior {
    /// Остановиться после последнего элемента.
    StopAfterLast,
    /// После последнего элемента продолжить с начала очереди.
    RepeatQueue,
    /// Повторять текущий элемент.
    RepeatOne,
}

/// Политика automatic traversal после media-ошибки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistErrorBehavior {
    /// Остановить automatic traversal и показать ошибку.
    Stop,
    /// Сохранить ошибку и перейти к следующему допустимому элементу.
    Skip,
}

/// Настройки playlist policy, применяемые через отдельный runtime owner.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct PlaylistConfig {
    /// Разрешает bounded подготовку source/demux следующего элемента до clean EOF.
    #[setting(
        id = "playlist.next_item_preload_enabled",
        path = "playlist.next_item_preload_enabled",
        section = "playlist",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.playlist.next_item_preload_enabled.label",
        label_ru = "Предзагружать следующий элемент",
        description_id = "settings.playlist.next_item_preload_enabled.description",
        description_ru = "Заранее подготавливать source и demux следующего элемента, не меняя текущий media и decoder до перехода.",
        help_id = "settings.playlist.next_item_preload_enabled.help",
        help_ru = "При ошибке, отмене или устаревании предзагрузки используется обычное безопасное открытие после EOF.",
        editor = "toggle",
        apply = "playlist.apply"
    )]
    pub next_item_preload_enabled: bool,

    /// Общий RAM/read-ahead budget; split A/V делит его между components.
    #[setting(
        id = "playlist.next_item_preload_budget_mb",
        path = "playlist.next_item_preload_budget_mb",
        section = "playlist",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.playlist.next_item_preload_budget_mb.label",
        label_ru = "Лимит памяти предзагрузки",
        description_id = "settings.playlist.next_item_preload_budget_mb.description",
        description_ru = "Общий предел RAM-cache и read-ahead следующего элемента; для раздельных аудио и видео делится пополам.",
        help_id = "settings.playlist.next_item_preload_budget_mb.help",
        help_ru = "Лимит не увеличивает обычный playback cache и не разрешает подготавливать больше одного элемента.",
        editor = "integer",
        min = crate::validation::MIN_PLAYLIST_NEXT_ITEM_PRELOAD_BUDGET_MB,
        max = crate::validation::MAX_PLAYLIST_NEXT_ITEM_PRELOAD_BUDGET_MB,
        step = 16,
        unit = "MiB",
        apply = "playlist.apply"
    )]
    pub next_item_preload_budget_mb: u64,

    /// За сколько media-time до известного EOF разрешается speculative preparation.
    #[setting(
        id = "playlist.next_item_preload_lead_time_ms",
        path = "playlist.next_item_preload_lead_time_ms",
        section = "playlist",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.playlist.next_item_preload_lead_time_ms.label",
        label_ru = "Окно запуска предзагрузки",
        description_id = "settings.playlist.next_item_preload_lead_time_ms.description",
        description_ru = "Предзагрузка начинается только когда до известного конца текущего media осталось не больше этого времени.",
        editor = "integer",
        min = crate::validation::MIN_PLAYLIST_NEXT_ITEM_PRELOAD_LEAD_TIME_MS,
        max = crate::validation::MAX_PLAYLIST_NEXT_ITEM_PRELOAD_LEAD_TIME_MS,
        step = 5_000,
        unit = "ms",
        apply = "playlist.apply"
    )]
    pub next_item_preload_lead_time_ms: u64,

    /// Максимальное wall-clock время удержания prepared source до authoritative EOF.
    #[setting(
        id = "playlist.next_item_preload_max_hold_ms",
        path = "playlist.next_item_preload_max_hold_ms",
        section = "playlist",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.playlist.next_item_preload_max_hold_ms.label",
        label_ru = "Срок готовой предзагрузки",
        description_id = "settings.playlist.next_item_preload_max_hold_ms.description",
        description_ru = "После этого срока подготовленный source считается устаревшим и не используется для перехода.",
        help_id = "settings.playlist.next_item_preload_max_hold_ms.help",
        help_ru = "Ограничение защищает от долгого удержания ресурсов и истёкших временных URL при паузе или зависшем переходе.",
        editor = "integer",
        min = crate::validation::MIN_PLAYLIST_NEXT_ITEM_PRELOAD_MAX_HOLD_MS,
        max = crate::validation::MAX_PLAYLIST_NEXT_ITEM_PRELOAD_MAX_HOLD_MS,
        step = 10_000,
        unit = "ms",
        apply = "playlist.apply"
    )]
    pub next_item_preload_max_hold_ms: u64,

    /// Запускать sibling discovery для следующих explicit local opens.
    #[setting(
        id = "playlist.load_siblings",
        path = "playlist.load_siblings",
        section = "playlist",
        group = "discovery",
        surface = "main-settings-window",
        label_id = "settings.playlist.load_siblings.label",
        label_ru = "Загружать соседние файлы",
        description_id = "settings.playlist.load_siblings.description",
        description_ru = "Добавлять подходящие файлы из той же папки при следующих явных открытиях local media.",
        help_id = "settings.playlist.load_siblings.help",
        help_ru = "Выключение отменяет текущий поиск только после успешного сохранения настроек; включение не запускает поиск задним числом.",
        editor = "toggle",
        apply = "playlist.apply"
    )]
    pub load_siblings: bool,

    /// Фильтр media topology для будущих sibling discovery jobs.
    #[setting(
        id = "playlist.sibling_media_filter",
        path = "playlist.sibling_media_filter",
        section = "playlist",
        group = "discovery",
        surface = "main-settings-window",
        label_id = "settings.playlist.sibling_media_filter.label",
        label_ru = "Тип соседних файлов",
        description_id = "settings.playlist.sibling_media_filter.description",
        description_ru = "Фильтр применяется только к следующим открытиям и не перестраивает текущую очередь или поиск.",
        editor = "select",
        apply = "playlist.apply",
        options(
            option(id = "video_only", label_id = "settings.playlist.sibling_media_filter.video_only", label_ru = "Только видео", value = PlaylistSiblingMediaFilter::VideoOnly),
            option(id = "all_media", label_id = "settings.playlist.sibling_media_filter.all_media", label_ru = "Все медиафайлы", value = PlaylistSiblingMediaFilter::AllMedia),
            option(id = "audio_only", label_id = "settings.playlist.sibling_media_filter.audio_only", label_ru = "Только аудио", value = PlaylistSiblingMediaFilter::AudioOnly),
            option(id = "same_as_opened", label_id = "settings.playlist.sibling_media_filter.same_as_opened", label_ru = "Того же типа, что открытый файл", value = PlaylistSiblingMediaFilter::SameAsOpened)
        )
    )]
    pub sibling_media_filter: PlaylistSiblingMediaFilter,

    /// Repeat policy только для новой очереди без совместимого persisted state.
    #[setting(
        id = "playlist.playback_behavior",
        path = "playlist.playback_behavior",
        section = "playlist",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.playlist.playback_behavior.label",
        label_ru = "Поведение после последнего файла",
        description_id = "settings.playlist.playback_behavior.description",
        description_ru = "Default для новой очереди; изменение не меняет repeat-режим уже существующей очереди.",
        editor = "select",
        apply = "playlist.apply",
        options(
            option(id = "stop_after_last", label_id = "settings.playlist.playback_behavior.stop_after_last", label_ru = "Остановиться", value = PlaylistPlaybackBehavior::StopAfterLast),
            option(id = "repeat_queue", label_id = "settings.playlist.playback_behavior.repeat_queue", label_ru = "Повторять очередь", value = PlaylistPlaybackBehavior::RepeatQueue),
            option(id = "repeat_one", label_id = "settings.playlist.playback_behavior.repeat_one", label_ru = "Повторять один файл", value = PlaylistPlaybackBehavior::RepeatOne)
        )
    )]
    pub playback_behavior: PlaylistPlaybackBehavior,

    /// Поведение automatic traversal после media-ошибки.
    #[setting(
        id = "playlist.error_behavior",
        path = "playlist.error_behavior",
        section = "playlist",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.playlist.error_behavior.label",
        label_ru = "Ошибка воспроизведения",
        description_id = "settings.playlist.error_behavior.description",
        description_ru = "Остановить очередь или сохранить ошибку и попробовать следующий элемент.",
        editor = "select",
        apply = "playlist.apply",
        options(
            option(id = "stop", label_id = "settings.playlist.error_behavior.stop", label_ru = "Остановиться", value = PlaylistErrorBehavior::Stop),
            option(id = "skip", label_id = "settings.playlist.error_behavior.skip", label_ru = "Пропустить", value = PlaylistErrorBehavior::Skip)
        )
    )]
    pub error_behavior: PlaylistErrorBehavior,

    /// Quiet period перед записью newest dirty playlist revision.
    #[setting(
        id = "playlist.state_save_debounce_ms",
        path = "playlist.state_save_debounce_ms",
        section = "playlist",
        group = "persistence",
        surface = "main-settings-window",
        label_id = "settings.playlist.state_save_debounce_ms.label",
        label_ru = "Задержка сохранения очереди",
        description_id = "settings.playlist.state_save_debounce_ms.description",
        description_ru = "Пауза после последнего изменения перед записью newest revision на диск.",
        help_id = "settings.playlist.state_save_debounce_ms.help",
        help_ru = "Меньшее значение сокращает окно потери при аварийном завершении, но увеличивает частоту записей.",
        editor = "integer",
        min = crate::validation::MIN_PLAYLIST_STATE_SAVE_DEBOUNCE_MS,
        max = crate::validation::MAX_PLAYLIST_STATE_SAVE_DEBOUNCE_MS,
        step = 250,
        unit = "ms",
        apply = "playlist.apply"
    )]
    pub state_save_debounce_ms: u64,

    /// Интервал periodic checkpoint-а подтверждённой media position.
    #[setting(
        id = "playlist.resume_checkpoint_interval_ms",
        path = "playlist.resume_checkpoint_interval_ms",
        section = "playlist",
        group = "persistence",
        surface = "main-settings-window",
        label_id = "settings.playlist.resume_checkpoint_interval_ms.label",
        label_ru = "Интервал запоминания позиции",
        description_id = "settings.playlist.resume_checkpoint_interval_ms.description",
        description_ru = "Как часто во время воспроизведения обновлять маленький sidecar текущей позиции.",
        help_id = "settings.playlist.resume_checkpoint_interval_ms.help",
        help_ru = "Пауза, подтверждённый seek, остановка, конец media и штатное завершение сохраняются сразу.",
        editor = "integer",
        min = crate::validation::MIN_PLAYLIST_RESUME_CHECKPOINT_INTERVAL_MS,
        max = crate::validation::MAX_PLAYLIST_RESUME_CHECKPOINT_INTERVAL_MS,
        step = 1000,
        unit = "ms",
        apply = "playlist.apply"
    )]
    pub resume_checkpoint_interval_ms: u64,

    /// Порог restart-current для команды Previous.
    #[setting(
        id = "playlist.previous_restart_threshold_ms",
        path = "playlist.previous_restart_threshold_ms",
        section = "playlist",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.playlist.previous_restart_threshold_ms.label",
        label_ru = "Порог перезапуска для «Предыдущий»",
        description_id = "settings.playlist.previous_restart_threshold_ms.description",
        description_ru = "Выше этого времени Previous сначала перезапускает текущий файл; 0 отключает перезапуск.",
        editor = "integer",
        min = crate::validation::MIN_PLAYLIST_PREVIOUS_RESTART_THRESHOLD_MS,
        max = crate::validation::MAX_PLAYLIST_PREVIOUS_RESTART_THRESHOLD_MS,
        step = 250,
        unit = "ms",
        apply = "playlist.apply"
    )]
    pub previous_restart_threshold_ms: u64,
}

impl Default for PlaylistConfig {
    fn default() -> Self {
        Self {
            next_item_preload_enabled: true,
            next_item_preload_budget_mb: 64,
            next_item_preload_lead_time_ms: 30_000,
            next_item_preload_max_hold_ms: 120_000,
            load_siblings: true,
            sibling_media_filter: PlaylistSiblingMediaFilter::SameAsOpened,
            playback_behavior: PlaylistPlaybackBehavior::StopAfterLast,
            error_behavior: PlaylistErrorBehavior::Stop,
            state_save_debounce_ms: 2_000,
            resume_checkpoint_interval_ms: 5_000,
            previous_restart_threshold_ms: 5_000,
        }
    }
}
