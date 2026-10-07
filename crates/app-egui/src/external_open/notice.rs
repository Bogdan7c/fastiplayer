//! Тексты уведомлений внешнего открытия (всё по-русски, без путей и технического жаргона).

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
}

impl DropNotice {
    /// Текст для показа в toast.
    pub(crate) fn text(self) -> String {
        match self {
            Self::PlaylistsSeparately => "Плейлисты перетаскивайте отдельно".to_owned(),
            Self::NothingToOpen => "Здесь нечего открывать".to_owned(),
            Self::FilesNotFound { count: 1 } => "Файл не найден".to_owned(),
            Self::FilesNotFound { count } => format!("Не найдено файлов: {count}"),
            Self::UnsupportedLink => "Такие ссылки не поддерживаются".to_owned(),
            Self::PlaylistsOneAtATime => "Плейлисты импортируются по одному".to_owned(),
            Self::FolderHasNoMedia => "В папке нет медиафайлов".to_owned(),
            Self::FolderUnreadable => "Не удалось прочитать папку".to_owned(),
            Self::FolderFileLimitReached { taken } => {
                format!("Добавлены первые {taken} файлов из папки")
            }
            Self::FolderDepthLimitReached => {
                "Часть вложенных папок пропущена: они расположены слишком глубоко".to_owned()
            }
            Self::LinksOneAtATime => "Ссылки добавляются по одной".to_owned(),
            Self::LinksSeparately => "Ссылки перетаскивайте отдельно".to_owned(),
            Self::TooManyFiles => "Слишком много файлов для одного плейлиста".to_owned(),
            Self::PlaylistStillLoading => {
                "Плейлист ещё загружается, попробуйте через секунду".to_owned()
            }
        }
    }
}
