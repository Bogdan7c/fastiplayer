//! Ожидание и отказ видны зрителю (мост, сессия 04b, решение 129).
//!
//! - Поколение, которое плеер играть не может (звуковая дорожка с кодеком, которого
//!   плеер не знает: `xxxx` в описании образца при честных байтах и MIME `mp4a`), —
//!   странице ровно один `stream_rejected{unsupported_codec}`: байты целы, кодек нам не по
//!   силам. Сеанс жив: следующее поколение играет до кадров на показе и до конца. В окне
//!   зритель видит сообщение об отказе (строка журнала окна), без окна его нет.
//! - Перемотка вне скачанного, на которую страница байтов так и не прислала: плеер через
//!   10 с сдаётся (`SeekTimeout`), и это событие доходит до уведомлений `AppState` окна —
//!   там его превращает в тост «Seeking timed out; try again» общий путь ошибок плеера
//!   (`state/notifications/player_feed.rs`, текст закреплён его тестом).
//!
//! Оконные тесты идут под частным `kwin_wayland --virtual` и печатают SKIPPED без него.

use super::*;

/// Строка журнала окна, когда оно показывает зрителю отказ поколения.
const REJECTION_NOTICE_LINE: &str = "Browser window shows the stream rejection notice";

/// Длина ролика фикстуры (`video.mp4` / `audio.mp4`), мкс.
const FIXTURE_CLIP_US: u64 = 400_000;

/// Звук фикстуры, у которого в описании образца `mp4a` заменён на `xxxx`: кодек, которого
/// плеер не знает (байты и MIME при этом честные).
fn audio_with_unknown_codec() -> Vec<u8> {
    let mut file = std::fs::read(Path::new(FIXTURES).join("media/audio.mp4")).expect("audio.mp4");
    let entry = file
        .windows(4)
        .position(|window| window == b"mp4a")
        .expect("mp4a sample entry");
    file[entry..entry + 4].copy_from_slice(b"xxxx");
    file
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(FIXTURES).join("media").join(name)).expect("fixture media")
}

/// Поколение `number`: объявленная длина, видео (полоса 0) и звук (полоса 1) одним куском,
/// при `sealed` — конец потока.
fn generation_messages(
    number: u32,
    declared_duration_us: u64,
    video: Vec<u8>,
    audio: Vec<u8>,
    sealed: bool,
) -> Vec<Message> {
    let generation = StreamGeneration::new(number).expect("generation");
    let mut messages = vec![
        Message::text(format!(
            r#"{{"type":"generation_started","generation":{number}}}"#
        )),
        Message::text(format!(
            r#"{{"type":"duration_changed","generation":{number},"duration":{{"kind":"finite","duration_us":{declared_duration_us}}}}}"#
        )),
        Message::text(format!(
            r#"{{"type":"source_buffer_added","generation":{number},"source_buffer":0,"mime_type":"video/mp4; codecs=\"avc1.42c00a\""}}"#
        )),
        Message::text(format!(
            r#"{{"type":"source_buffer_added","generation":{number},"source_buffer":1,"mime_type":"audio/mp4; codecs=\"mp4a.40.2\""}}"#
        )),
    ];
    for (source_buffer, bytes) in [(0, video), (1, audio)] {
        let header = DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
            generation,
            source_buffer: SourceBufferIndex::new(source_buffer).expect("source buffer"),
            sequence: AppendSequence(0),
        });
        messages.push(Message::binary(
            encode_data_frame(&header, &bytes).expect("data frame"),
        ));
    }
    if sealed {
        messages.push(Message::text(format!(
            r#"{{"type":"end_of_stream","generation":{number}}}"#
        )));
    }
    messages
}

fn send_all(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>, messages: Vec<Message>) {
    for message in messages {
        socket.send(message).expect("stream message is sent");
    }
}

fn rejections(messages: &[PlayerSessionMessage]) -> Vec<&PlayerSessionMessage> {
    messages
        .iter()
        .filter(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. }))
        .collect()
}

#[test]
fn an_unplayable_generation_is_rejected_and_the_next_one_plays() {
    unplayable_generation_is_rejected_and_the_next_one_plays(VideoMode::Headless);
}

#[test]
fn an_unplayable_generation_is_rejected_in_a_window_and_the_next_one_plays() {
    unplayable_generation_is_rejected_and_the_next_one_plays(VideoMode::Window);
}

fn unplayable_generation_is_rejected_and_the_next_one_plays(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("unplayable-generation", mode) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();

    // 1. Поколение, которое плеер играть не может: ровно один отказ с честной причиной.
    send_all(
        &mut socket,
        generation_messages(
            1,
            FIXTURE_CLIP_US,
            fixture("video.mp4"),
            audio_with_unknown_codec(),
            true,
        ),
    );
    let before_rejection = read_player_messages_until(&mut socket, "stream_rejected", |message| {
        matches!(message, PlayerSessionMessage::StreamRejected { .. })
    });
    assert_eq!(
        rejections(&before_rejection),
        [&PlayerSessionMessage::StreamRejected {
            generation: StreamGeneration::FIRST,
            reason: StreamRejection::UnsupportedCodec,
        }]
    );
    // Зритель окна видит сообщение об отказе; без окна показывать некому.
    if mode == VideoMode::Window {
        let notice = bus.wait_for_log_line(REJECTION_NOTICE_LINE);
        assert!(notice.contains("reason=unsupported_codec"), "{notice}");
    }

    // 2. Сеанс жив: следующее поколение играет до конца.
    send_all(
        &mut socket,
        generation_messages(
            2,
            FIXTURE_CLIP_US,
            fixture("video.mp4"),
            fixture("audio.mp4"),
            true,
        ),
    );
    let after_rejection =
        read_player_messages_until(&mut socket, "the clock state ended", |message| {
            matches!(
                message,
                PlayerSessionMessage::Clock {
                    state: ClockState::Ended,
                    ..
                }
            )
        });
    assert!(
        rejections(&after_rejection).is_empty(),
        "the playable generation is not rejected: {after_rejection:?}"
    );
    bus.wait_for_log_line("Browser playback ended");

    // 3. Кадры второго поколения дошли до показа (в окне — до рендера окна), отказанное
    //    поколение в плеер не грузилось.
    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line("Browser playback summary kind=final");
    assert_eq!(
        log_number(&summary, "presented_frames"),
        FIXTURE_VIDEO_FRAMES,
        "{summary}"
    );
    assert_eq!(log_number(&summary, "loads"), 1, "{summary}");
    bus.wait_for_log_line("Browser session process finished");
}

/// Перемотка страницей вне скачанного, байтов с цели нет: `SeekTimeout` доходит до
/// уведомлений окна (событие плеера читает только `AppState`; сеанс получает копию —
/// строка журнала окна ниже пишется по копии, которую отвод снимает в той же точке,
/// где `AppState` отдаёт событие владельцу уведомлений).
#[test]
fn a_seek_the_page_never_feeds_times_out_into_the_window_notifications() {
    let Some(process) = SessionProcessUnderTest::start("seek-timeout", VideoMode::Window) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    // Объявлено 10 с, скачано 0,4 с; конца потока нет — страница «качает дальше».
    send_all(
        &mut socket,
        generation_messages(
            1,
            10_000_000,
            fixture("video.mp4"),
            fixture("audio.mp4"),
            false,
        ),
    );
    read_player_messages_until(&mut socket, "a playing clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Playing,
                ..
            }
        )
    });
    socket
        .send(Message::text(
            r#"{"type":"seek_started","target_us":5000000,"cause":{"kind":"page_seek"}}"#,
        ))
        .expect("seek_started is sent");

    let timeout = bus.wait_for_log_line("Browser window player reported an error");
    assert!(timeout.contains("kind=SeekTimeout"), "{timeout}");
    // Процесс пережил несостоявшуюся перемотку: зритель закрывает окно — странице
    // прощание и код «плеер закрыт», процесс выходит штатно.
    process
        .display()
        .expect("a window test has a display")
        .close_fastiplayer_window();
    assert_eq!(
        read_close_code(&mut socket),
        CloseCode::from(SessionCloseReason::PlayerClosed.close_code())
    );
    bus.wait_for_log_line("Browser session process finished");
}
