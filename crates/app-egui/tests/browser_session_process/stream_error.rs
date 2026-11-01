//! Обрыв связи по ошибке потока больше не молчаливый (мост, сессия 04d, решение 132).
//!
//! Страница игнорирует «полон» и шлёт сегменты, пока плеер не упрётся в потолок полки
//! (`IntakeLimitExceeded` → плеер сам закрывает связь кодом 4003). Потолок снижен
//! тестовой переменной `FASTIPLAYER_TEST_BROWSER_MAX_HELD_STREAM_BYTES` (настоящий — 256
//! МиБ), все правила остаются настоящими. Что обязано случиться:
//! - странице приходит `return_to_page{T, paused}` РАНЬШЕ кадра 4003; T — позиция
//!   наших часов (в пределах тика от последних часов);
//! - в окне — сообщение «Stream error — video returned to the page» (строка журнала с
//!   причиной), окно закрывается не раньше срока сообщения; процесс выходит штатно;
//! - безоконный процесс прощается так же и выходит сразу.
//!
//! Оконный вариант идёт под частным `kwin_wayland --virtual` и печатает SKIPPED без него.

use super::session_ends::{
    END_NOTICE_PERIOD_AT_LEAST, assert_farewell_within_a_clock_tick, read_farewell_then_close,
    send_all, six_seconds_downloaded, wait_until_playing_past,
};
use super::*;

/// Потолок удерживаемых байтов полки в процессе под тестом. Вся «играющая» часть потока
/// (два init, 2 куска видео, 3 куска звука — 23 933 байта) под ним, а следующий кусок
/// видео (+5 039) уже нет: упор в потолок честный, не из-за одного огромного сегмента.
const HELD_BYTES_CEILING: &str = "28000";

/// Сколько видео- и аудиокусков страница шлёт до того, как плеер начнёт играть.
const VIDEO_FRAGMENTS_BEFORE_PLAYING: usize = 2;
const AUDIO_FRAGMENTS_BEFORE_PLAYING: usize = 3;

/// Режет fMP4 на куски по верхним боксам: init (`ftyp`+`moov`), затем `moof`+`mdat`.
fn split_fragmented_mp4(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut pieces: Vec<Vec<u8>> = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let size =
            u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("box size")) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let whole_box = &bytes[offset..offset + size];
        match kind {
            // init и каждая пара moof+mdat начинают новый кусок.
            b"ftyp" | b"moof" => pieces.push(whole_box.to_vec()),
            _ => pieces
                .last_mut()
                .expect("a box before any ftyp/moof")
                .extend_from_slice(whole_box),
        }
        offset += size;
    }
    pieces
}

/// Кадр данных: кусок `piece` полосы `source_buffer` с порядковым номером `sequence`.
fn data_frame(source_buffer: u16, sequence: u64, piece: &[u8]) -> Message {
    let header = DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
        generation: StreamGeneration::FIRST,
        source_buffer: SourceBufferIndex::new(source_buffer).expect("source buffer"),
        sequence: AppendSequence(sequence),
    });
    Message::binary(encode_data_frame(&header, piece).expect("data frame"))
}

/// Все куски полосы кадрами с номерами по порядку (нумерация подряд — страница не врёт).
fn lane_frames(source_buffer: u16, media: &str) -> Vec<Message> {
    let bytes = std::fs::read(Path::new(FIXTURES).join("media").join(media)).expect("media");
    split_fragmented_mp4(&bytes)
        .iter()
        .enumerate()
        .map(|(index, piece)| data_frame(source_buffer, index as u64, piece))
        .collect()
}

#[test]
fn a_stream_error_returns_the_video_to_the_page_before_4003_without_a_window() {
    a_stream_error_returns_the_video_to_the_page(VideoMode::Headless);
}

#[test]
fn a_stream_error_returns_the_video_to_the_page_before_4003_in_a_window() {
    a_stream_error_returns_the_video_to_the_page(VideoMode::Window);
}

fn a_stream_error_returns_the_video_to_the_page(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start_with_env(
        "stream-error-farewell",
        mode,
        &[(
            "FASTIPLAYER_TEST_BROWSER_MAX_HELD_STREAM_BYTES",
            HELD_BYTES_CEILING,
        )],
    ) else {
        return;
    };
    let bus = &process.bus;
    let mut socket = process.open_attached_session();

    // Начало ролика: объявление поколения, затем то, что помещается под потолок.
    let mut announcements = six_seconds_downloaded();
    announcements.truncate(4);
    send_all(&mut socket, announcements);
    let video = lane_frames(1, "video-64-6s.mp4");
    let audio = lane_frames(0, "audio-6s.mp4");
    let video_head = 1 + VIDEO_FRAGMENTS_BEFORE_PLAYING;
    let audio_head = 1 + AUDIO_FRAGMENTS_BEFORE_PLAYING;
    send_all(&mut socket, video[..video_head].to_vec());
    send_all(&mut socket, audio[..audio_head].to_vec());
    // Плеер играет: часы идут, кадры доходят до показа.
    let clock_seen = wait_until_playing_past(&mut socket, 500_000);

    // Страница не слушает «полон» и шлёт остальное. После закрытия связи запись может
    // не пройти — это ожидаемо (плеер уже оборвал связь), ошибку записи не считаем.
    let flood: Vec<Message> = video[video_head..]
        .iter()
        .chain(&audio[audio_head..])
        .cloned()
        .collect();
    for message in flood {
        if socket.send(message).is_err() {
            break;
        }
    }

    let (last_clock, farewell, code) = read_farewell_then_close(&mut socket, clock_seen);
    assert_eq!(
        code,
        CloseCode::from(SessionCloseReason::ProtocolViolation.close_code()),
        "the player closes the link with 4003"
    );
    assert_farewell_within_a_clock_tick(last_clock, farewell);
    assert!(
        farewell >= 500_000,
        "the farewell carries the played position, not zero: {farewell} µs"
    );
    let farewell_log = bus.wait_for_log_line("Session closed with a farewell to the page");
    assert!(
        farewell_log.contains(r#"occasion="stream_error""#),
        "{farewell_log}"
    );
    assert!(farewell_log.contains("FarewellQueued"), "{farewell_log}");

    if mode == VideoMode::Window {
        // Сообщение в окне с причиной, окно висит срок сообщения и закрывается.
        let notice = bus.wait_for_log_line("Browser session end notice shown");
        let notice_at = Instant::now();
        assert!(
            notice.contains("notice=stream_error cause=protocol_violation"),
            "{notice}"
        );
        bus.wait_for_log_line("Browser window closed after the end notice");
        let shown_for = notice_at.elapsed();
        assert!(
            shown_for >= END_NOTICE_PERIOD_AT_LEAST,
            "the window closed {shown_for:?} after the notice"
        );
    }
    // Процесс вышел штатно (код 0 — `Completed`), а не упал.
    bus.wait_for_log_line("Browser session process finished");
}
