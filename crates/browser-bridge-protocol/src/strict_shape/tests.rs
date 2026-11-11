//! Строгая форма: чужая форма отвергается одинаково и при прямом разборе
//! типа, и внутри сообщения (через буфер serde), а прежние виды причин
//! (лишнее/повторное/недостающее поле, незнакомая метка) не меняются.

use crate::codec::decode_message;
use crate::error::{MessageErrorKind, classify_json_error};
use crate::session::page::RemovalEnd;
use crate::session::player::ClockState;
use crate::time::MediaTime;
use crate::version::ProtocolVersion;
use crate::{PageSessionMessage, PlayerSessionMessage};

/// Вид причины прямого разбора одного значения из JSON-текста.
fn direct_kind<T: serde::de::DeserializeOwned + std::fmt::Debug>(json: &str) -> MessageErrorKind {
    let error = serde_json::from_str::<T>(json).expect_err("значение должно быть отвергнуто");
    classify_json_error(&error)
}

/// Вид причины разбора сообщения страницы `source_buffer_removed` с данным `end`.
fn removal_kind(end_json: &str) -> MessageErrorKind {
    let message = format!(
        r#"{{"type":"source_buffer_removed","generation":1,"source_buffer":0,"start_us":0,"end":{end_json}}}"#
    );
    decode_message::<PageSessionMessage>(message.as_bytes())
        .expect_err("сообщение должно быть отвергнуто")
        .kind()
}

/// Вид причины разбора тика часов с данным `state`.
fn clock_state_kind(state_json: &str) -> MessageErrorKind {
    let message =
        format!(r#"{{"type":"clock","seq":1,"position_us":0,"state":{state_json},"rate":1000}}"#);
    decode_message::<PlayerSessionMessage>(message.as_bytes())
        .expect_err("сообщение должно быть отвергнуто")
        .kind()
}

#[test]
fn object_is_parsed_from_an_object_and_rejected_from_an_array_directly() {
    let parsed: ProtocolVersion =
        serde_json::from_str(r#"{"major":1,"minor":2}"#).expect("объект — правильная форма");
    assert_eq!(parsed, ProtocolVersion { major: 1, minor: 2 });
    assert_eq!(
        direct_kind::<ProtocolVersion>("[1,2]"),
        MessageErrorKind::WrongValueType
    );
    assert_eq!(
        direct_kind::<ProtocolVersion>(r#""1.2""#),
        MessageErrorKind::WrongValueType
    );
}

#[test]
fn tagged_enum_inside_a_message_is_parsed_only_from_an_object_with_a_text_tag() {
    let accepted = decode_message::<PageSessionMessage>(
        br#"{"type":"source_buffer_removed","generation":1,"source_buffer":0,"start_us":0,"end":{"kind":"at","time_us":5}}"#,
    )
    .expect("правильная форма принимается");
    let PageSessionMessage::SourceBufferRemoved { end, .. } = accepted else {
        panic!("разобрано не то сообщение: {accepted:?}");
    };
    assert_eq!(
        end,
        RemovalEnd::At {
            time_us: MediaTime::from_micros(5).expect("5 мкс — допустимо")
        }
    );

    // Массив «метка, поля по порядку» и номер варианта вместо метки.
    assert_eq!(
        removal_kind(r#"["at",5]"#),
        MessageErrorKind::WrongValueType
    );
    assert_eq!(
        removal_kind(r#"{"kind":0,"time_us":5}"#),
        MessageErrorKind::WrongValueType
    );
    // Номер вне предела — та же ошибка формы, а не «число вне диапазона».
    assert_eq!(
        removal_kind(r#"{"kind":99}"#),
        MessageErrorKind::WrongValueType
    );
    assert_eq!(
        removal_kind(r#"{"kind":true}"#),
        MessageErrorKind::WrongValueType
    );
}

#[test]
fn tagged_enum_parsed_directly_also_rejects_a_numeric_tag_and_an_array() {
    assert_eq!(
        direct_kind::<RemovalEnd>(r#"{"kind":0,"time_us":5}"#),
        MessageErrorKind::WrongValueType
    );
    assert_eq!(
        direct_kind::<RemovalEnd>(r#"["at",5]"#),
        MessageErrorKind::WrongValueType
    );
}

#[test]
fn label_is_parsed_only_from_a_string() {
    let parsed: ClockState =
        serde_json::from_str(r#""waiting""#).expect("строка — правильная форма");
    assert_eq!(parsed, ClockState::Waiting);

    // Внутри сообщения (через буфер serde) и напрямую — одинаково.
    assert_eq!(
        clock_state_kind(r#"{"playing":null}"#),
        MessageErrorKind::WrongValueType
    );
    assert_eq!(clock_state_kind("0"), MessageErrorKind::WrongValueType);
    assert_eq!(
        direct_kind::<ClockState>(r#"{"playing":null}"#),
        MessageErrorKind::WrongValueType
    );
    assert_eq!(
        direct_kind::<ClockState>("1"),
        MessageErrorKind::WrongValueType
    );
}

#[test]
fn strict_shape_keeps_the_previous_reasons_for_wrong_content_in_the_right_shape() {
    // Незнакомая метка строкой — по-прежнему «незнакомое значение».
    assert_eq!(
        clock_state_kind(r#""rewinding""#),
        MessageErrorKind::UnknownValue
    );
    assert_eq!(
        removal_kind(r#"{"kind":"forever"}"#),
        MessageErrorKind::UnknownValue
    );
    // Обёртка имён не мешает serde замечать повтор метки и поля во вложенном объекте.
    assert_eq!(
        removal_kind(r#"{"kind":"at","kind":"at","time_us":5}"#),
        MessageErrorKind::DuplicateField
    );
    assert_eq!(
        removal_kind(r#"{"kind":"at","time_us":5,"time_us":6}"#),
        MessageErrorKind::DuplicateField
    );
    assert_eq!(
        removal_kind(r#"{"kind":"at","time_us":5,"x":1}"#),
        MessageErrorKind::UnknownField
    );
    assert_eq!(
        removal_kind(r#"{"kind":"at"}"#),
        MessageErrorKind::MissingField
    );
    assert_eq!(
        removal_kind(r#"{"time_us":5}"#),
        MessageErrorKind::MissingField
    );
    // Значения внутри объекта проверяются как раньше.
    assert_eq!(
        removal_kind(r#"{"kind":"at","time_us":"5"}"#),
        MessageErrorKind::WrongValueType
    );
}

#[test]
fn writing_is_unchanged_by_the_strict_layer() {
    let message = PlayerSessionMessage::Clock {
        seq: crate::session::player::ClockSequence(7),
        position_us: MediaTime::from_micros(1_000).expect("1000 мкс — допустимо"),
        state: ClockState::Playing,
        rate: crate::time::PlaybackRate::from_permille(1_000).expect("1× — допустимо"),
        // Без номера поколения (протокол 1.10): ключа на проводе нет вовсе.
        generation: None,
    };
    let text = crate::codec::encode_message(&message).expect("сообщение записывается");
    assert_eq!(
        text,
        r#"{"type":"clock","seq":7,"position_us":1000,"state":"playing","rate":1000}"#
    );
    let decoded = decode_message::<PlayerSessionMessage>(text.as_bytes())
        .expect("записанное читается обратно");
    assert_eq!(decoded, message);
}
