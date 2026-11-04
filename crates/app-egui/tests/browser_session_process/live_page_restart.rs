//! Эфир, который страница перезапускает в том же `MediaSource` (сессия 09b2, решение 194).
//!
//! Живая находка (Twitch, Chromium): зритель жмёт паузу в нашем окне → страница «выгружает»
//! эфир: стирает весь буфер обеих полос и ставит элемент на 0. Play → страница заново
//! грузит эфир В ТОМ ЖЕ `MediaSource` с новой шкалой от ~0 (кладёт байты с ~0,09 с) и
//! перематывается туда. Плеер сперва отвергал эти перемотки (`SeekTargetExpired`), а после
//! починки окна эфира звук пошёл, но картинка стояла ~24 с: полка держала старую шкалу
//! вперемешку с новой. Два корня, оба здесь воспроизведены:
//! - MSE стирает до следующего ключевого кадра, а страница называет только свой диапазон:
//!   подчистки прошлого оставляли на полке хвосты групп кадров без ключевых кадров.
//!   Перемотка возвращала их в очередь, подача уходила в них и вставала в конце старой
//!   шкалы, а байты новой шкалы оказывались «в прошлом»;
//! - шов между эпизодами сравнивал время показа: ключевой кадр нового качества показывается
//!   раньше выданного кадра старого (у того больше задержка показа), хотя декодируется
//!   после него, — группа кадров выбрасывалась как повтор.
//!
//! Что доказывает тест (настоящий процесс, безоконно и в окне): эфир (`duration` live)
//! низкой задержки — по кадру на фрагмент, B-кадры — играет старую шкалу → страница
//! подчищает прошлое, стирает всё и перематывается на 0 → кладёт новую шкалу с другим init
//! и перематывается на 0,089 с посреди докачки → через 1,2 с качество меняется, и новая
//! группа кадров внахлёст по показу с прошлой. Плеер идёт за страницей: часы «играю»
//! внутри новой шкалы, а кадров показано столько, сколько часы прошли, — картинка идёт
//! вровень со звуком; ни одной строки `SeekTargetExpired` в журнале.
//!
//! Оконный вариант нужен `kwin_wayland` (иначе SKIPPED).

use super::generation_switch_clicks::{
    FRAME_US, PageSourceBuffers, SplitMedia, page_seek, recorded_message, split_fmp4,
    split_fmp4_bytes,
};
use super::session_ends::{last_clock_us, send_all, wait_until_playing_past};
use super::*;

/// Объявление эфира: длительность «бесконечна», как её сообщает экстеншен для Twitch.
const LIVE_DURATION: &str =
    r#"{"type":"duration_changed","generation":1,"duration":{"kind":"live"}}"#;
/// Фикстуры источника: ролик низкой задержки.
const SOURCE_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../pushed-media-source/fixtures"
);
/// H.264 с B-кадрами: 40 кадров по 40 мс, по одному на фрагмент, группа из 8 кадров.
const LOW_LATENCY_CLIP: &str = "video-h264-64-bframes-per-frame.mp4";
/// Сколько раз ролик идёт подряд в старой шкале (группы кадров закрытые): 3,2 с.
const OLD_TIMELINE_CLIP_REPEATS: u64 = 2;
/// Сколько фрагментов звука в старой шкале (~4 с): звук не кончается раньше видео.
const OLD_TIMELINE_AUDIO_FRAGMENTS: usize = 4;
/// До какой позиции старая шкала успевает сыграть перед «выгрузкой»: дальше всех подчисток.
const OLD_TIMELINE_PLAYED_TO_US: u64 = 1_400_000;
/// Показ первого кадра ролика низкой задержки.
const FIRST_FRAME_US: u64 = 80_000;
/// Подчистки прошлого (мс): как `remove(buffered.start, …)` у Twitch, каждая начинается с
/// ключевого кадра и стирает его, а MSE сносит и всё до следующего ключевого.
const PAST_TRIMS_MS: [(u64, u64); 4] = [(80, 120), (400, 440), (720, 760), (1_040, 1_080)];
/// Начало «выгрузки» (мс): следующий ключевой кадр, начало буфера страницы.
const UNLOAD_START_MS: u64 = 1_360;
/// Конец стирания (мс): дальше любой шкалы ролика.
const REMOVE_ALL_END_MS: u64 = 10_000;
/// Сколько кадров новой шкалы приходит до второй перемотки страницы (0,4 с).
const NEW_TIMELINE_FRAMES_BEFORE_SEEK: usize = 10;
/// Сколько кадров низкой задержки идёт до смены качества: декодируются раньше 1,2 с, а
/// показываются вплоть до 1,32 с.
const NEW_TIMELINE_FRAMES_BEFORE_SWITCH: usize = 30;
/// С какого фрагмента ролика без B-кадров идёт новое качество (1,2 с).
const SWITCH_FRAGMENT: usize = 1;
/// Вторая перемотка страницы — к началу своих новых байтов (у Twitch 89 319 мкс).
const NEW_TIMELINE_SEEK_US: u64 = 89_319;
/// Плеер обязан играть новую шкалу хотя бы до этой позиции.
const NEW_TIMELINE_PLAYED_TO_US: u64 = 3_600_000;
/// Верхняя граница ожидания часов: конец новой шкалы.
const NEW_TIMELINE_END_US: u64 = 5_400_000;
/// Сколько кадров могут не показаться вокруг перемотки и смены качества (кадры, опоздавшие
/// на доли кадра); дыра в картинке от старой шкалы или шва — десятки кадров.
const FRAMES_LOST_AROUND_SWITCHES: u64 = 6;
/// Пауза страницы между «выгрузкой» и новыми байтами (сеть Twitch).
const RELOAD_DELAY: Duration = Duration::from_millis(500);
/// Пауза между первыми байтами новой шкалы и остальными.
const REST_OF_RELOAD_DELAY: Duration = Duration::from_millis(100);
/// Строки журнала, которых при перезапуске эфира страницей быть не должно.
const EXPIRY_LINES: [&str; 2] = ["SeekTargetExpired", "PlayerEvent::FatalError"];

#[test]
fn a_live_stream_restarted_by_the_page_plays_its_new_timeline() {
    live_restart_by_the_page(VideoMode::Headless);
}

/// То же в окне браузерного режима (путь загрузки `AppState`, настоящий показ).
#[test]
fn a_live_stream_restarted_by_the_page_plays_its_new_timeline_in_a_window() {
    live_restart_by_the_page(VideoMode::Window);
}

/// Время того же фрагмента на шкале, сдвинутой на `ticks` тиков: прибавляет их к `tfdt`
/// каждого `traf`. Боксы фикстуры известны и версия `tfdt` любая.
fn shift_fragment(fragment: &mut [u8], ticks: u64) {
    let tfdt = fragment
        .windows(4)
        .position(|window| window == b"tfdt")
        .expect("a fragment has tfdt");
    let version = fragment[tfdt + 4];
    let body = tfdt + 8;
    if version == 1 {
        let time: [u8; 8] = fragment[body..body + 8].try_into().expect("tfdt v1");
        let shifted = u64::from_be_bytes(time) + ticks;
        fragment[body..body + 8].copy_from_slice(&shifted.to_be_bytes());
    } else {
        let time: [u8; 4] = fragment[body..body + 4].try_into().expect("tfdt v0");
        let shifted = u64::from(u32::from_be_bytes(time)) + ticks;
        let shifted = u32::try_from(shifted).expect("the clip is short");
        fragment[body..body + 4].copy_from_slice(&shifted.to_be_bytes());
    }
}

/// `tfdt` фрагмента в тиках (его шкала нужна только как разность двух фрагментов).
fn decode_time(fragment: &[u8]) -> u64 {
    let tfdt = fragment
        .windows(4)
        .position(|window| window == b"tfdt")
        .expect("a fragment has tfdt");
    let body = tfdt + 8;
    if fragment[tfdt + 4] == 1 {
        u64::from_be_bytes(fragment[body..body + 8].try_into().expect("tfdt v1"))
    } else {
        u64::from(u32::from_be_bytes(
            fragment[body..body + 4].try_into().expect("tfdt v0"),
        ))
    }
}

/// Тот же init другими байтами: перезапуск даёт новый init с тем же смыслом, а источник
/// отличает новый init от повтора по байтам. Меняется только `creation_time` в `mvhd`.
fn with_other_bytes(init: &[u8]) -> Vec<u8> {
    let mut other = init.to_vec();
    let movie_header = other
        .windows(4)
        .position(|window| window == b"mvhd")
        .expect("an init has mvhd");
    // За типом бокса: версия и флаги (4 байта), затем `creation_time`.
    other[movie_header + 8] ^= 1;
    other
}

/// Страница эфира: одно поколение, полосы 0 — звук, 1 — видео.
struct LivePage {
    buffers: PageSourceBuffers,
    audio: SplitMedia,
    /// Ролик без B-кадров: новое качество новой шкалы.
    video: SplitMedia,
    /// Ролик низкой задержки: старая шкала и начало новой.
    low_latency: SplitMedia,
}

impl LivePage {
    fn new() -> Self {
        let bytes = std::fs::read(Path::new(SOURCE_FIXTURES).join(LOW_LATENCY_CLIP))
            .expect("low latency clip");
        Self {
            buffers: PageSourceBuffers::new(1),
            audio: split_fmp4("audio-6s.mp4"),
            video: split_fmp4("video-64-6s.mp4"),
            low_latency: split_fmp4_bytes(&bytes),
        }
    }

    /// Объявление эфира и старая шкала: ролик низкой задержки, повторённый подряд.
    fn old_timeline(&mut self) -> Vec<Message> {
        let mut messages = vec![
            recorded_message("001.json"),
            Message::text(LIVE_DURATION),
            recorded_message("003.json"),
            recorded_message("004.json"),
            self.buffers.frame(0, &self.audio.init),
            self.buffers.frame(1, &self.low_latency.init),
        ];
        for fragment in &self.audio.fragments[..OLD_TIMELINE_AUDIO_FRAGMENTS] {
            messages.push(self.buffers.frame(0, fragment));
        }
        let frame_ticks = decode_time(&self.low_latency.fragments[1])
            - decode_time(&self.low_latency.fragments[0]);
        let clip_ticks = frame_ticks * self.low_latency.fragments.len() as u64;
        for repeat in 0..OLD_TIMELINE_CLIP_REPEATS {
            for fragment in &self.low_latency.fragments {
                let mut shifted = fragment.clone();
                shift_fragment(&mut shifted, clip_ticks * repeat);
                messages.push(self.buffers.frame(1, &shifted));
            }
        }
        messages
    }

    /// Начало новой шкалы: новый init и первые кадры, до второй перемотки страницы.
    fn new_timeline_start(&mut self) -> Vec<Message> {
        let mut messages = vec![
            self.buffers.frame(0, &self.audio.init),
            self.buffers
                .frame(1, &with_other_bytes(&self.low_latency.init)),
        ];
        for fragment in &self.audio.fragments {
            messages.push(self.buffers.frame(0, fragment));
        }
        for fragment in &self.low_latency.fragments[..NEW_TIMELINE_FRAMES_BEFORE_SEEK] {
            messages.push(self.buffers.frame(1, fragment));
        }
        messages
    }

    /// Остаток новой шкалы: кадры низкой задержки до 1,2 с и новое качество внахлёст по
    /// показу: его ключевой кадр показывается раньше уже выданного.
    fn new_timeline_rest(&mut self) -> Vec<Message> {
        let mut messages = Vec::new();
        let frames = NEW_TIMELINE_FRAMES_BEFORE_SEEK..NEW_TIMELINE_FRAMES_BEFORE_SWITCH;
        for fragment in &self.low_latency.fragments[frames] {
            messages.push(self.buffers.frame(1, fragment));
        }
        messages.push(self.buffers.frame(1, &self.video.init));
        for fragment in &self.video.fragments[SWITCH_FRAGMENT..] {
            messages.push(self.buffers.frame(1, fragment));
        }
        messages
    }
}

/// `SourceBuffer.remove(start, end)` полосы.
fn removal(source_buffer: u16, start_ms: u64, end_ms: u64) -> Message {
    let (start_us, end_us) = (start_ms * 1_000, end_ms * 1_000);
    Message::text(format!(
        r#"{{"type":"source_buffer_removed","generation":1,"source_buffer":{source_buffer},"start_us":{start_us},"end":{{"kind":"at","time_us":{end_us}}}}}"#
    ))
}

/// «Выгрузка» эфира, как Twitch на паузе: подчистки прошлого видео и стирание всего, что
/// осталось у обеих полос.
fn unload_everything() -> Vec<Message> {
    let mut messages: Vec<Message> = PAST_TRIMS_MS
        .iter()
        .map(|(start_ms, end_ms)| removal(1, *start_ms, *end_ms))
        .collect();
    messages.push(removal(0, 0, REMOVE_ALL_END_MS));
    messages.push(removal(1, UNLOAD_START_MS, REMOVE_ALL_END_MS));
    messages
}

/// Часы «играю» внутри новой шкалы, далеко за стыком смены качества и старой шкалой.
fn is_playing_the_new_timeline(message: &PlayerSessionMessage) -> bool {
    matches!(
        message,
        PlayerSessionMessage::Clock {
            state: ClockState::Playing,
            position_us,
            ..
        } if (NEW_TIMELINE_PLAYED_TO_US..NEW_TIMELINE_END_US).contains(&position_us.as_micros())
    )
}

fn live_restart_by_the_page(mode: VideoMode) {
    let Some(process) = SessionProcessUnderTest::start("live-page-restart", mode) else {
        return;
    };
    let bus = &process.bus;
    // Любая прочитанная строка с отказом перемотки роняет тест сразу.
    bus.forbid_in_log(&EXPIRY_LINES);
    let mut socket = process.open_attached_session();
    let mut page = LivePage::new();
    send_all(&mut socket, page.old_timeline());
    let old_position_us = wait_until_playing_past(&mut socket, OLD_TIMELINE_PLAYED_TO_US);

    // «Выгрузка» эфира: всё стёрто, элемент страницы на 0.
    send_all(&mut socket, unload_everything());
    socket.send(page_seek(0)).expect("page seek is sent");
    // Пока страница грузит эфир заново, плеер ждёт её, а не отвергает перемотку.
    bus.expect_no_log_line_for(RELOAD_DELAY, &EXPIRY_LINES);
    // Перезапуск: новая шкала с начала, перемотка к её первым байтам посреди докачки.
    send_all(&mut socket, page.new_timeline_start());
    socket
        .send(page_seek(NEW_TIMELINE_SEEK_US))
        .expect("page seek is sent");
    bus.expect_no_log_line_for(REST_OF_RELOAD_DELAY, &EXPIRY_LINES);
    send_all(&mut socket, page.new_timeline_rest());
    let messages = read_player_messages_until(
        &mut socket,
        "a playing clock inside the new timeline",
        is_playing_the_new_timeline,
    );
    assert!(
        !messages
            .iter()
            .any(|message| matches!(message, PlayerSessionMessage::StreamRejected { .. })),
        "the stream was rejected: {messages:?}"
    );

    let new_position_us = last_clock_us(&messages).expect("a clock");

    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line_without("Browser playback summary kind=final", &[]);
    // Картинка шла вровень с часами: до выгрузки — кадры старой шкалы от её первого кадра до
    // позиции часов, после неё — от цели до позиции часов. Застывшая картинка при идущем звуке
    // не показала бы десятки этих кадров.
    let old_frames = (old_position_us - FIRST_FRAME_US) / FRAME_US;
    let new_frames = (new_position_us - NEW_TIMELINE_SEEK_US) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames")
            >= old_frames + new_frames - FRAMES_LOST_AROUND_SWITCHES,
        "{summary}"
    );
}
