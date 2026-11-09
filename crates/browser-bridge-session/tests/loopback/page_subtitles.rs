//! Субтитры страницы на проводе (протокол 1.5, сессия 13b1): заголовок
//! `subtitle_track_offered` доходит до приёмника сообщений, байты документа рамками вида 3 —
//! до приёмника байтов, уже проверенные сторожем; при общей 1.4 и при нарушении порядка
//! документа сеанс закрывается кодом 4003, и до приложения ничего не доходит.

use browser_bridge_protocol::session::PageSubtitleTrackId;
use browser_bridge_protocol::{
    ByteOffset, DataFrameHeader, PageSessionMessage, PageSubtitleBytesHeader, ProtocolVersion,
    SessionCloseReason, encode_data_frame,
};

use crate::protocol_versions::{attached_after_hello, attached_version, wait_for_violation_end};
use crate::support::{Env, read_close_code, round_trip_ping, send_binary, send_text};

/// Документ дорожки 1 из двух кусков.
const DOCUMENT: &[u8] = b"WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n";
/// Где документ режется на два куска.
const SPLIT_AT: usize = 10;

/// Заголовок дорожки `track` длиной `byte_length` (как пишет экстеншен).
fn offer_json(track: u32, byte_length: usize) -> String {
    format!(
        concat!(
            r#"{{"type":"subtitle_track_offered","track":{},"language":{{"kind":"known","tag":"en"}},"#,
            r#""label":{{"kind":"none"}},"format_hint":"web_vtt","origin":"page_download","#,
            r#""page_display":"showing","byte_length":{}}}"#
        ),
        track, byte_length
    )
}

/// Рамка вида 3: кусок документа дорожки `track` со смещения `offset`.
fn subtitle_frame(track: u32, offset: usize, payload: &[u8]) -> Vec<u8> {
    encode_data_frame(
        &DataFrameHeader::PageSubtitleBytes(PageSubtitleBytesHeader {
            track: PageSubtitleTrackId::new(track).unwrap(),
            offset: ByteOffset::new(u64::try_from(offset).unwrap()).unwrap(),
        }),
        payload,
    )
    .unwrap()
}

#[test]
fn after_hello_1_5_the_offer_reaches_the_app_and_the_document_bytes_reach_the_byte_sink() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_5);
    assert_eq!(attached_version(&env), ProtocolVersion::V1_5);
    send_text(&mut socket, &offer_json(1, DOCUMENT.len()));
    send_binary(&mut socket, subtitle_frame(1, 0, &DOCUMENT[..SPLIT_AT]));
    send_binary(
        &mut socket,
        subtitle_frame(1, SPLIT_AT, &DOCUMENT[SPLIT_AT..]),
    );
    // Сервер всё прочитал и соединение живо: pong пришёл, а не кадр закрытия.
    round_trip_ping(&mut socket, b"subtitles-accepted");

    let seen = env.events.seen.wait_for("заголовок субтитров", |seen| {
        seen.page_messages.len() == 1
    });
    let PageSessionMessage::SubtitleTrackOffered {
        track, byte_length, ..
    } = &seen.page_messages[0]
    else {
        panic!("до приложения дошло не то: {:?}", seen.page_messages);
    };
    assert_eq!(track.get(), 1);
    assert_eq!(
        byte_length.as_bytes(),
        u64::try_from(DOCUMENT.len()).unwrap()
    );
    let received = env
        .bytes
        .received
        .wait_for("оба куска документа", |received| {
            received.page_subtitle_offsets.len() == 2
        });
    // Куски дошли по порядку со своими смещениями и складываются в документ байт в байт.
    assert_eq!(
        received.page_subtitle_offsets,
        [(1, 0), (1, u64::try_from(SPLIT_AT).unwrap())]
    );
    assert_eq!(
        received.page_subtitles.get(&1).map(Vec::as_slice),
        Some(DOCUMENT)
    );
    // Байты субтитров не притворяются ни MSE, ни файлом.
    assert!(received.source_buffers.is_empty());
    assert!(received.ranges.is_empty());
}

#[test]
fn after_hello_1_4_a_subtitle_frame_closes_the_session_with_4003_and_never_reaches_the_app() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_4);
    assert_eq!(attached_version(&env), ProtocolVersion::V1_4);
    send_binary(&mut socket, subtitle_frame(1, 0, DOCUMENT));
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    wait_for_violation_end(&env);
    let received = env.bytes.received.get();
    assert!(received.page_subtitles.is_empty());
    assert_eq!(received.frames, 0);
}

#[test]
fn after_hello_1_4_the_offer_is_a_violation_too() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_4);
    send_text(&mut socket, &offer_json(1, DOCUMENT.len()));
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    wait_for_violation_end(&env);
    assert!(env.events.seen.get().page_messages.is_empty());
}

#[test]
fn a_subtitle_frame_for_a_track_that_was_not_offered_closes_the_session_with_4003() {
    let env = Env::start();
    let mut socket = attached_after_hello(&env, ProtocolVersion::V1_5);
    send_text(&mut socket, &offer_json(1, DOCUMENT.len()));
    send_binary(&mut socket, subtitle_frame(2, 0, DOCUMENT));
    assert_eq!(
        read_close_code(&mut socket),
        SessionCloseReason::ProtocolViolation.close_code()
    );
    wait_for_violation_end(&env);
    // Заголовок дорожки 1 дошёл (он был до нарушения), байты чужой дорожки — нет.
    assert_eq!(env.events.seen.get().page_messages.len(), 1);
    assert!(env.bytes.received.get().page_subtitles.is_empty());
}
