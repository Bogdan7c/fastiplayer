//! Всё, что владелец знает о субтитрах ОДНОГО media: дорожки, выбор, сдвиг и фоновые задачи.
//!
//! Создаётся в `begin_media` и целиком отбрасывается при смене media: именно поэтому
//! выбор, список дорожек и сдвиг прежнего media не могут «просочиться» в новое.

use std::sync::Arc;

use subtitle_core::{SubtitleSyncOffset, SubtitleTrackId};

use player_core::MediaInstanceId;

use super::active_track::ActiveSubtitleTrack;
use super::intents::SubtitleVisibility;
use super::job_runner::SidecarJob;
use super::owner_embedded::{EmbeddedAssTracks, EmbeddedPgsTracks};
use super::owner_memory::{MemoryDirty, PendingRememberedChoice};
use super::owner_stream::StreamTracks;
use super::sidecar_job::SubtitleLoadFailure;
use super::track_list::{SubtitleTrackList, TrackLoadState};
use super::user_file_failures::UserRequestedFiles;
use super::web_download::WebSubtitleDownloader;

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

/// Какие реплики показываемой дорожки идут на экран.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DisplayMode {
    /// Все реплики дорожки.
    AllCues,
    /// Только картинки с флагом forced (PGS при выключенных субтитрах, как в проигрывателях
    /// Blu-ray, решение владельца сессии 11).
    ForcedPicturesOnly,
}

/// Что показывать: дорожка и режим.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DisplayChoice {
    /// Показываемая дорожка списка.
    pub(super) track_id: SubtitleTrackId,
    /// Все реплики или только forced-картинки.
    pub(super) mode: DisplayMode,
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
    /// PGS-дорожка нужного языка, чьи forced-ОБЪЕКТЫ показываются при `Hidden`, когда
    /// принудительной дорожки (`forced_fallback`) нет.
    pub(super) forced_pictures_fallback: Option<SubtitleTrackId>,
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
    /// ASS-заголовки и сценарии встроенных дорожек этого media (отбрасываются вместе с ним).
    pub(super) embedded_ass: EmbeddedAssTracks,
    /// Декодеры, хранилища композиций и роли PGS-дорожек этого media.
    pub(super) embedded_pgs: EmbeddedPgsTracks,
    /// Кто скачивает web-дорожки этого media (`None` — у media web-дорожек нет).
    pub(super) web_downloader: Option<Arc<dyn WebSubtitleDownloader>>,
    /// Источник и лента потоковых дорожек (HLS-рендиции) этого media.
    pub(super) streams: StreamTracks,
    /// Файлы, которые пользователь добавил сам, и их ещё не показанные отказы (для toast).
    pub(super) user_files: UserRequestedFiles,
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
            forced_pictures_fallback: None,
            visibility,
            sync_offset: SubtitleSyncOffset::ZERO,
            user_chose_track: false,
            load_job: None,
            pending_memory: None,
            memory_dirty: MemoryDirty::default(),
            embedded_ass: EmbeddedAssTracks::default(),
            embedded_pgs: EmbeddedPgsTracks::default(),
            web_downloader: None,
            streams: StreamTracks::default(),
            user_files: UserRequestedFiles::default(),
        }
    }

    /// Что нужно показывать: выбранную дорожку целиком при `Shown`; при `Hidden` - принудительную
    /// дорожку, а если её нет - forced-объекты PGS-дорожки нужного языка.
    pub(super) fn display_choice(&self) -> Option<DisplayChoice> {
        let (track_id, mode) = match self.visibility {
            SubtitleVisibility::Shown => (self.selected?, DisplayMode::AllCues),
            SubtitleVisibility::Hidden => match self.forced_fallback {
                Some(track_id) => (track_id, DisplayMode::AllCues),
                None => (
                    self.forced_pictures_fallback?,
                    DisplayMode::ForcedPicturesOnly,
                ),
            },
        };
        Some(DisplayChoice { track_id, mode })
    }

    /// Какую дорожку нужно показывать (режим не важен: загрузка, роли памяти, меню).
    pub(super) fn display_track_id(&self) -> Option<SubtitleTrackId> {
        self.display_choice().map(|choice| choice.track_id)
    }

    /// Готовая дорожка для показа: сама дорожка списка либо её вид «только forced-картинки».
    /// `None` - показывать нечего или дорожка ещё не готова.
    pub(super) fn displayed_track(&self) -> Option<&ActiveSubtitleTrack> {
        let choice = self.display_choice()?;
        let TrackLoadState::Ready(track) = &self.tracks.get(choice.track_id)?.load else {
            return None;
        };
        match choice.mode {
            DisplayMode::AllCues => Some(track),
            DisplayMode::ForcedPicturesOnly => {
                self.embedded_pgs.forced_pictures_view(choice.track_id)
            }
        }
    }

    /// Есть ли работающие фоновые задачи (нужны ли ещё wake-события).
    pub(super) fn has_running_jobs(&self) -> bool {
        matches!(self.discovery, DiscoveryState::Searching(_)) || self.load_job.is_some()
    }
}
