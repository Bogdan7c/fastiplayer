//! Read-only представление субтитров для меню и кнопки CC.
//!
//! UI получает готовые к показу данные и не знает ни о внутренних состояниях загрузки, ни о
//! фоновых задачах: достаточно прочитать [`SubtitleMenuView`] и вызвать intent-методы.
//! Подписи дорожек собирает UI из частей [`SubtitleTrackLabel`] (тексты интерфейса —
//! английские и принадлежат UI).

use subtitle_core::{SubtitleFormat, SubtitleLanguage, SubtitleSyncOffset, SubtitleTrackId};
use web_media_core::SubtitleKind;

use super::intents::SubtitleVisibility;

/// Откуда у дорожки файл.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtitleFileOrigin {
    /// Найден автоматически рядом с видео.
    NextToVideo,
    /// Добавлен пользователем (бросок на окно или «Загрузить файл…»).
    ChosenByUser,
    /// Дорожка внутри контейнера видео (MKV, MP4…): отдельного файла нет.
    Embedded,
    /// Дорожка скачивается по требованию с сайта (YouTube): ручная, распознанная речь или
    /// автоперевод.
    Web(SubtitleKind),
    /// Дорожка потока (рендиция HLS): субтитры подгружаются лентой вокруг позиции.
    Stream,
}

/// Части подписи дорожки; пустые поля пропускаются при сборке текста.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubtitleTrackLabel {
    /// Язык, если известен (по имени файла); без языка UI пишет «Track N».
    pub(crate) language: Option<SubtitleLanguage>,
    /// Формат дорожки (SRT, WebVTT, ASS…); `None` — у встроенной дорожки формат без названия
    /// в модели (например VobSub или DVB): UI пишет «other».
    pub(crate) format: Option<SubtitleFormat>,
    /// Откуда файл.
    pub(crate) origin: SubtitleFileOrigin,
    /// Принудительная дорожка (только иностранная речь).
    pub(crate) is_forced: bool,
    /// Дорожка для слабослышащих.
    pub(crate) is_hearing_impaired: bool,
    /// Нераспознанные слова из имени файла.
    pub(crate) title: Option<String>,
    /// Имя файла без каталога (для показа).
    pub(crate) file_name: Option<String>,
    /// Запасное имя web-дорожки (название от источника либо код языка), если языка нет в
    /// таблице известных. У дорожек не из web — `None`.
    pub(crate) fallback_name: Option<String>,
    /// Различимое название web-дорожки, если у нескольких дорожек одного вида одно название
    /// языка (`Chinese (Taiwan)`); заменяет название из таблицы. Выставляет владелец списка.
    pub(crate) distinguishing_name: Option<String>,
}

impl SubtitleTrackLabel {
    /// Автоматический перевод: такие дорожки лежат в подменю «Auto-translate».
    pub(crate) fn is_auto_translation(&self) -> bool {
        self.origin == SubtitleFileOrigin::Web(SubtitleKind::AutoTranslated)
    }
}

/// Почему дорожку нельзя выбрать. `Display` английский, без путей.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SubtitleUnavailableReason {
    /// Формат найден, но показ ещё не поддержан (ASS/SSA до сессии 08).
    #[error("this subtitle format is not supported yet")]
    FormatNotSupportedYet,
    /// Файл не прочитан или не разобран; вид проблемы определяет формулировку подсказки.
    #[error("subtitle file problem: {0:?}")]
    LoadFailed(SubtitleFileProblem),
}

/// Почему файл субтитров не загрузился, в терминах, которые различает человек: что ему делать.
/// Технические детали (вид ошибки ввода-вывода, строка разбора) остаются в логе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubtitleFileProblem {
    /// Файл не открылся: нет доступа, нет файла или иная ошибка чтения.
    CannotOpen,
    /// Путь ведёт не к обычному файлу (канал, папка, устройство, сокет).
    NotARegularFile,
    /// Файл больше разрешённого лимита и не читался.
    TooLarge,
    /// Файл прочитан, но реплик из него не получилось (повреждён, чужой формат, пуст или
    /// реплик больше лимита).
    CannotParse,
    /// Любая другая причина (внутренний сбой): без подробностей для человека.
    Other,
}

/// Можно ли выбрать дорожку.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubtitleTrackAvailability {
    /// Дорожку можно выбрать (она может ещё грузиться: выбор не блокируется).
    Available,
    /// Дорожка недоступна по указанной причине.
    Unavailable(SubtitleUnavailableReason),
}

/// Одна строка меню.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubtitleMenuTrack {
    /// Идентификатор для `select_subtitle_track`.
    pub(crate) id: SubtitleTrackId,
    /// Части подписи.
    pub(crate) label: SubtitleTrackLabel,
    /// Отмечена ли строка: субтитры включены и выбрана именно эта дорожка.
    pub(crate) is_selected: bool,
    /// Доступность.
    pub(crate) availability: SubtitleTrackAvailability,
}

/// Всё, что нужно кнопке CC и меню, одним снимком.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubtitleMenuView {
    /// Есть ли у текущего media хотя бы одна дорожка (нет дорожек — кнопки CC нет).
    pub(crate) has_tracks: bool,
    /// Включены ли субтитры (кнопка подчёркнута).
    pub(crate) visibility: SubtitleVisibility,
    /// Дорожки в порядке меню.
    pub(crate) tracks: Vec<SubtitleMenuTrack>,
    /// Текущий сдвиг синхронизации.
    pub(crate) sync_offset: SubtitleSyncOffset,
}

impl SubtitleMenuView {
    /// Пустой вид: нет media или нет дорожек; `visibility` — запомненное предпочтение.
    pub(crate) fn empty(visibility: SubtitleVisibility) -> Self {
        Self {
            has_tracks: false,
            visibility,
            tracks: Vec::new(),
            sync_offset: SubtitleSyncOffset::ZERO,
        }
    }
}
