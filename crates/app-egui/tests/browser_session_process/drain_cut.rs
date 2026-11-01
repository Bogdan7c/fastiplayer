//! Досмотр после ухода вкладки кончается на конце первой кончившейся дорожки
//! (04d, решение владельца 133) — настоящим процессом браузерного режима.
//!
//! Живой показ 04b: YouTube качает звук дальше картинки; после ухода вкладки картинка
//! стояла, а звук и ползунок шли ещё 12–16 с, и на «доиграно» позиция прыгала в
//! длительность фильма. Здесь «страница» шлёт видео короче звука и объявляет фильм
//! длиннее обоих; после `tab_gone` процесс доигрывает только общее у обеих дорожек:
//! звук кончается с картинкой, позиция на конце — там, где встал показ.
//!
//! Медиа — фикстуры `pushed-media-source` (64×64 H.264 25 к/с, фрагменты по 0,4 с;
//! AAC 48 кГц, фрагменты по 405 мс): видео — первые [`DOWNLOADED_VIDEO_FRAGMENTS`]
//! фрагментов, звук — весь (4,05 с), длительность — 10 с.

use super::*;

/// Каталог фикстур источника.
const SOURCE_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../pushed-media-source/fixtures"
);
/// Скачанных фрагментов видео (по 0,4 с).
const DOWNLOADED_VIDEO_FRAGMENTS: usize = 3;
/// Конец скачанного видео, мс.
const VIDEO_END_MS: u64 = 1_200;
/// Конец скачанного звука, мс (весь звук фикстуры).
const AUDIO_END_MS: u64 = 4_053;
/// Длительность фильма, объявленная страницей, мс.
const MOVIE_DURATION_MS: u64 = 10_000;
/// Допуск конца: один кадр видео (40 мс) — шаг, на котором плеер замечает конец.
const END_TOLERANCE_MS: u64 = 40;

/// Файл фикстуры источника.
fn source_fixture(name: &str) -> Vec<u8> {
    std::fs::read(Path::new(SOURCE_FIXTURES).join(name)).expect("pushed-media-source fixture")
}

/// Байты fMP4 от начала до конца `fragments`-го фрагмента (`moof` + `mdat`).
///
/// Обход только по заголовкам боксов верхнего уровня: init (`ftyp` + `moov`) идёт
/// целиком, затем фрагменты по порядку.
fn fragmented_mp4_prefix(file: &[u8], fragments: usize) -> Vec<u8> {
    let mut position = 0;
    let mut finished_fragments = 0;
    while position < file.len() && finished_fragments < fragments {
        let size_bytes: [u8; 4] = file[position..position + 4].try_into().expect("box size");
        let size = usize::try_from(u32::from_be_bytes(size_bytes)).expect("box size fits");
        if &file[position + 4..position + 8] == b"mdat" {
            finished_fragments += 1;
        }
        position += size;
    }
    assert_eq!(
        finished_fragments, fragments,
        "the fixture has enough fragments"
    );
    file[..position].to_vec()
}

/// Рамка байтов SourceBuffer `source_buffer` поколения 1.
fn source_buffer_frame(source_buffer: u16, sequence: u64, bytes: &[u8]) -> Message {
    let header = DataFrameHeader::SourceBufferBytes(SourceBufferBytesHeader {
        generation: StreamGeneration::FIRST,
        source_buffer: SourceBufferIndex::new(source_buffer).expect("source buffer"),
        sequence: AppendSequence(sequence),
    });
    Message::binary(encode_data_frame(&header, bytes).expect("data frame"))
}

/// Страница: видео короче звука, фильм длиннее обоих, `endOfStream` нет — страница
/// ещё качала бы, если бы не ушла.
fn send_shorter_video_than_audio(socket: &mut WebSocket<MaybeTlsStream<TcpStream>>) {
    let duration_changed = format!(
        r#"{{"type":"duration_changed","generation":1,"duration":{{"kind":"finite","duration_us":{}}}}}"#,
        MOVIE_DURATION_MS * 1_000
    );
    for text in [
        r#"{"type":"generation_started","generation":1}"#,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":0,"mime_type":"video/mp4; codecs=\"avc1.42c00a\""}"#,
        r#"{"type":"source_buffer_added","generation":1,"source_buffer":1,"mime_type":"audio/mp4; codecs=\"mp4a.40.2\""}"#,
        duration_changed.as_str(),
    ] {
        socket.send(Message::text(text)).expect("message is sent");
    }
    let video = fragmented_mp4_prefix(
        &source_fixture("video-h264-64-4s.mp4"),
        DOWNLOADED_VIDEO_FRAGMENTS,
    );
    let audio = source_fixture("audio-aac-4s.mp4");
    socket
        .send(source_buffer_frame(0, 0, &video))
        .expect("video bytes are sent");
    socket
        .send(source_buffer_frame(1, 0, &audio))
        .expect("audio bytes are sent");
}

/// Без окна: журнал процесса — единственный свидетель. Плеер успел прочитать звук
/// впрок за концом видео ещё до ухода вкладки (сборка «видео + звук» пускает его на
/// 3 с вперёд, пока видео ждёт байтов) — и всё равно звук встаёт с картинкой, а
/// позиция — на конце видео, а не в длительности фильма.
#[test]
fn after_the_tab_is_gone_sound_and_position_stop_with_the_shorter_video() {
    let Some(ended) = play_until_the_tab_is_gone_and_drained("drain-cut") else {
        return;
    };
    let audio_played = log_number(&ended, "audio_played_ms");
    assert!(
        audio_played <= VIDEO_END_MS + END_TOLERANCE_MS,
        "sound must stop with the picture ({VIDEO_END_MS} ms), not at {AUDIO_END_MS} ms: {ended}"
    );
    let position = log_number(&ended, "position_ms");
    assert!(
        position.abs_diff(VIDEO_END_MS) <= END_TOLERANCE_MS,
        "the position stays at the end of the video, not at {MOVIE_DURATION_MS} ms: {ended}"
    );
    assert!(log_number(&ended, "presented_frames") > 0, "{ended}");
    assert_eq!(log_number(&ended, "position_regressions"), 0, "{ended}");
}

/// Страница шлёт видео короче звука, плеер начинает играть, вкладка уходит; процесс
/// доигрывает и выходит. Возвращает строку журнала `Browser playback ended` (`None` —
/// окружение не подходит для теста).
fn play_until_the_tab_is_gone_and_drained(name: &str) -> Option<String> {
    let process = SessionProcessUnderTest::start(name, VideoMode::Headless)?;
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_shorter_video_than_audio(&mut socket);

    // Плеер начал играть (первые часы «играет») — вкладка уходит посреди фильма.
    read_player_messages_until(&mut socket, "the playing clock", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Playing,
                ..
            }
        )
    });
    send_tab_gone_and_expect_close(&mut socket);

    bus.wait_for_log_line("Browser session ended");
    let ended = bus.wait_for_log_line("Browser playback ended");
    bus.wait_for_log_line("Playback drain finished class=played_to_end");
    bus.wait_for_log_line("Browser session process finished");
    Some(ended)
}
