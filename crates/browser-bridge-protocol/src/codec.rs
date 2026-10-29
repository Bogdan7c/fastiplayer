//! Строгий разбор и запись JSON-сообщений протокола.
//!
//! Все JSON-сообщения — объекты с полем `type` (имя сообщения в snake_case),
//! остальные поля — содержимое. Запись компактная, поля в порядке объявления:
//! одно и то же сообщение всегда даёт одни и те же байты, поэтому общие
//! фикстуры сверяются байт в байт.
//!
//! Две ловушки serde, которые закрыты здесь и в объявлениях типов:
//! - у перечисления с полем-меткой (`tag = "type"`/`"kind"`) **варианты без
//!   полей** serde разбирает, молча пропуская лишние поля даже при
//!   `deny_unknown_fields`. Поэтому такие варианты объявлены пустыми
//!   структурами (`Attached {}`, `Live {}`): у них лишнее поле — ошибка;
//! - структуру serde разбирает и из JSON-массива (`["hello"]`). Поэтому
//!   «объект ли это» проверяется явно до разбора.
//!
//! Обе ловушки закреплены фикстурами `field-in-unit-*` и `not-an-object`.

use serde::Deserialize;
use serde::Serialize;
use serde::de::{DeserializeOwned, IgnoredAny};

use crate::error::{MessageError, MessageErrorKind, ValueViolation, classify_json_error};
use crate::limits::MAX_JSON_MESSAGE_BYTES;

/// Вид JSON-сообщений одного направления одного участка моста.
pub trait ProtocolMessage: Serialize + DeserializeOwned {
    /// Все значения `type`, которые знает этот вид сообщений.
    ///
    /// Нужны, чтобы отличить «незнакомое сообщение» от «знакомое, но битое».
    /// Совпадение списка с вариантами перечисления проверяют тесты фикстур.
    const KNOWN_TYPES: &'static [&'static str];

    /// Проверки между полями, которые не выразить типом одного поля
    /// (уникальность ID в меню, порядок концов диапазона).
    ///
    /// # Errors
    /// Нарушенное правило.
    fn validate(&self) -> Result<(), ValueViolation>;
}

/// Только поле `type`, остальное пропускается: первый взгляд на сообщение.
#[derive(Deserialize)]
struct TypeTagProbe {
    #[serde(rename = "type")]
    message_type: String,
}

/// Разбирает одно JSON-сообщение вида `M` и проверяет его целиком.
///
/// # Errors
/// [`MessageError`] с точным видом причины: размер, не UTF-8, не JSON, не
/// объект, нет `type`, незнакомый `type`, лишнее/недостающее/повторное поле,
/// значение вне правил.
pub fn decode_message<M: ProtocolMessage>(bytes: &[u8]) -> Result<M, MessageError> {
    if bytes.len() > MAX_JSON_MESSAGE_BYTES {
        return Err(MessageError::new(MessageErrorKind::TooLarge));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| MessageError::new(MessageErrorKind::NotUtf8))?;
    let message_type = probe_message_type(text)?;
    if !M::KNOWN_TYPES.contains(&message_type.as_str()) {
        return Err(MessageError::new(MessageErrorKind::UnknownType));
    }
    let message: M = serde_json::from_str(text)
        .map_err(|error| MessageError::new(classify_json_error(&error)))?;
    message.validate()?;
    Ok(message)
}

/// Записывает сообщение в компактный JSON после той же проверки, что при разборе.
///
/// Отправитель не должен слать то, что получатель отвергнет: лучше узнать об
/// этом у себя, с точной причиной.
///
/// # Errors
/// Нарушено правило между полями или итог длиннее лимита сообщения.
pub fn encode_message<M: ProtocolMessage>(message: &M) -> Result<String, MessageError> {
    message.validate()?;
    // Ошибка записи возможна только у типов с неудачной сериализацией (карты с
    // нестроковыми ключами); у протокола таких нет, но вид причины честный.
    let text = serde_json::to_string(message)
        .map_err(|_| MessageError::new(MessageErrorKind::WrongValueType))?;
    if text.len() > MAX_JSON_MESSAGE_BYTES {
        return Err(MessageError::new(MessageErrorKind::TooLarge));
    }
    Ok(text)
}

/// Достаёт значение `type`, различая «не JSON», «не объект», «нет поля» и «не строка».
fn probe_message_type(text: &str) -> Result<String, MessageError> {
    // 1. Синтаксис целиком, включая мусор после JSON.
    serde_json::from_str::<IgnoredAny>(text)
        .map_err(|error| MessageError::new(classify_json_error(&error)))?;
    // 2. Корень — объект. Синтаксис уже верен, значит первый непробельный
    //    символ однозначно говорит, что это за значение.
    if !text.trim_start().starts_with('{') {
        return Err(MessageError::new(MessageErrorKind::NotAnObject));
    }
    // 3. Поле `type`.
    serde_json::from_str::<TypeTagProbe>(text)
        .map(|probe| probe.message_type)
        .map_err(|error| {
            let kind = match classify_json_error(&error) {
                MessageErrorKind::MissingField => MessageErrorKind::MissingType,
                other => other,
            };
            MessageError::new(kind)
        })
}
