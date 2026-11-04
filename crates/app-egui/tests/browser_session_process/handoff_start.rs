//! Передача посреди просмотра, когда начало страница уже стёрла (живая проверка 09b2, запись
//! Twitch, Firefox: 0 кадров навсегда, ни одной ошибки декодера).
//!
//! Страница перемоталась вперёд и стёрла начало; её журнал («память без копий», решение 176)
//! повторяет историю целиком, вместе со стёртым, а T0 плеер узнавал только в конце повтора.
//! Поколение успевало уйти плееру раньше: вставало на начало истории, первая же перемотка
//! снимала стёртое (решение 196) и просила страницу туда вернуться. Страница бросала своё место,
//! стирала то, что держала, и качала начало заново, а там плеер садился на зависимый кадр
//! посреди группы и ждал ключевой, которого в скачанном нет.
//!
//! Что доказывает тест (настоящий процесс, безоконно и в окне): страница, стоящая в поздней
//! части ролика, остаётся там. Плеер играет с T0 до конца, странице не уходит ни одной
//! просьбы перемотаться, а кадров показано столько, сколько лежит от T0 до конца. Два порядка
//! на проводе: T0 сразу за объявлением поколения (как шлёт экстеншен) и T0 только после загрузки
//! поколения: второй проверяет, что и без подсказки стёртое не становится стартом.
//!
//! Оконный вариант нужен `kwin_wayland` (иначе SKIPPED).

use super::generation_switch_clicks::{
    FRAME_US, PageSourceBuffers, SplitMedia, is_ended, page_seek, page_seek_targets,
    recorded_message, split_fmp4,
};
use super::*;
use std::ops::Range;

/// Длительность ролика (фикстура `quality-switch-new-media-source`).
const CLIP_US: u64 = 6_000_000;
/// Какие фрагменты страница держала и стёрла (видео — по 1,2 с, звук — по 1 с): её история
/// начинается не с нуля, как у Twitch (начало истории 82 мс), иначе перемотка не нужна вовсе.
const ERASED_VIDEO_FRAGMENTS: Range<usize> = 1..2;
const ERASED_AUDIO_FRAGMENTS: Range<usize> = 1..3;
/// Конец стёртого диапазона: всё до него плееру играть незачем.
const ERASED_END_US: u64 = 3_000_000;
/// С какого фрагмента страница держит ролик: видео с 3,6 с, звук с 3 с.
const HELD_VIDEO_FROM: usize = 3;
const HELD_AUDIO_FROM: usize = 3;
/// T0: страница стоит на паузе внутри того, что держит (`buffered`), посреди группы кадров.
const PAGE_POSITION_US: u64 = 4_000_000;
/// Звук и картинка могут стартовать на шаг часов позже цели; ноль позиции — уже провал.
const START_WINDOW_US: u64 = 500_000;
/// Сколько кадров могут не показаться на старте (опоздавшие на доли кадра).
const FRAMES_LOST_AT_START: u64 = 3;

/// Где плеер узнаёт T0.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PositionOnTheWire {
    /// Сразу за объявлением поколения, до первого байта истории (порядок экстеншена).
    BeforeHistory,
    /// Только когда поколение уже загружено: плеер должен справиться и без подсказки.
    AfterLoad,
}

#[test]
fn a_handoff_starts_at_the_page_position_named_before_its_history() {
    handoff_plays_from_the_page_position(VideoMode::Headless, PositionOnTheWire::BeforeHistory);
}

#[test]
fn a_handoff_starts_at_the_page_position_named_before_its_history_in_a_window() {
    handoff_plays_from_the_page_position(VideoMode::Window, PositionOnTheWire::BeforeHistory);
}

#[test]
fn an_erased_history_does_not_drag_the_page_when_its_position_comes_last() {
    handoff_plays_from_the_page_position(VideoMode::Headless, PositionOnTheWire::AfterLoad);
}

/// `SourceBuffer.remove(0, ERASED_END_US)` полосы.
fn erasure(source_buffer: u16) -> Message {
    Message::text(format!(
        r#"{{"type":"source_buffer_removed","generation":1,"source_buffer":{source_buffer},"start_us":0,"end":{{"kind":"at","time_us":{ERASED_END_US}}}}}"#
    ))
}

/// «Играть»: страница играла, когда зритель отдал видео.
fn page_plays() -> Message {
    Message::text(r#"{"type":"page_playback","request":{"kind":"play"}}"#)
}

/// Повтор журнала в порядке вызовов страницы.
fn replayed_history(position: PositionOnTheWire) -> Vec<Message> {
    let audio: SplitMedia = split_fmp4("audio-6s.mp4");
    let video: SplitMedia = split_fmp4("video-64-6s.mp4");
    let mut page = PageSourceBuffers::new(1);
    let mut messages: Vec<Message> = ["001.json", "002.json", "003.json", "004.json"]
        .into_iter()
        .map(recorded_message)
        .collect();
    if position == PositionOnTheWire::BeforeHistory {
        messages.push(page_seek(PAGE_POSITION_US));
    }
    messages.push(page.frame(0, &audio.init));
    messages.push(page.frame(1, &video.init));
    // История: начало, которое страница потом стёрла.
    for fragment in &audio.fragments[ERASED_AUDIO_FRAGMENTS] {
        messages.push(page.frame(0, fragment));
    }
    for fragment in &video.fragments[ERASED_VIDEO_FRAGMENTS] {
        messages.push(page.frame(1, fragment));
    }
    messages.push(erasure(0));
    messages.push(erasure(1));
    // То, что страница держит.
    for fragment in &audio.fragments[HELD_AUDIO_FROM..] {
        messages.push(page.frame(0, fragment));
    }
    for fragment in &video.fragments[HELD_VIDEO_FROM..] {
        messages.push(page.frame(1, fragment));
    }
    messages.push(Message::text(r#"{"type":"end_of_stream","generation":1}"#));
    messages
}

/// Всё одной пачкой, как журнал страницы: стирание приходит раньше, чем поток успевает
/// открыться (живой повтор: 24 мс на всю историю).
pub(super) fn send_burst(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    messages: Vec<Message>,
) {
    for message in messages {
        socket.write(message).expect("the message is queued");
    }
    socket.flush().expect("the burst is sent");
}

/// Часы «играю» по порядку прихода.
pub(super) fn playing_positions(messages: &[PlayerSessionMessage]) -> Vec<u64> {
    messages
        .iter()
        .filter_map(|message| match message {
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect()
}

fn handoff_plays_from_the_page_position(mode: VideoMode, position: PositionOnTheWire) {
    // Тесты идут параллельно: имя различает их каталоги.
    let name = match position {
        PositionOnTheWire::BeforeHistory => "handoff-start-named-first",
        PositionOnTheWire::AfterLoad => "handoff-start-named-last",
    };
    let Some(process) = SessionProcessUnderTest::start(name, mode) else {
        return;
    };
    let mut socket = process.open_attached_session();
    send_burst(&mut socket, replayed_history(position));
    if position == PositionOnTheWire::AfterLoad {
        process
            .bus
            .wait_for_log_line("Browser stream generation loaded");
    }
    // Итоговое T0 есть при любом порядке (шаг 3 договора `handoff-controller`).
    send_burst(&mut socket, vec![page_seek(PAGE_POSITION_US), page_plays()]);
    let messages = read_player_messages_until(&mut socket, "the clock state ended", is_ended);

    assert_eq!(
        page_seek_targets(&messages),
        Vec::<u64>::new(),
        "the page must stay at its own position: {messages:?}"
    );
    let playing = playing_positions(&messages);
    assert!(
        playing
            .iter()
            .any(|clock| (PAGE_POSITION_US..PAGE_POSITION_US + START_WINDOW_US).contains(clock)),
        "the player went through the page position: {playing:?}"
    );
    if position == PositionOnTheWire::BeforeHistory {
        assert!(
            playing
                .iter()
                .all(|clock| *clock + FRAME_US >= PAGE_POSITION_US),
            "the player showed the history before the page position: {playing:?}"
        );
    }

    send_tab_gone_and_expect_close(&mut socket);
    let summary = process
        .bus
        .wait_for_log_line("Browser playback summary kind=final");
    let frames_from_position = (CLIP_US - PAGE_POSITION_US) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames") + FRAMES_LOST_AT_START >= frames_from_position,
        "every frame from the page position reaches the screen: {summary}"
    );
}
