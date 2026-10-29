//! Типизированные причины, по которым сообщение протокола отвергнуто.
//!
//! Политика версий строгая (решение владельца 86): получатель не угадывает и
//! не пропускает незнакомое. Каждая причина — отдельный вид, чтобы плеер,
//! посредник и экстеншен реагировали точно, а общие фикстуры
//! (`fixtures/manifest.json`) проверяли одинаково на Rust и TypeScript.
//!
//! В ошибке нет текста из сообщения: всё, что пришло со страницы, — данные
//! врага (инвариант 1), и в журналы по умолчанию оно не попадает.

use serde::{Deserialize, Serialize};
use serde_json::error::Category;

/// Вид нарушения в одном значении (строка, число, список).
///
/// Это тип ошибки проверяемых обёрток (`crate::text`, `crate::time`). Они
/// разбираются внутри serde, поэтому причина проходит через сообщение ошибки
/// serde как машинный код из [`ValueViolation::code`] и восстанавливается в
/// `classify_json_error`. Коды стабильны: на них опираются фикстуры.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ValueViolation {
    /// Пустая строка там, где нужно значение.
    #[error("text_empty")]
    TextEmpty,
    /// Строка длиннее лимита.
    #[error("text_too_long")]
    TextTooLong,
    /// Строка короче нижней границы (одноразовый токен).
    #[error("text_too_short")]
    TextTooShort,
    /// Символ, запрещённый правилами допуска поля.
    #[error("text_forbidden_character")]
    TextForbiddenCharacter,
    /// Число вне допустимого диапазона.
    #[error("number_out_of_range")]
    NumberOutOfRange,
    /// Список длиннее лимита.
    #[error("too_many_items")]
    TooManyItems,
    /// Повтор идентификатора там, где он должен быть уникален.
    #[error("duplicate_id")]
    DuplicateId,
    /// Взаимоисключающие отметки (например, два выбранных варианта в меню).
    #[error("conflicting_selection")]
    ConflictingSelection,
    /// Диапазон, у которого конец не позже начала.
    #[error("empty_range")]
    EmptyRange,
}

impl ValueViolation {
    /// Все виды по порядку — для восстановления вида по коду.
    const ALL: [Self; 9] = [
        Self::TextEmpty,
        Self::TextTooLong,
        Self::TextTooShort,
        Self::TextForbiddenCharacter,
        Self::NumberOutOfRange,
        Self::TooManyItems,
        Self::DuplicateId,
        Self::ConflictingSelection,
        Self::EmptyRange,
    ];

    /// Машинный код вида; совпадает с именем в [`MessageErrorKind`].
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TextEmpty => "text_empty",
            Self::TextTooLong => "text_too_long",
            Self::TextTooShort => "text_too_short",
            Self::TextForbiddenCharacter => "text_forbidden_character",
            Self::NumberOutOfRange => "number_out_of_range",
            Self::TooManyItems => "too_many_items",
            Self::DuplicateId => "duplicate_id",
            Self::ConflictingSelection => "conflicting_selection",
            Self::EmptyRange => "empty_range",
        }
    }
}

/// Вид отказа JSON-сообщения. Имена в snake_case — общий словарь для фикстур.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageErrorKind {
    /// Сообщение длиннее [`crate::limits::MAX_JSON_MESSAGE_BYTES`].
    TooLarge,
    /// Байты не являются UTF-8.
    NotUtf8,
    /// Не JSON или JSON оборван.
    Syntax,
    /// JSON, но не объект.
    NotAnObject,
    /// У объекта нет поля `type`.
    MissingType,
    /// Значение `type` не знакомо этой версии протокола.
    UnknownType,
    /// Лишнее поле (строгая политика: незнакомое — ошибка).
    UnknownField,
    /// Нет обязательного поля.
    MissingField,
    /// Поле повторено дважды.
    DuplicateField,
    /// Незнакомое значение перечисления внутри сообщения.
    UnknownValue,
    /// Значение не того типа (строка вместо числа, дробь вместо целого).
    WrongValueType,
    /// См. [`ValueViolation::TextEmpty`].
    TextEmpty,
    /// См. [`ValueViolation::TextTooLong`].
    TextTooLong,
    /// См. [`ValueViolation::TextTooShort`].
    TextTooShort,
    /// См. [`ValueViolation::TextForbiddenCharacter`].
    TextForbiddenCharacter,
    /// См. [`ValueViolation::NumberOutOfRange`].
    NumberOutOfRange,
    /// См. [`ValueViolation::TooManyItems`].
    TooManyItems,
    /// См. [`ValueViolation::DuplicateId`].
    DuplicateId,
    /// См. [`ValueViolation::ConflictingSelection`].
    ConflictingSelection,
    /// См. [`ValueViolation::EmptyRange`].
    EmptyRange,
}

impl From<ValueViolation> for MessageErrorKind {
    fn from(violation: ValueViolation) -> Self {
        match violation {
            ValueViolation::TextEmpty => Self::TextEmpty,
            ValueViolation::TextTooLong => Self::TextTooLong,
            ValueViolation::TextTooShort => Self::TextTooShort,
            ValueViolation::TextForbiddenCharacter => Self::TextForbiddenCharacter,
            ValueViolation::NumberOutOfRange => Self::NumberOutOfRange,
            ValueViolation::TooManyItems => Self::TooManyItems,
            ValueViolation::DuplicateId => Self::DuplicateId,
            ValueViolation::ConflictingSelection => Self::ConflictingSelection,
            ValueViolation::EmptyRange => Self::EmptyRange,
        }
    }
}

/// Сообщение отвергнуто: только вид причины, без содержимого сообщения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("browser bridge message rejected: {kind:?}")]
pub struct MessageError {
    kind: MessageErrorKind,
}

impl MessageError {
    /// Создаёт ошибку заданного вида.
    #[must_use]
    pub const fn new(kind: MessageErrorKind) -> Self {
        Self { kind }
    }

    /// Вид причины отказа.
    #[must_use]
    pub const fn kind(self) -> MessageErrorKind {
        self.kind
    }
}

impl From<ValueViolation> for MessageError {
    fn from(violation: ValueViolation) -> Self {
        Self::new(violation.into())
    }
}

/// Переводит ошибку serde_json в вид отказа.
///
/// serde не даёт типизированного вида для «лишнее поле» или «нет поля», только
/// текст. Поэтому вид восстанавливается по началу текста ошибки: формулировки
/// serde стабильны много лет, а тесты фикстур (каждый вид — своя фикстура)
/// сразу поймают, если обновление serde их изменит. Ошибки наших проверяемых
/// обёрток начинаются с кода [`ValueViolation::code`].
#[must_use]
pub(crate) fn classify_json_error(error: &serde_json::Error) -> MessageErrorKind {
    match error.classify() {
        // Обрыв и синтаксис — одинаково «не JSON»; Io при разборе из памяти не бывает.
        Category::Syntax | Category::Eof | Category::Io => MessageErrorKind::Syntax,
        Category::Data => classify_data_error_text(&error.to_string()),
    }
}

/// Вид отказа по тексту ошибки данных serde.
fn classify_data_error_text(text: &str) -> MessageErrorKind {
    // Наши обёртки кладут свой код первым словом сообщения.
    if let Some(violation) = ValueViolation::ALL
        .into_iter()
        .find(|violation| text.starts_with(violation.code()))
    {
        return violation.into();
    }
    // Формулировки из serde::de::Error (unknown_field, missing_field и т. д.).
    if text.starts_with("unknown field") {
        MessageErrorKind::UnknownField
    } else if text.starts_with("missing field") {
        MessageErrorKind::MissingField
    } else if text.starts_with("duplicate field") {
        MessageErrorKind::DuplicateField
    } else if text.starts_with("unknown variant") {
        MessageErrorKind::UnknownValue
    } else if text.starts_with("invalid value: integer") {
        // Целое не влезло в тип поля (отрицательное в u64, 2^40 в u32).
        MessageErrorKind::NumberOutOfRange
    } else {
        MessageErrorKind::WrongValueType
    }
}
