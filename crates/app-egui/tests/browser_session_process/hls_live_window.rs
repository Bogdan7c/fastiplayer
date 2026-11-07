//! Сквозные тесты 10a3 (решение 232): окно эфира live HLS странице и перемотка страницы
//! внутри окна.
//!
//! Эфир — настоящий: окно плейлиста едет по часам стены (новый сегмент каждые ~0,4 с),
//! сегменты — фрагменты фикстуры, повторённые по кругу со сдвинутым временем
//! (`tfdt`), так что эфир идёт сколько угодно без новых файлов в репозитории.

use std::rc::Rc;

use browser_bridge_protocol::LiveSeekable;

use super::generation_switch_clicks::page_seek;
use super::hls_page::{
    FragmentedMovie, HELLO_1_1, HELLO_1_2, HlsPage, PageResource, announce_hls_generation,
    bridge_address, final_summary, fmp4_vod_playlist, fragmented_movie,
    open_attached_session_speaking,
};
use super::*;

/// Номерок live-корня.
const ROOT_HANDLE: u32 = 0;
/// Номерок init (`EXT-X-MAP`).
const INIT_HANDLE: u32 = 1;
/// Номерок сегмента 0; сегмент `n` — `FIRST_SEGMENT_HANDLE + n`.
const FIRST_SEGMENT_HANDLE: u32 = 100;
/// Сколько сегментов эфира заготовлено (~100 с; тесты идут меньше 30 с).
const PREBUILT_SEGMENTS: usize = 250;
/// Сколько сегментов уже «вышло в эфир» к первому запросу корня (~4 с).
const INITIAL_SEGMENTS: usize = 10;
/// Длина окна плейлиста в сегментах (~6 с): старое уходит из плейлиста.
const PLAYLIST_WINDOW_SEGMENTS: usize = 15;
/// Длительность сегмента в эфире (как у фрагмента фикстуры).
const SEGMENT_SECONDS: f64 = 0.4;

/// Сдвиг времени одного трека фикстуры.
#[derive(Debug, Clone, Copy)]
struct TrackLoop {
    /// Номер трека (`tfhd.track_ID`).
    track_id: u32,
    /// Длина одного круга фикстуры в тиках трека.
    loop_ticks: u64,
}

/// Эфир по кругу: init и заготовленные сегменты.
struct LoopedLive {
    init: Vec<u8>,
    segments: Vec<Vec<u8>>,
}

/// Дети бокса `[start, end)`: `(вид, начало тела, конец бокса)`.
fn child_boxes(bytes: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut children = Vec::new();
    let mut position = start;
    while position + 8 <= end {
        let size = u32::from_be_bytes(bytes[position..position + 4].try_into().expect("size"));
        let kind: [u8; 4] = bytes[position + 4..position + 8].try_into().expect("kind");
        let box_end = position + size as usize;
        children.push((kind, position + 8, box_end));
        position = box_end;
    }
    children
}

/// Трек и длительность (в тиках трека) одного `traf`.
fn traf_track_and_duration(fragment: &[u8], body: usize, end: usize) -> (u32, u64) {
    let mut track_id = 0;
    let mut default_duration = 0_u64;
    let mut duration = 0_u64;
    for (kind, child, _) in child_boxes(fragment, body, end) {
        let flags = u32::from_be_bytes([
            0,
            fragment[child + 1],
            fragment[child + 2],
            fragment[child + 3],
        ]);
        let word = |at: usize| {
            u64::from(u32::from_be_bytes(
                fragment[at..at + 4].try_into().expect("u32"),
            ))
        };
        match &kind {
            b"tfhd" => {
                track_id = word(child + 4) as u32;
                // После track_ID: base_data_offset (0x01, 8 байт), sample_description_index (0x02).
                let mut at = child + 8;
                if flags & 0x01 != 0 {
                    at += 8;
                }
                if flags & 0x02 != 0 {
                    at += 4;
                }
                if flags & 0x08 != 0 {
                    default_duration = word(at);
                }
            }
            b"trun" => {
                let samples = word(child + 4);
                let mut at = child + 8;
                if flags & 0x01 != 0 {
                    at += 4;
                }
                if flags & 0x04 != 0 {
                    at += 4;
                }
                let per_sample = [0x100, 0x200, 0x400, 0x800]
                    .iter()
                    .filter(|flag| flags & **flag != 0)
                    .count()
                    * 4;
                duration += if flags & 0x100 != 0 {
                    (0..samples as usize)
                        .map(|index| word(at + index * per_sample))
                        .sum::<u64>()
                } else {
                    samples * default_duration
                };
            }
            _ => {}
        }
    }
    (track_id, duration)
}

/// Длина круга фикстуры по трекам: сумма длительностей фрагментов.
fn track_loops(movie: &FragmentedMovie) -> Vec<TrackLoop> {
    let mut loops: Vec<TrackLoop> = Vec::new();
    for fragment in &movie.fragments {
        for (kind, body, end) in child_boxes(fragment, 0, fragment.len()) {
            if &kind != b"moof" {
                continue;
            }
            for (traf_kind, traf_body, traf_end) in child_boxes(fragment, body, end) {
                if &traf_kind != b"traf" {
                    continue;
                }
                let (track_id, duration) = traf_track_and_duration(fragment, traf_body, traf_end);
                match loops.iter_mut().find(|known| known.track_id == track_id) {
                    Some(known) => known.loop_ticks += duration,
                    None => loops.push(TrackLoop {
                        track_id,
                        loop_ticks: duration,
                    }),
                }
            }
        }
    }
    assert_eq!(loops.len(), 2, "the fixture has video and audio");
    loops
}

/// Фрагмент круга `round`: `tfdt` каждого трека сдвинут на круги, `mfhd` — свой номер.
fn shifted_fragment(fragment: &[u8], round: u64, sequence: u32, loops: &[TrackLoop]) -> Vec<u8> {
    let mut shifted = fragment.to_vec();
    for (kind, body, end) in child_boxes(fragment, 0, fragment.len()) {
        if &kind != b"moof" {
            continue;
        }
        for (child_kind, child_body, child_end) in child_boxes(fragment, body, end) {
            if &child_kind == b"mfhd" {
                shifted[child_body + 4..child_body + 8].copy_from_slice(&sequence.to_be_bytes());
            }
            if &child_kind != b"traf" {
                continue;
            }
            let (track_id, _) = traf_track_and_duration(fragment, child_body, child_end);
            let shift = loops
                .iter()
                .find(|known| known.track_id == track_id)
                .expect("known track")
                .loop_ticks
                * round;
            for (traf_kind, traf_body, _) in child_boxes(fragment, child_body, child_end) {
                if &traf_kind == b"tfdt" {
                    // Фикстура пишет `tfdt` версии 1: 64-битное время.
                    assert_eq!(fragment[traf_body], 1, "tfdt version 1");
                    let at = traf_body + 4;
                    let base = u64::from_be_bytes(fragment[at..at + 8].try_into().expect("u64"));
                    shifted[at..at + 8].copy_from_slice(&(base + shift).to_be_bytes());
                }
            }
        }
    }
    shifted
}

/// Эфир из фикстуры по кругу.
fn looped_live() -> LoopedLive {
    let movie = fragmented_movie();
    let loops = track_loops(&movie);
    let per_round = movie.fragments.len();
    let segments = (0..PREBUILT_SEGMENTS)
        .map(|index| {
            shifted_fragment(
                &movie.fragments[index % per_round],
                (index / per_round) as u64,
                index as u32 + 1,
                &loops,
            )
        })
        .collect();
    LoopedLive {
        init: movie.init,
        segments,
    }
}

/// Live-корень, окно которого идёт по часам стены с момента `on_air_since`.
fn live_playlist_now(on_air_since: Instant) -> Vec<u8> {
    let aired =
        INITIAL_SEGMENTS + (on_air_since.elapsed().as_secs_f64() / SEGMENT_SECONDS) as usize;
    let aired = aired.min(PREBUILT_SEGMENTS);
    let first = aired.saturating_sub(PLAYLIST_WINDOW_SEGMENTS);
    let mut text = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:1\n#EXT-X-MEDIA-SEQUENCE:{first}\n\
         #EXT-X-MAP:URI=\"{}\"\n",
        bridge_address(INIT_HANDLE)
    );
    for index in first..aired {
        text.push_str(&format!(
            "#EXTINF:{SEGMENT_SECONDS:.3},\n{}\n",
            bridge_address(FIRST_SEGMENT_HANDLE + index as u32)
        ));
    }
    text.into_bytes()
}

/// Страница с эфиром по часам (отсчёт эфира — с этого момента).
fn live_page() -> HlsPage {
    let live = looped_live();
    let on_air_since = Instant::now();
    let mut resources = vec![
        (
            ROOT_HANDLE,
            PageResource::Generated(Rc::new(move || live_playlist_now(on_air_since))),
        ),
        (INIT_HANDLE, PageResource::Body(live.init)),
    ];
    for (index, segment) in live.segments.into_iter().enumerate() {
        resources.push((
            FIRST_SEGMENT_HANDLE + index as u32,
            PageResource::Body(segment),
        ));
    }
    HlsPage::new(resources)
}

/// Сообщение плеера и когда страница его прочитала.
struct Heard {
    message: PlayerSessionMessage,
    at: Instant,
}

/// Отвечает на запросы `duration` и возвращает всё прочитанное с моментами.
fn serve_for(
    page: &mut HlsPage,
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    what: &str,
    duration: Duration,
) -> Vec<Heard> {
    let deadline = Instant::now() + duration;
    let mut heard = Vec::new();
    page.serve_until(socket, what, |_, message| {
        heard.push(Heard {
            message: message.clone(),
            at: Instant::now(),
        });
        Instant::now() >= deadline
    });
    heard
}

/// Отвечает, пока не придёт сообщение под `stop`; возвращает всё прочитанное.
fn serve_until_heard(
    page: &mut HlsPage,
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    what: &str,
    mut stop: impl FnMut(&PlayerSessionMessage) -> bool,
) -> Vec<Heard> {
    let mut heard = Vec::new();
    page.serve_until(socket, what, |_, message| {
        heard.push(Heard {
            message: message.clone(),
            at: Instant::now(),
        });
        stop(message)
    });
    heard
}

/// Окно эфира словами теста (мкс): край и `[start, end]` (`None` — без DVR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeardWindow {
    generation: u32,
    edge_us: u64,
    range: Option<(u64, u64)>,
}

fn window_of(message: &PlayerSessionMessage) -> Option<HeardWindow> {
    let PlayerSessionMessage::LiveWindow {
        generation,
        edge_us,
        seekable,
    } = message
    else {
        return None;
    };
    let range = match seekable {
        LiveSeekable::None {} => None,
        LiveSeekable::Range { start_us, end_us } => {
            Some((start_us.as_micros(), end_us.as_micros()))
        }
    };
    Some(HeardWindow {
        generation: generation.get(),
        edge_us: edge_us.as_micros(),
        range,
    })
}

fn windows(heard: &[Heard]) -> Vec<HeardWindow> {
    heard
        .iter()
        .filter_map(|heard| window_of(&heard.message))
        .collect()
}

/// Позиции тиков «играю».
fn playing(heard: &[Heard]) -> Vec<u64> {
    heard
        .iter()
        .filter_map(|heard| match heard.message {
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } => Some(position_us.as_micros()),
            _ => None,
        })
        .collect()
}

fn no_rejections(heard: &[Heard]) {
    assert!(
        !heard
            .iter()
            .any(|heard| matches!(heard.message, PlayerSessionMessage::StreamRejected { .. })),
        "the live generation was rejected"
    );
}

/// Последнее окно с DVR среди прочитанного.
fn latest_dvr_range(heard: &[Heard]) -> (u64, u64) {
    windows(heard)
        .iter()
        .rev()
        .find_map(|window| window.range)
        .expect("a live window with a DVR range")
}

/// Страница 1.2 видит окно эфира: оно идёт вместе с эфиром, держит порядок
/// `start ≤ end ≤ edge`, часы играют внутри окна, и окна приходят не чаще раза в секунду.
fn a_1_2_page_gets_a_moving_live_window_around_the_clock(mode: VideoMode, name: &str) {
    let Some(process) = SessionProcessUnderTest::start(name, mode) else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_2);
    announce_hls_generation(&mut socket, ROOT_HANDLE);
    let mut page = live_page();
    let mut heard = serve_until_heard(&mut page, &mut socket, "the first live window", |message| {
        window_of(message).is_some()
    });
    let first_window_at = heard.last().expect("heard").at;
    let observed = Duration::from_secs(6);
    heard.extend(serve_for(
        &mut page,
        &mut socket,
        "six seconds of live",
        observed,
    ));

    let heard_windows = windows(&heard);
    no_rejections(&heard);
    assert!(
        heard_windows.len() >= 3,
        "too few live windows: {heard_windows:?}"
    );
    assert!(
        heard_windows.iter().all(|window| window.generation == 1),
        "{heard_windows:?}"
    );
    for window in &heard_windows {
        if let Some((start, end)) = window.range {
            assert!(start <= end && end <= window.edge_us, "{window:?}");
        }
    }
    assert!(
        heard_windows.iter().any(|window| window.range.is_some()),
        "the live stream never offered a DVR range: {heard_windows:?}"
    );
    // Край идёт вместе с эфиром и не отступает.
    assert!(
        heard_windows
            .windows(2)
            .all(|pair| pair[0].edge_us <= pair[1].edge_us),
        "{heard_windows:?}"
    );
    let first_edge = heard_windows[0].edge_us;
    let last_edge = heard_windows.last().expect("window").edge_us;
    assert!(
        last_edge >= first_edge + 3_000_000,
        "the live edge did not move with the stream: {heard_windows:?}"
    );
    // Предел частоты: за `observed` после первого окна — не больше одного окна в секунду
    // (+1 на округление границ), и между соседними окнами успевает пройти не меньше двух
    // тиков часов (тик — 250 мс; без предела окна шли бы с каждым сегментом).
    let after_first = heard_windows.len() - 1;
    let elapsed = heard.last().expect("heard").at - first_window_at;
    assert!(
        after_first as u64 <= elapsed.as_secs() + 1,
        "{after_first} windows in {elapsed:?}: {heard_windows:?}"
    );
    let mut ticks_since_window = None;
    for heard in &heard {
        match heard.message {
            PlayerSessionMessage::LiveWindow { .. } => {
                if let Some(ticks) = ticks_since_window {
                    assert!(
                        ticks >= 2,
                        "two live windows only {ticks} clock ticks apart"
                    );
                }
                ticks_since_window = Some(0);
            }
            PlayerSessionMessage::Clock { .. } => {
                ticks_since_window = ticks_since_window.map(|ticks| ticks + 1);
            }
            _ => {}
        }
    }
    // Часы и окно — одно время: каждая позиция «играю» внутри последнего окна.
    let mut latest: Option<HeardWindow> = None;
    let mut checked = 0;
    for heard in &heard {
        if let Some(window) = window_of(&heard.message) {
            latest = Some(window);
            continue;
        }
        if let (
            Some(window),
            PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            },
        ) = (latest, &heard.message)
        {
            let position = position_us.as_micros();
            assert!(
                position <= window.edge_us,
                "clock {position} beyond {window:?}"
            );
            if let Some((start, _)) = window.range {
                assert!(position >= start, "clock {position} before {window:?}");
            }
            checked += 1;
        }
    }
    assert!(checked >= 10, "too few playing ticks to compare: {checked}");

    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    assert!(summary.presented_frames > 100, "{}", summary.line);
}

#[test]
fn a_1_2_page_gets_a_moving_live_window_around_the_clock_without_a_window() {
    a_1_2_page_gets_a_moving_live_window_around_the_clock(VideoMode::Headless, "live-window");
}

#[test]
fn a_1_2_page_gets_a_moving_live_window_around_the_clock_in_a_window() {
    a_1_2_page_gets_a_moving_live_window_around_the_clock(VideoMode::Window, "live-window-gui");
}

/// Страница 1.1: ни одного `live_window` (её декодер закрыл бы сеанс), а эфир играет,
/// как раньше.
#[test]
fn a_1_1_page_gets_no_live_window_and_the_live_stream_still_plays() {
    let Some(process) = SessionProcessUnderTest::start("live-window-1-1", VideoMode::Headless)
    else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_1);
    announce_hls_generation(&mut socket, ROOT_HANDLE);
    let mut page = live_page();
    let mut heard = serve_until_heard(&mut page, &mut socket, "live playback", |message| {
        matches!(
            message,
            PlayerSessionMessage::Clock {
                state: ClockState::Playing,
                ..
            }
        )
    });
    heard.extend(serve_for(
        &mut page,
        &mut socket,
        "four seconds of live",
        Duration::from_secs(4),
    ));

    no_rejections(&heard);
    assert!(windows(&heard).is_empty(), "a 1.1 page got a live window");
    let positions = playing(&heard);
    let first = *positions.first().expect("playing ticks");
    let last = *positions.last().expect("playing ticks");
    assert!(
        last >= first + 2_500_000,
        "the live stream did not play: {positions:?}"
    );
    send_tab_gone_and_expect_close(&mut socket);
}

/// HLS-ролик (VOD) на странице 1.2 окна эфира не получает.
#[test]
fn a_vod_hls_generation_gets_no_live_window_on_a_1_2_page() {
    let Some(process) = SessionProcessUnderTest::start("live-window-vod", VideoMode::Headless)
    else {
        return;
    };
    let movie = fragmented_movie();
    let mut resources = vec![
        (
            ROOT_HANDLE,
            PageResource::Body(fmp4_vod_playlist(
                INIT_HANDLE,
                FIRST_SEGMENT_HANDLE,
                movie.fragments.len(),
            )),
        ),
        (INIT_HANDLE, PageResource::Body(movie.init.clone())),
    ];
    for (index, fragment) in movie.fragments.iter().enumerate() {
        resources.push((
            FIRST_SEGMENT_HANDLE + index as u32,
            PageResource::Body(fragment.clone()),
        ));
    }
    let mut socket = open_attached_session_speaking(&process, HELLO_1_2);
    announce_hls_generation(&mut socket, ROOT_HANDLE);
    let mut page = HlsPage::new(resources);
    let messages = page.serve_until_ended(&mut socket);

    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::LiveWindow { .. })),
        "a VOD stream got a live window"
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    assert!(summary.presented_frames >= 90, "{}", summary.line);
}

/// Играет эфир, пока окно DVR не станет шире `span` позади позиции; возвращает
/// прочитанное (последнее окно в нём — с DVR).
fn play_live_until_dvr_covers(
    page: &mut HlsPage,
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    span: Duration,
) -> Vec<Heard> {
    let mut heard = Vec::new();
    let mut latest_start = None;
    let mut latest_position = None;
    heard.extend(serve_until_heard(
        page,
        socket,
        "a DVR window behind the position",
        |message| {
            if let Some(HeardWindow {
                range: Some((start, _)),
                ..
            }) = window_of(message)
            {
                latest_start = Some(start);
            }
            if let PlayerSessionMessage::Clock {
                position_us,
                state: ClockState::Playing,
                ..
            } = message
            {
                latest_position = Some(position_us.as_micros());
            }
            matches!((latest_start, latest_position), (Some(start), Some(position))
            if position >= start + span.as_micros() as u64)
        },
    ));
    heard
}

/// Перемотка страницы назад внутри окна эфира приземляется: часы около цели, кадры идут
/// дальше, сеанс жив.
#[test]
fn a_page_seek_inside_the_live_window_lands_and_playback_goes_on() {
    let Some(process) = SessionProcessUnderTest::start("live-window-seek", VideoMode::Headless)
    else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_2);
    announce_hls_generation(&mut socket, ROOT_HANDLE);
    let mut page = live_page();
    let before = play_live_until_dvr_covers(&mut page, &mut socket, Duration::from_secs(3));
    let (start, _) = latest_dvr_range(&before);
    let position_before = *playing(&before).last().expect("playing");
    // Цель — внутри окна, на 2,5 с позади позиции и не ближе 0,3 с к его началу.
    let target = (position_before - 2_500_000).max(start + 300_000);

    socket.send(page_seek(target)).expect("page seek is sent");
    let landed = serve_until_heard(
        &mut page,
        &mut socket,
        "playback at the target",
        |message| {
            matches!(message, PlayerSessionMessage::Clock { position_us, state: ClockState::Playing, .. }
            if (target..target + 1_000_000).contains(&position_us.as_micros()))
        },
    );
    let after = serve_for(
        &mut page,
        &mut socket,
        "playback after the seek",
        Duration::from_secs(2),
    );

    no_rejections(&landed);
    no_rejections(&after);
    let positions = playing(&after);
    let last = *positions.last().expect("playing after the seek");
    assert!(
        last >= target + 1_500_000 && last < position_before + 1_000_000,
        "playback did not go on from the target {target} (was at {position_before}): {positions:?}"
    );
    // Окно после перемотки — по-прежнему про то же поколение и держит позицию внутри.
    let (start_after, _) = latest_dvr_range(&after);
    assert!(
        start_after <= last,
        "position {last} is before the window start {start_after}"
    );
    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    // Перемотка назад откатом позиции не считается; кадры до и после неё показаны.
    assert_eq!(summary.position_regressions, 0, "{}", summary.line);
    assert!(summary.presented_frames > 100, "{}", summary.line);
}

/// Строки журнала о перемотке как поломке плеера (`dispatch_player_command` → fatal).
const FATAL_SEEK_LINES: [&str; 2] = ["Player worker command failed", "PlayerEvent::FatalError"];

/// Перемотка страницы за окно эфира (гонка: окно уехало, пока страница перематывала):
/// сеанс не рвётся, поколение не отвергнуто, плеер продолжает играть, а часы и следующее
/// окно говорят правду (позиция внутри окна).
///
/// Решение владельца 233: такая перемотка — мягкий отказ команды, а не фатальная ошибка.
/// Раньше ядро делало её фатальной (`dispatch_player_command` → `mark_fatal_error`,
/// состояние `Failed`), и учёт загрузок отвергал поколение
/// (`stream_rejected{playback_failed}`) примерно в трети прогонов.
#[test]
fn a_page_seek_behind_the_live_window_keeps_the_session_playing() {
    let Some(process) = SessionProcessUnderTest::start("live-window-expired", VideoMode::Headless)
    else {
        return;
    };
    // Любая прочитанная строка с фатальной перемоткой роняет тест сразу.
    process.bus.forbid_in_log(&FATAL_SEEK_LINES);
    let mut socket = open_attached_session_speaking(&process, HELLO_1_2);
    announce_hls_generation(&mut socket, ROOT_HANDLE);
    let mut page = live_page();
    let before = play_live_until_dvr_covers(&mut page, &mut socket, Duration::from_secs(1));
    let (start, _) = latest_dvr_range(&before);
    let position_before = *playing(&before).last().expect("playing");
    // Цель — заведомо раньше окна (такого места у плеера уже нет).
    let target = start.saturating_sub(2_000_000);
    assert!(target < start);

    socket.send(page_seek(target)).expect("page seek is sent");
    let after = serve_for(
        &mut page,
        &mut socket,
        "playback after the expired seek",
        Duration::from_secs(4),
    );

    no_rejections(&after);
    let positions = playing(&after);
    let last = *positions
        .last()
        .expect("the player keeps playing after the expired seek");
    assert!(
        last >= position_before + 2_000_000,
        "the player did not go on playing (was at {position_before}): {positions:?}"
    );
    // Последнее, что услышала страница о часах, — «играет» (не пауза и не ошибка).
    let last_clock_state = after.iter().rev().find_map(|heard| match heard.message {
        PlayerSessionMessage::Clock { state, .. } => Some(state),
        _ => None,
    });
    assert_eq!(last_clock_state, Some(ClockState::Playing));
    // Часы не прыгали к цели за окном (откаты ловит сводка `position_regressions`).
    assert!(
        positions.iter().all(|position| *position >= start),
        "the clock jumped behind the live window to {target}: {positions:?}"
    );
    let windows_after = windows(&after);
    let latest = windows_after.last().expect("live windows keep coming");
    assert!(last <= latest.edge_us, "{last} beyond {latest:?}");
    if let Some((start_after, _)) = latest.range {
        assert!(last >= start_after, "{last} before {latest:?}");
    }
    // Ядро честно отказало в перемотке за окно (а не прижало её молча) — и не фатально.
    process.bus.wait_for_log_line("kind=SeekTargetExpired");
    process
        .bus
        .expect_no_log_line_for(Duration::from_millis(300), &FATAL_SEEK_LINES);
    // Сеанс жив: уход вкладки закрывает его штатно (не 4003).
    send_tab_gone_and_expect_close(&mut socket);
    let summary = final_summary(&process.bus);
    assert_eq!(summary.position_regressions, 0, "{}", summary.line);
}

/// Время страницы, в котором тень шлёт стартовую перемотку передачи (её сдвиг ещё не
/// известен): число внутри будущего окна эфира плеера — исполненная, она увела бы плеер
/// с его края на чужое место.
const PAGE_TIME_T0_US: u64 = 3_500_000;

/// Передача эфира так, как её делает тень (пакет C 10a3): `page_seek{T0}` во времени
/// страницы сразу после объявления поколения и ещё раз — пока поколение открывается
/// (на первый запрос сегмента; до ответа на него, так что плеер получает перемотку
/// раньше байтов). Плеер встаёт у своего края: позиция не прыгает к T0, отказа
/// перемотки нет. Фазы «загружено, не показано» и «показано, окна ещё нет» закрывают
/// тесты медиа-части (`session_media/tests/live_hls.rs`): сквозь процесс их моменты не
/// поймать без гонки.
#[test]
fn page_seeks_in_page_time_before_the_first_live_window_are_ignored() {
    let Some(process) = SessionProcessUnderTest::start("live-window-t0", VideoMode::Headless)
    else {
        return;
    };
    let mut socket = open_attached_session_speaking(&process, HELLO_1_2);
    announce_hls_generation(&mut socket, ROOT_HANDLE);
    socket
        .send(page_seek(PAGE_TIME_T0_US))
        .expect("handoff page seek is sent");
    let mut page = live_page();
    // Первый запрос сегмента придержан: сначала перемотка, потом байты.
    page.hold(FIRST_SEGMENT_HANDLE..FIRST_SEGMENT_HANDLE + PREBUILT_SEGMENTS as u32);
    let opening = page.serve_until(&mut socket, "the first segment request", |page, _| {
        page.requests
            .iter()
            .any(|request| request.resource >= FIRST_SEGMENT_HANDLE)
    });
    socket
        .send(page_seek(PAGE_TIME_T0_US))
        .expect("repeated handoff page seek is sent");
    page.release_held(&mut socket);
    let mut heard = serve_until_heard(&mut page, &mut socket, "the first live window", |message| {
        window_of(message).is_some()
    });
    heard.extend(serve_for(
        &mut page,
        &mut socket,
        "live playback",
        Duration::from_secs(3),
    ));

    assert!(
        !opening
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the live generation was rejected while opening"
    );
    no_rejections(&heard);
    // Перемотки к T0 плеер не получал вовсе: ни исполненной, ни отвергнутой ядром.
    process
        .bus
        .expect_no_log_line_for(Duration::from_millis(300), &["kind=SeekTargetExpired"]);
    let positions = playing(&heard);
    assert!(positions.len() >= 8, "{positions:?}");
    let (start, _) = latest_dvr_range(&heard);
    assert!(
        positions[0] < PAGE_TIME_T0_US - 1_500_000,
        "the player did not start at its own live edge (window from {start}): {positions:?}"
    );
    assert!(
        positions.windows(2).all(|pair| pair[1] < pair[0] + 700_000),
        "the position jumped towards the page time T0: {positions:?}"
    );
    send_tab_gone_and_expect_close(&mut socket);
}
