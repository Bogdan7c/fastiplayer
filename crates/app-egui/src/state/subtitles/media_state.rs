//! Всё, что владелец знает о субтитрах ОДНОГО media: дорожки, выбор, сдвиг и фоновые задачи.
//!
//! Создаётся в `begin_media` и целиком отбрасывается при смене media: именно поэтому
//! выбор, список дорожек и сдвиг прежнего media не могут «просочиться» в новое.

use subtitle_core::{SubtitleSyncOffset, SubtitleTrackId};

use player_core::MediaInstanceId;

use super::intents::SubtitleVisibility;
use super::job_runner::SidecarJob;
use super::owner_memory::{MemoryDirty, PendingRememberedChoice};
use super::sidecar_job::SubtitleLoadFailure;
use super::track_list::SubtitleTrackList;

/// Поиск файлов рядом с видео.
pub(super) enum DiscoveryState {
    /// Поиск фоновой задачей ещё идёт.
    Searching(SidecarJob),
    /// Поиск завершён (кандидатов могло не быть) либо не применим (не локальный источник).
    Finished,
    /// Поиск не удался; воспроизведение не затронуто. Причину владелец уже записал в лог
    /// (`warn!` при приёме результата), UI она не нужна: у видео просто нет дорожек. Поле
    /// читают только тесты.
    Failed(
        #[cfg_attr(
            not(test),
            expect(dead_code, reason = "в production причина уже записана в лог")
        )]
        SubtitleLoadFailure,
    ),
}

/// Фоновая загрузка одной дорожки.
pub(super) struct TrackLoadJob {
    /// Для какой дорожки идёт загрузка.
    pub(super) track_id: SubtitleTrackId,
    /// Сама задача (владеет потоком).
    pub(super) job: SidecarJob,
}

/// Субтитры текущего media.
pub(super) struct MediaSubtitles {
    /// Media, которому принадлежит всё состояние.
    pub(super) media_instance_id: MediaInstanceId,
    /// Состояние поиска файлов рядом с видео.
    pub(super) discovery: DiscoveryState,
    /// Дорожки (найденные рядом и добавленные пользователем).
    pub(super) tracks: SubtitleTrackList,
    /// Выбранная дорожка (автовыбором или пользователем); показывается при `Shown`.
    pub(super) selected: Option<SubtitleTrackId>,
    /// Принудительная дорожка нужного языка: показывается при `Hidden`.
    pub(super) forced_fallback: Option<SubtitleTrackId>,
    /// Включены ли субтитры.
    pub(super) visibility: SubtitleVisibility,
    /// Общий сдвиг синхронизации всех дорожек media.
    pub(super) sync_offset: SubtitleSyncOffset,
    /// Пользователь сам выбрал дорожку (или добавил файл): поздний результат поиска не
    /// имеет права переопределить выбор автовыбором.
    pub(super) user_chose_track: bool,
    /// Идущая загрузка выбранной (или принудительной) дорожки. Одновременно не больше одной.
    pub(super) load_job: Option<TrackLoadJob>,
    /// Выбор из памяти «для этого видео», который ещё не нашёл свою дорожку (дорожки приходят
    /// позже: файлы — из фонового поиска, встроенные — из снимка плеера). Сбрасывается, когда
    /// дорожка найдена, когда её нет, и при любом действии пользователя с субтитрами.
    pub(super) pending_memory: Option<PendingRememberedChoice>,
    /// Что пользователь изменил с последней выдачи в память выбора.
    pub(super) memory_dirty: MemoryDirty,
}

impl MediaSubtitles {
    /// Пустое состояние нового media.
    pub(super) fn new(media_instance_id: MediaInstanceId, visibility: SubtitleVisibility) -> Self {
        Self {
            media_instance_id,
            discovery: DiscoveryState::Finished,
            tracks: SubtitleTrackList::new(),
            selected: None,
            forced_fallback: None,
            visibility,
            sync_offset: SubtitleSyncOffset::ZERO,
            user_chose_track: false,
            load_job: None,
            pending_memory: None,
            memory_dirty: MemoryDirty::default(),
        }
    }

    /// Какую дорожку нужно показывать: выбранную при `Shown`, принудительную при `Hidden`.
    pub(super) fn display_track_id(&self) -> Option<SubtitleTrackId> {
        match self.visibility {
            SubtitleVisibility::Shown => self.selected,
            SubtitleVisibility::Hidden => self.forced_fallback,
        }
    }

    /// Есть ли работающие фоновые задачи (нужны ли ещё wake-события).
    pub(super) fn has_running_jobs(&self) -> bool {
        matches!(self.discovery, DiscoveryState::Searching(_)) || self.load_job.is_some()
    }
}
