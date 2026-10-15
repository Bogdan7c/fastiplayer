//! Типы намерений и ответов API субтитров, видимые остальному приложению (UI, хоткеи,
//! внешнее открытие). Здесь только маленькие явные enum-ы: никаких позиционных `bool`.

/// Показаны ли субтитры. Это и есть предпочтение «были включены», которое запоминается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtitleVisibility {
    /// Субтитры включены: показывается выбранная дорожка.
    Shown,
    /// Субтитры выключены (принудительная forced-дорожка нужного языка всё равно видна).
    Hidden,
}

impl SubtitleVisibility {
    /// Из булева значения конфига (`subtitles.enabled`).
    pub(crate) const fn from_enabled(enabled: bool) -> Self {
        if enabled { Self::Shown } else { Self::Hidden }
    }

    /// Обратно в булево значение конфига.
    pub(crate) const fn is_enabled(self) -> bool {
        matches!(self, Self::Shown)
    }

    /// Противоположное состояние.
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Shown => Self::Hidden,
            Self::Hidden => Self::Shown,
        }
    }
}

/// Итог явного выбора дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectTrackOutcome {
    /// Дорожка выбрана, субтитры включены.
    Selected,
    /// Такой дорожки у текущего media нет (устаревший id); состояние не менялось.
    UnknownTrack,
    /// Дорожка есть, но недоступна (формат пока не поддержан или файл не читается);
    /// состояние не менялось.
    TrackUnavailable,
}

/// Итог «следующая дорожка» (цикл: дорожки, затем «выкл», затем снова первая).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CycleOutcome {
    /// Выбрана эта дорожка (субтитры включены).
    Track(subtitle_core::SubtitleTrackId),
    /// Цикл дошёл до «выкл»: субтитры скрыты.
    Off,
    /// Дорожек нет (или все недоступны): ничего не изменилось.
    NoTracks,
}

/// Итог добавления файла субтитров к текущему media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachOutcome {
    /// Файл добавлен как дорожка, выбран и субтитры включены. Содержимое читается в
    /// фоне; нечитаемый файл станет недоступной дорожкой, а видео не затрагивается.
    Attached(subtitle_core::SubtitleTrackId),
    /// Файл добавлен в список, но показать его нельзя (например, ASS до сессии 08): выбор и
    /// видимость не менялись, меню покажет причину.
    AddedButUnavailable(subtitle_core::SubtitleTrackId),
    /// Сейчас нет открытого media: прикреплять не к чему (UI: «Open a video first…»).
    NoMediaToAttachTo,
    /// Расширение не относится к файлам субтитров.
    NotASubtitleFile,
}

/// Итог постановки файла в очередь для следующего media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueOutcome {
    /// Файл ждёт следующего открытого media.
    Queued,
    /// Расширение не относится к файлам субтитров; очередь не менялась.
    NotASubtitleFile,
}
