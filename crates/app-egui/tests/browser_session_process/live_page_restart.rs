//! Эфир, который страница перезапускает в том же `MediaSource` (сессия 09b2, решение 194).
//!
//! Живая находка (Twitch, Chromium): зритель жмёт паузу в нашем окне → страница «выгружает»
//! эфир: стирает весь буфер обеих полос и ставит элемент на 0. Play → страница заново
//! грузит эфир В ТОМ ЖЕ `MediaSource` с новой шкалой от ~0 (кладёт байты с ~0,09 с) и
//! перематывает элемент туда. Плеер отвергал эти перемотки страницы:
//! `SeekTargetExpired: Live seek target 0 ms is outside latest DVR range 53,9…56,8 с` —
//! окно перемотки эфира источник считал только по своей полке, а страница, хозяин шкалы,
//! уже стёрла старое и ушла в новое место. Плеер стоял в ожидании, пока новая шкала не
//! доросла до старой позиции (~55 с).
//!
//! Что доказывает тест (настоящий процесс, безоконно и в окне): эфир (`duration` live)
//! играет со «старой» шкалы (2,4–6 с ролика) → страница стирает всё и перематывается на 0
//! → кладёт новую шкалу с начала ролика (0–2,4 с) и перематывается на 0,089 с → плеер идёт
//! за страницей: часы «играю» внутри новой шкалы, кадры новой шкалы дошли до показа, ни
//! одной строки `SeekTargetExpired` в журнале.
//!
//! Оконный вариант нужен `kwin_wayland` (иначе SKIPPED).

use super::generation_switch_clicks::{
    FRAME_US, PageSourceBuffers, SplitMedia, VIDEO_FRAGMENT_US, page_seek, recorded_message,
    split_fmp4,
};
use super::session_ends::{send_all, wait_until_playing_past};
use super::*;

/// Объявление эфира: длительность «бесконечна», как её сообщает экстеншен для Twitch.
const LIVE_DURATION: &str =
    r#"{"type":"duration_changed","generation":1,"duration":{"kind":"live"}}"#;
/// Старая шкала эфира начинается с 3-го фрагмента видео ролика (2,4 с) …
const OLD_TIMELINE_FIRST_VIDEO_FRAGMENT: usize = 2;
/// … и 3-го фрагмента звука (~2 с).
const OLD_TIMELINE_FIRST_AUDIO_FRAGMENT: usize = 2;
/// Начало старой шкалы (видео): всё раньше — только новая шкала.
const OLD_TIMELINE_START_US: u64 = 2 * VIDEO_FRAGMENT_US;
/// До какой позиции старая шкала успевает сыграть перед «выгрузкой».
const OLD_TIMELINE_PLAYED_TO_US: u64 = 3_000_000;
/// Новая шкала: видео 0–2,4 с (два фрагмента), звук 0–3 с (три фрагмента).
const NEW_TIMELINE_VIDEO_FRAGMENTS: usize = 2;
const NEW_TIMELINE_AUDIO_FRAGMENTS: usize = 3;
/// Вторая перемотка страницы — к началу своих новых байтов (у Twitch 89 319 мкс).
const NEW_TIMELINE_SEEK_US: u64 = 89_319;
/// Плеер обязан играть новую шкалу хотя бы до этой позиции (раньше старой шкалы).
const NEW_TIMELINE_PLAYED_TO_US: u64 = 1_200_000;
/// Пауза страницы между «выгрузкой» и новыми байтами (сеть Twitch).
const RELOAD_DELAY: Duration = Duration::from_millis(500);
/// Конец стирания: дальше любой шкалы ролика.
const REMOVE_ALL_END_US: u64 = 10_000_000;
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

/// Страница эфира: одно поколение, полосы 0 — звук, 1 — видео.
struct LivePage {
    buffers: PageSourceBuffers,
    audio: SplitMedia,
    video: SplitMedia,
}

impl LivePage {
    fn new() -> Self {
        Self {
            buffers: PageSourceBuffers::new(1),
            audio: split_fmp4("audio-6s.mp4"),
            video: split_fmp4("video-64-6s.mp4"),
        }
    }

    /// Объявление эфира и старая шкала: страница подключилась к эфиру посреди ролика.
    fn old_timeline(&mut self) -> Vec<Message> {
        let mut messages = vec![
            recorded_message("001.json"),
            Message::text(LIVE_DURATION),
            recorded_message("003.json"),
            recorded_message("004.json"),
        ];
        messages.extend(self.timeline(
            OLD_TIMELINE_FIRST_AUDIO_FRAGMENT..self.audio.fragments.len(),
            OLD_TIMELINE_FIRST_VIDEO_FRAGMENT..self.video.fragments.len(),
        ));
        messages
    }

    /// Новая шкала после перезапуска: init заново и начало ролика.
    fn new_timeline(&mut self) -> Vec<Message> {
        self.timeline(
            0..NEW_TIMELINE_AUDIO_FRAGMENTS,
            0..NEW_TIMELINE_VIDEO_FRAGMENTS,
        )
    }

    /// Init обеих полос и указанные фрагменты.
    fn timeline(
        &mut self,
        audio_fragments: std::ops::Range<usize>,
        video_fragments: std::ops::Range<usize>,
    ) -> Vec<Message> {
        let mut messages = vec![
            self.buffers.frame(0, &self.audio.init),
            self.buffers.frame(1, &self.video.init),
        ];
        for fragment in &self.audio.fragments[audio_fragments] {
            messages.push(self.buffers.frame(0, fragment));
        }
        for fragment in &self.video.fragments[video_fragments] {
            messages.push(self.buffers.frame(1, fragment));
        }
        messages
    }
}

/// Страница стирает всё у обеих полос (`SourceBuffer.remove(0, …)`), как Twitch на паузе.
fn remove_everything() -> Vec<Message> {
    (0..2)
        .map(|source_buffer| {
            Message::text(format!(
                r#"{{"type":"source_buffer_removed","generation":1,"source_buffer":{source_buffer},"start_us":0,"end":{{"kind":"at","time_us":{REMOVE_ALL_END_US}}}}}"#
            ))
        })
        .collect()
}

/// Часы «играю» внутри новой шкалы (раньше начала старой).
fn is_playing_the_new_timeline(message: &PlayerSessionMessage) -> bool {
    matches!(
        message,
        PlayerSessionMessage::Clock {
            state: ClockState::Playing,
            position_us,
            ..
        } if (NEW_TIMELINE_PLAYED_TO_US..OLD_TIMELINE_START_US).contains(&position_us.as_micros())
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
    wait_until_playing_past(&mut socket, OLD_TIMELINE_PLAYED_TO_US);

    // «Выгрузка» эфира: всё стёрто, элемент страницы на 0.
    send_all(&mut socket, remove_everything());
    socket.send(page_seek(0)).expect("page seek is sent");
    // Пока страница грузит эфир заново, плеер ждёт её, а не отвергает перемотку.
    bus.expect_no_log_line_for(RELOAD_DELAY, &EXPIRY_LINES);
    // Перезапуск: новая шкала с начала и перемотка к её первым байтам.
    send_all(&mut socket, page.new_timeline());
    socket
        .send(page_seek(NEW_TIMELINE_SEEK_US))
        .expect("page seek is sent");
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

    send_tab_gone_and_expect_close(&mut socket);
    let summary = bus.wait_for_log_line_without("Browser playback summary kind=final", &[]);
    // Кадры обеих шкал дошли до показа: старая (2,4 → 3 с) и новая (0,089 → 1,2 с).
    let old_frames = (OLD_TIMELINE_PLAYED_TO_US - OLD_TIMELINE_START_US) / FRAME_US;
    let new_frames = (NEW_TIMELINE_PLAYED_TO_US - NEW_TIMELINE_SEEK_US) / FRAME_US;
    assert!(
        log_number(&summary, "presented_frames") >= old_frames + new_frames,
        "{summary}"
    );
}
