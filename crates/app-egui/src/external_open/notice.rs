//! Тексты уведомлений внешнего открытия (всё по-английски, без путей и технического жаргона).

/// Короткое сообщение пользователю о том, что сделано (или не сделано) с броском.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropNotice {
    /// Плейлист принесли вместе с другими файлами: плейлисты пропущены.
    PlaylistsSeparately,
    /// В броске не оказалось ничего, что можно открыть.
    NothingToOpen,
    /// Часть путей уже не существует.
    FilesNotFound {
        /// Сколько путей не найдено.
        count: usize,
    },
    /// Ссылка или адрес неподдерживаемого вида.
    UnsupportedLink,
    /// Принесено несколько ссылок: добавляется только первая.
    LinksOneAtATime,
    /// Ссылки принесли вместе с файлами или папками: ссылки пропущены.
    LinksSeparately,
    /// Принесено несколько плейлистов: импортируется только первый.
    PlaylistsOneAtATime,
    /// В брошенной папке (с подпапками) нет медиафайлов.
    FolderHasNoMedia,
    /// Папку не удалось прочитать.
    FolderUnreadable,
    /// Взяты только первые файлы: сработал лимит числа файлов.
    FolderFileLimitReached {
        /// Сколько файлов взято.
        taken: usize,
    },
    /// Часть вложенных папок пропущена: слишком глубокая вложенность.
    FolderDepthLimitReached,
    /// Слишком много файлов для одной очереди.
    TooManyFiles,
    /// Плейлист ещё загружается и не готов принять файлы.
    PlaylistStillLoading,
    /// Субтитры принесли вместе с файлами на панель плейлиста: прикрепить не к чему.
    SubtitlesNeedPlayingVideo,
}

impl DropNotice {
    /// Текст для показа в toast.
    pub(crate) fn text(self) -> String {
        match self {
            Self::PlaylistsSeparately => "Drag Playlists separately".to_owned(),
            Self::NothingToOpen => "There is nothing to open here".to_owned(),
            Self::FilesNotFound { count: 1 } => "File not found".to_owned(),
            Self::FilesNotFound { count } => format!("Files not found: {count}"),
            Self::UnsupportedLink => "These links are unsupported".to_owned(),
            Self::PlaylistsOneAtATime => "Import Playlists one at a time".to_owned(),
            Self::FolderHasNoMedia => "The folder contains no media files".to_owned(),
            Self::FolderUnreadable => "Could not read the folder".to_owned(),
            Self::FolderFileLimitReached { taken } => {
                format!("Added the first {taken} files from the folder")
            }
            Self::FolderDepthLimitReached => {
                "Some nested folders were skipped because they are too deep".to_owned()
            }
            Self::LinksOneAtATime => "Add links one at a time".to_owned(),
            Self::LinksSeparately => "Drag links separately".to_owned(),
            Self::TooManyFiles => "Too many files for one Playlist".to_owned(),
            Self::PlaylistStillLoading => {
                "Playlist is still loading; try again in a second".to_owned()
            }
            Self::SubtitlesNeedPlayingVideo => "Drop subtitles onto a playing video.".to_owned(),
        }
    }
}
