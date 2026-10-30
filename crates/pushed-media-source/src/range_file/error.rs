//! Ошибки «файла по запросам байтов» и их грубые классы.
//!
//! Любая ошибка — это ошибка одного файла: процесс живёт, а владелец канала
//! (переходник протокола) по классу сообщает странице причину отказа. Первая
//! ошибка побеждает: всё, что сломалось после неё, — следствие.

/// Почему раскладка файла не поддерживается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedFileLayout {
    /// Оглавление MP4 (`moov`) лежит в конце, а размер файла неизвестен: до конца
    /// без перемотки не добраться. Честный отказ вместо вечного ожидания.
    MovieAtEndWithoutKnownSize,
    /// Оглавление MP4 больше предела, который источник согласен читать целиком.
    MovieTooLarge {
        /// Предел в байтах.
        limit: u64,
    },
}

/// Ошибка файла.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushedFileError {
    /// Рамка или итог для номера, который источник не выдавал.
    #[error("range request {id} was never issued")]
    UnissuedRequest {
        /// Номер из рамки.
        id: u32,
    },
    /// Рамка начинается не там, где кончились прежние байты запроса.
    #[error("range request {id}: bytes start at offset {actual}, expected {expected}")]
    NonContiguousBytes {
        /// Номер запроса.
        id: u32,
        /// Где должны начинаться байты.
        expected: u64,
        /// Где начались.
        actual: u64,
    },
    /// Байты вышли за конец запрошенного диапазона.
    #[error("range request {id}: bytes go past the requested end at offset {requested_end}")]
    BytesBeyondRequest {
        /// Номер запроса.
        id: u32,
        /// Первый байт после запрошенного диапазона.
        requested_end: u64,
    },
    /// Байты или итог для запроса, который уже завершён.
    #[error("range request {id} was already finished")]
    RequestAlreadyFinished {
        /// Номер запроса.
        id: u32,
    },
    /// Запрос кончился раньше запрошенного, хотя файл продолжается.
    #[error(
        "range request {id}: finished at offset {ended_at}, before the requested end {requested_end}"
    )]
    ShortRangeResponse {
        /// Номер запроса.
        id: u32,
        /// Где кончились байты.
        ended_at: u64,
        /// Где должен был кончиться запрос.
        requested_end: u64,
    },
    /// Размер файла изменился между запросами.
    #[error("the resource size changed from {before} to {after} bytes")]
    ResourceSizeChanged {
        /// Прежний размер.
        before: u64,
        /// Новый размер.
        after: u64,
    },
    /// Страница то знает размер, то не знает.
    #[error("the page switched between a known and an unknown resource size")]
    ResourceSizeKnowledgeChanged,
    /// Байты вышли за размер файла, который сообщила страница.
    #[error("bytes reach offset {end} past the resource size {size}")]
    BytesBeyondResourceSize {
        /// Размер файла.
        size: u64,
        /// Где кончились байты.
        end: u64,
    },
    /// Писатель уже закрыт (`seal`): после «больше байтов не будет» они не приходят.
    #[error("the file writer was already sealed")]
    WriterSealed,
    /// Открытие уже выдано или отдано: повторно не поддерживается.
    #[error("the pushed file was already opened")]
    AlreadyOpened,

    /// Повторы после сетевых сбоев исчерпали бюджет.
    #[error("the file stayed unavailable after {attempts} failed attempts")]
    RetryBudgetExhausted {
        /// Сколько сбоев было в последней серии.
        attempts: u32,
    },
    /// Страница сообщила, что файл изменился на сервере.
    #[error("the file changed while it was being read")]
    ResourceChanged,
    /// Страница отвергла диапазон внутри файла.
    #[error("the page rejected a range inside the file at offset {offset}")]
    RangeRejected {
        /// Смещение запроса.
        offset: u64,
    },
    /// Байтов больше не будет, а размер файла так и не стал известен.
    #[error("no more bytes will arrive and the file was not opened")]
    SealedBeforeOpen,
    /// Источник попросил бы пустой диапазон (смещение у предела `u64`): не запрашиваем.
    #[error("an empty range at offset {offset} cannot be requested")]
    EmptyRangeRequest {
        /// Смещение.
        offset: u64,
    },
    /// Номера запросов сеанса кончились.
    #[error("the session ran out of range request numbers")]
    RequestIdsExhausted,
    /// Не удалось запустить открытие файла.
    #[error("the file open could not start: {reason}")]
    OpenerStart {
        /// Причина (без данных страницы).
        reason: String,
    },

    /// Файл зашифрован.
    #[error("the file carries protected (encrypted) content")]
    ProtectedContent,
    /// Раскладка файла не поддерживается.
    #[error("unsupported file layout: {layout:?}")]
    UnsupportedLayout {
        /// Что именно.
        layout: UnsupportedFileLayout,
    },
    /// Демуксер не смог разобрать файл.
    #[error("the file could not be parsed: {reason}")]
    UnparsableMedia {
        /// Причина демуксера.
        reason: String,
    },
}

/// Грубый класс ошибки — то, что можно честно сказать источнику байтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushedFileFailureClass {
    /// Защищённое видео: не играем принципиально.
    ProtectedContent,
    /// Формат или раскладка, которые плеер не поддерживает.
    UnsupportedMedia,
    /// Байты не разбираются.
    UnparsableMedia,
    /// Страница нарушила порядок рамок и итогов (ошибка того, кто приносит байты).
    InputMisuse,
    /// Файл окончательно недоступен: сеть не вернулась, файл изменился, страница
    /// отвергла диапазон, вкладка ушла до открытия.
    SourceUnavailable,
}

impl PushedFileError {
    /// Класс ошибки для ответа источнику байтов.
    #[must_use]
    pub const fn failure_class(&self) -> PushedFileFailureClass {
        match self {
            Self::UnissuedRequest { .. }
            | Self::NonContiguousBytes { .. }
            | Self::BytesBeyondRequest { .. }
            | Self::RequestAlreadyFinished { .. }
            | Self::ShortRangeResponse { .. }
            | Self::ResourceSizeChanged { .. }
            | Self::ResourceSizeKnowledgeChanged
            | Self::BytesBeyondResourceSize { .. }
            | Self::WriterSealed
            | Self::AlreadyOpened => PushedFileFailureClass::InputMisuse,
            Self::RetryBudgetExhausted { .. }
            | Self::ResourceChanged
            | Self::RangeRejected { .. }
            | Self::SealedBeforeOpen
            | Self::RequestIdsExhausted
            | Self::OpenerStart { .. } => PushedFileFailureClass::SourceUnavailable,
            Self::ProtectedContent => PushedFileFailureClass::ProtectedContent,
            Self::UnsupportedLayout { .. } => PushedFileFailureClass::UnsupportedMedia,
            Self::UnparsableMedia { .. } | Self::EmptyRangeRequest { .. } => {
                PushedFileFailureClass::UnparsableMedia
            }
        }
    }
}
