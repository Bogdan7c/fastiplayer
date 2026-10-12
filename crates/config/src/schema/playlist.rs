use serde::{Deserialize, Serialize};

/// Сколько файлов берём из брошенной папки по умолчанию.
pub const DEFAULT_DROPPED_FOLDER_MAX_FILES: u16 = 2_000;
/// Минимум файлов из брошенной папки (меньше смысла нет).
pub const MIN_DROPPED_FOLDER_MAX_FILES: u16 = 1;
/// Максимум файлов из брошенной папки: защита от случайного броска корня диска.
pub const MAX_DROPPED_FOLDER_MAX_FILES: u16 = 10_000;
/// Глубина подпапок по умолчанию (0 — только сама брошенная папка).
pub const DEFAULT_DROPPED_FOLDER_MAX_DEPTH: u16 = 8;
/// Минимальная глубина: только сама папка, без подпапок.
pub const MIN_DROPPED_FOLDER_MAX_DEPTH: u16 = 0;
/// Максимальная глубина подпапок.
pub const MAX_DROPPED_FOLDER_MAX_DEPTH: u16 = 32;

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

/// Что делать с файлом плейлиста (`.m3u`, `.m3u8`, `.xspf`, `.cue`), брошенным в окно.
///
/// Во всех вариантах пользователь видит обычный предпросмотр импорта; вариант
/// «новый плейлист» дополнительно проходит подтверждение замены непустой очереди.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DroppedPlaylistFileAction {
    /// По месту броска: на панель плейлиста — добавить в конец, на видео — заменить очередь.
    ByDropTarget,
    /// Всегда открывать как новый плейлист (заменить очередь), куда бы ни бросили.
    NewPlaylist,
    /// Всегда добавлять в конец текущей очереди, куда бы ни бросили.
    AppendToQueue,
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
        label_ru = "Preload next item",
        description_id = "settings.playlist.next_item_preload_enabled.description",
        description_ru = "Prepare the next item's source and demuxer ahead of time, keeping the current media and decoder until the transition.",
        help_id = "settings.playlist.next_item_preload_enabled.help",
        help_ru = "If preloading fails, is canceled, or becomes outdated, use normal safe opening after EOF.",
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
        label_ru = "Preload memory limit",
        description_id = "settings.playlist.next_item_preload_budget_mb.description",
        description_ru = "Total RAM cache and read-ahead limit for the next item; split equally for separate audio and video.",
        help_id = "settings.playlist.next_item_preload_budget_mb.help",
        help_ru = "This limit does not increase the regular playback cache or allow preparing more than one item.",
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
        label_ru = "Preload start window",
        description_id = "settings.playlist.next_item_preload_lead_time_ms.description",
        description_ru = "Preloading starts only when the known end of the current media is within this time window.",
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
        label_ru = "Prepared preload lifetime",
        description_id = "settings.playlist.next_item_preload_max_hold_ms.description",
        description_ru = "After this time, the prepared source is considered outdated and will not be used for the transition.",
        help_id = "settings.playlist.next_item_preload_max_hold_ms.help",
        help_ru = "This limit prevents holding resources too long or using expired temporary URLs during a pause or stalled transition.",
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
        label_ru = "Load neighboring files",
        description_id = "settings.playlist.load_siblings.description",
        description_ru = "Add suitable files from the same folder when explicitly opening local media.",
        help_id = "settings.playlist.load_siblings.help",
        help_ru = "Turning this off cancels the current search only after settings are saved successfully; turning it on does not start a search retroactively.",
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
        label_ru = "Neighboring file type",
        description_id = "settings.playlist.sibling_media_filter.description",
        description_ru = "This filter applies only to future opens and does not rebuild the current queue or search.",
        editor = "select",
        apply = "playlist.apply",
        options(
            option(id = "video_only", label_id = "settings.playlist.sibling_media_filter.video_only", label_ru = "Video only", value = PlaylistSiblingMediaFilter::VideoOnly),
            option(id = "all_media", label_id = "settings.playlist.sibling_media_filter.all_media", label_ru = "All media files", value = PlaylistSiblingMediaFilter::AllMedia),
            option(id = "audio_only", label_id = "settings.playlist.sibling_media_filter.audio_only", label_ru = "Audio only", value = PlaylistSiblingMediaFilter::AudioOnly),
            option(id = "same_as_opened", label_id = "settings.playlist.sibling_media_filter.same_as_opened", label_ru = "Same type as the opened file", value = PlaylistSiblingMediaFilter::SameAsOpened)
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
        label_ru = "Behavior after the last file",
        description_id = "settings.playlist.playback_behavior.description",
        description_ru = "Default for a new queue; changing this does not change an existing queue's repeat mode.",
        editor = "select",
        apply = "playlist.apply",
        options(
            option(id = "stop_after_last", label_id = "settings.playlist.playback_behavior.stop_after_last", label_ru = "Stop", value = PlaylistPlaybackBehavior::StopAfterLast),
            option(id = "repeat_queue", label_id = "settings.playlist.playback_behavior.repeat_queue", label_ru = "Repeat queue", value = PlaylistPlaybackBehavior::RepeatQueue),
            option(id = "repeat_one", label_id = "settings.playlist.playback_behavior.repeat_one", label_ru = "Repeat one file", value = PlaylistPlaybackBehavior::RepeatOne)
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
        label_ru = "Playback error",
        description_id = "settings.playlist.error_behavior.description",
        description_ru = "Stop the queue, or keep the error and try the next item.",
        editor = "select",
        apply = "playlist.apply",
        options(
            option(id = "stop", label_id = "settings.playlist.error_behavior.stop", label_ru = "Stop", value = PlaylistErrorBehavior::Stop),
            option(id = "skip", label_id = "settings.playlist.error_behavior.skip", label_ru = "Skip", value = PlaylistErrorBehavior::Skip)
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
        label_ru = "Queue save delay",
        description_id = "settings.playlist.state_save_debounce_ms.description",
        description_ru = "Quiet period after the last change before writing the newest revision to disk.",
        help_id = "settings.playlist.state_save_debounce_ms.help",
        help_ru = "A smaller value reduces potential loss after a crash but increases write frequency.",
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
        label_ru = "Position save interval",
        description_id = "settings.playlist.resume_checkpoint_interval_ms.description",
        description_ru = "How often to update the small current-position sidecar during playback.",
        help_id = "settings.playlist.resume_checkpoint_interval_ms.help",
        help_ru = "Pause, confirmed seek, Stop, end of media, and normal shutdown save the position immediately.",
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
        label_ru = "Previous restart threshold",
        description_id = "settings.playlist.previous_restart_threshold_ms.description",
        description_ru = "Above this time, Previous first restarts the current file; 0 disables restarting.",
        editor = "integer",
        min = crate::validation::MIN_PLAYLIST_PREVIOUS_RESTART_THRESHOLD_MS,
        max = crate::validation::MAX_PLAYLIST_PREVIOUS_RESTART_THRESHOLD_MS,
        step = 250,
        unit = "ms",
        apply = "playlist.apply"
    )]
    pub previous_restart_threshold_ms: u64,

    /// Максимум media-файлов, которые берутся из брошенной в окно папки (с подпапками).
    #[setting(
        id = "playlist.dropped_folder_max_files",
        path = "playlist.dropped_folder_max_files",
        section = "playlist",
        group = "dropped_items",
        surface = "main-settings-window",
        label_id = "settings.playlist.dropped_folder_max_files.label",
        label_ru = "Files from a dropped folder",
        description_id = "settings.playlist.dropped_folder_max_files.description",
        description_ru = "Maximum media files to take from a folder dropped into the window, including subfolders.",
        help_id = "settings.playlist.dropped_folder_max_files.help",
        help_ru = "If there are more files, take the first files in order and notify the user.",
        editor = "integer",
        min = crate::MIN_DROPPED_FOLDER_MAX_FILES,
        max = crate::MAX_DROPPED_FOLDER_MAX_FILES,
        step = 100,
        unit = "files",
        apply = "playlist.apply"
    )]
    pub dropped_folder_max_files: u16,

    /// Максимальная глубина вложенности подпапок при обходе брошенной папки.
    #[setting(
        id = "playlist.dropped_folder_max_depth",
        path = "playlist.dropped_folder_max_depth",
        section = "playlist",
        group = "dropped_items",
        surface = "main-settings-window",
        label_id = "settings.playlist.dropped_folder_max_depth.label",
        label_ru = "Subfolder depth",
        description_id = "settings.playlist.dropped_folder_max_depth.description",
        description_ru = "How many levels to descend into a dropped folder's subfolders; 0 means only the folder itself.",
        editor = "integer",
        min = crate::MIN_DROPPED_FOLDER_MAX_DEPTH,
        max = crate::MAX_DROPPED_FOLDER_MAX_DEPTH,
        step = 1,
        unit = "levels",
        apply = "playlist.apply"
    )]
    pub dropped_folder_max_depth: u16,

    /// Поведение при броске файла плейлиста в окно.
    #[setting(
        id = "playlist.dropped_playlist_file_action",
        path = "playlist.dropped_playlist_file_action",
        section = "playlist",
        group = "dropped_items",
        surface = "main-settings-window",
        label_id = "settings.playlist.dropped_playlist_file_action.label",
        label_ru = "Playlist file dropped into the window",
        description_id = "settings.playlist.dropped_playlist_file_action.description",
        description_ru = "What to do with a dropped M3U, XSPF, or CUE Playlist file; a preview is always shown before applying.",
        editor = "select",
        apply = "playlist.apply",
        options(
            option(id = "by_drop_target", label_id = "settings.playlist.dropped_playlist_file_action.by_drop_target", label_ru = "By drop location: append on panel, replace on video", value = DroppedPlaylistFileAction::ByDropTarget),
            option(id = "new_playlist", label_id = "settings.playlist.dropped_playlist_file_action.new_playlist", label_ru = "Always create a new Playlist", value = DroppedPlaylistFileAction::NewPlaylist),
            option(id = "append_to_queue", label_id = "settings.playlist.dropped_playlist_file_action.append_to_queue", label_ru = "Always append", value = DroppedPlaylistFileAction::AppendToQueue)
        )
    )]
    pub dropped_playlist_file_action: DroppedPlaylistFileAction,
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
            // Решение владельца (сессия 07): битый файл не останавливает очередь.
            error_behavior: PlaylistErrorBehavior::Skip,
            state_save_debounce_ms: 2_000,
            resume_checkpoint_interval_ms: 5_000,
            previous_restart_threshold_ms: 5_000,
            dropped_folder_max_files: DEFAULT_DROPPED_FOLDER_MAX_FILES,
            dropped_folder_max_depth: DEFAULT_DROPPED_FOLDER_MAX_DEPTH,
            dropped_playlist_file_action: DroppedPlaylistFileAction::ByDropTarget,
        }
    }
}
