//! Клик зрителя во время смены поколения на VP9 (04d, путь C, решения 146–149, 313) — два порядка
//! событий, оба закреплены отдельно (16b3c).
//!
//! Сценарий как на живом YouTube: новое поколение, сразу «я на T», зритель кликает, пока оно
//! открывается, байты приходят только у T, страница возвращается на T. Плееру, чтобы поставить
//! VP9, нужен первый пакет видео (профиль и глубина цвета — в заголовке кадра), поэтому
//! поколение, отданное на клике без данных, не ставится вовсе: проверено временной поломкой
//! ворот — отказ `VideoStreamConfiguration` («no VP9 header until the end of the stream») →
//! `stream_rejected`; на живом 4K то же место кончалось `VideoPreflightTimeout`. С воротами
//! поколение встаёт на T. Возврат страницы на T после клика — повод повторить клик один раз
//! (решение 149).
//!
//! Почему два теста. Повторённый клик ведёт за конец скачанного. Если страница уже сказала
//! `end_of_stream`, медиа доигрывает до конца за миллисекунды, и исход зависит от того, что
//! придёт раньше: второй возврат страницы на T или конец медиа. Прежний единый тест молча
//! полагался на первый порядок и падал на втором (16b3c: удержание кадра смены медиа сдвинуло
//! время, и порядок стал выпадать в 2–5 прогонах из 10; с выключенным удержанием и паузой
//! 150 мс перед вторым возвратом — 6 из 6). Теперь каждый порядок задан явно:
//! - страница вернулась раньше, чем объявила конец, — плеер встаёт на T сразу, третьего `seek` нет;
//! - медиа уже доиграло, — перемотка страницы держится до её `play` (решение 313), потом
//!   поколение играет с T, третьего `seek` тоже нет.

use super::generation_switch_clicks::{
    PLAYBACK_REFUSALS, PageSourceBuffers, START_WINDOW_US, first_generation_head, is_ended,
    is_playing_from, page_seek, page_seek_targets, viewer_seek_by, wait_until_controllable,
};
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// Где страница держит новое поколение: «я на T» и единственные байты медиа.
const PAGE_POSITION_US: u64 = 3_000_000;
/// Клик зрителя вперёд, туда, где у нового поколения медиа не будет.
const VIEWER_CLICK_BY_US: i64 = 4_200_000;
/// Пауза перед первым возвратом страницы: поколение успевает открыться у T.
const PAGE_RETURN_DELAY: Duration = Duration::from_millis(500);
/// `play` страницы — то, что отпускает перемотку, задержанную после конца медиа (решение 313).
const PAGE_PLAY: &str = r#"{"type":"page_playback","request":{"kind":"play"}}"#;
/// Строка журнала плеера: перемотка страницы после конца медиа ждёт её `play`.
const ENDED_SEEK_HELD: &str = "Page seek of the ended media is held until the page plays";

/// Конец init-сегмента `muxed.webm` (EBML-заголовок, Segment Info и Tracks — всё до
/// первого `Cluster`, ID `1F 43 B6 75`).
fn webm_init_end(bytes: &[u8]) -> usize {
    bytes
        .windows(4)
        .position(|window| window == [0x1F, 0x43, 0xB6, 0x75])
        .expect("a WebM file has a cluster")
}

/// Поколение 2 — WebM VP9 + Opus одним SourceBuffer (как PeerTube и Twitch), длительность
/// 10 с; только init — медиа нет нигде.
fn vp9_generation_announced(page: &mut PageSourceBuffers, webm: &[u8]) -> Vec<Message> {
    vec![
        Message::text(r#"{"type":"generation_started","generation":2}"#),
        Message::text(
            r#"{"type":"duration_changed","generation":2,"duration":{"kind":"finite","duration_us":10000000}}"#,
        ),
        Message::text(
            r#"{"type":"source_buffer_added","generation":2,"source_buffer":0,"mime_type":"video/webm; codecs=\"vp9,opus\""}"#,
        ),
        page.frame(0, &webm[..webm_init_end(webm)]),
    ]
}

/// Общее начало обоих порядков: первое поколение играет, VP9-поколение открывается у T, зритель
/// кликнул, страница положила байты у T и вернулась туда; плеер повторил клик (решение 149).
///
/// `end_of_stream` страница шлёт вместе с первым возвратом, только если так велит порядок.
/// Возвращает сокет сеанса и всё, что плеер прислал к этому мигу.
fn vp9_switch_until_the_click_is_repeated(
    process: &SessionProcessUnderTest,
    end_of_stream: EndOfStreamTiming,
) -> (
    WebSocket<MaybeTlsStream<TcpStream>>,
    Vec<PlayerSessionMessage>,
) {
    let bus = &process.bus;
    let mut socket = process.open_attached_session();
    send_all(&mut socket, first_generation_head());
    wait_until_playing_past(&mut socket, 1_000_000);
    wait_until_controllable(bus);

    let webm = std::fs::read(Path::new(FIXTURES).join("media/muxed.webm")).expect("media");
    let mut second = PageSourceBuffers::new(2);
    // Как YouTube: новое поколение, сразу «я на T» (03e) и init; байтов медиа ещё нет.
    let mut announced = vp9_generation_announced(&mut second, &webm);
    announced.insert(3, page_seek(PAGE_POSITION_US));
    send_all(&mut socket, announced);
    let mut messages = read_player_messages_until(&mut socket, "a clock tick", |message| {
        matches!(message, PlayerSessionMessage::Clock { .. })
    });
    // Зритель кликает, пока поколение открывается: туда, где медиа не будет.
    viewer_seek_by(bus, VIEWER_CLICK_BY_US);
    messages.extend(read_player_messages_until(
        &mut socket,
        "the page seek for the click",
        |message| matches!(message, PlayerSessionMessage::Seek { .. }),
    ));

    // Байты страницы приходят только у T: поколение открывается, но у клика данных нет.
    send_all(
        &mut socket,
        vec![
            Message::text(format!(
                r#"{{"type":"timestamp_offset_changed","generation":2,"source_buffer":0,"offset_us":{PAGE_POSITION_US}}}"#
            )),
            second.frame(0, &webm[webm_init_end(&webm)..]),
        ],
    );
    std::thread::sleep(PAGE_RETURN_DELAY);
    // Страница возвращается на своё место (живая находка: после «loaded»).
    let mut first_return = vec![page_seek(PAGE_POSITION_US)];
    if end_of_stream == EndOfStreamTiming::WithTheFirstReturn {
        first_return.push(end_of_stream_message());
    }
    send_all(&mut socket, first_return);
    // Поколение встало у страницы (данные есть только там), а после показа клик
    // повторяется странице один раз (решение 149).
    messages.extend(read_player_messages_until(
        &mut socket,
        "the repeat of the viewer click",
        |message| matches!(message, PlayerSessionMessage::Seek { .. }),
    ));
    let targets = page_seek_targets(&messages);
    assert_eq!(targets.len(), 2, "the click and one repeat: {targets:?}");
    assert_eq!(targets[0], targets[1], "the repeat is the same click");
    (socket, messages)
}

/// Когда страница объявляет, что докачала всё (`end_of_stream`), — этим задаётся порядок
/// «второй возврат страницы» против «конец медиа».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EndOfStreamTiming {
    /// Вместе с первым возвратом на T: повторённый клик за конец скачанного доигрывает медиа.
    WithTheFirstReturn,
    /// Только после второго возврата: медиа не может кончиться раньше него.
    AfterTheSecondReturn,
}

fn end_of_stream_message() -> Message {
    Message::text(r#"{"type":"end_of_stream","generation":2}"#)
}

/// Читает до тика часов «играет у T» и проверяет, что плеер встал там, где страница.
fn read_until_playing_at_the_page_position(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    what: &str,
) -> Vec<PlayerSessionMessage> {
    let playing = read_player_messages_until(socket, what, |message| {
        is_playing_from(message, PAGE_POSITION_US)
    });
    let Some(PlayerSessionMessage::Clock {
        position_us: first, ..
    }) = playing.last()
    else {
        panic!("a playing clock ends the read");
    };
    assert!(
        first.as_micros() < PAGE_POSITION_US + START_WINDOW_US,
        "the player did not stay where the page is: {first:?}"
    );
    playing
}

/// Общий конец обоих порядков: медиа доиграло, третьего `seek` нет, поколение не отвергнуто,
/// загрузок ровно две.
fn finish_and_check_the_vp9_switch(
    process: &SessionProcessUnderTest,
    mut socket: WebSocket<MaybeTlsStream<TcpStream>>,
    mut messages: Vec<PlayerSessionMessage>,
) {
    messages.extend(read_player_messages_until(
        &mut socket,
        "the end of the VP9 bytes",
        is_ended,
    ));
    assert_eq!(
        page_seek_targets(&messages).len(),
        2,
        "no third page seek after the page disobeyed twice"
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the VP9 generation was rejected"
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary = process
        .bus
        .wait_for_log_line_without("Browser playback summary kind=final", &PLAYBACK_REFUSALS);
    assert_eq!(log_number(&summary, "loads"), 2, "{summary}");
}

/// Конфиг теста: срок перемотки длинный, чтобы ожидание данных у клика не кончилось отказом.
fn vp9_switch_config_text() -> String {
    let mut config = fastiplayer_config::AppConfig::default();
    config.player.seek.commit_timeout_ms = 120_000;
    config.to_pretty_toml().expect("config text")
}

/// Порядок 1: страница вернулась на T второй раз раньше, чем объявила конец. Повторённый клик
/// ждёт данных (страница жива, конца нет), возврат страницы ставит плеер на T, третьего `seek` нет.
#[test]
fn a_click_during_a_switch_to_vp9_waits_for_data_instead_of_failing_the_install() {
    let Some(process) = SessionProcessUnderTest::start_window_with_config(
        "switch-click-vp9",
        &vp9_switch_config_text(),
    ) else {
        return;
    };
    let (mut socket, mut messages) =
        vp9_switch_until_the_click_is_repeated(&process, EndOfStreamTiming::AfterTheSecondReturn);
    // Страница не подчинилась и со второго раза — снова вернулась к себе: плеер остаётся
    // там, где страница.
    socket
        .send(page_seek(PAGE_POSITION_US))
        .expect("page seek is sent");
    messages.extend(read_until_playing_at_the_page_position(
        &mut socket,
        "the VP9 generation playing at the page position",
    ));
    // Теперь страница докачала всё: медиа доиграет до конца скачанного.
    socket
        .send(end_of_stream_message())
        .expect("end of stream is sent");
    finish_and_check_the_vp9_switch(&process, socket, messages);
}

/// Порядок 2: страница уже объявила конец, и повторённый клик за конец скачанного доиграл медиа
/// раньше второго возврата страницы. Возврат после конца держится до `play` страницы (решение
/// 313), затем поколение играет с T; третьего `seek` нет.
#[test]
fn a_page_return_after_the_repeated_click_ended_the_vp9_media_waits_for_the_page_play() {
    let Some(process) = SessionProcessUnderTest::start_window_with_config(
        "switch-click-vp9-ended",
        &vp9_switch_config_text(),
    ) else {
        return;
    };
    let (mut socket, mut messages) =
        vp9_switch_until_the_click_is_repeated(&process, EndOfStreamTiming::WithTheFirstReturn);
    messages.extend(read_player_messages_until(
        &mut socket,
        "the media ended at the repeated click",
        is_ended,
    ));
    socket
        .send(page_seek(PAGE_POSITION_US))
        .expect("page seek is sent");
    process
        .bus
        .wait_for_log_line_without(ENDED_SEEK_HELD, &PLAYBACK_REFUSALS);
    socket
        .send(Message::text(PAGE_PLAY))
        .expect("page play is sent");
    messages.extend(read_until_playing_at_the_page_position(
        &mut socket,
        "the VP9 generation playing at the page position after the page play",
    ));
    finish_and_check_the_vp9_switch(&process, socket, messages);
}
