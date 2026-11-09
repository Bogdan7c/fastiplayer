//! Субтитры страницы (протокол 1.5, сессия 13b1, решения 270–274).
//!
//! Весь договор в одном месте:
//! 1. Экстеншен скачал (или собрал со страницы) документ дорожки субтитров и шлёт заголовок
//!    `subtitle_track_offered` (`PageSessionMessage::SubtitleTrackOffered`): номер дорожки
//!    [`PageSubtitleTrackId`], язык, подпись, формат, откуда документ, была ли дорожка
//!    включена на странице, и точная длина документа [`PageSubtitleByteLength`].
//! 2. Затем идут байты документа двоичными рамками вида 3 (`crate::data_frame`,
//!    `PageSubtitleBytesHeader`): номер дорожки и смещение внутри документа, подряд с 0.
//! 3. Документ готов, когда пришло ровно `byte_length` байт — отдельного «готово» нет.
//! 4. Повторное предложение того же номера дорожки — замена: прежний документ (в том числе
//!    недокачанный) забывается, байты нового снова идут с 0.
//!
//! Сообщение не привязано к поколению потока (как `tab_gone`): документ живёт весь сеанс,
//! переживает смены поколения, и его байты не трогают ни поколений, ни SourceBuffer-ов.
//! Правила порядка (номер предложен, смещение подряд, не длиннее объявленного, не больше
//! `crate::limits::MAX_PAGE_SUBTITLE_TRACKS` разных дорожек) — в `crate::stream_gate`.
//!
//! Необязательные значения (язык, подпись) — по конвенции протокола метки, а не
//! отсутствующие поля: `{"kind":"unknown"}` / `{"kind":"known","tag":"en"}` и
//! `{"kind":"none"}` / `{"kind":"text","text":"English"}`. Поле обязательно: его
//! отсутствие — `missing_field`, `null` — `wrong_value_type`.

use serde::{Deserialize, Serialize};

use crate::error::ValueViolation;
use crate::limits::MAX_PAGE_SUBTITLE_DOCUMENT_BYTES;
use crate::strict_shape::{strict_label, strict_object};
use crate::text::{SubtitleLabel, SubtitleLanguageTag};
use crate::version::ProtocolVersion;

// Строгая форма JSON (`crate::strict_shape`): объекты и перечисления с меткой —
// только объектом, метки — только строкой; чужая форма — `wrong_value_type`.
strict_object!(PageSubtitleLanguage, PageSubtitleLabel);
strict_label!(SubtitleFormatHint, PageSubtitleOrigin, PageSubtitleDisplay);

/// Версия протокола, в которой появились субтитры страницы: и сообщение
/// `subtitle_track_offered`, и рамка вида 3.
pub const PAGE_SUBTITLES_INTRODUCED_IN: ProtocolVersion = ProtocolVersion::V1_5;

/// Номер дорожки субтитров страницы, от 1. Выдаёт экстеншен на сеанс; тот же номер в новом
/// предложении — замена документа этой дорожки.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct PageSubtitleTrackId(u32);

impl PageSubtitleTrackId {
    /// Номер из числа.
    ///
    /// # Errors
    /// Ноль: номера дорожек считаются с 1 (ноль в рамке — признак забытого поля).
    pub const fn new(number: u32) -> Result<Self, ValueViolation> {
        if number == 0 {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(number))
    }

    /// Число.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for PageSubtitleTrackId {
    type Error = ValueViolation;

    fn try_from(number: u32) -> Result<Self, Self::Error> {
        Self::new(number)
    }
}

impl From<PageSubtitleTrackId> for u32 {
    fn from(track: PageSubtitleTrackId) -> Self {
        track.0
    }
}

/// Точная длина документа субтитров: от 1 байта до [`MAX_PAGE_SUBTITLE_DOCUMENT_BYTES`].
///
/// Пустого документа не бывает: дорожке без реплик нечего показывать, и экстеншен её не
/// предлагает.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct PageSubtitleByteLength(u64);

impl PageSubtitleByteLength {
    /// Длина из числа байт.
    ///
    /// # Errors
    /// Ноль или больше лимита документа.
    pub const fn new(bytes: u64) -> Result<Self, ValueViolation> {
        if bytes == 0 || bytes > MAX_PAGE_SUBTITLE_DOCUMENT_BYTES {
            return Err(ValueViolation::NumberOutOfRange);
        }
        Ok(Self(bytes))
    }

    /// Число байт.
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        self.0
    }
}

impl TryFrom<u64> for PageSubtitleByteLength {
    type Error = ValueViolation;

    fn try_from(bytes: u64) -> Result<Self, Self::Error> {
        Self::new(bytes)
    }
}

impl From<PageSubtitleByteLength> for u64 {
    fn from(length: PageSubtitleByteLength) -> Self {
        length.0
    }
}

/// Язык дорожки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PageSubtitleLanguage {
    /// Страница язык не назвала.
    Unknown {},
    /// Тег BCP-47, как его дала страница.
    Known {
        /// Тег языка.
        tag: SubtitleLanguageTag,
    },
}

/// Подпись дорожки для человека.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    remote = "Self",
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PageSubtitleLabel {
    /// Подписи нет: плеер назовёт дорожку сам (по языку или номеру).
    None {},
    /// Подпись страницы.
    Text {
        /// Текст подписи.
        text: SubtitleLabel,
    },
}

/// В каком формате, по мнению экстеншена, документ. Это подсказка, а не обещание: плеер
/// всё равно узнаёт формат по содержимому и при расхождении верит содержимому.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum SubtitleFormatHint {
    /// WebVTT (`web_vtt`).
    WebVtt,
    /// SubRip.
    Srt,
    /// TTML / IMSC / DFXP.
    Ttml,
    /// XML-формат YouTube `srv3` (`fmt=srv3`).
    Srv3,
    /// JSON-формат YouTube `json3` (`fmt=json3`, решение 272: модель — как у srv3).
    Json3,
    /// Экстеншен формата не знает.
    Unknown,
}

/// Откуда экстеншен взял документ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum PageSubtitleOrigin {
    /// Страница сама скачала файл субтитров (перехват ответа, повторный запрос).
    PageDownload,
    /// Элемент `<track src>` у видео.
    TrackElement,
    /// Документ собран из реплик `TextTrack.cues`, которые страница добавила скриптом.
    TextTrackCues,
}

/// Была ли дорожка включена на странице в момент предложения (решение 271: «скачала —
/// значит включено»: плеер включает такую дорожку у себя).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum PageSubtitleDisplay {
    /// Дорожка показывалась на странице.
    Showing,
    /// Дорожка была скачана или видна, но не показывалась.
    NotShowing,
}
